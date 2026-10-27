#!/usr/bin/env bash
# Qualify the embedded default through real acquisition, startup, RPC, and offline restart.
set -euo pipefail
TASK_REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -z "${THRUPUT_CLI:-}" ]]; then
    THRUPUT_CLI="$(cargo build --locked --manifest-path "$TASK_REPO_ROOT/Cargo.toml" \
        -p thruput-cli --bin thruput --message-format=json | python3 -c '
import json, sys
artifacts = [json.loads(line) for line in sys.stdin]
paths = [a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact"
         and a.get("target", {}).get("name") == "thruput" and a.get("executable")]
if len(paths) != 1:
    raise SystemExit("expected one thruput executable from Cargo")
print(paths[0])')"
fi
python3 - "$THRUPUT_CLI" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request


def snapshot(directory):
    return {str(p.relative_to(directory)): (p.stat().st_mtime_ns, hashlib.sha256(p.read_bytes()).hexdigest())
            for p in directory.rglob('*') if p.is_file()}


root = Path(tempfile.mkdtemp(prefix='thruput-managed-smoke-'))
session = None


def session_processes():
    if session is None:
        return []
    pids = subprocess.check_output(['ps', '-axo', 'pid='], text=True).split()
    owned = []
    for value in pids:
        pid = int(value)
        try:
            if os.getsid(pid) == session:
                owned.append(pid)
        except ProcessLookupError:
            pass
    return owned


def stop_session():
    # hog-devnode creates its own process group, but stays in this smoke's session.
    for signum in (signal.SIGTERM, signal.SIGKILL):
        for pid in session_processes():
            try:
                if os.getsid(pid) == session:
                    os.kill(pid, signum)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 5
        while session_processes() and time.monotonic() < deadline:
            time.sleep(0.1)
        if not session_processes():
            return
    raise RuntimeError(f'owned processes remain; preserving smoke directory {root}')


try:
    # Remove sibling discovery as well as environment overrides: this must exercise acquisition.
    binary = root / 'thruput'
    shutil.copy2(Path(sys.argv[1]).resolve(), binary)
    env = os.environ.copy()
    for name in ('THRUPUT_DEVNODE_BINARY', 'THRUPUT_TOOLCHAIN_BASE_URL', 'THRUPUT_OFFLINE',
                 'THRUPUT_PROFILE', 'HOGD_PROTOCOL_OVERRIDE'):
        env.pop(name, None)
    env['XDG_CONFIG_HOME'] = str(root / 'config')
    env['XDG_CACHE_HOME'] = str(root / 'cache')

    def command(*args):
        return subprocess.run([str(binary), *args], env=env, cwd=root,
                              capture_output=True, text=True, timeout=60)

    def check_status():
        before = snapshot(root / 'cache' / 'thruput' / 'toolchain')
        result = command('--json', 'status')
        # Other diagnostics can fail on a fresh developer machine; managed discovery must work.
        payload = json.loads(result.stdout)
        devnode = payload['data']['environment']['devnode']
        assert devnode['reachable'], devnode
        assert str(root / 'cache') in devnode['path'], devnode
        assert snapshot(root / 'cache' / 'thruput' / 'toolchain') == before, 'status mutated cache'

    for attempt, offline in enumerate((False, True, True)):
        kill_parent = attempt == 2
        phase = 'parent SIGKILL' if kill_parent else ('offline restart' if offline else 'fresh-cache startup')
        args = [str(binary), '--json'] + (['--offline'] if offline else [])
        args += ['node', '--port', '0', '--data-dir', str(root / 'node')]
        output = root / ('offline.json' if offline else 'online.json')
        errors = root / ('offline.log' if offline else 'online.log')
        with output.open('w') as stdout, errors.open('w') as stderr:
            process = subprocess.Popen(args, env=env, cwd=root, stdout=stdout, stderr=stderr, start_new_session=True)
            session = process.pid
            try:
                deadline = time.monotonic() + 90
                startup = None
                while time.monotonic() < deadline:
                    lines = output.read_text().splitlines()
                    if lines:
                        startup = json.loads(lines[0])
                        break
                    if process.poll() is not None:
                        raise RuntimeError(f'{phase} exited {process.returncode}: {errors.read_text()}')
                    time.sleep(0.1)
                if startup is None:
                    raise RuntimeError(f'{phase} timed out: {errors.read_text()}')
                assert startup['ok'], startup
                data = startup['data']
                request = urllib.request.Request(data['rpcUrl'], data=json.dumps({
                    'jsonrpc': '2.0', 'id': 1, 'method': 'getChainTip', 'params': {}
                }).encode(), headers={'Content-Type': 'application/json'})
                with urllib.request.urlopen(request, timeout=10) as response:
                    rpc = json.load(response)
                assert 'result' in rpc and 'error' not in rpc, rpc
                if offline:
                    assert data['chainState'] == 'recovered', data
                cleanup = command('--json', 'toolchain', 'clean')
                assert cleanup.returncode != 0, 'cleanup removed a running node bundle'
                assert 'busy' in cleanup.stderr.lower() or 'busy' in cleanup.stdout.lower(), cleanup
                check_status()
                if kill_parent:
                    rows = subprocess.check_output(['ps', '-axo', 'pid=,ppid='], text=True).splitlines()
                    leaders = [int(row.split()[0]) for row in rows if int(row.split()[1]) == process.pid]
                    assert len(leaders) == 1, f'expected one devnode child, got {leaders}'
                    process.kill()
                    process.wait(timeout=10)
                    assert session_processes(), 'parent exit unexpectedly terminated every backend'
                    cleanup = command('--json', 'toolchain', 'clean')
                    assert cleanup.returncode != 0, 'parent exit released the live backend bundle lock'
                    assert 'busy' in cleanup.stderr.lower() or 'busy' in cleanup.stdout.lower(), cleanup
                    os.kill(leaders[0], signal.SIGKILL)
                    deadline = time.monotonic() + 5
                    while leaders[0] in session_processes() and time.monotonic() < deadline:
                        time.sleep(0.05)
                    if session_processes():
                        cleanup = command('--json', 'toolchain', 'clean')
                        assert cleanup.returncode != 0, 'devnode leader exit released its descendants bundle lock'
                        assert 'busy' in cleanup.stderr.lower() or 'busy' in cleanup.stdout.lower(), cleanup
                    else:
                        print('Published backend has no surviving descendants after leader exit', flush=True)
                print(f'Passed {phase}, chainTip RPC, and cleanup refusal while running', flush=True)
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                        raise RuntimeError('node did not stop cleanly')
                survivors = session_processes()
                stop_session()
                if survivors and process.returncode == 0:
                    raise RuntimeError('CLI exited without stopping its backend processes')
            assert process.returncode == (-signal.SIGKILL if kill_parent else 0), errors.read_text()
        check_status()
    result = command('toolchain', 'which')
    assert result.returncode == 0 and Path(result.stdout.strip()).is_file(), result
    result = command('--json', 'toolchain', 'clean')
    assert result.returncode == 0, result
    assert json.loads(result.stdout)['data']['changed'], result
    print('Passed managed discovery, clean stop, non-mutating status, and cleanup after shutdown', flush=True)
finally:
    stop_session()
    shutil.rmtree(root)
PY
