# Native-wallet migration: funding decision and delivery plan

Status: implementation started, 2026-09-24. The funding decision below was
accepted for development. No tracker updates or releases have been made.
Parent: [GH-371](https://linear.app/grndhg/issue/GH-371).
Developer migration: [GH-372](https://linear.app/grndhg/issue/GH-372).
Bootstrap retirement: [GH-373](https://linear.app/grndhg/issue/GH-373).

## Accepted funding decision

Use an additive atomic funded-deployment operation in the existing native
DefaultWallet for the normal developer deployment flow. Keep the existing
unfunded `deploy` operation for compatible callers. This requires a backend
release before native deployment becomes the CLI default; it does not require
retiring bootstrap dev wallets first.

The alternative, explicit prefund followed by deploy, remains technically
possible on the current backend. It needs durable recovery for both submissions
and explicit acceptance of potentially stranded target funds. Resuming a failed
deployment is not the same as recovering the money: its address is bound to the
deployer, deployment nonce, and contract hash. Changing broken code changes that
address. The current source establishes no general refund operation for an
undeployed target, whose address is not the sender's pubkey account.

Do not silently fall back from atomic deployment to two money-moving
transactions on older nodes. Retain explicit legacy bindings and report an
unsupported native deployment capability before submission.

## Current evidence and the pre-step

- GH-372 is In Progress, with no linked implementation PR. GH-373 is Backlog
  and blocked by GH-372. The former allocator PR association was incorrect.
- The previous temporary worktree is absent. `mm/gh-372-native-wallets` still
  points at its starting commit, `7389e24`; its reflog contains only creation.
  The prior native signing helpers were uncommitted. Treat them as reference
  work to reconstruct and revalidate, not an implementation ready to rebase.
- RPC main is `035144a48cae3bab9bf62bb4b9c874dcf8cdec32`.
  The managed devnode catalog selects `0.1.28-rc.3`. GH-372's successful native
  transfer and prefunded-deploy experiment used `0.1.27-rc.7` and the TS adapter;
  it did not qualify the Rust CLI. The implementation qualification below now
  covers the Rust signing and public-RPC path on `0.1.28-rc.3`.
- hogd upstream was checked at `a9a8f421cd3cec496e168b2acb2dddeaec204a6d`.
  Its default-wallet implementation and runtime resolution match the inspected
  local checkout. The pinned integration-contract revision is
  `502cf1c8cc041ce6ace1e7290d0d90a30d0fcac1`, also canonical main at inspection.
- The native C++ wallet and Rust reference both expose `execute` and unfunded
  `deploy`. Native fallback applies only at the root. Calling native `deploy`
  as the second child of an `execute` batch cannot implement atomic funding.
  A factory is a different deployer/address scheme, not a transparent substitute.
- Legacy dev-wallet `deploy` transfers 100,000,000 raw HOG to the target first.
  Do not copy that constant: its inadequacy under changed storage prices is
  already tracked in integration_test_contracts #44. The old 50-HOG native
  probe was a feasibility experiment, not a measured minimum.
- Current client-ts at `c17efd7968d8f9c253b77fc0425b2be816d72fd7` already has
  stable deployment preparation, wallet adapters, and receipt/address checking.
  Reuse those concepts rather than create a second deployment abstraction.
- Rust pubkey derivation/auth already exists in signing `account.rs`. The shared
  Rust client's `CallRequest` and `prepare_call` still use the dev-wallet builder;
  migrating only CLI command files would leave downstream consumers behind.

Before broad implementation, close the small contract-design slice below:
pin the supported runtime, specify funding and identity semantics, and run a
repeatable Rust public-RPC qualification against that exact bundle. The old WIP
and old experiment are useful inputs, not the release baseline.

## Funding operation contract

Proposed shape, with final name/selector generated from the canonical interface:
`deploy_funded(deployment_nonce, contract_hash, funding_raw_amount) -> address`.

- Authenticate the same root pubkey account as existing native operations.
  Derive the address from that account, nonce, and canonical contract hash.
- Transfer the explicitly authorized HOG amount to the target and deploy its
  attached code in one transaction. Failure leaves no committed funding transfer,
  deployment, or associated success events. Normal replay/lifecycle bookkeeping
  follows the transaction contract and must be tested separately.
- Preserve existing `execute`/`deploy` selectors and behavior. Update the Rust
  reference/interface, generated bindings, and native C++ implementation together;
  retain native/reference parity tests. Do not add a syscall or weaken root auth
  unless investigation demonstrates that the existing primitives are insufficient.
- Define zero funding, an already-funded target, duplicate deployment, insufficient
  sender funds, insufficient target collateral, changed storage prices, and malformed
  or missing attached code. Funding is an exact transfer, not an implicit
  concurrency-sensitive "top up to balance" calculation.
- Keep target storage collateral separate from sender gas capacity, sender storage,
  and later contract-state growth. Initial deployment funding does not promise
  unlimited future writes or a general withdrawal facility.
- Clients may supply an explicit amount without an estimator. Any advertised
  minimum/default derived from network costs must use authoritative values with
  provenance, through the relevant GH-425 work. The local genesis manifest alone
  is not a current public-network quote. Do not call a chosen amount the minimum.
- Specify activation and compatibility on a network whose default wallet is
  chain-configured. Qualify retained-state upgrades and old/new callers; choose
  an honest capability signal or pinned supported-runtime contract. A new binary
  or a successful unit test does not establish deployment support on a live chain.

GH-427 should qualify whether initialization also needs an atomic operation.
Fund-and-deploy must not be presented as deploy-and-initialize. Keep initialization
and generic contract-effect metadata outside this first primitive unless its
qualification establishes a concrete dependency.

## Implementation slices

| Slice | Scope and ownership | Exit evidence |
|---|---|---|
| 0. Re-establish the contract | RPC/signing + hogd/interface: reconstruct native codecs on current main; pin bundle; lock explicit funding amount, identity selection, and compatibility behavior | Rust native transfer, unfunded failure, prefunded deploy and address/receipt checks against an isolated released node; recorded artifact versions/hashes |
| 1. Atomic funded deployment | integration_test_contracts + hogd, then producer release and SDK bindings | Native/reference parity; successful exact funding/deploy; failure rolls both back; auth, duplicates, changed-cost, gas and missing-code cases; published bundle qualification and activation contract |
| 2. Shared wallet routing | signing + shared Rust client + CLI account resolution | Native pubkey execute/deploy builders, strict single-child return decoding; explicit legacy adapter; stable signed hash before submission; current downstream consumers compile |
| 3. Native account setup and funding | CLI wallet/config, init/checks/status, airdrop | `wallet create` derives/saves an empty native account for a fresh ordinary key, without deploying or funding it; `airdrop` explicitly funds the selected account; local and hosted funding succeeds; saved bindings and dev-N retain identity |
| 4. Transaction commands and testkit | deploy/call/transfer + Rust testkit + bundled starter guidance | Native and legacy runtime matrix passes; dry-run/confirmation/JSON describe actual effects; receipts preserve return values and wallet errors; restart and ambiguous-submission recovery |
| 5. Bootstrap and consumers | GH-373: hogd local/hosted genesis, faucet, manifests, gateway/supervisor funding, bench, sim, E2E and ops renderers | Fresh native bootstrap and hosted funding; versioned schema transition; explicit retained-state policy; all controlled consumers accept the new manifests |
| 6. Artifact retirement and release | packaging + toolchain catalog + docs/examples | Compatible producer bundles published first; consumers qualified on supported targets; dev_wallet removed from runtime requirements; genesis_root retained; offline restart and state recovery |

Slices 2 and 3 can be developed while slice 1 proceeds. Ship the new default
account workflow only once its complete create/fund/deploy/call path passes on
the supported bundle; do not default users into an account that cannot deploy.
Each natural
implementation slice receives independent review, followed by a final end-to-end
review before delivery. Do not resurrect old source wholesale across intervening
client, config, terminal-policy, and test changes.

### Account creation and funding UX

Keep account creation and faucet funding as separate user actions:

```sh
groundhog wallet create
groundhog airdrop
```

For a fresh ordinary key, `wallet create` derives and saves its nonce-0 native
account binding. It submits no wallet-deployment transaction and requests no
funds. Creation must succeed without an available faucet or a funded sender.
Report the account address and an explicit funding next step; do not manufacture
a creation transaction hash, block height, or unqueried balance.

`airdrop` is the explicit request for faucet funds. Its existing optional
recipient already defaults to the effective wallet; extend that shared
resolution to native bindings while preserving current selection rules.
If the newly created binding is not the effective wallet, the creation output
must suggest `groundhog airdrop <wallet-name>` so the next step funds that
account. Show the faucet's actual amount and normal confirmation behavior.

Repeat creation must preserve an existing native address and any funds already
there. "Empty wallet" describes a fresh account, not a promise to reset its
balance. Native creation must reject the legacy `--prefund` option with an
`airdrop` hint rather than silently funding or ignoring the option.

Acceptance tests must prove that fresh creation writes the correct binding with
no transaction submission or faucet call, an unavailable faucet does not prevent
creation, explicit airdrop funds the intended native account on local and hosted
networks, and repeating creation neither changes identity nor requests funds.
Update init guidance, help, JSON reports, and examples to use this two-command
flow. Preserve existing saved legacy and manifest-backed dev-N identities.

This decision concerns funding the user's account. Atomic `deploy_funded`
separately transfers an explicitly authorized amount from that funded account
to a new contract's address and deploys its code; it does not obtain faucet funds.

### Identity and client rules

Existing untyped saved bindings retain their prior legacy interpretation. Newly
saved native bindings carry enough explicit metadata to validate their derivation
and choose authentication. Preserve selected-wallet precedence, owner checks,
ambiguous-binding errors, network binding, and manifest-backed dev-N addresses.
Only an ordinary key with no existing binding defaults to its native account.
Do not infer a new spending identity merely from an owner key or address syntax.

Testkit must exercise native callers for the new acceptance flow, while its
manifest-backed legacy accounts remain explicit compatibility paths. Funding a
native test account from the bootstrap faucet is distinct from silently changing
the meaning of an existing dev-N account.

Use common builders and return decoding for CLI, testkit, and the shared Rust
client. The Stellar CLI adapter is a downstream shared-client consumer to check.
Native execute returns a vector; decode the one expected child for a single call.
Native wallet errors can discard child abort payloads today. Preserve what the
runtime actually returns; GH-426 owns richer failure provenance.

Prepare and show sender, network, target, contract hash, deployment nonce, funding
amount and transaction effects before approval. Honor global `--non-interactive`
without granting consent. Preserve the original signed bytes/hash on ambiguous
submission; observation timeout or `unknown` is not proof of non-execution.
Save aliases only after verified success, and retain enough evidence to recover
local alias-write failures without deploying again.

Version changed command JSON contracts deliberately: deriving an account has no
wallet-creation transaction hash or height. Update help, reports, error hints,
bundled knowledge, and fixtures together rather than inventing those fields.

If the two-transaction alternative is later chosen, it additionally needs a
durable attempt record written before either send, fixed deployment identity and
artifact, separate transaction hashes, process-safe concurrent retry control,
confirmed funding before deployment, and reconciliation after disconnect,
expiry, restart and terminal failure. Do not refresh an expiring funding
transaction or fund again solely because a receipt lookup is inconclusive.
Explicitly disclose that abandonment may leave funds at the target.

## Tracking map and gaps

| Existing work | Relationship |
|---|---|
| [GH-371](https://linear.app/grndhg/issue/GH-371), [GH-372](https://linear.app/grndhg/issue/GH-372), [GH-373](https://linear.app/grndhg/issue/GH-373) | Keep the parent and two delivery boundaries. Add funded deployment as an explicit prerequisite to native deploy enablement. |
| [GH-425](https://linear.app/grndhg/issue/GH-425), Backlog | Authoritative storage costs and effective limits. Required for an automatic trustworthy amount, not for an explicitly supplied amount. |
| [GH-427](https://linear.app/grndhg/issue/GH-427), Backlog | Atomic deploy-and-initialize qualification; coordinate primitive scope without conflating it with funding. |
| [GH-419](https://linear.app/grndhg/issue/GH-419), Todo, including GH-263 | Retry/discard/funding diagnostics. Reuse its lifecycle contract; do not block all native routing on richer diagnostics. |
| [GH-426](https://linear.app/grndhg/issue/GH-426), Backlog | Nested error provenance. Preserve current wallet errors honestly while this proceeds. |
| [integration_test_contracts #44](https://github.com/groundhogchain/integration_test_contracts/issues/44), Open | Existing hard-coded deployment deposit. Decide its legacy disposition explicitly; native migration does not fix saved legacy wallets. |
| [RPC #329](https://github.com/groundhogchain/groundhog-rpc/issues/329), Open | Existing faucet wallet-create cancellation hole. New native account creation can bypass wallet-create, but legacy clients still need a fix or explicit retirement plan. Do not copy that state machine as a recovery guarantee. |
| [RPC #325](https://github.com/groundhogchain/groundhog-rpc/issues/325), Open | Alias recovery after a successful on-chain deploy. Useful adjacent UX, not a backend prerequisite. |
| GH-227, GH-301, GH-266, GH-305, GH-254, GH-404, Done | Delivered native/TS foundations, wallet selection, funding errors and guidance to preserve. Their completion does not mean GH-372 is implemented. |

No dedicated atomic funded-deployment implementation issue/PR was found in the
Linear and GitHub searches performed. Proposed tracking additions, not yet filed:

1. Native DefaultWallet atomic funded deployment, covering canonical ABI, native
   parity, rollback, and supported-runtime activation/release.
2. GH-372 checklist for shared Rust routing, account/config compatibility, CLI and
   testkit acceptance, including TS binding adoption for the new primitive.
3. GH-373 checklist for schema/identity migration, faucet/bench/sim/ops consumers,
   then producer-first packaging and catalog retirement.

Do not duplicate GH-425/426/427 or turn all adjacent work into mandatory blockers.

## Implementation evidence

The Rust signing crate now builds native single-call `execute` transactions and
unfunded `deploy` transactions, binding both the root address and auth claim to
the same pubkey account. It unwraps exactly one child return and rejects malformed
or extra return data. These helpers are additive; shared-client identity routing
and CLI defaults still need the later migration slices.

An opt-in test in `crates/dev/e2e-tests/tests/native_wallet.rs` passed against
published `hog-devnode 0.1.28-rc.3`, producer commit
`8349f7a66c9d04c5a2569b3361e52f4294e02e87`, on macOS arm64. The executable SHA-256
was `c21d03b1a0d3713eaa647403512640788ed603be37b1b3c5bbb33823dc186917`.
It checked native account funding, an unfunded target's storage rejection,
an exact five-HOG transfer to a distinct target, deployment with attached code,
then Counter initialization and readback through native `execute`.

The public balance assertions deduct replay and balance-item storage collateral
from the local manifest. The manifest is checked against the RPC network hash;
this fixture does not provide a production funding estimate. The negative
transaction's target is never funded or reused because `recentFailure` is
advisory, not final rejection. Each positive submission gets a fresh expiry,
prints its precomputed hash, and checks the server's returned hash.

The additive backend operation is implemented in the canonical contract at
`e1b139fb8461e848cb1666b16c5ad0d9f31c45ba` and hogd commit
`4d1d2f81ea8203654fa24c6dbe707b75fa09ae38`. These commits are local and unpublished.
Generated bindings preserve `execute` (`c3edb11fdd6d5d58`) and `deploy`
(`6be0cb0f6de0917a`); `deploy_funded` is `1f843596f5b57b18`.
The Rust signing crate also provides the funded builder, with an explicit raw
amount and no automatic capability discovery or two-transaction fallback.

Backend tests prove committed funding and deployment, exact Transfer events,
duplicate/unknown-code rollback, insufficient sender funds and target collateral,
zero additional funding with existing collateral, authentication, and
native/RV32 parity. Reverted transactions retain normal replay bookkeeping:
the funding transfer rolls back, but the sender can still pay replay collateral.
A rejected two-phase commit leaves no included transaction or funding effects.
The prior unfunded operation now has an explicit successful-commit regression test.

Both Rust public-RPC tests passed on a fresh local devnode built from the backend
change: prefund then deploy, and single-transaction funded deploy. Each also
initialized and read the deployed Counter. The local executable SHA-256 was
`09b2bfda83a198c8d68568f994b07d197dfc51e70782ae876b00a9f890e518f8`;
it reports the base hogd revision plus a dirty source marker, not a published
release. The Clang 23 build needed `-include cstdlib` for RocksDB's
`util/stderr_logger.cc` object only; this was a generated-build override and
does not change dependency source. The test now uses OS randomness for fresh
accounts so concurrent cases cannot collide through timestamp resolution.

Validation: 25 Rust signing tests, five canonical wallet tests, 94 backend
wallet/token/deployment tests (one pre-existing invariant-test skip), and both
local public-RPC acceptance tests. Independent slice and whole-change reviews
found no remaining correctness findings after the test-accounting fixes.
The Rust workspace compiles, and focused signing/E2E Clippy and formatting checks
pass. Full workspace tests and the repository's Linux CI matrix remain unrun.
Changed-line Clang 23 lint checks pass for all three modified C++ files. The
unfiltered local run reports existing/generated-header diagnostics, so this is
not a claim that the full repository lint suite passes on that toolchain.

These results establish the source implementation and a fresh local runtime
path. Publication, network activation/capability selection, retained-state
upgrades, shared-client routing, and CLI migration remain delivery gates.

## Source anchors

- [Native wallet implementation](https://github.com/groundhogchain/hogd/blob/a9a8f421cd3cec496e168b2acb2dddeaec204a6d/hogd/hog_exec/precompiles/precompile_default_wallet.cc)
  and [runtime resolution/deployment](https://github.com/groundhogchain/hogd/blob/a9a8f421cd3cec496e168b2acb2dddeaec204a6d/hogd/hog_exec/runtime/syscall_handler.cc).
- [Canonical wallet interface](https://github.com/groundhogchain/integration_test_contracts/blob/502cf1c8cc041ce6ace1e7290d0d90a30d0fcac1/default_wallet/src/interface.rs)
  and [legacy funded deployment](https://github.com/groundhogchain/integration_test_contracts/blob/502cf1c8cc041ce6ace1e7290d0d90a30d0fcac1/dev_wallet/src/implementation.rs).
- [TS deployment preparation](https://github.com/groundhogchain/client-ts/blob/c17efd7968d8f9c253b77fc0425b2be816d72fd7/src/wallet/deployment.ts)
  and [wallet adapters](https://github.com/groundhogchain/client-ts/blob/c17efd7968d8f9c253b77fc0425b2be816d72fd7/src/wallet/invocationAdapter.ts).
- RPC: `crates/tools/signing/src/{account,build}.rs`, `crates/client/src/lib.rs`,
  `crates/tools/cli/src/{account_resolution,deploy,wallet,call,transfer}.rs`,
  `crates/tools/cli/src/config/project.rs`, `crates/dev/testkit/src/account.rs`,
  `crates/json-rpc/json-rpc-types/src/{network_config,runtime_manifest}.rs`,
  `crates/tools/toolchain/catalog/hog-devnode.json`.
- Packaging dependency inventory was read locally at `9e5352e28dfd1c10ad2d87df16cf51a75c65c56a`:
  tarball/macOS assembly, public client/devnode publishers, faucet renderer and
  qualification scripts. Ops inventory was read locally at
  `6f235734e0b84df0a855d188c740d9a883bc49ad`: hosted bootstrap and faucet rendering.
  These two inventories were not refreshed against upstream during this pass.

First implementation deliverable: the bounded slice-0 Rust qualification and
canonical funded-deployment contract, followed by the reviewed backend primitive.
No current-live-chain or new-bundle runtime claim is implied by this plan.
