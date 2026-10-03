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
prefetch, and locate its pinned bundle. `--offline node` forbids managed downloads.
`GROUNDHOG_DEVNODE_BINARY` selects a matching build. Diagnose compatibility
failures; do not override protocols. Never use fixture keys in production.

`--quiet` does not grant consent. `--non-interactive` disables prompts/UI: protected
operations require authorized `--force`. Key imports need piped stdin; Cargo/test
subprocesses cannot read terminal stdin. Use `--json` for machine results.

`--no-ui` selects plain output; `NO_COLOR` disables color. Explicit live-output
modes stream immediately without animation.

Hosted signing discovers network hashes from matching descriptor/RPC identities;
configured hashes stay enforced.

Timeout or `unknown` does not prove transaction failure. Inspect the hash before
retrying. `node --verbose` enables backend logs; restarting an ephemeral node
loses its state.

`tx list` matches root invocation targets, not every touched address.
See [Event pages and restart](#event-pages-and-restart) for pagination.

### Key and address interoperability

`keys import <name>` reads a checksummed S StrKey or 32-byte hex seed from
hidden input or piped stdin, never a 64-byte keypair or CLI argument. It does not
overwrite names. Names such as `dev-1` are ordinary stored keys.
Import JSON uses camelCase (`data.schemaVersion = 1`). Invalid config fails before
storing. If default selection fails after saving, run `keys use <name>`; do not
reimport. `isDefault` is null if config cannot be reread.

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
Local schema-4 manifests contain public allocations. `node --key alice` allocates
100 HOG to that stored key; no key starts only the random private faucet.
Harnesses own fixture signers and pass `--bootstrap-accounts`. Chain state and
dumps retain the private faucet and funding journal. Old local state requires its
previous runtime and CLI. Retry uncertain local funding with the retained
recipient, amount and nonce. Hosted bootstrap derivation stays separate.

Wallet creation, selection, address inspection, and `status` ownership checks use
cached public metadata without a private signer. Older keyfiles derive missing
metadata from the keypair. Keep metadata consistent: transactions still load the
signer and validate selected-wallet ownership.

`view --from` accepts wallets, stored keys, aliases, or addresses. Saved wallets
take precedence and retain identity. Omission leaves the caller unset.

Native deploy requires `--fund <HOG>` and hogd 0.1.29-rc.2+. Funding is exact;
zero skips transfer. Use `--init <METHOD>` and repeated `--init-arg <VALUE>` with
the artifact spec, or a 0x selector and raw hex argument. `--init-gas-limit` sets
child gas. Init runs as the wallet, discards returns, and rolls back with funding
and deployment on failure. Dry-run submits nothing. Inspect unverified signed
identity before retrying: nonce/address change. Aliases require verified success
at the predicted address.
