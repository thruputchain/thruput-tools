# Proposal: native RPC delivery plan

**Draft for team review, 15 October 2026.** Read the [API proposal](api-proposal.md)
first. Implementation and Linear changes remain on hold until explicitly approved.

The release path is **land the slices → assemble one artifact set → deploy fresh
devnet → qualify → announce**. Internal candidates can iterate. There is no user
state to migrate or legacy endpoint to maintain.

## Scope and required fixes

The announced endpoint includes all 15 core methods. Naming, encoding and field
changes mostly reproject existing behaviour. These guarantees need real fixes:

| Area | Required work | Slice |
| --- | --- | --- |
| Wire contract | Strict request DTOs; matching dispatch, Rust client and generated schemas; typed errors and tolerant response readers | 1, 4 |
| History | Typed cursor/query errors; fail iterator, key and hydration errors; process-safe consistency checks; reject regressed watermarks; candidate cap and direct seeking | 2 |
| Submission | Runtime duplicate correctness; truthful admission/status; dispatch evidence through queues and deadlines | 2, 3 |
| Receipt | Validate backend hash/height before parsing; require return/abort bytes; fail malformed events; preserve commitment through later failures | 3, 5, 6 |
| Identity | Match manifest protocol to linked codecs; reset old Thruput stores/approvals while preserving Stellar keys and flows | 3, 7 |
| Consumers | SDK/history workflows, Rust callers, faucet, chuckd, proxies, generated projects and operational tools | 4–8 |

The [contract appendix](appendix/contract-details.md) defines the acceptance
rules. Omitting history from announcement would require an explicit product
scope change and corresponding CLI, explorer, smoke and PQ-demo changes.

Funding stays narrow: migrate the optional methods and consumers, preserve
signed evidence and retry hints, and correct stop/unresolved errors. SDK
`requestAirdrop` calls `faucet_requestAirdrop` once on hosted or local endpoints,
then waits through read-only status/receipt calls when pending includes a hash.
Durability, replica coordination and richer funding outcomes remain follow-up work.

## Eight review slices

Slices can contain several PRs. **Start** describes available development inputs;
**land** requires buildable commits with matching dependencies. Publication and
final qualification have separate gates below. All starts require the hold to be
lifted and shared-file ownership to be agreed.

| Slice | Start | Landing dependencies | Acceptance boundary |
| --- | --- | --- | --- |
| **1. Wire foundation and fixtures** | Reviewed contract and inspected GH-506/507 diffs | Agreed codecs/vectors and wire major; include required same-workspace compile fixes | DTOs, registration/client plumbing and generated schemas agree on names, params, optional/null rules and errors |
| **2. Runtime correctness** | Current backend and agreed history/submission semantics, alongside 1 | History and duplicate repairs can land independently; final history integration needs matching protocol/bindings and slice-1 error types | Both history methods meet integrity rules; candidate cap/direct seek advance correctly; nominated duplicates and full-capacity admission are safe |
| **3. Server and adapters** | Usable types/fixtures from 1 | Non-history handlers can land independently of 2; final history exposure needs 2; include Rust compile fixes from 6 | Gateway/local parity, codec checks, truthful admission/status, validated receipt evidence, event integrity and configured discovery |
| **4. TS transport and readers** | Stable fixtures from 1, alongside 3 | Matching encoding/provider codecs; can land before live server integration, which is required before release | Named params, canonical codecs, error/result/history-page decoding, wire/network/method gates and raw-preserving unknown variants |
| **5. SDK workflows** | State-machine and history-walker fixtures alongside 3/4 | Integrate with types from 4 and server contract from 3; can land with automatic redispatch disabled | Call/deploy/wait/assert composition, retained evidence, sticky commitment, single-call funding and complete history walking/checkpoint rules |
| **6. Rust, bootstrap and generated consumers** | Producer interfaces from 1/3; metadata and Go bindings for chuckd | Rust fixes accompany producers; bootstrap tooling precedes SDK publication; final generated TS consumers follow 4/5; chuckd follows metadata/bindings independently | Rust receipt waiting and CLI output, faucet RPC client, bootstrap/manifest path, generated projects and Ed25519/PQ smokes |
| **7. Apps, wallet and documentation** | Proxy/cache/identifier audits and fixtures | SDK integrations need 4/5; generated examples need the relevant part of 6; align React peers and wallet/provider versions | All three proxies, explorer/history flows, fresh/reset wallet state, preserved Stellar behaviour and runnable docs |
| **8. Launchers, artifacts and qualification** | Launcher/probe audit and workload/threshold design | Compatible operational fixes can land early; final qualification needs the complete pinned artifact set | Install/restart, hosted bootstrap, live flows, method/error metrics and accepted resource measurements |

