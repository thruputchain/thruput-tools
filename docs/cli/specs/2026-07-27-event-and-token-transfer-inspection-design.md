# Event and Token Transfer Inspection
**Date:** 2026-07-27 **Status:** Ready for implementation planning

> September 2026 compatibility update: the token-event assumptions below are
> historical. The current command queries canonical `TransferEvent`, `MintEvent`,
> and `BurnEvent` signatures from ERC20 interface 0.1.24. A zero address remains
> an ordinary participant, not a mint/burn marker. JSON schema version 2 uses
> `from: null` for mints and `to: null` for burns. `--from` includes transfers and
> burns; `--to` includes transfers and mints; specifying both includes only
> transfers. `--account` includes all three event kinds in one paginated request.

**Companions:**

- `2026-07-23-transfer-and-transaction-inspection-design.md` - native HOG transfer and transaction status, receipt, and root-invocation history.

- `2026-05-07-transaction-detail-options.md` - deferred deep transaction detail and replay analysis.

- `../roadmap.md` - current CLI transaction and read surfaces.

## Goal
Expose the public event index through focused CLI commands:

```bash
groundhog events list counter
groundhog events list counter --spec ./counter.spec.xdr --event Incremented
groundhog token transfers token --account alice
```

The workstream adds:

1. raw, paginated event history for one emitting contract;

2. optional custom-event decoding from an explicitly supplied contract spec;

3. decoded ERC20 transfer history with server-side participant filtering.


The commands should work consistently against local and hosted profiles, use the existing target and RPC override model, return one page per invocation, and provide clear human output plus stable JSON output.
## Non-goals
This work does not add:

- a global event stream across every contract;

- websocket subscriptions, polling, or `--follow`;

- automatic multi-page crawling or `--all`;

- event lookup by transaction hash, because `tx show` already returns receipt events;

- historical transaction-body, call-tree, or state-delta inspection;

- contract-spec discovery from chain state, registries, aliases, or deployment history;

- inference that a supplied spec belongs to the queried contract;

- reverse decoding of hashed indexed values, or typed projection of directly encoded custom topics in v1;

- token metadata discovery, decimal formatting, balances, approvals, allowance history, or token transfer submission;

- aggregation into balances, accounting statements, or a complete asset ledger;

- new public RPC methods or indexer schemas.

## Current capabilities and constraints
The public RPC already exposes:

```text
groundhog_getEvents
```

The shared request and response definitions are in `crates/json-rpc/json-rpc-types/src/events.rs`; the generated client method is declared in `crates/json-rpc/json-rpc-api/src/lib.rs`.

These CLI commands always construct an event query with:

- one or more event filters, ORed together;

- one or more emitter addresses in each filter;

- positional topic constraints for topic 0, 1, and 2;

- iteration order and an initial block anchor;

- an opaque continuation cursor;

- a page limit.


Within one filter, emitter and topic constraints are ANDed. Multiple values at one topic position are ORed, while constraints across positions are ANDed. A topic constraint is a prefix match: specifying topic 0 still matches an event with topics 1 and 2.

Important limits:

- every event filter requires at least one emitter address because the indexer cannot efficiently serve an all-contract scan;

- events have zero to three 32-byte topics and up to 1 KiB of total serialized event data;

- the endpoint rejects values above its configured page cap, currently 200 by default;

- `limit: 0` selects the endpoint default, currently 50;

- an empty event array with a non-empty cursor is a valid continuable response;

- the response reports the latest indexed block and oldest retained block, not the live chain tip;

- cursor contents are server-owned and may encode the original order and anchor.


The CLI already resolves saved wallets, contract aliases, direct addresses, and manifest-backed `dev-N` addresses. It loads a contract spec from an XDR-encoded file whose extension is `.xdr`, including `.spec.xdr` sidecars.


The current loader does not consume `.spec.json` sidecars. This design does not add a second JSON spec parser.

Contract specs define each event's name, topic-0 signature, named indexed fields, and typed payload fields. Indexed fields remain 32-byte topic values. The spec can name them but cannot generally recover an original preimage from a hash.

The SDK event encoder stores any already-32-byte topic representation directly and hashes other CXS-encoded indexed values. Canonical ERC20 `ContractAddress` topics use the direct form, so the token command can filter and project `from` and `to` from raw topic bytes. Generic `events list` still leaves every custom indexed value as raw hex in v1 because the spec alone does not record whether a topic was direct or hashed in a way the CLI can safely generalize.

The standard Groundhog ERC20 interface defines `TransferEvent` as:

```text
topic 0: TransferEvent signature
topic 1: from address
topic 2: to address
data:    CXS-encoded nonnegative i128 amount
```

