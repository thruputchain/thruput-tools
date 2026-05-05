# Dev-Account Bootstrap Spec

**Date:** 2026-05-03
**Status:** Draft implementation contract for PR 4e.

**Companion to:**

- [`../plans/2026-04-29-local-node-hogd-dev-supervisor.md`](../plans/2026-04-29-local-node-hogd-dev-supervisor.md)
  - supervisor plan; this spec is PR 4e's bootstrap contract.
- [`2026-04-29-hogd-dev-process-contract.md`](2026-04-29-hogd-dev-process-contract.md)
  - hogd-dev process contract; this spec extends the local-node bootstrap
    profile and readiness/control metadata.

## Goal

Unblock the default CLI local loop:

`groundhog new` -> `groundhog build` -> `groundhog deploy` -> receipt ->
`groundhog call`

The local node must start with deterministic funded dev accounts whose wallet
contracts are present, initialized, and authorized for the corresponding
ed25519 signer keys. The supervisor must expose those accounts through the
dev JSON-RPC surface so CLI commands and tooling can discover the same
addresses the backend bootstrapped.

## Scope: Per-Environment vs Universal

Three slices of work live in PR 4e. Keep them separated when reasoning
about reuse:

| Slice | Lifetime | Reuse |
|---|---|---|
| Local orchestration: `--dev-accounts`, `dev-N` resolver, `groundhog_dev_getAccounts`, supervisor discovery glue. | Local-only. | Does not escape `groundhog node`. Replaced on testnet/mainnet by faucet UX or imported keys. |
| Wallet contract + auth wiring: deploy + do_action + return-data forwarding under protocol-rs 0.10 source-address auth. | Universal. | The same wallet shape is what testnet faucets and mainnet onboarding flows would deploy. |
| Bootstrap-artifact shape: `GenesisConfiguration` + vetted init batches + dev-wallet bytes/hash + public account metadata. | Universal. | Reusable for testnet/mainnet genesis tooling and for a future gateway+real-hogd simulation rig with different inputs. |

PR 4e is mostly local-orchestration sugar wrapped around two
universally-needed primitives (the wallet contract and the bootstrap
artifact shape) that we have to build somewhere first. Local is the
convenient "somewhere" because we own the whole world and can iterate
fast. The universal slices surviving into testnet/mainnet/rig contexts
is a benefit, not the design goal.

Implication for the Rust code: the address-aware signer abstraction
(CLI holds a key, signs against an address discovered from the backend)
is universal and belongs on the general code paths. The `dev-N` resolver
is local-only and should stay isolated behind the dev backend's RPC
surface so testnet/mainnet flows do not branch on it.

## Key Findings

The original draft assumed protocol-rs 0.10 source-address auth could replace
the dev-wallet contract entirely. That is not true for the current CLI and hogd
execution shape.

- `groundhog deploy` and `groundhog call` invoke wallet entrypoints at the
  account address. Source-address auth can authorize that invoked address, but
  it does not provide wallet code, a deploy entrypoint, or call return-value
  forwarding.
- hogd's current `GenesisConfiguration` can express deterministic deployment
  claims and allocation entries, but it does not let the Rust supervisor deploy
  arbitrary wallet code at an arbitrary pubkey-derived address.
- The legacy native-exec dev node directly installed wallet WASM at
  pubkey-derived addresses. That was an in-process shortcut, not a portable
  process contract.
- hogd loadgen and chuckd already use the more durable shape: a genesis
  configuration plus vetted initialization transaction batches that are pumped
  and committed before normal workload traffic begins.

PR 4e should therefore make bootstrap explicit instead of planting
Rust-private state.

## Recommended Path

Use a hogd-owned bootstrap profile that produces a concrete bootstrap artifact:

- a `GenesisConfiguration`;
- one or more vetted initialization transaction batches;
- deterministic dev-account metadata;
- the dev-wallet contract bytes/hash and initialization calldata used by those
  batches.

For `hogd-dev` v1, the binary may build and apply this artifact internally when
started with the local dev-account bootstrap profile. For a future simulation
or bench rig, the same artifact shape should be usable by loadgen/chuckd or a
real gateway plus hogd process suite.