Slice 2 has independent history and scheduler tracks. Coordinate history with
[hogd #4454](https://github.com/thruputchain/hogd/pull/4454), which still skips
missing hydration at the reviewed head, and
[#4458](https://github.com/thruputchain/hogd/pull/4458), which addresses direct
seeking. Include [#4468](https://github.com/thruputchain/hogd/pull/4468)-equivalent
nominated-duplicate repair and [#4471](https://github.com/thruputchain/hogd/pull/4471)-equivalent
full-capacity coverage before announcement, even when SDK retries are disabled.
Raw callers can submit duplicates themselves. The current [scheduler cleanup](https://github.com/thruputchain/hogd/blob/15d536912ebe70dae5a9b4b471440b928a6f365c/hogd/hog_mempool/scheduler.cc#L1451-L1454)
requires a cached entry for each nominated hash.

Slice 6 has three landing paths, not a dependency on the whole SDK:

- **Rust consumers:** compile fixes land with 1/3; sticky receipt waiting, CLI
  receipt rendering and the faucet client complete against those interfaces.
- **Generator/bootstrap:** prepare the tools candidate needed to build the SDK;
  pin final generated TS templates only after the matching SDK is available.
- **chuckd:** migrate hogd metadata parsing and hosted manifest output with the
  matching Go bindings; qualify its final installed artifact in slice 8.

Slices 1/2 and operational preparation can start in parallel. Server and TS work
then proceed from shared fixtures. SDK/app work follows usable interfaces rather
than waiting for entire slices. **Automatic exact-byte redispatch is optional:**
slice 5 and announcement can complete with it disabled. Enabling it additionally
requires nomination, abandonment, reenqueue and replay qualification plus the
[recovery preconditions](appendix/contract-details.md#sdk-recovery-policy).

## PQ, encoding and ownership

Inspect the actual GH-507/GH-506 diffs before editing shared types, parsers or
schema code. A supplied patch or local review is sufficient; branch publication
is not required. Coordinate GH-502/503 vectors, GH-513 protocol/bindings, GH-505
manifests and GH-500 PQ qualification. PQ binary/signing changes, textual encoding
and native API changes must converge on matching artifacts.

Wire 13 is the planning target, subject to confirming it is unpublished with a
different contract. Assign one wire-major owner. Freeze token event/error
signature identity at version 0.1 and `groundhog-erc20-if`; run existing native/RV32
parity tests against final pins. Package renaming alone does not preserve that identity.

Before work starts, name component/shared-file owners, the publication owner and
the qualification owner. Existing issue assignees do not implicitly own this
expanded scope. Refresh overlapping PRs: tools #553/#292/#293/#394/#469,
Freighter #3, examples #134 and ops #230. Tools #187 and the earlier StrKey branch
are design inputs to reconcile, not additional API commitments.

## Consumer coverage

| Consumer | Required migration and checks |
| --- | --- |
| CLI, client, simulator, testkit, bench | Typed calls and reports, Rust receipt waiting, generated templates, offline build behaviour |
| Hosted faucet | Typed tip/send/receipt client, telemetry and final artifact pin |
| chuckd | Bootstrap metadata, Go protocol pin, hosted manifest output and packaged binary |
| TS SDK/provider | Evidence, discovery, approvals, package exports and unsupported outcomes |
| thruput-react | Case-sensitive identifiers, network/query caches, peer ranges and example pins |
| Freighter/Stellar adapter | Provider gate, fresh install/reopen, Thruput reset and preserved Stellar keys/flows |
| Counter, swap, token-launchpad | Named params, allowlists, filtered discovery and bounded typed errors; preserve each proxy's write policy |
| Explorer | SDK/codecs, generated bindings, root/event history and pins |
| Packaging/ops | Installed commands, probes, report readers, eBPF labels/map sizes/symbols and collectors |
| Docs/agent guidance | Runnable examples, selected bundled snapshot and guidance in its owning repository |

Remove obsolete `--nthreads`/`-n` and `EXEC_NTHREADS` launcher wiring and test
packaged commands. Remove the unsupported CLI detail probe; preserve the event
formatter used by `view` and audit exported types before deletion. Proxies must
advertise the intersection of upstream support and their policy. Test all three;
shared helpers are optional.

Metrics cover every registered method and selected error family with bounded
labels. A `-32011` relabel cannot precede freshness moving to `-32012` unless it
distinguishes framework/application origin. These fixes can land independently
when compatible with the current wire.

## Artifact and release order

Keep main buildable and freeze stable publication of affected packages while
partial breaking changes land. Current tools/SDK publishing is manually dispatched.

1. Select matching protocol/bindings, including Go, and runtime. Qualify and
   publish the public devnode required by the tools catalog.
2. Build the slice-6 bootstrap tools candidate and publish a controlled RC if
   needed by SDK publication. Local `THRUPUT_CLI`/`THRUPUT_DEVNODE_BINARY`
   overrides do not establish an equivalent hosted publication path.
3. Qualify and publish the SDK. The current npm workflow has no isolated `next`
   channel; keep candidates local unless channel support is explicitly qualified.
4. Pin final tools templates to that SDK. Assemble tools, chuckd, images,
   manifests, React, Freighter, apps, probes and docs at exact versions.
5. Deploy fresh devnet, qualify the installed set and announce after acceptance.

The hosted path must pass **hogd metadata → chuckd bootstrap commit → hosted
manifest → gateway startup** with matching codecs. Preserve raw XDR as hex and
keep Git/artifact identities distinct from chain hashes. Bootstrap packages are
build dependencies, not an intermediate hosted API. Changed final artifacts need
qualification again.

Relevant source: [chuckd bootstrap](https://github.com/thruputchain/chuckd/blob/6db13472df621feba39626c49e65e292f8baef16/chuckd/cmd/hosted_bootstrap.go),
[Go bindings pin](https://github.com/thruputchain/chuckd/blob/6db13472df621feba39626c49e65e292f8baef16/go.mod),
[packaging pin](https://github.com/thruputchain/packaging/blob/73566ec94a6152b5bc4899b15e43ca005aa60b3c/release-manifest.toml#L21-L26),
and [tools manifest reader](https://github.com/thruputchain/thruput-tools/blob/d0a8cdc93c70a0172ebc2d60cab320a0b03b0f99/crates/json-rpc/json-rpc-types/src/runtime_manifest.rs).

## Deferred work

| Follow-up | Release boundary |
| --- | --- |
| General scan budgets/cancellation | Candidate cap/direct seek are required now; time/byte budgets and cancellation follow. Qualify work after timeout. |
| Retention, repair and reset recovery | Strict fresh origin and within-instance checkpoints; no durable cross-reset cursors. |
| Replacement/nonexecution proof | No automatic expiry/nonce replacement or inference from missing/dropped status. |
| Funding durability/richer outcomes | Shared storage, replica ownership/retention, original-amount replay, persisted terminal observations and richer producer outcomes follow. No durable hosted recovery promise now. |
| Optional Stellar copy/QR | Base58 remains default; C-format display needs separate cross-chain safety qualification. |
| Dynamic capability typing/transport metadata | Runtime gates now; broader type redesign and layer-specific limit discovery later. |
| Independent event-version override | Freeze existing identity now; add controls only when package versions must cross it. |
| New API capabilities | Block/body/envelope reads, registered PQ, global feeds, health and historical state need separate producers/contracts. |

## Review and readiness

Announcement requires:

- All 15 core methods, truthful optional discovery and DTO/schema/client parity;
  all 12 application error kinds under 11 code components, plus framework errors.
- History integrity and runtime duplicate correctness from slice 2, independently
  of whether automatic SDK redispatch is enabled.
- Gateway/local parity, Rust/TS unknown-variant handling, sticky commitment and
  the local faucet pending-to-receipt path.
- Installed CLI/generated-project, faucet/proxy and wallet connect/sign/reopen/
  reset tests; Ed25519/PQ/mixed invalid traffic, envelope/batch bounds, lost replies,
  receipt lag and malformed-data checks.
- One exact artifact set, working installation/restart and bounded method/error
  metrics. A named owner must set dataset, query/submission mix, concurrency,
  duration and numerical latency/resource/overload/recovery thresholds before
  measurement, including work after timeout or abandonment.
- A public admission policy and acceptable measured resource envelope. Failure
  blocks announcement unless product scope and its consumers are explicitly changed.

Track native qualification separately, linked to GH-512 encoding, GH-500 PQ,
GH-422 lifecycle and GH-432 admission/resource work. No runtime qualification is
claimed here. The [source baseline](appendix/contract-details.md#source-baseline)
records the review boundary.

Proposed Linear changes remain unapplied: link the contract under GH-413/GH-470;
clarify shared ownership under GH-502/503/506/507; expand GH-428 history acceptance;
record duplicate correctness, SDK history walking, Rust commitment, chuckd/proxy
coverage and wallet reset; assign qualification ownership and separate follow-ups.