The zero address in `from` represents a mint. The zero address in `to` represents a burn.
## Decisions
| #   | Decision | Rationale |
| --- | --- | --- |
| D1  | Add `groundhog events list <CONTRACT>` for generic event history. | Event history is contract-oriented and distinct from root-invocation transaction history. |
| D2  | Require one emitter contract in v1. | The RPC and indexer require a non-empty emitter filter and cannot efficiently scan every contract. |
| D3  | Return raw events by default. | Raw topics and data are authoritative and require no local artifact. |
| D4  | Add optional `--spec <PATH>` decoding to `events list`. | A user-supplied artifact is the only reliable local source for custom event types. |
| D5  | Treat spec decoding as an annotation, never a replacement for raw evidence. | Specs can be stale, incorrect, or malicious, and decoding can fail on individual historical events. |
| D6  | Add `groundhog token transfers <TOKEN>` as a standard projection rather than special output from `events list`. | Transfer history has stable participant, direction, and amount semantics that generic events do not. |
| D7  | Filter token participants in the RPC query. | Client-side filtering after a capped generic page can omit valid transfers and produce misleading pagination. |
| D8  | Both commands return one newest-first page and expose the opaque continuation cursor. | This matches transaction history and avoids hidden unbounded work. |
| D9  | A non-empty cursor is an opaque position token; users must repeat the same contract and topic filters to continue the same logical query. | Current cursors carry position and order but are not bound to filters, and the CLI cannot verify consistency without replacing the server cursor contract. |
| D10 | Malformed matching events returned by `getEvents` remain visible and do not fail the whole page. | One bad historical record must not hide other returned records or erase forensic evidence. |
| D11 | `--event` requires `--spec` and selects exactly one named spec event. | The spec owns event names and signatures; names have no raw RPC meaning. |
| D12 | Repeated topic options express OR within a topic position. | This maps directly to the RPC without inventing a query language. |
| D13 | Do not decode generic indexed values beyond naming their raw topic bytes. | Event topics may contain hashes, and the spec does not prove that a preimage can be recovered. |
| D14 | Token amounts are raw decimal strings in v1. | Decimal display requires trusted token metadata that this workstream does not fetch. |
| D15 | No RPC or gateway changes are expected. | Existing filtering, pagination, provenance, and retention metadata are sufficient. |
| D16 | CLI JSON uses a flattened, command-owned event projection while preserving every field in the shared RPC event. | The flattened shape is easier to consume beside decode metadata; explicit conversions and schema tests prevent drift from the nested RPC model. |
## Command surface
### `groundhog events list <CONTRACT>`
List one page of events emitted by a contract:

```bash
groundhog events list counter
groundhog events list 0xabcd... --limit 25
groundhog events list counter --topic0 0x0123...
groundhog events list counter --topic1 0xabcd... --topic1 0xef01...
groundhog events list counter --cursor 0x0123...
groundhog events list counter --spec ./target/counter.spec.xdr --event Incremented
```

Options:

```text
--topic0 <TOPIC>       Repeat for OR within topic position 0
--topic1 <TOPIC>       Repeat for OR within topic position 1
--topic2 <TOPIC>       Repeat for OR within topic position 2
--spec <PATH>          Decode matching events with a .xdr contract spec
--event <NAME>         Select one event declared by --spec
--limit <COUNT>
--cursor <OPAQUE_HEX>
--profile <PROFILE>
--rpc-url <URL>
--runtime-manifest <PATH>
```

Contract resolution

Resolve `CONTRACT` using the existing broad target precedence:

1. saved wallet name;

2. contract alias;

3. direct 32-byte `0x` address;

4. manifest-backed `dev-N` address.


This intentionally matches `balance` and `tx list`. The positional name says `CONTRACT` because the address is used as an emitter filter, but the CLI does not perform a chain read to prove that code is deployed there.

Preserve the user's positional argument as `contractInput` and return the canonical resolved address as `contractAddress`.

Topic filters

Each `--topicN` value must be exactly 32 bytes of hexadecimal data. Accept the shared fixed-size topic parser's supported input forms and emit canonical lowercase `0x` hex.

Accept at most 16 values at each topic position, matching the RPC-layer bound. Reject excess repeated options before constructing the request.

Repeated values at one position are ORed. Different positions are ANDed. For example:

```bash
groundhog events list counter \
  --topic0 0xtransfer... \
  --topic1 0xalice... \
  --topic1 0xbob...
```

means topic 0 equals the transfer signature and topic 1 equals either Alice or Bob.

The command creates one RPC `EventFilter` containing the resolved emitter and the three positional topic lists. It does not expose multiple top-level filters in v1.

Spec decoding

Without `--spec`, every item contains raw event data and provenance plus the final v1 decode placeholders: `decodeStatus: "notRequested"` and `decoded: null`.

With `--spec`, open, size-check, load, and validate the contract spec before RPC access. The `.xdr` file must decode as a `ContractSpec`. Missing specs, malformed files, and unsupported extensions fail the command before the event request.

`--spec` accepts a trusted local developer sidecar, such as output from `groundhog build` or trusted CI. It is not a hardened parser for arbitrary downloaded or uploaded files in v1. Read at most 16 MiB before parsing, accept only `.xdr`, and reuse the CLI's existing owned contract-spec loader.

