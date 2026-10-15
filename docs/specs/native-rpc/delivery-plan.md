# Proposal: native RPC delivery plan

**Draft for team review, 15 October 2026.** Read the [API proposal](api-proposal.md)
first. Implementation and Linear changes remain on hold. This document proposes
scope and sequencing; it does not assign people or authorize releases.

We can replace devnet without preserving user state. The path is:
**land the slices → assemble one final artifact set → deploy fresh devnet →
qualify → announce**. Internal candidate builds can iterate. Compatibility across
components must be established before announcement.

## Scope: contract changes and implementation gaps

Most naming and field work reprojects existing behaviour, but several promises
need real fixes. The split matters for estimates and parallel work.

| Area | Contract and consumer migration | Behaviour that needs implementation or qualification |
| --- | --- | --- |
| Rust API, types and CLI | Bare names, height fields, nested execution, error numbers, config groups and call sites | Strict request DTOs shared by dispatch, client and generated schema; preserve evidence through errors |
| Gateway and local node | Configured registration, discovery and manifest fields | Codec/protocol equality, status fallback, dispatch evidence, receipt identity and fail-closed event projection |
| TS SDK, React and applications | Methods, params, codecs, pins, case-sensitive identifiers and proxies | Discovery gates, immutable submission/commitment evidence, conservative recovery and history walking |
| hogd and protocol | Existing PQ/encoding integration and manifests | History integrity, candidate-bounded event scans, typed cursor/query failures and nominated-duplicate correctness |
| chuckd | Hosted bootstrap metadata and runtime-manifest codecs, matching Go bindings | Preserve protocol/hash equality checks through the installed bootstrap-to-gateway path |
| Packaging and ops | Native probes, metric tables and removed launcher flags | Artifact acquisition/launch/restart and a measured public resource envelope |

The full announced endpoint includes all 15 core methods. Partial developer
endpoints remain useful, but omitting history would require an explicit product
scope change and changes to CLI, explorer, smokes and the PQ demo. It is not an
automatic escape from unfinished integrity work.

### Required correctness work

- **History:** typed invalid/unsupported/mismatched cursors; query binding;
  iterator I/O/corruption and malformed keys; missing or invalid hydration;
  out-of-range event positions; typed query errors instead of process-killing
  consistency assertions; rejection of a serving watermark below cursor W.
  Cover both event and root-contract history. Add a positive event candidate cap
  and an advancing last-validated-candidate cursor, including nonmatches. Pair
  this with direct seeking; test both directions, overlapping filters, empty
  nonterminal pages and exact exhaustion. No partial success on integrity error.
  Candidate count alone does not bound latency, bytes or cancellation tails.
- **Receipt and simulation:** fail malformed events and contradictory execution
  data. Require return/abort bytes for their respective receipt variants. Check
  the backend log hash and height before returning receipt or commitment evidence;
  error observations contain only hash/height. Fix commitment retention in the
  shared Rust waiter as well as the TS SDK, including later errors and timeouts.
- **Status and admission:** preserve commitment and surface indexer failure when
  only a weaker negative scheduler observation is available. Map scheduler
  AuthenticationRejected, Retryable and InternalError to their distinct error
  families, with rejection evidence scoped to this attempt. Carry phase/evidence
  through gateway and local submit queues and outer deadlines. `notAttempted`
  requires proof that cancellation or queue removal prevents later dispatch;
  otherwise preserve uncertainty. Hashes are optional until validly computed.
- **Duplicate submission:** require hogd #4468-equivalent nominated-cache
  correctness and #4471-equivalent full-capacity coverage. Disabling SDK retries
  cannot protect the server from manual duplicates. Enable automatic recovery
  only after its broader nomination, abandonment, reenqueue and replay tests.
- **Identity:** require configured manifest protocol to match linked codecs.
  Reset/version old Thruput stores and approvals; preserve Stellar vault/keys.
  A network hash alone is not a reset identifier.

Early internal fail-closed history fixes can land independently. The final
contract still needs typed protocol/binding outcomes and query binding. Current
hogd #4454 still skips missing hydration; #4458 supplies direct seek work to
combine with the candidate cap. General byte/time budgets and cancellation remain
separate. Coordinate those changes rather than duplicate them.

