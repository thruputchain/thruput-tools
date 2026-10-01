# Groundhog Faucet

`thruput-faucet` serves the hosted Devnet network descriptor and airdrops.
The signing key is loaded once at startup and never belongs in TOML config, process arguments, or logs.

## Run Locally

Copy the example config and replace the placeholder network, contract, and key
source values with values from your local or hosted Devnet setup:

```sh
cp crates/faucet/faucet.example.toml crates/faucet/faucet.toml
cargo run -p thruput-faucet -- --config crates/faucet/faucet.toml
```

Override the listen address when needed:

```sh
cargo run -p thruput-faucet -- --config crates/faucet/faucet.toml --listen 127.0.0.1:8081
```

Logs use `RUST_LOG` and default to `info`:

```sh
RUST_LOG=debug cargo run -p thruput-faucet -- --config crates/faucet/faucet.toml
```

## Config

Heap profiling is opt-in on a separate private listener:

- Set `GROUNDHOG_HEAP_PROFILING=1` and pass `--pprof-listen 127.0.0.1:9647`.
  Use a private address. Without the listen option no profile listener is opened;
  without the environment switch the path returns 404.
- `GET /debug/pprof/heap` returns an uncompressed pprof protobuf with
  `Content-Type: application/octet-stream`.
- Dump and conversion run on a blocking worker; a concurrent dump request returns HTTP 429.
- Dumps require a writable temporary directory. Private dump files use mode 0600
  and are removed after each request.
- The environment switch is supported only on Linux; requesting it elsewhere fails
  at startup. Linux builds use jemalloc even with profiling inactive, and compile
  in profiling support with a 512 KiB average allocation sampling interval.

Start from `faucet.example.toml`. The config includes:

- `listen_address` and `cors_allowed_origins` for HTTP serving.
- `[network]` metadata served by `GET /v1/network`.
- `[network.faucet]` public faucet wallet metadata.
- `[airdrop]` transfer settings and `[airdrop.secret_key_source]`.
- `[policy]` optional private-preview quota overrides. Defaults are used when omitted.

Do not commit real faucet private keys.

## Local Key Source

Use an environment variable for local `hog-devnode` development:

```toml
[airdrop.secret_key_source]
type = "environment"
variable = "GROUNDHOG_FAUCET_SECRET_KEY"
```

Set the variable to the faucet account's 32-byte Ed25519 secret key as 64 hex
characters, with an optional `0x` prefix. Its derived public key must equal
`network.faucet.public_key`; startup fails before serving if they differ. The
configured `network.faucet.wallet_address` must equal the nonce-zero pubkey account
derived from that public key; startup rejects a different address. Hosted Devnet retains its separate deterministic bootstrap configuration.
A schema-4 local node owns a random private faucet credential in its chain
directory and serves local funding directly; public runtime metadata cannot
provide its signing key.

## Hosted AWS Key Source

Hosted Devnet loads the same hex string from AWS Secrets Manager:

```toml
[airdrop.secret_key_source]
type = "aws_secrets_manager"
secret_id = "thruput/devnet/faucet"
version_id = "EXAMPLE_VERSION_ID"
region = "us-west-2"
```

The secret must contain a `SecretString` whose entire value is the hex-encoded
32-byte key. `SecretBinary` and JSON-wrapped values are rejected. The hosted
unit should receive an instance/task role granting only
`secretsmanager:GetSecretValue` on this secret. The SDK gets those credentials
from the environment, shared AWS config, or EC2 instance metadata. Do not put
static AWS credentials in faucet config.

Set `network.faucet.public_key` and `network.faucet.wallet_address` from the
non-secret hosted-bootstrap faucet metadata. Only the secret ID/ARN and region
belong in the hosted runtime config; the secret payload remains in Secrets
Manager.

Startup fails before serving requests when the source is incomplete,
inaccessible, binary, or malformed. Errors identify the source and operation
but never include the secret payload.

## API

- `GET /health` reports service health.
- `GET /v1/network` returns the schema-4 public network descriptor.
- `POST /v1/airdrop` funds an existing address using the configured airdrop amount.

`POST /v1/airdrop` requires `clientNonce`, `networkId`, `networkHash`, and
`genesisHash`. These identities scope retries to the network and chain instance
the client validated.

## Policy State

Private-preview policy state is in memory. Restarting the faucet clears quota
counters and idempotency records. The current deployment model assumes one
faucet instance.

`[policy.airdrop].max_retained_attempts` bounds airdrop records globally (default
10,000; supported range 1–1,000,000). Terminal records remain for at least 12 hours, or the recipient
cooldown if longer. Size this bound for aggregate traffic across all clients
over that retention period, plus unresolved attempts; it does not replenish
each time an hourly quota resets. For example, 10,000 accepted attempts/hour
needs at least 120,000 terminal records plus headroom. Increasing the bound
increases memory use and policy-store scan costs; storage is allocated on demand.

Signed transactions are retained in the policy store before broadcast. A later
request with the same client nonce observes the original hash. Scheduler `retry`
replays the same signed bytes at most once per request, after checking the original
expiry. Receipts take precedence; `dropped` and `invalid` end the attempt. A lost
submission response, backend error, or expiry without a receipt keeps the original
attempt unresolved. These attempts count toward the bounded capacity. At
capacity, the faucet rejects new attempt keys
but continues serving existing keys. This fail-closed behavior prevents an
expired idempotency record from submitting a second transfer. Operators should
investigate sustained capacity errors before restarting because a restart also
clears protection for previously accepted transactions.

## Web UI

The static faucet web UI lives in `web/`. See `web/README.md` for local
development and production build instructions.