After loading, perform complete event-focused structural validation before RPC. Reject cycles, ambiguous definitions, unresolved event-reachable types, unsupported event shapes, more than 4,096 spec definitions/members, more than 64 event-reachable type levels, and validation graphs requiring more than 65,536 type visits. While decoding one returned event, cap materialized values at 4,096 and reject expansions before allocating their output containers. These post-load checks protect event decoding and output but do not harden the initial owned XDR decode itself.

Known residual risk: a deliberately crafted local artifact may consume excessive CPU or memory, overflow parser recursion, or trigger a parser panic before post-load validation. The consequence is local CLI denial of service; the artifact is not executed and cannot sign or submit transactions. Hardened hostile-artifact decoding belongs in a broader SDK/CLI follow-up and does not block this workstream.

Perform one complete structural validation pass before querying events. Reject duplicate event names, duplicate event signatures, duplicate type-definition names, duplicate topic-field names within an event, duplicate payload-field names within an event, unsupported definitions, unresolved named types reachable from an event, cyclic reachable type definitions, ambiguous members in reachable structs, enums, or unions, and any event whose signature plus indexed fields exceeds the protocol's three-topic limit. Ambiguous members include duplicate field or variant names and duplicate enum or union discriminants. This validation must not depend on which signatures happen to appear in the returned page.

Every event name and event-reachable type or member name must decode through `spec_string_text`, be non-empty, and be unique in its containing definition. Event topic names, event payload-field names, struct field names, and union struct-field names must also fail `str::parse::<usize>()` because those values become JSON object keys; the existing codec's array projection for such numeric field names is not part of the event output contract. Numeric enum variant and union case names remain valid because they are emitted as string values rather than object keys. Unrelated interface and method metadata remains governed by existing spec canonicalization rather than this event projection rule.

Build a local signature map from the spec's event definitions. For each returned event:

1. preserve transaction hash, block height, sequence number, emitter, topics, and data unchanged;

2. inspect topic 0;

3. find the spec event with the same signature;

4. require the exact topic count declared by that event, including signature topic 0;

5. associate topics 1 onward with the spec's indexed field names as canonical raw hex;

6. decode all payload fields in declaration order with the existing CXS contract-type decoder;

7. reject trailing payload bytes for that item;

8. attach the event name, named topics, and decoded fields on success.


If topic 0 is absent or unknown to the spec, leave `decoded` null without a decode error. The event is valid raw evidence but not identified by this spec.

If a known signature has the wrong topic count, malformed payload, invalid value, unknown enum or union discriminant, exceeded per-event decode budget, or trailing bytes, leave `decoded` null and attach a stable item-level decode status and diagnostic. Continue rendering the page and exit successfully. Diagnostics must identify the event and failure but must not include host filesystem contents or panic details. Structurally invalid specs, including unresolved named types, fail during pre-RPC validation instead. The event decoder must add strict enum-membership validation rather than inheriting the existing call decoder's numeric fallback for unknown enum discriminants.

Spec-based payload values use the same JSON projection as existing typed call return values. Preserve validated spec field names exactly. The current type-based integer contract emits `u64` and `i64` as JSON numbers and emits `u128` and `i128` as decimal strings; narrower integers follow their existing numeric projection. Bytes and addresses use canonical lowercase `0x` hex. This work does not independently change the shared codec's handling of JavaScript safe-integer limits.

`--event <NAME>` performs two operations:

1. require exactly one event with that case-sensitive name in the supplied spec;

2. constrain RPC topic 0 to that event's declared signature.


If the user also supplies `--topic0`, every supplied value must equal the selected event signature. Reject a conflicting combination before RPC instead of issuing a query guaranteed not to match. `--topic1` and `--topic2` remain available for indexed-field filtering.

`--event` without `--spec` is invalid. An unknown event name is invalid. Every event declaration, whether selected or not, must fit the protocol's three total topics.

Pagination

The initial request always uses descending order anchored at the end of the indexed range. A non-empty cursor continues server-authored traversal. The CLI sends the current emitter and topic filters with the cursor but does not inspect or modify cursor contents. The user must repeat the same filters returned in the prior command context; the current backend accepts a cursor with different filters and applies its position to the new stream, which can skip or repeat logical results.

Defaults and limits:

- default page size: 50;

- `--limit` must parse as a nonzero `u32`;

- endpoint page-size validation remains authoritative;

- the CLI validates cursor hex and decodes it into the shared variable-length `PaginationCursor`;

- endpoint cursor-size validation remains authoritative.


An empty `events` array with a non-empty cursor is a valid page, including when gateway framing checks discard every indexed record returned in that page. Human output must distinguish it from exhaustion and show the reusable cursor. An empty array with an empty cursor normally means the stream is exhausted.

Cursor validation is intentionally limited. The CLI rejects malformed hex, and the RPC rejects cursors over its byte cap, but the current indexer converts many syntactically valid yet malformed, unknown-version, or stale opaque cursor payloads into an empty exhausted page. The CLI cannot distinguish that response from real exhaustion and must not claim that the backend validated filter consistency or cursor provenance.
### `groundhog token transfers <TOKEN>`
List one page of canonical ERC20 transfer events emitted by a token contract:

