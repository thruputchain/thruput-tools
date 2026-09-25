# Native-wallet migration: funding decision and delivery plan

Status: unified deployment revision in development, 2026-09-24. Canonical #56
and hogd #4144 are open; no releases have been made. Breaking the unpublished
deployment ABI is accepted in favor of one simple API.
Parent: [GH-371](https://linear.app/grndhg/issue/GH-371).
Developer migration: [GH-372](https://linear.app/grndhg/issue/GH-372).
Bootstrap retirement: [GH-373](https://linear.app/grndhg/issue/GH-373).

## Accepted funding decision

Use one atomic deployment operation in DefaultWallet:
`deploy(nonce, contract_hash, amount, initializer: Option<WalletInitCall>)`.
Remove the separate unfunded and funded entrypoints. A zero amount skips the
token transfer; a positive amount transfers exactly the caller-authorized HOG.
An optional initializer runs on the newly deployed contract, from the wallet,
before the transaction completes. This requires a supported backend release
before becoming the CLI default; it does not require retiring bootstrap wallets
first. Existing bootstrap plumbing remains only where it is still required.

The alternative, explicit prefund followed by deploy, remains technically
possible on the current backend. It needs durable recovery for both submissions
and explicit acceptance of potentially stranded target funds. Resuming a failed
deployment is not the same as recovering the money: its address is bound to the
deployer, deployment nonce, and contract hash. Changing broken code changes that
address. The current source establishes no general refund operation for an
undeployed target, whose address is not the sender's pubkey account.

Do not silently fall back from atomic deployment to two money-moving
transactions on older nodes. Retain explicit legacy bindings and guard native deployment before submission
unless the operator explicitly opts into a qualified experimental runtime.
The current public RPC has no reliable capability signal.

## Initial investigation (before implementation)

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

Canonical shape:
`deploy(nonce, contract_hash, amount, initializer: Option<WalletInitCall>) -> address`.
`WalletInitCall` contains only `entrypoint`, `gas_limit`, and `calldata`.

- Authenticate the root pubkey account. Derive the address from that account,
  nonce and canonical contract hash.
- Transfer the explicit amount only when nonzero, deploy, then invoke the optional
  initializer on that address. The initializer sees the wallet as caller and
  receives no forwarded auth tokens. Discard its return value and return only the
  deployed address. No constructor-name convention or generic effect metadata.
- Failure rolls back funding, deployment, initializer state and success events.
  Normal transaction replay bookkeeping still applies. Contracts must guard
  subsequent initializer calls according to their own authorization policy.
- Share child-call execution and error handling with `execute`: initializer aborts
  become `CallAborted`; invocation faults become `CallFaulted`.
- Update the canonical Rust interface, generated bindings, native C++ runtime,
  Rust builders/CLI and TypeScript adapters together. Do not retain the obsolete
  native ABI or add a decoder fallback. The method-name-derived `deploy` selector
  remains `6be0cb0f6de0917a`; its argument encoding intentionally changes.
- Test zero funding, existing target collateral, duplicate deployment, insufficient
  sender funds and target collateral, malformed/missing code, initializer calldata,
  caller identity, aborts, faults and gas limits in native/reference parity.
- Funding covers deployment and initializer storage. It is an exact transfer,
  not a minimum estimate or a promise of unlimited later state growth. GH-425 owns
  authoritative cost inputs for a future estimator.
- Runtime activation still matters even without backward compatibility. Select a
  qualified runtime explicitly until the producer bundle and network configuration
  support the new ABI; never fall back to separate funding/deployment submissions.

This implements the wallet primitive needed for GH-427's atomic initialization
qualification. Generic contract-effect metadata remains separate.

## Implementation slices

| Slice | Scope and ownership | Exit evidence |
|---|---|---|
| 0. Re-establish the contract | RPC/signing + hogd/interface: reconstruct native codecs on current main; pin bundle; lock explicit funding amount, identity selection, and compatibility behavior | Rust native transfer, unfunded failure, prefunded deploy and address/receipt checks against an isolated released node; recorded artifact versions/hashes |
| 1. Atomic deployment and initialization | integration_test_contracts + hogd + Rust and TypeScript consumers | Native/reference parity; exact funding, optional initialization and complete rollback; caller identity, zero transfer, gas, duplicates and missing-code cases; published bundle qualification and activation contract |
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

This decision concerns funding the user's account. Atomic `deploy`
separately transfers an explicitly authorized amount from that funded account
to a new contract's address, deploys its code and optionally initializes it; it does not obtain faucet funds.

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
amount, initializer entrypoint/calldata/gas and transaction effects before approval. Honor global `--non-interactive`
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
| [GH-427](https://linear.app/grndhg/issue/GH-427), Backlog | Atomic deploy-and-initialize qualification; the unified wallet operation addresses the missing primitive. Generic method-effect metadata remains separate. |
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

## Earlier implementation evidence

The following milestones describe the earlier two-entrypoint ABI. They record
prior experiments, not qualification of the unified deployment revision above.
Current-revision verification is recorded separately below.


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
`4d1d2f81ea8203654fa24c6dbe707b75fa09ae38`. The changes are published as draft
[canonical #56](https://github.com/groundhogchain/integration_test_contracts/pull/56)
and [hogd #4144](https://github.com/groundhogchain/hogd/pull/4144). The backend
head is `a525364926b64a0ea929b3a40940415d09b7850c` after test-only CI fixes for
explicit optional guards and parity-harness event finalization. Neither is released.
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

## Empty creation and shared routing slice (local implementation)

The CLI now saves native bindings with `kind = "native"` and `account_nonce = 0`.
Untyped existing bindings remain legacy. Native owner, nonce and derived address
are validated during sender selection and recipient lookup. Creation derives
from the stored signer, does not contact RPC or faucet services, and performs a
locked, idempotent config insertion that preserves existing identities and
explicit or inherited defaults. Dry-run does not write config. `--prefund` is
rejected; the funding hint identifies the exact account address and retains
explicit target overrides.

The shared Rust client has additive native/legacy preparation and native
single-child return decoding. Its existing `CallRequest`, `prepare_call` and
`SubmittedCall { tx_hash }` APIs remain available. CLI calls and transfers select
the transaction format from the saved binding, and native signing checks the
configured network against the endpoint. The subsequent deployment slice below
adds an explicitly opted-in native path while retaining the default guard.

Local runtime qualification on the source-built hog-devnode described above:

- A new CLI-created account had a queried balance of zero at height 1.
- Explicit `airdrop` sent 10 HOG to the same address at height 2. Its observed
  free balance was 9.9998766 HOG after storage collateral, not a fabricated 10.
- Native `transfer` sent 1 HOG to the manifest-backed dev-1 wallet at height 3.
- Legacy dev-1 deployed the Counter fixture at height 4.
- Native CLI calls initialized Counter to 42 at height 5 and read 42 at height 6.

These are source-build results, not qualification of a published bundle.
Hosted airdrop is covered by a local HTTP/RPC fixture, not a public Devnet write.
Testkit migration, ordinary unsaved-key fallback, downstream
consumer qualification and supported backend publication remain separate gates.
Do not ship the native default before the complete supported deployment flow is
available. The backend draft PRs above require the canonical change first.

The existing `scripts/smoke-local-dx.sh` still encodes legacy auto-prefunding
and new-wallet deployment expectations. Updating and passing that complete
smoke is a release gate for the next transaction-command slice; the focused
live checks above do not replace it.

Validation for this slice passed: full workspace tests and build, all-target
Clippy with warnings denied, formatting, generated knowledge consistency and
CLI package inclusion. The CLI binary suite includes 298 passing tests.

## Native CLI funded deployment slice

Native `deploy` now requires `--experimental-native-deploy --fund <HOG>`.
The amount is exact and positive, with no implicit deposit or estimator. The
flag records `runtimeSupport: "operator_asserted"`; it is not a capability probe.
The public version, chain-config and view methods cannot reliably prove that
a chain has activated `deploy_funded`. A supported producer release and explicit
activation contract remain prerequisites to enabling native deploy by default.

The CLI prepares one signed transaction and shows its funding and identity
before approval. It submits once, compares the returned hash, checks receipt
identity and requires the exact predicted address before saving an alias.
Send errors, lookup failures, timeouts and malformed success results retain the
original hash, signed bytes, predicted address, endpoint, nonce, expiry and
funding details. They never trigger prefunding or resubmission. Native aborts
remain generic when the wallet does not expose the nested cause. Existing legacy
deployment semantics are preserved. Deploy JSON advances to schema version 5.

Process tests cover explicit opt-in and amount validation, legacy flag rejection,
network mismatch, signed dry-run identity, admitted-send errors, mismatched
submission/receipt hashes, lookup failures, missing/short/wrong return addresses,
reverts and successful alias persistence. A polling timeout test checks that the
CLI does not submit again.

Live CLI qualification used the same source-built backend after restarting its
retained local state (`chain_state: recovered`):

- Dry-run returned a signed transaction and predicted address without submission.
- Atomic deployment transferred five HOG and deployed Counter at height 7.
  Receipt `9281d74d955ee3fd867cdd27883eaeb0f51bd5d06245aefd64f427bffc30b4c7`
  returned the exact predicted address; its Transfer event named the sender and
  target. The CLI saved the verified alias.
- Native calls initialized Counter to 42 at height 8 and read 42 at height 9.
  After initialization, the contract's free balance was 4.9918756 HOG; the
  transfer amount and free balance differ because of storage collateral.
- Legacy deployment still succeeded at height 10.
- A separate one-raw-unit deployment reported `recentFailure` with an
  `insufficientFunds` conflict. The CLI preserved its original signed bytes,
  hash, target and funding details, submitted once and saved no alias. That
  advisory failure is not a final rejection; its target was not funded or reused.

This checks CLI behavior on a retained source-built test chain. It does not prove
an old-to-new backend activation or a published bundle upgrade. Process tests
also cover recovery with an explicitly selected profile and no default, and
alias-write failure after verified on-chain success.

Remaining GH-372 work: native testkit and ordinary unsaved-key routing, complete
local DX smoke migration, downstream shared-client compatibility, supported
bundle qualification and retained-state activation/recovery. GH-373 bootstrap,
manifest, faucet and consumer migration, then artifact retirement, remain later
slices. The experimental CLI implementation does not close those release gates.

Validation for the native deployment slice: full Rust workspace tests passed
(2,470 passed, 28 ignored), workspace build, all-target Clippy with warnings
denied, formatting, generated knowledge consistency and CLI package inclusion.
Eight native deployment process tests pass. Independent slice and final reviews
are clean after preserving the explicit profile in recovery commands. Local
backend wallet tests pass (38 passed, one existing skip); the backend CI rerun
must confirm the debug event lifecycle and explicit optional guards.

## Unified deployment revision

Geoff's review prompted a simpler API: keep only `deploy` with explicit amount
and optional initialization. Backward compatibility with the old native argument
layout is deliberately dropped. Canonical #56 is at `e13d297`; hogd #4144 is at
`0e915d40d` after direct-include, test-copy and fixture trace-snapshot fixes. Rust signing/CLI and
TypeScript bindings/adapters use the new layout. Geoff approved canonical
`e13d297`; his unified-method and optional-initializer threads are resolved.
The TypeScript high-level API requires an amount and accepts an existing typed
invocation as initializer, validating its target before encoding the targetless
wallet call. It no longer offers legacy DevWallet deployment through that API.

The CLI accepts `--fund 0`, `--init`, repeated `--init-arg`, and
`--init-gas-limit`. Its total gas budget includes both deployment and the selected
initializer allowance; overflow is rejected before RPC. Preview, schema-6 JSON
and recovery data include the initializer and gas budgets.

Current validation:

- Rust workspace: 2,474 passed, 28 ignored. Workspace build, all-target Clippy
  with warnings denied, formatting, generated knowledge and package checks pass.
  The subsequent simulator migration passed 328 tests (seven ignored), plus
  all-target Clippy with warnings denied.
- Eight canonical native tests and Clippy pass. Backend tests: 137 passed,
  two existing skips. Native/RV32 tests prove caller identity, no forwarded auth,
  exact funding, zero-transfer behavior, discarded output, initializer gas,
  rollback of code/state/events and existing execute behavior.
- TypeScript: 772 tests passed, two existing skips; all required build, type,
  documentation, package, formatting and generated-binding checks pass. Eight
  built-package tests passed against the source-built local node, including
  atomic funding/deployment/initialize(42), readback and event handling.
- A fresh local CLI wallet had zero balance, then received an explicit airdrop.
  Named initialize(42) committed with deployment at height 3 and read back 42.
  A named initializer abort at height 5 left the target balance zero, emitted no
  events and saved no alias; only seven raw units of normal replay collateral
  were charged. Final CLI initialize(77) with a 500,000-gas child budget committed
  at height 17; readback survived a restart of the final source-built runtime.
  A one-gas initializer reverted at height 18 with `CallFaulted` and no saved alias.
- The live CLI fixture was built from the current scaffold with SDK 0.38.0.
  Artifact SHA-256: `505fb86826972518a650714d6336f8362705d97379380c623792867e7bb9a80a`.
  TypeScript regeneration uses its pinned CLI 0.18.0 for the backend SDK 0.36.1
  spec format. The development CLI uses a newer spec format; no legacy decoder
  was added. Aligning published compiler/spec/runtime artifacts remains a release
  requirement.
- Independent slice and combined reviews found one stale CLI README section;
  it was corrected. No outstanding correctness findings were reported. CI also
  caught an exact gas snapshot that needed updating after the initializer
  fixture changed its dispatcher cost; trace-shape assertions are preserved.
  All eight trace-shape tests and 137 wallet/runtime tests pass after that fix
  (two existing runtime skips). Current-head backend CI is pending.

This qualifies source builds on a disposable local chain, including a restart
with the same ABI. Published runtime delivery and an old-to-new chain activation
are not established by these results. Native testkit/default-key routing and
bootstrap retirement remain the later migration slices listed above.

The simulator now uses the same signing builder with its configured amount and
no initializer. Its separate prefunding transaction and duplicate transaction
metadata are removed. It records the signed hash before dispatch, consumes one
nonce, and verifies the submission hash, receipt hash and returned address.
Independent review of this follow-up found no issues. Native testkit routing
remains a distinct later slice.

Published-client consumers need an explicit migration when adopting the new
package/runtime: `groundhog-examples` Counter, swap and token-launchpad provisioning
currently encode the old deployment arguments and initialize separately. The
wallet extension's deployment approval presentation must also describe funding
and initializer effects. Hackathon snapshots and dispatch app variants contain
copies of that older published API. These applications are outside this runtime
and client revision; do not treat the source qualification above as a completed
consumer rollout or keep an old decoder to accommodate them.
