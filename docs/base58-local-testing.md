# Local identifier-encoding smoke

`scripts/smoke-base58-local.py` checks the CLI and RPC identifier boundary against
a real local runtime without compiling a contract. It requires Python 3.11+,
a built `thruput` CLI, and a matching `hog-devnode` executable. Building those
executables requires the private registries and native dependencies documented
in their respective repository setup guides.

Use matching source revisions: this smoke expects RPC wire format **12.0.0**,
local bootstrap/runtime/data metadata schema **5**, and readiness-pointer schema
**4**. Published older runtimes and existing local data directories are not
compatible. The script creates its own new data directory and isolated CLI
configuration; it does not migrate an existing chain.

The preliminary matching companion revisions are:

- `hogd`: `2a87b7263` on `railway/base58-runtime`.
- `thruput-sdk-ts`: `ed8c673` on `railway/base58-foundation`.
- Protocol specification/vectors: `e9e5e64` on `railway/base58-foundation`.

These are local development commits, not published packages. Use the tooling
revision supplied with the handoff. The runtime's generated build identity can
describe its configure-time revision; use the source pins rather than a version
string alone to select matching builds.

From the tooling checkout:

```sh
cargo build --locked -p thruput-cli
python3 scripts/smoke-base58-local.py \
  --cli "$PWD/target/debug/thruput" \
  --devnode /absolute/path/to/hog-devnode
```

To check the TypeScript SDK against the same transaction, first run `npm ci` and
`npm run build` in the matching `thruput-sdk-ts` checkout (Node 24 and private
GitHub Packages access required), then add:

```sh
python3 scripts/smoke-base58-local.py \
  --devnode /absolute/path/to/hog-devnode \
  --sdk-root /absolute/path/to/thruput-sdk-ts
```

The script:

1. Generates isolated sender/recipient keys and starts a funded fresh chain.
2. Verifies base58 network identity, manifest hashes, addresses, and public keys.
3. Reads the recipient balance using both a base58 RPC address and a C StrKey CLI
   input. The sender's saved binding uses a C address and G public key.
4. Signs a one-HOG transfer, submits its unchanged hex transaction bytes, and
   checks the returned base58 hash against the locally computed hash.
5. Waits for a successful receipt, checks typed event emitters and retained hex
   topics/data, and verifies the recipient gained exactly `10000000` raw units.
6. Confirms that legacy hex addresses and transaction hashes are rejected by the RPC boundary.
7. Optionally reads the balance, network identity, and receipt through the SDK.

Success ends with `PASS` messages. The node stops on exit. `--timeout` controls
startup/confirmation timeouts (120 seconds by default); `--ramdb-bytes` forwards
an explicit RAM database capacity to the runtime if your machine needs one.

The printed private temporary directory retains `node.stderr`, `node.jsonl`,
and `result.json`. For a failure, share the failing assertion and relevant
runtime log; `result.json` contains public transaction evidence when the CLI
flow completed. Do not share the entire directory: it also contains the
generated private keys and local faucet state.

This is a preliminary boundary check. Contract deployment, generated bindings,
restart/dump/restore behavior, hosted services, explorer, and wallet-provider UI
flows require their separate integration suites.