### Narrow funding scope

Keep optional method/field migration, the hosted faucet's own RPC-client update,
service-stop/unresolved error mappings, existing signed evidence and checked
retry-hint preservation. For this major, the hosted helper makes one service call
and never automatically repeats it, including policy refusals and pending polls.
It observes a known hash through reads or returns unresolved evidence. Explicit
caller retry/resume remains available with retained identity and delay hints. Do
not add a funding dispatch field solely for optional automatic retries, or reuse
transaction admission for funding-policy errors.

Durable shared storage, replica coordination, terminal retention, amount replay
and richer funding outcomes stay in a follow-up. Explicit hosted resume must
state the duplicate-test-funding risk when continuity is not proven.

## Eight review slices

These are review boundaries, not eight monolithic PRs. Start means inputs needed
for development after the hold is lifted. Land means reviewed, buildable commits
with compatible dependencies. Release is controlled publication, separate from
merge. Relative size reflects breadth and behavioural work, not elapsed-time
estimates.

| Slice | Can start with | Needs before landing/integration | Acceptance boundary |
| --- | --- | --- | --- |
| **1. Wire foundation and fixtures (L)** | Contract review and inspected GH-506/507 overlap | One owner per shared file, agreed codecs/vectors and coordinated major; carry same-workspace consumers | Strict DTO dispatch, typed client and generated schema agree on every method, optional/null rule and error |
| **2. History integrity (L)** | Current backend and proposed error/cursor semantics, in parallel with 1 | Coordinate protocol/bindings/hogd/adapter changes and #4454/#4458; internal fixes can land early | Complete history minimum on both methods, candidate cap/direct seek with advancing cursors, poison rows and regressed W |
| **3. Shared server, gateway and local node (M–L)** | Usable types/fixtures from 1; do not wait for all of 2 | Runtime codec/manifest agreement; pull compile fixes from 6; history exposure needs 2 | Gateway/local parity, race-aware admission, validated receipt identity, no silent event loss and final configured discovery |
| **4. TS transport, parsers and gates (M–L)** | Stable fixtures from 1, alongside 3 | Coordinate GH-506/provider codecs; integrate with 3 before release | Named params, canonical codecs, network/wire/method gates, minor-feature checks and tolerant results |
| **5. SDK lifecycle, wallet and funding (M)** | Evidence/recovery state-machine fixtures alongside 3/4 | Final decoded types from 4 and server error contract from 3; qualified runtime for automatic redispatch | Existing call/deploy/wait/assert composition, retained signed evidence and commitment, safe funding behaviour |
| **6. CLI, Rust, faucet, chuckd and generated consumers (M)** | Inventory and interfaces from 1/3 as available | Compile consumers accompany producer changes; generated TS flows need 4/5; chuckd needs matching metadata/Go bindings; coordinate #553 and runtime pins | Sticky Rust commitment, receipt-based CLI output, faucet RPC path, chuckd manifest parsing, generated projects and Ed25519/PQ smokes |
| **7. React, apps, provider and docs (M–L)** | Proxy/cache/identifier audits and candidate fixtures | Apps need 4/5; generated examples also 6; matching React peers and wallet/provider gate | Counter/swap/token-launchpad proxy flows, fresh/reset state, preserved Stellar behaviour and complete history |
| **8. Launchers, artifacts and qualification (M + measurement)** | Workload/threshold design and launcher/probe audit alongside other slices | Compatible launcher fixes can land early; final tests need the artifact chain below | Exact final set, installation/restart, live flows, bounded method/error metrics and accepted resource measurements |

After hold release, the main parallel tracks are:

1. Establish the contract/encoding fixtures, then develop server and TS transport
   in parallel.
2. Develop history/runtime correctness alongside that track. Encoding does not
   change the underlying history semantics, but final protocol artifacts must
   contain both sets of work.
3. Develop SDK behaviour and move consumers as each producer interface becomes
   usable. Rust call-site fixes do not wait for all TS SDK work.