```bash
groundhog token transfers token
groundhog token transfers token --from alice
groundhog token transfers token --to bob
groundhog token transfers token --account alice --limit 25
groundhog token transfers 0xabcd... --cursor 0x0123...
```

Options:

```text
--from <ADDRESS>
--to <ADDRESS>
--account <ADDRESS>
--limit <COUNT>
--cursor <OPAQUE_HEX>
--profile <PROFILE>
--rpc-url <URL>
--runtime-manifest <PATH>
```

Resolve `TOKEN`, `--from`, `--to`, and `--account` through the same broad target resolver used by `events list`. Preserve `tokenInput` and participant input strings in JSON while returning canonical resolved addresses separately.

Filters

Every token transfer query constrains:

```text
contract address = resolved TOKEN
topic 0         = canonical TransferEvent signature
```

Participant options map to RPC filters as follows:

| Options | RPC filters | Meaning |
| --- | --- | --- |
| none | one filter with topic 0 | every indexed transfer from this token |
| `--from A` | one filter with topic 0 and topic 1 = A | transfers from A |
| `--to B` | one filter with topic 0 and topic 2 = B | transfers to B |
| `--from A --to B` | one filter with all three topics | transfers from A to B |
| `--account A` | two filters ORed: topic 1 = A; topic 2 = A | transfers involving A |

`--account` is mutually exclusive with `--from` and `--to`. This keeps its inclusive OR meaning unambiguous and avoids a hidden query-language product.

The command must send both filters for `--account` in one `getEvents` request. It must not fetch an unfiltered page and discard unrelated records locally. ORed filters form one boolean predicate over the indexed stream, so a conforming backend returns a self-transfer once even though it matches both filters. The CLI preserves the returned page unchanged rather than silently deduplicating backend anomalies.

Decoding and classification

Decode each matching event with the canonical SDK `TransferEvent` definition, then apply the standard transfer invariants. Require:

- exactly three topics;

- the canonical transfer signature in topic 0;

- topic 1 to be present and use its raw 32 bytes as the source address;

- topic 2 to be present and use its raw 32 bytes as the destination address;

- one CXS-encoded `i128` amount with no trailing bytes;

- a nonnegative amount after successful `i128` decoding.


Every 32-byte topic value is representable as a `ContractAddress`; there is no additional address encoding to validate. The SDK decoder accepts any encoded `i128`, while standard emitters enforce nonnegativity. The CLI therefore marks a successfully decoded negative amount malformed as a semantic invariant violation rather than describing the bytes as undecodable.

Classify a valid record as:

- `mint` when source is the zero address, including the zero-to-zero edge case;

- `burn` when destination is the zero address and source is nonzero;

- `transfer` otherwise.


Source-zero precedence matches the canonical ERC20 event display behavior. Zero-amount transfers are valid.

The projected amount is a base-unit decimal string named `rawAmount`. Do not fetch token metadata or display a decimal token amount in v1.

A matching but malformed event remains in the page as a record with raw evidence, `transfer: null`, and `decodeStatus: "malformed"`. The page remains a successful query. Human output labels the row malformed and prints a concise warning. This prevents one bad contract record from hiding later valid transfers while making clear that no transfer fields were established.

Token transfer pagination uses the same default, validation, newest-first initial request, sparse-page behavior, and opaque continuation rules as `events list`.
## Output contracts
Both commands use the existing `CommandResult` envelope. Command names are:

```text
events.list
token.transfers
```

Each command data object uses `schemaVersion: 1`, camelCase field names, and canonical lowercase `0x` hex. JSON data fields and command-error codes are the versioned machine contract. Human wording, hints, and item-level decode diagnostics may evolve while remaining deterministic and script-clean.

Raw event objects use one flattened shape across both commands:

```json
{
  "txHash": "0x...",
  "blockHeight": 123,
  "sequenceNumber": 0,
  "contractAddress": "0x...",
  "topics": ["0x...", "0x..."],
  "data": "0x..."
}
```

The tuple `(txHash, blockHeight, sequenceNumber)` identifies an indexed event within command output. The CLI preserves RPC order and duplicate records. Conversion from the shared nested `IdentifiedEvent` must map its transaction hash, block height, sequence number, emitter address, complete topic array, and data exactly once; schema tests lock this command-owned projection to every current shared field.
### Event-list data
Raw event page:

```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "contractInput": "counter",
  "contractAddress": "0x...",
  "filters": {
    "topic0": [],
    "topic1": [],
    "topic2": []
  },
  "spec": null,
  "events": [
    {
      "txHash": "0x...",
      "blockHeight": 123,
      "sequenceNumber": 0,
      "contractAddress": "0x...",
      "topics": ["0x..."],
      "data": "0x...",
      "decodeStatus": "notRequested",
      "decoded": null
    }
  ],
  "cursor": "0x...",
  "latestBlock": 130,
  "oldestBlock": 0
}
```

