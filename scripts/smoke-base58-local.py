#!/usr/bin/env python3
"""Exercise wire-12 identifiers against a matching local hog-devnode (Python 3.11+).

Run after building thruput-cli and hog-devnode:
  python3 scripts/smoke-base58-local.py --devnode /absolute/path/to/hog-devnode
Artifacts are retained in a private temporary directory, printed at startup.
This starts a fresh isolated chain and stops it on exit; existing CLI state is untouched.
"""

import argparse
import base64
import binascii
import json
import os
import signal
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path

ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def base58(raw):
    number = int.from_bytes(raw, "big")
    text = ""
    while number:
        number, digit = divmod(number, 58)
        text = ALPHABET[digit] + text
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + text


def identifier(text):
    assert isinstance(text, str) and 1 <= len(text) <= 44, text
    number = 0
    for character in text:
        number = number * 58 + ALPHABET.index(character)
    raw = b"\0" * (len(text) - len(text.lstrip("1")))
    raw += number.to_bytes((number.bit_length() + 7) // 8, "big")
    assert len(raw) == 32 and base58(raw) == text, text
    return raw


def strkey(version, raw):
    payload = bytes([version]) + raw
    checksum = binascii.crc_hqx(payload, 0).to_bytes(2, "little")
    return base64.b32encode(payload + checksum).decode().rstrip("=")


def opaque(text):
    assert text.startswith("0x") and text == text.lower(), text
    return bytes.fromhex(text[2:])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--devnode", required=True, type=Path)
    parser.add_argument(
        "--cli",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "target/debug/thruput",
    )
    parser.add_argument(
        "--ramdb-bytes", help="Optional local-node RAM capacity, e.g. 256M"
    )
    parser.add_argument(
        "--sdk-root",
        type=Path,
        help="Built thruput-sdk-ts checkout; also verify through Node.js",
    )
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    cli, devnode = args.cli.resolve(strict=True), args.devnode.resolve(strict=True)
    work = Path(tempfile.mkdtemp(prefix="thruput-base58-"))
    print(f"Artifacts: {work}", flush=True)
    env = dict(
        os.environ,
        XDG_CONFIG_HOME=str(work / "config"),
        XDG_CACHE_HOME=str(work / "cache"),
    )
    # Avoid ambient profile/runtime overrides selecting a different chain.
    for key in list(env):
        if key.startswith("THRUPUT_"):
            del env[key]

    def command(*arguments):
        result = subprocess.run(
            [str(cli), "--json", "--non-interactive", *arguments],
            check=False,
            cwd=work,
            env=env,
            capture_output=True,
            text=True,
            timeout=args.timeout,
        )
        if result.returncode:
            raise RuntimeError(
                f"CLI {arguments[0]} failed: {result.stderr or result.stdout}"
            )
        report = json.loads(result.stdout)
        assert report["ok"], report
        return report["data"]

    keys = {
        name: command("keys", "generate", name)["key"]
        for name in ("sender", "recipient")
    }
    for key in keys.values():
        identifier(key["address"])
        identifier(key["public_key"])
    (work / "allocations.json").write_text(
        json.dumps(
            [
                {"publicKey": key["public_key"], "initialStorageTokens": "10000000000"}
                for key in keys.values()
            ]
        )
    )
    config = work / "thruput.toml"
    config.write_text(
        "default_profile = 'local'\n[profile.local]\ndefault_key = 'sender'\n"
    )
    node_args = [
        str(cli),
        "--json",
        "--non-interactive",
        "node",
        "--port",
        "0",
        "--startup-timeout-secs",
        str(args.timeout),
        "--hog-devnode-binary",
        str(devnode),
        "--data-dir",
        str(work / "node-data"),
        "--bootstrap-accounts",
        str(work / "allocations.json"),
    ]
    if args.ramdb_bytes:
        node_args += ["--ramdb-bytes", args.ramdb_bytes]
    with (
        (work / "node.jsonl").open("w") as output,
        (work / "node.stderr").open("w") as errors,
    ):
        node = subprocess.Popen(
            node_args,
            cwd=work,
            env=env,
            stdout=output,
            stderr=errors,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + args.timeout
            startup = None
            while time.monotonic() < deadline:
                for line in (work / "node.jsonl").read_text().splitlines():
                    try:
                        candidate = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if candidate.get("ok") and candidate.get("command") == "node":
                        startup = candidate["data"]
                        break
                if startup:
                    break
                if node.poll() is not None:
                    raise RuntimeError(f"Node exited; inspect {work / 'node.stderr'}")
                time.sleep(0.1)
            assert startup, f"Node startup timed out; inspect {work / 'node.stderr'}"
            url = startup["rpc_url"]
            manifest_path = startup["runtime_manifest"]["path"]
            manifest = json.loads(Path(manifest_path).read_text())
            assert manifest["schemaVersion"] == 5, manifest
            identifier(manifest["networkHash"])
            identifier(startup["runtime_manifest"]["hash"])
            for account in manifest["bootstrapAccounts"]:
                identifier(account["publicKey"])
                identifier(account["walletAddress"])

            def rpc(method, params):
                request = urllib.request.Request(
                    url,
                    json.dumps(
                        {
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "groundhog_" + method,
                            "params": params,
                        }
                    ).encode(),
                    {"Content-Type": "application/json"},
                )
                with urllib.request.urlopen(request, timeout=10) as response:
                    return json.load(response)

            sender, recipient = keys["sender"], keys["recipient"]
            recipient_c = strkey(16, identifier(recipient["address"]))
            # C/G inputs are accepted independently of canonical report encoding.
            config.write_text(
                "default_profile = 'local'\n[profile.local]\ndefault_key = 'sender'\n"
                "default_wallet = 'sender'\n"
                + f"rpc_url = {json.dumps(url)}\n"
                + f"runtime_manifest = {json.dumps(manifest_path)}\n"
                + "[profile.local.wallets.sender]\nkind = 'native'\naccount_nonce = 0\n"
                + f"address = '{strkey(16, identifier(sender['address']))}'\n"
                + f"owner_public_key = '{strkey(48, identifier(sender['public_key']))}'\n"
            )
            network = rpc("networkInfo", [])["result"]
            version = rpc("version", [])["result"]
            assert version["wireFormat"] == "12.0.0", version
            assert network["networkHash"] == manifest["networkHash"], network
            before = rpc("getBalance", [recipient["address"]])["result"]
            assert before["address"] == recipient["address"], before
            assert int(before["rawBalance"]) > 0, "recipient must have an existing balance item"
            balance = command("balance", recipient_c)
            assert balance["address"] == recipient["address"], balance
            prepared = command("transfer", recipient_c, "1", "--dry-run")
            assert prepared["sender"] == sender["address"], prepared
            assert prepared["signerPublicKey"] == sender["public_key"], prepared
            assert prepared["recipient"] == recipient["address"], prepared
            tx_hash = prepared["txHash"]
            identifier(tx_hash)
            opaque(prepared["signedTransaction"])
            submitted = rpc("sendRawTransaction", [prepared["signedTransaction"]])
            assert submitted.get("result") == tx_hash, submitted
            deadline = time.monotonic() + args.timeout
            while time.monotonic() < deadline:
                inspected = command("tx", "show", tx_hash)
                if inspected.get("receipt") is not None:
                    break
                time.sleep(0.5)
            receipt = inspected.get("receipt")
            assert receipt and receipt["txHash"] == tx_hash, inspected
            assert receipt["status"]["result"] == "ok", receipt
            assert receipt["events"], "HOG transfer should emit events"
            for field in ("returnData", "abortData"):
                if receipt.get(field) is not None:
                    opaque(receipt[field])
            for event in receipt["events"]:
                identifier(event["contractAddress"])
                for topic in event["topics"]:
                    opaque(topic)
                opaque(event["data"])
            after = rpc("getBalance", [recipient["address"]])["result"]
            assert int(after["rawBalance"]) - int(before["rawBalance"]) == 10000000, (
                before,
                after,
            )
            legacy = rpc("getBalance", ["0x" + identifier(recipient["address"]).hex()])
            assert "error" in legacy, legacy
            legacy_hash = rpc(
                "getTransactionStatus", ["0x" + identifier(tx_hash).hex()]
            )
            assert "error" in legacy_hash, legacy_hash
            evidence = {
                "rpcUrl": url,
                "manifest": manifest_path,
                "sender": sender["address"],
                "recipient": recipient["address"],
                "txHash": tx_hash,
                "rawAmount": "10000000",
                "storageDepositRaw": "0",
                "receipt": receipt,
                "before": before,
                "after": after,
                "legacyHexRejected": True,
            }
            (work / "result.json").write_text(json.dumps(evidence, indent=2) + "\n")
            if args.sdk_root:
                subprocess.run(
                    ["npm", "run", "test:integration"],
                    cwd=args.sdk_root.resolve(strict=True),
                    env={
                        **env,
                        "THRUPUT_CLI_TRANSFER_RESULT": str(work / "result.json"),
                    },
                    check=True,
                    timeout=args.timeout,
                )
            print(
                f"PASS: base58 balance, C/G inputs, hex submission, receipt/events, +1 HOG, legacy rejection\nEvidence: {work / 'result.json'}"
            )
        finally:
            if node.poll() is None:
                os.killpg(node.pid, signal.SIGINT)
                try:
                    node.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(node.pid, signal.SIGKILL)
                    node.wait()


if __name__ == "__main__":
    main()