Launcher repair, inventory, compatible metrics fixes and qualification design
can proceed independently of the complete native API. A global `-32011` metrics
relabel cannot precede freshness moving to `-32012` unless it distinguishes the
framework/application origin.

## Coordination with PQ and encoding

Three changes meet in shared files and release artifacts:

- **PQ:** binary envelopes, signing, account derivation and runtime support.
- **Encoding:** textual identifiers, codecs, manifests and consumer parsing.
- **Native API:** method names, request/result/error shapes and discovery.

Reuse GH-507/GH-506 work after inspecting its actual diff. Reported unpublished
commits are not an inspected integration baseline. An exact supplied patch or
local review can satisfy this dependency; publishing those branches is optional
and needs owner authorization. Agree ownership before editing overlapping types, parsers or schema code. Coordinate GH-502/503 vectors,
GH-513 protocol/bindings, GH-505 manifests and GH-500 PQ qualification.

Wire 13 is the planning target, subject to confirming it has not been published
with a different contract. One owner coordinates the major; parallel tasks must
not publish competing interpretations. Keep token event/error signature identity
at version 0.1 with the existing `groundhog-erc20-if` label and run existing
native/RV32 parity coverage. Package rename alone is not the entire identity rule.

## Consumer coverage

| Consumer | Work that literal method-name searches can miss |
| --- | --- |
| CLI, client, simulator, testkit and bench | Typed Rust calls, shared receipt waiter and error evidence, report readers, generated-project templates and offline build behaviour |
| chuckd | hogd bootstrap metadata, Go protocol pin, hosted manifest output and final packaging pin |
| Hosted faucet | Tip/send/receipt calls through its typed RPC client, telemetry and final artifact pin |
| TS SDK/provider and generated bindings | Evidence retention, discovery events, approvals, package exports and unsupported outcomes |
| thruput-react | Identifier case folding, network/query cache keys, SDK peer range and example pins |
| Freighter and Stellar adapter | Final wire/provider gate, fresh install/reopen, Thruput store reset and preservation of Stellar keys/flows |
| Counter, swap and token-launchpad proxies | Named-object validation, allowlists, filtered discovery, bounded typed errors and saved identities; preserve Counter write policy and the others' read-only policy |
| Explorer | Indirect SDK calls, hex helpers, generated bindings, root/event history and package pins |
| Packaging and ops | Installed launch commands, qualification readers, eBPF labels/map sizes/build symbols and collectors |
| Runtime/protocol/Rust SDK | Manifests, PQ bindings and token event/error identity |
| Docs and agent guidance | Runnable examples, selected bundled snapshot and guidance in its owning repository |

Remove obsolete `--nthreads`/`-n` and `EXEC_NTHREADS` wiring from packaging
entrypoints and ops validator/bootstrap/nightly launchers. Test the actual
packaged commands. Proxies must expose bootstrap discovery/network methods,
intersect advertised support with their policy and preserve bounded typed error
data. Test all three independently; shared helpers are optional. Remove the CLI
call/deploy probe for unserved `groundhog_dev_getTransactionDetail` and render
receipt data. Preserve the event formatter used by `view`; delete exported detail
DTOs/dependencies only after a reference and public-API audit.

Tools #553, Freighter #3, examples #134 and ops #230 overlap this migration.
Tools #187 is design input, not an implicit block-method commitment. Tools
#292/#293/#394/#469 and the earlier StrKey branch need current-base coordination.
Exact reviewed heads and evidence are in the [appendix](appendix/review-evidence.md).

## Artifact and release order

Use a manual stable-publication freeze for affected packages while partial
breaking changes land. Current tools and SDK publication workflows are manually
dispatched. Keep main buildable; no long-lived integration branch is required.

1. Select protocol/bindings (including Go) and runtime. Align chuckd with hogd
   bootstrap metadata and tools runtime-manifest schemas. Preserve raw XDR as hex
   and distinguish chain hashes from Git/artifact identities. Qualify and publish
   the public devnode artifact required by the tools catalog.