With a spec and successful decoding, `spec` and an item become:

```json
{
  "spec": {
    "source": "userSupplied",
    "path": "./target/counter.spec.xdr",
    "selectedEvent": "Incremented"
  },
  "event": {
    "txHash": "0x...",
    "blockHeight": 123,
    "sequenceNumber": 0,
    "contractAddress": "0x...",
    "topics": ["0x...", "0x..."],
    "data": "0x...",
    "decodeStatus": "decoded",
    "decoded": {
      "name": "Incremented",
      "topics": {
        "caller": "0x..."
      },
      "fields": {
        "previous": "41",
        "current": "42"
      }
    }
  }
}
```

The second snippet abbreviates the enclosing page and labels the item `event` for illustration; the actual item remains inside `events`. In this example, `previous` and `current` are declared as `u128`, so the shared codec represents them as decimal strings.

Every event item always contains `decodeStatus` and nullable `decoded`. Stable statuses are:

```text
notRequested
unknownSignature
decoded
malformed
```

When `decodeStatus` is `malformed`, add `decodeError` as a concise diagnostic string. Omit `decodeError` for all other statuses. Unknown signatures are expected and do not carry an error.

`filters` reports the effective canonical topic values, including a topic-0 signature introduced by `--event`. It does not report order or anchor because a continuation cursor may carry values not exposed by the response.

`spec.source` is always the stable literal `userSupplied`; the CLI does not authenticate or discover specs. `spec.path` preserves the user-provided path string and does not expose a canonical absolute path. `selectedEvent` is nullable when the spec is used without `--event`.
### Token-transfer data
```json
{
  "schemaVersion": 1,
  "profile": "devnet",
  "rpcUrl": "https://rpc.devnet.example",
  "tokenInput": "token",
  "tokenAddress": "0x...",
  "participantFilter": {
    "kind": "account",
    "input": "alice",
    "address": "0x..."
  },
  "records": [
    {
      "txHash": "0x...",
      "blockHeight": 123,
      "sequenceNumber": 0,
      "contractAddress": "0x...",
      "topics": ["0x...", "0x...", "0x..."],
      "data": "0x...",
      "decodeStatus": "decoded",
      "transfer": {
        "kind": "transfer",
        "from": "0x...",
        "to": "0x...",
        "rawAmount": "10000000"
      }
    }
  ],
  "cursor": "0x...",
  "latestBlock": 130,
  "oldestBlock": 0
}
```

`participantFilter` is always present and nullable. Its forms are:

```json
null
```

```json
{ "kind": "from", "input": "alice", "address": "0x..." }
```

```json
{ "kind": "to", "input": "bob", "address": "0x..." }
```

```json
{
  "kind": "fromTo",
  "fromInput": "alice",
  "from": "0x...",
  "toInput": "bob",
  "to": "0x..."
}
```

```json
{ "kind": "account", "input": "alice", "address": "0x..." }
```

Every token record always contains `decodeStatus` and nullable `transfer`. Stable statuses are `decoded` and `malformed`. A malformed record adds `decodeError`; a decoded record omits it.

The raw `contractAddress`, `topics`, and `data` remain present even when `transfer` is decoded. Consumers can independently verify the projection and remain resilient to future schema additions.

Human `events list` output shows the resolved emitter, supplied spec path and selected event when present, then a blank line before every returned event block. Transaction hash has its own line. The command-level emitter is not repeated inside normal event blocks; an event-level emitter is shown only if a returned record unexpectedly differs. Unknown and malformed records show numbered raw topics. Successfully decoded records show topic 0 as `signature` and topics 1 onward under their validated spec names, without duplicating the same bytes under numbered labels. Raw data and decoded payload fields remain visible. JSON always retains the complete raw and decoded representations.

Human `token transfers` output shows the token address and participant filter, then one concise row per decoded record containing block height, transaction hash, sequence number, kind, source, destination, and raw amount. A malformed token record instead shows its raw topics and data with a warning because no transfer projection was established.

Both commands show the reusable cursor when non-empty and always show `latestBlock` and `oldestBlock`. An empty page with a response cursor is labeled `no displayable events in this page; continue with cursor`. When the request did not supply a cursor and the response cursor is empty, label it `no events found; stream exhausted`. When the request supplied a cursor but the response is empty with no continuation, use the qualified wording `no events returned; no continuation cursor` because the CLI cannot distinguish real exhaustion from a backend-rejected opaque cursor. Output must not claim that `latestBlock` is the chain tip or calculate indexer lag in v1. All fields pass through the existing terminal-safety escaping before display.
## Validation and errors
Use these stable command-error mappings:

| Condition | Code |
|---|---|
| invalid or unknown target | existing `invalid_address_target` or `unknown_address_target` |
| malformed or wrong-length topic | `invalid_event_topic` |
| more than 16 values at one topic position | `invalid_event_filter` |
| malformed cursor hex | `invalid_event_cursor` |
| zero, nonnumeric, negative, or overflowing limit | `invalid_event_limit` |
| `--event` without `--spec`, unknown selected event, conflicting topic/event filters, or conflicting participant options | `invalid_event_options` |
| missing, unreadable, oversized, unsupported, malformed, or structurally invalid spec | `invalid_event_spec` |
| `MethodNotFound` or Groundhog `NotImplemented` | existing `rpc_capability_unsupported` |
| all other RPC failures or invalid response shapes | existing `rpc_method_failed` |

Map existing loader and codec errors into `invalid_event_spec` at the command boundary rather than exposing build-oriented `spec_failed` hints. Update the shared capability hint so it names the required RPC method generically rather than saying `transaction method`. Error `message`, `hint`, and optional `details` remain diagnostic and may evolve; only the listed code is versioned for each class.

Covered conditions include:

- unknown contract, token, or participant target;

- malformed direct address;

- malformed or wrong-length topic;

- too many topic values at one position;

- invalid cursor encoding;

- invalid event page limit;

- missing, unreadable, unsupported, or malformed contract spec;

- duplicate event names or signatures, ambiguous nested members, unresolved types, or cyclic reachable types in a supplied spec;

- unknown `--event` name;

- `--event` without `--spec`;

- conflicting `--event` and `--topic0` values;

- mutually exclusive token participant options;

- a supplied event definition that exceeds protocol topic limits;

- a spec that exceeds artifact, structure, nesting, or decode budgets;

- unsupported backend capability;

- RPC unavailable, timeout, overload, or rejected request.


Input and spec validation happen before RPC where possible. Direct CLI addresses require a `0x` or `0X` prefix followed by exactly 64 case-insensitive hexadecimal digits. Output fields containing addresses, topics, transaction hashes, data, and cursors use canonical lowercase `0x` hex. Input fields preserve the user's arguments verbatim.

Reject a successful RPC response containing more events than the requested limit as `rpc_method_failed`. This preserves the caller's one-page resource bound and matches `tx list` response validation.

Also reject any returned record with more than three topics or with more than 1,024 total serialized event bytes, calculated as `32 * topics.len() + data.len()`. The shared response types use unbounded vectors, so a nonconforming endpoint can otherwise bypass the protocol's event bounds before rendering or decoding.

Malformed events returned by `getEvents` are item-level data states, not command errors. A page exits successfully when the RPC query succeeds even if some records are unknown to a spec or fail decoding. Spec loading and complete structural validation are command-level errors because no trustworthy decoder can be built. Records with invalid framing may be rejected by the gateway before they reach the CLI and therefore cannot be preserved in command output.

Do not convert `NotImplemented` into an empty event page. Explain that the selected endpoint does not expose the required event-index capability and suggest using a full local devnode or compatible remote RPC.

Treat JSON-RPC `MethodNotFound` and Groundhog `NotImplemented` as the same unsupported-capability class. A gateway without a connected indexer may instead return indistinguishable `BackendUnavailable`; retain that as `rpc_method_failed` rather than claiming a permanent capability diagnosis. Other unavailable, timeout, overload, invalid-params, and rejected-request responses also remain RPC failures. Do not add a separate capability-discovery request.

Important edge cases:

- aliases and wallet names may collide; broad target precedence remains deterministic;

- the resolved emitter address may not contain deployed code;

- a supplied spec may not belong to the queried address;

- different specs may reuse event names or signatures;

- events may omit topic 0 or contain a signature unknown to the supplied spec;

- historical records may predate a contract upgrade and require a different spec;

- topic values cannot generally be reversed into typed source values;

- valid transfer amounts include zero but not negative values;

- mints and burns use the zero address and may match participant filters for that address;

- a self-transfer can match both filters used by `--account`;

- a foreign, stale, malformed, unknown-version, or filter-inconsistent cursor may be accepted and appear exhausted or may resume the wrong filtered stream; callers must reuse the prior cursor with unchanged filters;

- indexer retention can make `oldestBlock` greater than zero;

- an empty page with a non-empty cursor may need another invocation before a match appears;

- unknown future RPC variants remain governed by the shared client's compatibility behavior.

## Security considerations
- Never read or print private keys or seed material; these commands are read-only.

- Treat contract specs as explicitly selected, trusted local developer sidecars. Cap file reads, validate loaded event structures and decode expansion, and document the remaining hostile-file denial-of-service risk.

- Do not claim that a supplied spec is authentic or belongs to the queried contract. Human and JSON output must identify it as user-supplied decoding metadata.

- Keep addresses, topics, cursors, event bytes, field names, decoded strings, and backend diagnostics as untrusted data. Validate bounds, avoid terminal escape interpretation, and do not treat values as markup or instructions.

- Preserve raw evidence beside decoded values so a misleading spec cannot replace indexed bytes in machine output.

- Do not follow paths referenced from spec contents. Read only the explicit `--spec` path.