This path avoids a protocol/XDR change for PR 4e, but it does not pretend the
current XDR shape can deploy wallet code at arbitrary addresses. If preserving
pubkey-derived wallet addresses is mandatory, that is a different design: add
an explicit process/XDR surface for arbitrary account deployment, auth data,
and funding, then update both hogd-dev and the future process-suite rig to
consume it.

The v1 artifact should use hogd's default storage cost model. If the model
changes later, the readiness/control metadata must still report the actual
post-bootstrap account funding rather than a Rust-side expectation.

## Non-Goals

- No Rust-only genesis/auth shim that silently reconstructs wallet state outside
  a hogd-owned bootstrap contract.
- No broad genesis-profile parser in the supervisor.
- No dedicated chuckd process in `groundhog node` v1. The supervisor-owned block
  driver remains the local single-node coordination policy.
- No typed scheduler-submit protocol work in PR 4e. The temporary submit error
  mapping remains tracked separately.
- No snapshot/revert/time-travel support in PR 4e.

## Account Model

PR 4e should separate signer identity from wallet address.

- The signer key remains deterministic and derived from the current dev seed
  algorithm.
- The account address returned by `groundhog_dev_getAccounts` is the deployed
  wallet contract address that hogd actually initialized.
- `public_key` identifies the signer authorized by that wallet.
- `private_key` is derived by the Rust CLI/supervisor for local dev only; hogd
  readiness should not print private key material.
- `storage_tokens` is reported by hogd-dev as the actual post-bootstrap funded
  balance for the wallet account.

This means the account address does not have to equal
`PubkeyAccount::new(public_key, DEFAULT_PUBKEY_ACCOUNT_NONCE)`. The current CLI
must be taught to resolve `dev-N` to the backend-advertised wallet address when
using the local hogd-dev backend.

`groundhog_dev_getAccounts` intentionally returns private keys for the
well-known local dev accounts. That is acceptable only on the local dev RPC
surface; readiness output stays public and never includes private keys.

If the team chooses to keep pubkey-derived account addresses as a product
requirement, PR 4e must first add the explicit arbitrary-deployment/bootstrap
surface described above. Do not reintroduce the native-exec shortcut inside the
Rust supervisor.

## Deterministic Signer Derivation

Inputs:

- `seed: string` (default `"groundhog-dev-accounts"`).
- `count: u32` (default 10).
- `tokens: u64` storage tokens per account (default 1_000_000_000).

For each `i` in `0..count`:

1. Compute `secret_key_seed = SHA256(utf8(seed) || 0x3A || utf8("account-N"))`,
   where `0x3A` is the literal `:` byte and `N` is the decimal ASCII
   representation of `i` with no padding.
2. Treat `secret_key_seed` as the ed25519 secret-key seed and derive the
   keypair.
3. Record the signer public key in the bootstrap account metadata.
4. Let the hogd bootstrap artifact determine the wallet contract address.

Both Rust and C++ implementations must produce byte-identical signer public
keys for the same `(seed, count)` inputs. Add golden coverage for the default
seed and `count = 10`.

Reference material:

- `crates/dev-node/src/accounts.rs` - old Rust derivation.
- `crates/cli/src/dev_keys.rs` - current CLI dev-key entrypoint.
- `crates/bench/src/main.rs::dev_signer` - benchmark derivation that must stay
  matched to the shared derivation crate after PR 4e.

V1 is default-seed only from the user's point of view. If a later CLI exposes a
seed override, the same seed must flow to hogd-dev bootstrap and Rust private
key derivation; otherwise deploy/call signing and backend auth will diverge.

## Dev Wallet Contract Requirements

The existing hogd `simple_wallet_authclaim` contract is not sufficient for the
CLI local loop because it lacks the deploy entrypoint and does not propagate
return data for calls.

PR 4e needs a current-protocol dev-wallet contract with:

- `initialize(public_key, auth_policy)` or equivalent initialization data;
- `deploy(...)` compatible with the CLI's `groundhog deploy` transaction shape;
- `do_action(...)` compatible with the CLI's `groundhog call` transaction shape;
- source-address auth against the wallet address using protocol-rs 0.10's
  `kSourceAddressAuthData { allowed_keys: 0x01, required_weight: 1 }`;
- return-data propagation for contract calls.

Preferred ownership: hogd or a hogd-built-contract target owns the canonical
dev-wallet bytes used by the bootstrap artifact. Rust should consume the
advertised account metadata and sign transactions; it should not synthesize
wallet deployment semantics privately.

Use hogd's existing loadgen/test-contract build pipeline as the precedent for
shipping canonical local-dev wallet bytes with the hogd-side bootstrap code.

## hogd-dev Bootstrap Contract

### Startup Surface

Add a small local bootstrap profile surface to hogd-dev. Exact flag names can
be adjusted to match hogd style, but the semantics should be:

| Flag | Type | Default | Behavior |
|---|---|---|---|
| `--dev-accounts` | u32 | 0 | Number of deterministic dev accounts to bootstrap. |
| `--dev-account-seed` | string | `groundhog-dev-accounts` | Signer derivation seed. |
| `--dev-account-tokens` | u64 | 1000000000 | Storage tokens per account. |

`--dev-accounts 0` means no account bootstrap and no `dev_accounts` readiness
field. The standalone hogd-dev default may remain zero accounts, but the Rust
supervisor must always pass the configured local-node count. If the supervisor
expects accounts and readiness omits them, returns an empty list, or returns a
different count, startup must fail before the public JSON-RPC server begins
serving.

Do not add `--coord-mode external` in PR 4e unless the hogd team wants to
exercise it immediately. A future real-process rig should be designed around
the bootstrap artifact plus chuckd/loadgen-style init pumping, not around a
half-supported local-node flag.

### Bootstrap Sequence

When dev-account bootstrap is enabled:

1. Build the bootstrap artifact from `(seed, count, tokens)`.
2. Load the artifact's `GenesisConfiguration`.
3. Submit the artifact's vetted initialization batches.
4. Nominate and commit each initialization batch before normal JSON-RPC traffic
   is considered ready.
5. Retain public account metadata for readiness/control output.

The v1 implementation can perform these steps inside hogd-dev before emitting
readiness. The important constraint is that the steps mirror the loadgen/chuckd
model closely enough that a future process-suite rig can reuse the same
artifact and test vectors.

### Readiness JSON

Extend readiness with public metadata only:

```json
{
  "status": "ready",
  "network_id": "groundhog-local",
  "endpoints": {
    "scheduler": "127.0.0.1:...",
    "read": "127.0.0.1:..."
  },
  "dev_accounts": [
    {
      "index": 0,
      "address": "0x...",
      "public_key": "0x...",
      "storage_tokens": 1000000000
    }
  ]
}
```

Rules:

- Omit `dev_accounts` when `--dev-accounts` is 0.
- Order matches derivation index.
- `address` is the deployed wallet address.
- `public_key` is the authorized signer key.
- `storage_tokens` is the actual funded storage-token balance after bootstrap.
- Hex strings are 0x-prefixed lowercase canonical bytes.
- Readiness does not include private keys.

### hogd Tests

- Golden derivation test for the default seed and `count = 10`.
- Bootstrap artifact test: expected account count, signer keys, wallet
  addresses, and init-batch count are stable.
- Integration smoke: boot hogd-dev with three dev accounts, submit a CLI-shaped
  deploy transaction signed by account 0, drive commit, and assert the indexer
  returns a receipt for the submitted transaction.
- If the C++ test cannot easily construct the exact CLI-shaped transaction,
  check in a canonical XDR fixture generated by groundhog-rpc's deploy builder
  instead of adding a hogd test dependency on groundhog-rpc internals.
- Optional lower-level assertions: auth data exists for each wallet address and
  the wallet account balance matches the advertised `storage_tokens`.

## groundhog-rpc Changes

### Supervisor

