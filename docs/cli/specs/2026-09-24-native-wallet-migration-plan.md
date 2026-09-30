# Native wallet migration (GH-372)

GH-372 moves ordinary CLI and testkit accounts to the native wallet. Wallet
creation derives and saves an empty account; users fund it explicitly with
`groundhog airdrop`. Native deployment transfers an exact amount and optionally
initializes the new contract in one transaction.

## Current prerequisites

The canonical wallet change merged in
[integration_test_contracts #56](https://github.com/groundhogchain/integration_test_contracts/pull/56),
and runtime support merged in [hogd #4144](https://github.com/groundhogchain/hogd/pull/4144).
The unified `DefaultWallet.deploy(nonce, contract_hash, amount, initializer)` ABI
is available from hogd 0.1.29-rc.2. The current CLI 0.19.0 catalog pins released
hogd 0.1.29-rc.4, with matching SDK 0.41.1 and interfaces 0.1.41.

[client-ts #79](https://github.com/groundhogchain/client-ts/pull/79) merged as
`1489c8b`. Client 0.13.0 was published from
`19f2d18cf70917106b90e2516151c84e48430b9b`; its release workflow completed and the
package registry reports that same source commit. Its deploy API takes explicit
funding and an optional ordinary typed invocation, whose target must match the
predicted contract address. Temporary experimental opt-ins are removed from the
CLI and testkit once these supporting runtime prerequisites are available.

## Account and transaction contracts

- Ordinary signing keys derive nonce-zero native accounts. `wallet create`
  saves that identity offline without a transaction, RPC request, or funding.
  It is idempotent; dry-run writes nothing. `--prefund` is rejected with guidance
  to create first and fund separately.
- Saved legacy bindings and manifest-backed dev-N wallets retain their address
  and transaction format. Untyped old bindings remain legacy. Native bindings
  validate their owner, nonce, and derived address. No silent identity remapping
  or legacy fallback is allowed.
- Calls, transfers, balance reads, and funding resolve the same selected account.
  Funding carries the resolved address independently of its display name so a
  colliding key/wallet label cannot redirect an airdrop.
- Native deployment requires explicit `--fund <HOG>`; zero skips the transfer.
  The amount is exact, not an automatic cost estimate or promised free balance.
  `--init`, repeated `--init-arg`, and `--init-gas-limit` describe optional atomic
  initialization. The initializer sees the wallet as caller; its result is
  discarded. Its gas allowance is included in the root budget, with overflow
  rejected before RPC. Any failure rolls back funding, code, and initialization.
- A signed operation is submitted once. Submission and receipt hashes and the
  returned deployment address must match before an alias is saved. Send errors,
  lookup failures, timeouts, and malformed success results retain the original
  signed bytes, hash, address, nonce, expiry, endpoint, funding, and initializer.
  An unknown outcome is inspected before retrying; no automatic resubmission or
  two-transaction prefund/deploy fallback is permitted.
- Testkit accounts derive from the scenario seed and start empty. Funding is
  explicit through `fund`/`submit_fund`; all deployment helpers require exact raw
  HOG funding and accept optional initialization. Attached and registered-code
  deployments share the builder. Registration recovery observes the retained
  attempt, verifies its receipt/address, and rejects changed effects while that
  attempt remains unresolved. Explicit legacy bootstrap mode supports existing
  calls and rejects deployment.

## Qualification

The resumed implementation is reconciled with current main at `487badf6`.
The complete local DX smoke passed against the catalog-pinned released
hogd 0.1.29-rc.4, using the current scaffold and published SDK 0.41.1:

- An unsaved native key starts at zero; explicit airdrop, funded deployment with
  initialize(21), and native calls succeed. Saving the wallet afterward preserves
  its address, balance, and chain height.
- A separate saved wallet starts at zero and creates no transaction. Explicit
  airdrop precedes funded deployment with initialize(11), readback, increment,
  and a native-to-dev-N transfer with balance-delta checks.
- Dump/restore and restart preserve wallet identity, funds, dev-N identity,
  deployed state, and continued contract progress. Repeated wallet creation
  remains idempotent.
- A one-gas initializer committed a reverted receipt, left the target balance
  zero, emitted no events, and saved no alias. The returned transaction hash
  matched the independently queried receipt.

All four live testkit scenarios passed with the caller-counter fixture rebuilt
on published SDK 0.41.1: atomic initialization, attached and registered code,
manual block production, repeated scenario runs, and registration recovery.
Workspace tests pass (2,544 passed, 29 ignored); workspace build,
strict all-target Clippy, formatting, generated guidance consistency, and packaged
skill inclusion pass.

Source-level process and RPC fixtures cover exact funding, initializer encoding,
network mismatch, dry-run, ambiguous submissions, malformed/mismatched receipts,
alias persistence, and registration recovery. These fixtures complement the
released-runtime smoke; they do not establish hosted network activation.

## Remaining delivery boundaries

The GH-372 CLI/testkit changes need remote CI, merge, and an RPC/CLI release.
Published client delivery is complete; dependent applications still need explicit
adoption of its breaking deployment API. Examples, wallet-extension deployment
approval, and copied application snapshots must describe funding and initializer
effects when they adopt the new client. This work does not claim those consumers
have migrated.

GH-373 owns backend bootstrap, faucet, manifest and artifact retirement, plus
retained-state policy. GH-372 preserves those legacy fixtures for compatibility
with existing identities. Authoritative funding-cost inputs (GH-425), future CLI estimation, and sponsored
deployment (GH-430) are separate follow-ups. No hosted network writes, old-to-new
chain activation, or legacy artifact removal are implied by local qualification.