- Enforce post-load structure, depth, and per-event decode budgets before allocating decoded output containers. Initial owned spec decoding retains the existing trusted-artifact risk model.

## Backend compatibility
No RPC schema change is required.

Expected behavior by endpoint capability:

| Capability | `events list` | `token transfers` |
|---|---|---|
| Full event indexer | Raw and spec-decoded event pages | Filtered and decoded transfer pages |
| Indexer without scheduler/submit support | Fully supported | Fully supported |
| Chain or submit RPC without event indexer | `NotImplemented` or backend unavailable | `NotImplemented` or backend unavailable |
| Read-only hosted indexer | Fully supported | Fully supported |
| Stale indexer | Returns internally consistent retained/indexed metadata; may omit newer chain events | Same limitation |

The CLI maps explicit `MethodNotFound` and `NotImplemented` responses to missing capability rather than inferring support from profile names such as `local` or `devnet`. It cannot distinguish a permanently absent indexer from a transient `BackendUnavailable` response. It reports `latestBlock` and `oldestBlock` without comparing them to another RPC, because cross-service reads can race and some read-only endpoints do not expose a chain tip.
## Implementation shape
Keep the work primarily inside `crates/tools/cli` and reuse generated RPC clients and shared types.

Expected areas:

- parser, command names, and dispatch in `src/main.rs`;

- an `events` command module for query construction, spec decoding, and output orchestration;

- a `token` command module for canonical transfer filters and projection;

- existing broad target resolution shared with `balance` and `tx list`;

- size-limited contract-spec loading from `.xdr` sidecars, reusing the existing owned loader for trusted local artifacts;

- the existing CXS contract-value decoder, exposed through the smallest helper needed to decode event payload fields;

- canonical `groundhog_erc20_if::TransferEvent` signature and decoder behavior from the `groundhog-erc20-if` standard-interface package at the same SDK revision as the CLI's Groundhog dependencies;

- stable versioned JSON report types;

- command-specific validation errors and hints.


Use one small shared page-query helper only if both commands immediately share RPC request construction, pagination validation, and capability mapping. Keep filter policy and result projection command-owned. Do not introduce a generalized query language, pagination framework, event registry, contract-artifact resolver, or cross-crate decoding service.

Spec event decoding must operate directly from `groundhog_contract_spec` definitions. Do not generate TypeScript or compile generated code at runtime. Reuse the same type-definition indexing and CXS value projection used by typed call return decoding so call and event representations cannot drift.

The token command should use the root-reexported `groundhog_erc20_if::TransferEvent` and its inherent `TransferEvent::SIGNATURE` constant as the authoritative source. If depending directly on that package would create an unsuitable CLI dependency, move or expose the protocol constant and narrow decoder from a shared SDK package first. Do not duplicate a hand-computed signature literal. In either shape, compatibility tests must encode an event with the authoritative SDK implementation and decode it through the CLI path.

No gateway, RPC API, indexer, or protocol change is expected. Hermetic binary tests should use mock RPC responses for non-empty pages, filters, malformed events, and pagination. Process-level event end-to-end tests require a compatible external `hog-devnode` and belong in the existing ignored smoke path.
## Phasing
### Phase 1: raw event history
- `events list <CONTRACT>`;

- emitter and positional topic filters;

- newest-first one-page requests and cursor continuation;

- raw human and JSON output;

- capability-aware errors.

Phase 1 emits the final schema-version-1 placeholders: page-level `spec: null` and item-level `decodeStatus: "notRequested"` plus `decoded: null`. Phase 2 fills these fields without changing the v1 shape.

### Phase 2: custom spec decoding
- `--spec` loading from `.xdr`;

- signature matching and strict payload decoding;

- `--event` selection and automatic topic-0 filtering;

- item-level unknown and malformed states;

- raw evidence retained beside decoded output.

### Phase 3: token transfer history
- `token transfers <TOKEN>`;

- canonical transfer-signature query;

- `--from`, `--to`, and `--account` server-side filters;

- transfer, mint, and burn projection;

- malformed-record preservation and pagination coverage.


Each phase should pass automated review and a human output gate before the next phase begins. Phase 3 may reuse Phase 1 pagination plumbing but must not depend on custom spec decoding from Phase 2.
## Acceptance criteria
1. `events list` resolves a supported target and returns one newest-first page emitted by that address.

2. Repeated topic options map to OR within a position, while different topic positions remain ANDed.

3. Both commands preserve opaque cursors, sparse-page semantics, and indexer retention metadata without claiming chain-tip freshness.

4. Raw event provenance, topics, and data remain available in every record regardless of decoding outcome.

5. `--spec` loads supported artifacts without executing them and decodes known event payloads through the canonical contract type model.

6. Unknown signatures and malformed known events remain visible with distinct stable statuses and do not fail an otherwise successful page.

7. `--event` resolves one case-sensitive spec event and constrains topic 0 without permitting contradictory filters.

8. `token transfers` queries only the canonical transfer signature and performs participant filtering in the RPC request.