1. Request the hogd-dev dev-account bootstrap profile with defaults:
   `count = 10`, `seed = "groundhog-dev-accounts"`,
   `tokens = 1_000_000_000`.
2. Parse readiness `dev_accounts` and retain account metadata alongside the
   locally derived private keys.
3. Expose `groundhog_dev_getAccounts`, returning
   `groundhog_rpc_api_types::dev::DevAccount` values. The
   `GroundhogDevApi::get_accounts` trait method and `DevAccount` type already
   exist; PR 4e must add the node-supervisor implementation, merge
   `GroundhogDevApiServer` into the supervisor JSON-RPC module, and cover it
   with fake hogd-dev tests.
4. `address`, `public_key`, and `storage_tokens` come from hogd readiness.
   `private_key` comes from local deterministic signer derivation.
5. Keep the supervisor block driver as the post-submit single-node coordination
   policy for ordinary user transactions.

If hogd-dev applies genesis internally, remove or bypass the supervisor's
current empty-genesis shim for that mode. If hogd-dev still expects the
supervisor to load genesis, the supervisor must load the bootstrap artifact
rather than the empty config.

### CLI

- Consolidate dev-key derivation into one Rust implementation used by
  `dev_keys.rs`, tests, and benchmarks.
- Teach `groundhog deploy` and `groundhog call` to use the wallet address
  returned for `dev-N` by `groundhog_dev_getAccounts`.
- Do not silently fall back to locally recomputing a pubkey-derived address. If
  `groundhog_dev_getAccounts` returns `MethodNotFound`, omits the requested
  account, or cannot be reached, `deploy` / `call` should emit a clear local
  dev-node-required error.
- Preserve deterministic local signing keys. The signer key is stable even if
  the wallet address is backend-advertised.

No new user-facing `groundhog node` flags are required for v1 unless manual
account-count overrides become useful during smoke testing.

### Rust Tests

- Readiness parser test for `dev_accounts`.
- Fake hogd-dev fixture test for `groundhog_dev_getAccounts`.
- CLI test proving `dev-0` resolves to the advertised wallet address, not a
  locally recomputed pubkey-derived address, when the backend provides account
  metadata.
- Real-binary smoke: default local loop reaches deploy receipt and call result.

## Acceptance

Do not call PR 4e complete until a real hogd-dev binary can pass:

1. `groundhog node --hogd-dev-binary <path>` starts and bootstraps 10 dev
   accounts.
2. `groundhog_dev_getAccounts` returns 10 accounts with wallet addresses,
   signer public keys, private keys, and storage tokens populated.
3. `groundhog deploy` signed as `dev-0` returns a transaction hash and the
   receipt reaches `committed`.
4. `groundhog call` signed as `dev-0` returns the expected contract value.
5. The read plane still returns receipts/events through hogd-dev's indexer.
6. The block driver still coalesces ordinary submits and advances `chainTip`.

## Open Decisions

1. Confirm with hogd/protocol owners that loadgen-style wallet deployment
   addresses are acceptable for local dev accounts. If not, start with the
   arbitrary-address bootstrap surface instead.
2. Choose the exact home for the current-protocol dev-wallet contract bytes:
   hogd loadgen/test-contract pipeline, hogd built-contract target, protocol
   integration-test contract, or another hogd-owned artifact.
3. Decide whether hogd-dev applies the bootstrap artifact internally before
   readiness, or exposes enough artifact data for the Rust supervisor to load
   and pump. Internal application is the faster local-node path; artifact
   reuse is the future rig requirement.

## Suggested PR Sequencing

1. **hogd PR**: dev-wallet contract, bootstrap artifact builder, hogd-dev
   bootstrap mode, readiness `dev_accounts`, and real data-flow smoke coverage.
2. **groundhog-rpc PR**: readiness parsing, `groundhog_dev_getAccounts`,
   dev-key consolidation, CLI wallet-address resolution, fake fixture updates,
   and real-binary smoke.
3. **Rig follow-up**: reuse the bootstrap artifact from a real chuckd/loadgen
   process-suite setup with gateway in front. Write a dedicated future rig spec
   before implementing that track.
