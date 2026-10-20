#!/usr/bin/env python3
"""Own an isolated node/gateway and persist repeated confirmed-read trials."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import shutil

from bench_diagnostics import Capture
import signal
import statistics
import subprocess
import time
import urllib.request


def positive(value):
    value = int(value)
    if value < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("cli", "devnode", "gateway", "bench", "out"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--candidate-gateway", type=Path)
    parser.add_argument("--scenario", choices=("read-confirmed", "pool-sweep"), default="read-confirmed")
    parser.add_argument("--pools", default=None, help="comma-separated indexer pool sizes")
    parser.add_argument("--outstanding", default="128,512")
    parser.add_argument("--repeats", type=positive, default=2)
    parser.add_argument("--count", type=positive, default=12000)
    parser.add_argument("--duration", type=positive, default=20)
    parser.add_argument("--warmup", type=positive, default=3)
    parser.add_argument("--runtime-workers", type=positive, default=8)
    parser.add_argument("--workers", type=positive, default=2)
    parser.add_argument("--setup-timeout", type=positive, default=600)
    parser.add_argument("--build-profile", choices=("release", "debug"), default="release", help="declared build profile of supplied artifacts")
    parser.add_argument("--gateway-log-filter", choices=("off", "error", "warn", "info", "debug", "trace"), default="warn")
    parser.add_argument("--gateway-log-sink", choices=("file", "discard"), default="file")
    parser.add_argument("--diagnostic", choices=("trace", "perf"), help="one separate diagnostic run, excluded from comparisons")
    parser.add_argument("--diagnostic-method", choices=("getTransactionStatus", "getTransactionReceipt"), default="getTransactionStatus")
    parser.add_argument("--diagnostic-seconds", type=positive, default=5)
    parser.add_argument("--diagnostic-max-mib", type=positive, default=16)
    args = parser.parse_args()
    pools = [int(v) for v in (args.pools or ("4,8,16,32,64" if args.scenario == "pool-sweep" else "16")).split(",")]
    outstanding = [int(v) for v in args.outstanding.split(",")]
    if len(set(pools)) != len(pools) or len(set(outstanding)) != len(outstanding):
        parser.error("pool sizes and outstanding counts must not contain duplicates")
    if any(v < 1 or v > 256 for v in pools) or any(v < 1 or v > 65536 for v in outstanding):
        parser.error("pool sizes must be 1..256 and outstanding 1..65536")
    if args.diagnostic:
        if args.candidate_gateway or len(pools) != 1 or len(outstanding) != 1:
            parser.error("diagnostics require one pool, one outstanding count, and no candidate")
        if args.diagnostic_seconds > 30 or args.diagnostic_max_mib > 64:
            parser.error("diagnostics are bounded to 30 seconds and 64 MiB")
        if args.diagnostic_seconds > args.warmup + args.duration:
            parser.error("diagnostic capture must fit within warmup plus measured load duration")
        if args.diagnostic == "perf" and (platform.system() != "Linux" or not shutil.which("perf")):
            parser.error("perf diagnostics require Linux and perf on PATH")
    for name in ("cli", "devnode", "gateway", "bench", "candidate_gateway"):
        value = getattr(args, name)
        if value is not None:
            value = value.resolve(strict=True)
            if not os.access(value, os.X_OK):
                parser.error(f"{name} is not executable")
            setattr(args, name, value)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, TOKIO_WORKER_THREADS=str(args.runtime_workers), RUST_LOG="warn",
               XDG_CONFIG_HOME=str(out / "config"), XDG_CACHE_HOME=str(out / "cache"))
    children, handles, trials = [], [], []
    capture = None

    def interrupted(signum, _frame):
        raise KeyboardInterrupt(f"received signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)


    def write(path, value):
        path.write_text(json.dumps(value, indent=2) + "\n")

    def launch(argv, name, gateway_process=False, trace=False):
        discard = gateway_process and args.gateway_log_sink == "discard"
        stdout = open(os.devnull, "w") if discard else (out / f"{name}.stdout").open("w")
        stderr = open(os.devnull, "w") if discard else (out / f"{name}.stderr").open("w")
        handles.extend((stdout, stderr))
        process = subprocess.Popen(list(map(str, argv)), env=dict(env, RUST_LOG="info" if trace else args.gateway_log_filter, NO_COLOR="1") if gateway_process else env, stdout=subprocess.PIPE if trace else stdout, stderr=stderr)
        children.append(process)
        if trace:
            capture.drain_trace(process.stdout)
        return process

    def stop(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=45)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)

    def free_port():
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            return sock.getsockname()[1]

    rpc_port, metrics_port = free_port(), free_port()
    url = f"http://127.0.0.1:{rpc_port}"

    def rpc(method):
        request = urllib.request.Request(url, data=json.dumps({"jsonrpc": "2.0", "id": 1,
            "method": method, "params": {}}).encode(), headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=2) as response:
            value = json.load(response)
        if "error" in value:
            raise RuntimeError(str(value["error"]))
        return value["result"]

    def wait_until(process, predicate, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError("owned process exited; inspect stdout/stderr")
            try:
                if predicate():
                    return
            except (OSError, ValueError, RuntimeError):
                pass
            time.sleep(0.2)
        raise RuntimeError("readiness deadline exceeded")

    def digest(path):
        with path.open("rb") as source:
            return "0x" + hashlib.file_digest(source, "sha256").hexdigest()

    cpu_model = None
    if platform.system() == "Linux":
        cpu_model = next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), None)
    host = dict(os=platform.system().lower(), arch=platform.machine(), cpuModel=cpu_model,
                logicalCpus=os.cpu_count(), osRelease=platform.release())
    binary_hashes = {str(path): digest(path) for path in (args.cli, args.gateway, args.devnode, args.bench, args.candidate_gateway) if path}
    write(out / "experiment.json", {"arguments": {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
          "host": host, "artifacts": binary_hashes, "ordering": "alternate forward/reverse pools and gateway variants per repeat",
          "cacheState": "warm; full corpus validated before each trial"})
    manifest = out / "node/runtime/runtime-manifest.json"
    corpus = out / "hashes.json"

    def gateway(binary, size, name, diagnostic=False):
        argv = [binary, "--runtime-manifest", manifest, "--listen", f"127.0.0.1:{rpc_port}",
                "--observability-listen", f"127.0.0.1:{metrics_port}", "--indexer-pool-size", str(size)]
        # The binary resolves its defaults; the runner does not duplicate them.
        effective = json.loads(subprocess.check_output(list(map(str, argv + ["--dump-effective-config"])),
                                                       env=dict(env, RUST_LOG="off"), timeout=30))
        trace = diagnostic and args.diagnostic == "trace"
        if trace:
            argv += ["--trace-spans"]
        process = launch(argv, name, gateway_process=True, trace=trace)
        wait_until(process, lambda: rpc("getChainTip"), 120)
        runtime = json.loads(manifest.read_text())
        info = {"schemaVersion": 1, "topology": "loopback gateway and isolated interval-driven devnode on one host",
            "gateway": {"artifactSha256": binary_hashes[str(binary)], "commitHash": rpc("getVersionInfo").get("commitHash"),
                "buildProfile": args.build_profile, "workerThreads": args.runtime_workers,
                "effectiveConfig": effective, "host": host,
                "logging": {"filter": "info" if trace else args.gateway_log_filter, "sink": "bounded-pipe" if trace else args.gateway_log_sink, "spanEvents": "close" if trace else "none"}},
            "backend": {"artifactSha256": binary_hashes[str(args.devnode)],
                "commitHash": runtime["producer"].get("commitHash"), "host": host,
                "effectiveConfig": {"ramdbBytes": 2147483648, "initialAllocationCount": len(allocations), "allocationFileSha256": digest(allocation_path), "blockDriver": "interval", "blockIntervalMs": 500}}}
        if diagnostic:
            info["diagnostic"] = args.diagnostic
        target = out / f"{name}-target.json"
        write(target, info)
        return process, target

    def bench(name, method, concurrency, process, target, seed=False, diagnostic=False):
        argv = [args.bench, "--url", url, "--runtime-manifest", manifest, "--method", method,
            "--outstanding", str(concurrency), "--workers", str(args.workers),
            "--warmup", str(args.warmup), "--duration", str(args.duration), "--json",
            "--target-info", target, "--scenario", args.scenario, "--setup-timeout-secs", str(args.setup_timeout),
            "--metrics-url", f"http://127.0.0.1:{metrics_port}/metrics"]
        if platform.system() == "Linux":
            argv += ["--gateway-pid", str(process.pid)]
        if seed:
            argv += ["--seed-count", str(args.count), "--seed-output", corpus, "--accounts", "100", "--account-seed", fixture_seed, "--force"]
        else:
            argv += ["--hashes-file", corpus]
        marker = out / "load-start"
        if diagnostic:
            argv += ["--load-start-file", marker]
        with (out / f"{name}.json").open("w") as stdout, (out / f"{name}.stderr").open("w") as stderr:
            load = subprocess.Popen(list(map(str, argv)), env=env, stdout=stdout, stderr=stderr)
            children.append(load)
            deadline = time.monotonic() + args.setup_timeout + args.duration + args.warmup + 60
            while load.poll() is None:
                if diagnostic and capture.started is None and marker.exists():
                    capture.start(process.pid)
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"benchmark deadline exceeded: {name}")
                time.sleep(0.02)
            if load.returncode:
                raise RuntimeError(f"benchmark exited {load.returncode}: {name}")
        result = json.loads((out / f"{name}.json").read_text())
        if result["outcome"] != "passed" or result["requests"]["successful"] == 0:
            raise RuntimeError(f"invalid trial {name}")
        return result

    fixture_seed = "read-confirmed-fixtures-v1"
    allocations = []
    for index in range(100):
        seed = hashlib.sha256(f"groundhog-bench:{fixture_seed}:account-{index}".encode()).hexdigest()
        imported = subprocess.run([str(args.cli), "--json", "--non-interactive", "keys", "import", f"bench-{index}"],
                                  input=seed + "\n", text=True, env=env, capture_output=True, check=True)
        key = json.loads(imported.stdout)["data"]["key"]
        allocations.append(dict(publicKey=key["publicKey"], initialStorageTokens="1000000000"))
    allocation_path = out / "bootstrap-accounts.json"
    write(allocation_path, allocations)

    try:
        node = launch([args.cli, "--json", "node", "--port", "0", "--hog-devnode-binary", args.devnode,
            "--data-dir", out / "node", "--ramdb-bytes", "2147483648", "--bootstrap-accounts", allocation_path,
            "--block-driver", "interval", "--block-interval-ms", "500"], "node")
        wait_until(node, lambda: manifest.exists() and (out / "node.stdout").stat().st_size, 120)
        process, target = gateway(args.gateway, pools[0], "seed-gateway")
        bench("seed", "getTransactionReceipt", 8, process, target, seed=True)
        stop(process)
        if args.diagnostic:
            capture = Capture(args.diagnostic, out, args.diagnostic_seconds, args.diagnostic_max_mib * 1024 * 1024)
            process, target = gateway(args.gateway, pools[0], "diagnostic-gateway", diagnostic=True)
            bench("diagnostic-result", args.diagnostic_method, outstanding[0], process, target, diagnostic=True)
            return
        variants = [("baseline", args.gateway)]
        if args.candidate_gateway:
            variants.append(("candidate", args.candidate_gateway))
        for repeat in range(args.repeats):
            for size in pools if repeat % 2 == 0 else list(reversed(pools)):
                for variant, binary in variants if repeat % 2 == 0 else list(reversed(variants)):
                    name = f"r{repeat:02d}-pool{size}-{variant}"
                    process, target = gateway(binary, size, name)
                    for concurrency in outstanding:
                        for method in ("getTransactionStatus", "getTransactionReceipt"):
                            trial_name = f"{name}-n{concurrency}-{method}"
                            result = bench(trial_name, method, concurrency, process, target)
                            trials.append(dict(file=f"{trial_name}.json", repeat=repeat, pool=size,
                                               variant=variant, outstanding=concurrency, method=method,
                                               throughput=result["rates"]["successfulRequestsPerSecond"]))
                            write(out / "trials.json", trials)
                            print(trial_name, round(trials[-1]["throughput"], 2), "req/s", flush=True)
                    stop(process)
        lines = ["| Variant | Pool | Outstanding | Method | Repeats | Req/s median [min, max] |", "|---|---:|---:|---|---:|---:|"]
        keys = sorted({(t["variant"], t["pool"], t["outstanding"], t["method"]) for t in trials})
        for key in keys:
            rates = [t["throughput"] for t in trials if (t["variant"], t["pool"], t["outstanding"], t["method"]) == key]
            lines.append(f"| {key[0]} | {key[1]} | {key[2]} | {key[3]} | {len(rates)} | {statistics.median(rates):.2f} [{min(rates):.2f}, {max(rates):.2f}] |")
        (out / "summary.md").write_text("\n".join(lines) + "\n")
        print("\n".join(lines), flush=True)
        if args.candidate_gateway:
            for pool in pools:
                for concurrency in outstanding:
                    for method in ("getTransactionStatus", "getTransactionReceipt"):
                        argv = [args.bench, "compare", "--vary", "gateway-revision"]
                        for trial in trials:
                            if (trial["pool"], trial["outstanding"], trial["method"]) == (pool, concurrency, method):
                                argv += ["--" + trial["variant"], out / trial["file"]]
                        name = f"compare-pool{pool}-n{concurrency}-{method}"
                        for extension, flags in (("md", []), ("json", ["--json"])):
                            with (out / f"{name}.{extension}").open("w") as stdout:
                                subprocess.run(list(map(str, argv + flags)), check=True, stdout=stdout, timeout=60)
    finally:
        for child in reversed(children):
            stop(child)
        for handle in handles:
            handle.close()
        if capture:
            error = capture.finish()
            if error:
                raise RuntimeError(error)


if __name__ == "__main__":
    main()