9. Token transfer output accurately distinguishes decoded transfers, mints, burns, zero amounts, and malformed matching records without requiring token metadata.

10. Explicit unsupported-capability responses produce actionable errors rather than empty results; indistinguishable `BackendUnavailable` responses remain ordinary RPC failures.

11. No command performs hidden multi-page traversal, N+1 receipt hydration, artifact discovery, or new RPC calls.

12. JSON output remains deterministic and versioned; human and JSON output stay cleanly separated from status and error streams.

13. Topic filters enforce the 16-value positional cap, trusted specs pass complete post-load event validation, and conflicting options fail before RPC with their assigned stable codes.

14. `--account` issues exactly one RPC request containing both ORed filters and remains mutually exclusive with `--from` and `--to`.

15. Responses over the requested page limit are rejected, while duplicate records within a conforming-size page are preserved in RPC order.

16. Phase 1 already emits every nullable decode field in the final schema-version-1 shape.

## Test plan
### Unit tests
- contract, token, and participant target precedence and collision behavior;

- mixed-case direct-address input and canonical lowercase output;

- topic parsing, repeated values, and effective filter construction;

- transaction hash, event data, and cursor canonical serialization;

- limit, topic-count, cursor-hex, per-record protocol bounds, and over-limit response validation;

- size-limited `.xdr` spec loading, unsupported extensions, malformed specs, and post-load validation boundaries;

- duplicate event names and signatures, ambiguous nested type members, and cyclic reachable types;

- event-name selection and conflicting topic-0 rejection;

- signature matching, exact topic counts, named raw topic projection, payload field decoding, unresolved types, unknown enum and union discriminants, exceeded decode budgets, malformed values, and trailing bytes;

- no-spec, unknown-signature, decoded, and malformed event statuses;

- canonical transfer signature and strict source, destination, amount, and trailing-byte decoding;

- transfer, mint, burn, self-transfer, zero amount, negative amount rejection, and malformed standard records;

- `--from`, `--to`, combined from/to, and two-filter `--account` query construction;

- preservation of duplicate records supplied by a nonconforming backend fixture;

- stable JSON serialization for raw, decoded, unknown, malformed, filtered, empty, sparse, and exhausted pages.

### Binary integration tests
Use mock RPC fixtures to cover:

- profile and RPC overrides;

- saved-wallet, alias, direct-address, and manifest-backed target resolution;

- exact `getEvents` request bodies for every filter form;

- exactly one `getEvents` call and no unrelated RPC methods for each successful command;

- default and explicit limits;

- cursor continuation without reinterpretation;

- empty initial exhaustion, empty continuable pages, and qualified empty continuation responses without a cursor;

- non-empty raw event pages and indexer metadata;

- custom spec decoding from XDR fixtures;

- selected-event filtering and conflicting arguments;

- mixed known, unknown, and malformed events in one successful page;

- token transfer pages with transfer, mint, burn, self-transfer, and malformed records;

- participant aliases resolved before query construction;

- `MethodNotFound`, `NotImplemented`, invalid params, backend unavailable, timeout, and overload responses;

- malformed input rejected before RPC;

- every stable error mapping, option conflict, spec failure, over-limit response, and capability hint;

- human output and JSON stream discipline;

- terminal-control bytes in diagnostics and decoded strings rendered safely.

### End-to-end tests
Run these through the ignored external-devnode smoke path:

- deploy or use a fixture contract that emits multiple custom events;

- list its raw events newest-first;

- decode the same events with its XDR spec sidecar;

- select one named event and verify server-side topic-0 filtering;

- continue through pages with stable, non-overlapping event identities;

- emit canonical token transfers, a mint, and a burn;

- list all token transfers and filter by source, destination, and involved account;

- verify a self-transfer appears once for `--account`;

- compare returned `latestBlock` and `oldestBlock` with the running indexer's known state;

- verify explicit `NotImplemented` reduced backends return capability errors and indexer-disconnected `BackendUnavailable` responses remain RPC failures.


Hosted devnet validation must account for indexer freshness. A missing recent event is not a CLI failure when the page's `latestBlock` predates the emitting transaction; validate against a local node or a caught-up hosted indexer before concluding that filtering is incorrect.
## Deferred follow-ups
- ascending order and explicit block-height anchors;

- `events follow` subscriptions or polling;

- automatic multi-page traversal and export;

- multiple emitter contracts or an advanced multi-filter query surface;

- `.spec.json` loading;

- authenticated on-chain or registry-backed spec discovery;

- contract-upgrade-aware selection among multiple specs;

- typed projection for custom topics that the current SDK encodes directly, while hashed topic preimages remain unrecoverable;

- hardened decoding for arbitrary hostile spec artifacts before owned XDR parsing.

- token metadata, decimal amount formatting, symbol display, balances, approvals, and allowance history;

- aggregation into account statements or a complete asset ledger;

- transaction-body and call-tree correlation;

- global topic-only search if the indexer adds an efficient topic index.
