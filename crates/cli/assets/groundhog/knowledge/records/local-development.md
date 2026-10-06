+++
schema_version = 1
id = "groundhog.local-development"
owner = "groundhog-rpc"
kind = "workflow"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 60
+++
## Local Development

Script setup with `groundhog init --non-interactive --json`. Missing Cargo,
profile, or stored key exits 1. Authorized `--generate-key` creates an initial key;
existing selections are preserved. `init` writes; `status --json` is read-only.
Create projects with `groundhog new`.

Start `groundhog node`. `toolchain list`, `install`, and `which` inspect,
prefetch, and locate its pinned bundle. `--offline node` forbids managed downloads,
not other RPC/Cargo access. `GROUNDHOG_DEVNODE_BINARY` selects a matching build.
Diagnose compatibility failures; do not override protocols. Use `status --json`
or `node --json` for runtime information and `--help` for flags. Never use
deterministic local keys in production.

`--quiet` does not grant consent. `--non-interactive` disables prompts/UI: protected
operations require authorized `--force`. Key imports need piped stdin; Cargo/test
subprocesses cannot read terminal stdin. Use `--json` for machine results.

`--no-ui` selects plain output; `NO_COLOR` disables color. Explicit live-output
modes stream immediately without animation.

Hosted signing can discover an unconfigured network hash from matching descriptor
and RPC identities. Configured hashes remain enforced.

Timeout or `unknown` does not prove transaction failure. Inspect the hash before
retrying. `node --verbose` enables backend logs; restarting an ephemeral node
loses its state.

`events list`, `tx list`, and `token transfers` return newest-first pages.
Omit `--cursor` to refresh; use the opaque cursor and unchanged target/filters
for older rows. `tx list` matches root invocation targets, not every touched address.
See [Event pages and restart](#event-pages-and-restart) for cursor rules.

### Key and address interoperability

`keys import <name>` reads a checksummed S StrKey or 32-byte hex seed from
hidden input or piped stdin, never a 64-byte keypair or CLI argument. It does not
overwrite names. Generation and import reject reserved bootstrap `dev-N` names.
Import JSON is camelCase (`data.schemaVersion = 1`). Malformed TOML or an invalid
`default_key` is rejected before storing. If default selection fails after saving,
run `keys use <name>`; do not reimport. `isDefault` is null if config cannot be reread.

`keys export <name> --format strkey` exports an S StrKey after confirmation;
the default is hex. For an authorized key transfer, pipe export stdout directly
to the receiving CLI's stdin. Do not capture it in logs.

G StrKeys identify Ed25519 public keys, not accounts. Address inputs and typed
arguments accept C StrKeys or legacy hex; wallet owners accept G or hex public
keys. Encodings preserve bytes, not network identity. Keyfiles and public RPC
encodings remain unchanged.

### Native accounts

Ordinary keys derive nonce-zero native accounts. `wallet create` saves offline;
fund with `airdrop`, not `--prefund`. Legacy wallets retain identity and format.
Schema-3 dev-N accounts use native nonce-zero identities from the advertised seed.
Use a matching runtime; schema-2 state needs its original runtime.

Wallet creation, selection, address inspection, and `status` ownership checks use
cached public metadata without a private signer. Older keyfiles derive missing
metadata from the keypair. Keep metadata consistent: transactions still load the
signer and validate selected-wallet ownership.

`view --from` accepts wallets, keys, aliases, dev-N, or literal addresses.
Dev-N takes bootstrap precedence; other saved wallets take
precedence. Bindings retain their native nonce or legacy address. Omission leaves
the caller unset.

Native deploy requires `--fund <HOG>` and hogd 0.1.29-rc.2+. Funding is exact;
zero skips transfer. Use `--init <METHOD>` and repeated `--init-arg <VALUE>` with
the artifact spec, or a 0x selector and raw hex argument. `--init-gas-limit` sets
child gas. Init runs as the wallet, discards returns, and rolls back with funding
and deployment on failure. Dry-run submits nothing. Inspect unverified signed
identity before retrying: nonce/address change. Aliases require verified success
at the predicted address.

`tx show`: indexer wins; resubmit `retry` transactions, modify `dropped` ones.