2. Build and, if needed by the SDK publication workflow, publish a controlled
   tools bootstrap RC through the normal release workflow. Local candidate
   checks can use `THRUPUT_CLI` and `THRUPUT_DEVNODE_BINARY`; those overrides do
   not establish an equivalent hosted publication path.
3. Qualify and publish the matching SDK. The current npm workflow has no
   isolated `next` channel: keep candidates local or explicitly qualify channel
   support before relying on it.
4. Pin final tools templates to that SDK, then assemble final tools, bundles,
   images, manifests, chuckd, React, Freighter, apps, probes and docs. Pin the
   final chuckd artifact, not just its source branch. Qualify installed hogd
   metadata → chuckd bootstrap commit → hosted manifest → gateway startup.
5. Deploy the final set to fresh devnet, qualify it and announce after acceptance.

Bootstrap artifacts are build dependencies, not an intermediate hosted API.
There is no legacy endpoint, preparation window, dual-wire probe or old-state
rollback requirement. A changed candidate requires qualification against its
final identities.

## Deferred work

| Follow-up | Boundary for this release |
| --- | --- |
| General scan budgets and cancellation | Candidate cap/direct seek land in slice 2; time/byte budgets and cancellation follow. Measure dense/selective/no-match history and work after timeout |
| Retention, repair and reset recovery | Strict fresh origin and within-instance checkpoint rules; no durable cross-reset cursors |
| Historical nonexecution proof and replacement helpers | No automatic expiry/nonce replacement or inference from a miss/dropped status |
| Funding durability and richer outcomes | Narrow contract/evidence fixes only, as described above |
| Optional Stellar-style copy/QR | Base58 is default; optional C copy needs explicit cross-chain safety qualification |
| Dynamic capability typing and transport metadata | Runtime method gates now; broader type redesign and layer-specific limit discovery later |
| Independent event wire-version override | Freeze existing identity for this release; add machinery only if package version must cross it |
| New API capabilities | Block/body/envelope reads, registered PQ, global feeds, public health and historical state need their own producers/contracts |

An unacceptable history resource envelope blocks announcement unless we
explicitly reduce the product scope and change its consumers. Deferral is not a
claim that downstream cancellation or durable recovery already works.

## Review and readiness

Before implementation, review the API and these scope boundaries, inspect the
in-flight shared diffs, and assign component/shared-file owners plus the
wire-major, publication and qualification owners. Those assignments remain open.
The implementation hold must be lifted explicitly.

Before announcement, require:

- Reviewed slices, one exact artifact set, full core method support, truthful
  optional discovery and generated schema/dispatch/client agreement. Cover all
  12 application error kinds under 11 code components, plus framework errors.
  Use thin typed Rust clients for flat DTOs; settle generator details in slice 1.
  Test unknown response variants with nested payloads in Rust and TS.
- Bounded metrics labels cover all registered methods and selected error codes,
  with an unknown bucket and framework/application distinctions; never label
  metrics with arbitrary method strings or diagnostic payloads.
- Real gateway/local, CLI/generated-project, faucet/proxy, wallet installation,
  connect/sign/reopen and reset-state qualification.
- Ed25519/PQ/mixed invalid traffic, envelope/batch bounds, lost replies,
  duplicate submission, commitment/receipt lag and malformed-data checks.
- A named measurement owner, fixed dataset and query/submission mix, concurrency,
  run duration, and numerical latency/resource/overload/recovery thresholds set
  before the run. Include work continuing after timeout or client abandonment.
- Public admission policy and a measured acceptable envelope. No qualification
  pass is claimed by this proposal.

Native API qualification should have a separate evidence record linked to
GH-512 encoding, GH-500 PQ, GH-422 lifecycle and GH-432 admission/resource work.
Existing assignees do not implicitly own this expanded scope.

Proposed Linear changes remain unapplied: link the contract under GH-413/GH-470;
clarify wire boundaries and ownership under GH-502/503/506/507; expand GH-428
history acceptance; record runtime duplicate qualification, candidate-bounded
scans, Rust commitment retention, chuckd/all-three-proxy coverage and fresh wallet
state; assign qualification ownership and keep deferred work separate. Review should settle whether these are the right announcement
requirements before implementation is scheduled.
