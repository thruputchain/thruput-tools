# Network Information and Airdrop RPC

**Date:** 2026-08-22 **Status:** Implemented

## Goal

Add two public JSON-RPC methods:

- `groundhog_networkInfo` returns the configured network identity.
- `groundhog_requestAirdrop` exposes the existing faucet operation through local and hosted development RPC endpoints.

The airdrop method is a facade over infrastructure that already exists. Hosted gateways call the current faucet `GET /v1/network` and `POST /v1/airdrop` endpoints. Local nodes use the validated `dev-0` account and current local-airdrop transaction helpers without starting a separate faucet process.

## Non-goals

This work does not:

- change consensus, transaction validity, or protocol sponsorship;
- replace or add a second faucet API;
- change the web faucet;
- add CAPTCHA, GitHub authentication, API keys, or user tiers;
- add a durable or distributed policy store;
- add network-wide HOG accounting or reserve management;
- define TypeScript cluster types or wallet-client APIs; or
- expose private runtime-manifest or key material.

## Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | Add `groundhog_networkInfo` to every RPC server. | Clients need the protocol network and genesis identity without discovering the faucet first. |
| D2 | Return `networkHash` and `genesisHash`. | `networkHash` is the authoritative protocol identity bound into transaction signatures; `genesisHash` identifies the materialized chain instance. |
| D3 | Register `groundhog_requestAirdrop` only when an airdrop implementation is configured. | Mainnet and RPC-only deployments should return standard JSON-RPC `Method not found`, not expose an unusable method. |
| D4 | Proxy hosted requests to the existing `/v1/airdrop` endpoint. | The faucet already owns policy, idempotency, signing, submission, receipt polling, and web support. |
| D5 | Preserve the current faucet request and response semantics. | One public contract avoids drift between REST, hosted RPC, and local RPC. |
| D6 | Use `dev-0` for local airdrops. | Local nodes already derive, validate, and fund this account. |
| D7 | Keep recipient, pending-attempt, and idempotency policy in the faucet. | REST and RPC must share the same authoritative policy state. |
| D8 | Give configured gateway source IPs a finite quota override in the existing faucet policy. | The faucet sees one gateway IP for all proxied users; the default client quota is too small, while an unlimited exemption is unsafe. |
| D9 | Keep local airdrops unlimited by default. | Local tests and examples should create accounts without public-service quotas. |
| D10 | Publish the combined network-information and airdrop surface as wire format `4.0.0`. | `origin/main` already exposes `NetworkInfo { networkHash }` at `3.2.0`. Making `genesisHash` required is a breaking result-shape change, so the rebased work requires a major bump; the additive airdrop method ships in the same version. |

## Architecture

### Hosted Devnet

```text
SDK / CLI / wallet
        |
        | groundhog_requestAirdrop
        v
public RPC gateway
        |
        | POST existing /v1/airdrop
        v
existing faucet service
        |
        | existing signed HOG transfer
        v
authoritative transaction submission
```

The gateway does not hold faucet signing material or duplicate faucet policy. It supplies the faucet's configured `networkId` and startup-held `networkHash` and `genesisHash`, forwards the caller's `recipient` and `clientNonce`, and translates the existing response.

### Localnet

```text
SDK / CLI / wallet
        |
        | groundhog_requestAirdrop
        v
local RPC server
        |
        | current local-airdrop transaction path with dev-0
        v
authoritative transaction submission
```

The node supervisor derives and validates `dev-0` once during startup. The RPC handler receives the validated signer; it does not re-read or re-validate the runtime manifest per request.

The hosted CLI calls `groundhog_requestAirdrop` and retains its generated `clientNonce` across retries. Its local administrative path remains direct because it supports caller-selected amounts and faucet-balance preflight, which are intentionally outside the fixed-amount public RPC contract.

## Network Information

### Prior gap

Before this change, no public JSON-RPC method returned network identity:

| Existing source | Available data | Limitation |
|---|---|---|
| `groundhog_chainTip` | Height and current block hash | No immutable protocol or genesis identity |
| `groundhog_version` | RPC wire and implementation versions | No network identity |
| Faucet `GET /v1/network` | `networkId`, `networkHash`, `genesisHash` | Separate REST service |
| Runtime manifest | `genesisHash`, `networkHash` | Private local file handoff |

`groundhog_getBlockByNumber` declares a `networkHash`, but both current server implementations return `NotImplemented`. Scheduler health checks already validate `networkHash` separately; this design does not create another validation system.

### Method

```text
groundhog_networkInfo
```

The method takes no parameters.

### Result

```json
{
  "networkHash": "0x1111111111111111111111111111111111111111111111111111111111111111",
  "genesisHash": "0x2222222222222222222222222222222222222222222222222222222222222222"
}
```

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInfo {
    pub network_hash: NetworkHash,
    pub genesis_hash: SHA256Hash,
}
```

The method returns immutable startup metadata and does not query a backend:

| Deployment | `networkHash` | `genesisHash` |
|---|---|---|
| Localnet | Validated runtime manifest | Validated runtime manifest |
| Hosted | Required gateway configuration | Required gateway configuration |

Hosted protocol identity comes from gateway startup configuration. The logical `networkId` remains separate and is required only when `faucet_url` enables the existing faucet REST adapter. The public descriptor advertises all three values so the gateway can reject a faucet configured to sign for another network. Validation against `GET /v1/network` gates airdrop execution and readiness.

Missing or malformed identity is a startup configuration error. Scheduler health checks verify that observed tips match the startup `networkHash`. `groundhog_networkInfo` stays available during degraded backend operation, like `groundhog_version`.

This method exposes the protocol `networkHash` already carried by scheduler headers and runtime manifests, plus the existing `genesisHash`. It does not treat the faucet descriptor's human-readable `networkId` as protocol identity.

## Airdrop Request

### Method

```text
groundhog_requestAirdrop
```

### Request

JSON-RPC uses one positional object parameter:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "groundhog_requestAirdrop",
  "params": [
    {
      "recipient": "0x1111111111111111111111111111111111111111111111111111111111111111",
      "clientNonce": "0x0123456789abcdef0123456789abcdef"
    }
  ]
}
```

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestAirdropRequest {
    pub recipient: ContractAddress,
    pub client_nonce: HexBytes,
}
```

Rules:

- `recipient` is a canonical 32-byte Groundhog address.
- `clientNonce` is non-empty and at most 32 bytes.
- Clients should generate 16 cryptographically random bytes.
- The caller does not select the amount.
- The gateway supplies its faucet-specific `networkId` and startup-held `networkHash` and `genesisHash` when calling the hosted faucet.

The 32-byte bound should be applied in the shared faucet request validator so REST and RPC accept the same nonce values. The current CLI already generates 16 bytes.

### `clientNonce`

`clientNonce` identifies one logical faucet attempt. It is not an authentication credential, quota allowance, transaction `Uniquifier`, pubkey-account nonce, or deployment nonce.

The client creates it before the first request and reuses it after a pending response or ambiguous transport failure. The faucet's existing attempt key remains:

```text
networkId + genesisHash + recipient + clientNonce
```

A new nonce creates a new logical attempt but does not bypass recipient, IP, or pending-attempt policy.

## Airdrop Response

The JSON-RPC method preserves the existing faucet attempt lifecycle while representing its
wire response as a tagged union. A pending response is:

```json
{
  "attemptStatus": "pending",
  "recipient": "0x1111111111111111111111111111111111111111111111111111111111111111",
  "rawAmount": "100000000",
  "txHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "retryAfterSeconds": 5
}
```

The corresponding succeeded response is:

```json
{
  "attemptStatus": "succeeded",
  "recipient": "0x1111111111111111111111111111111111111111111111111111111111111111",
  "rawAmount": "100000000",
  "txHash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "height": 42,
  "status": { "result": "ok" }
}
```

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "attemptStatus",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AirdropAttemptResponse {
    Pending {
        recipient: ContractAddress,
        raw_amount: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tx_hash: Option<TxHash>,
        retry_after_seconds: u64,
    },
    Succeeded {
        recipient: ContractAddress,
        raw_amount: String,
        tx_hash: TxHash,
        height: BlockHeight,
        status: ExecStatus,
    },
}
```

The response fields and status transitions must match `/v1/airdrop`. Promote the existing private
response type into the shared `json-rpc-types` tagged union used by both adapters rather than
maintaining parallel structs. `pending` always includes `retryAfterSeconds`, may include `txHash`
once submission has been accepted, and never includes committed height or execution status.
`succeeded` always includes `txHash`, `height`, and `status`, and never includes
`retryAfterSeconds`.

When `attemptStatus` is `pending`, the client repeats the same request after `retryAfterSeconds`. A wallet client may hide this polling loop. When it is `succeeded`, `txHash`, `height`, and `status` describe the committed faucet transaction.

## Hosted Gateway Adapter

At startup, a hosted gateway configured with `faucet_url`:

1. fetches `GET /v1/network`;
2. verifies `networkId`, `networkHash`, and `genesisHash` against gateway configuration; and
3. registers the optional airdrop RPC module.

An identity mismatch or malformed descriptor is a configuration error and fails startup. If the
faucet is unreachable, the gateway preserves its existing degraded-startup posture: it starts with
the configured method registered and marks the faucet dependency unavailable in readiness. Airdrop
requests share one in-flight descriptor-validation attempt and return `BackendUnavailable` until a
matching descriptor has been validated. The gateway never forwards an airdrop before validation.
A descriptor that becomes reachable but is malformed or mismatched leaves the dependency unready
and disables execution until configuration or the faucet descriptor is corrected. An intentionally
unconfigured deployment does not register the method.

For each request, the adapter:

1. adds the configured `networkId`, `networkHash`, and `genesisHash`;
2. posts the existing request to `POST /v1/airdrop`;
3. preserves pending and succeeded responses; and
4. maps REST failures to the existing JSON-RPC error catalog.

No new faucet route, internal protocol, request channel, signing service, or persistent store is introduced.

The forwarded REST body is exactly:

```json
{
  "recipient": "0x1111111111111111111111111111111111111111111111111111111111111111",
  "clientNonce": "0x0123456789abcdef0123456789abcdef",
  "networkId": "devnet-preview",
  "networkHash": "0x3333333333333333333333333333333333333333333333333333333333333333",
  "genesisHash": "0x2222222222222222222222222222222222222222222222222222222222222222"
}
```

## Local Adapter

The local server reuses the current CLI local-airdrop construction path rather than depending on the non-publishable faucet crate. Startup prepares one immutable local airdrop context:

| Input | Existing source |
|---|---|
| `dev-0` signer and wallet address | Validated supervisor account configuration and runtime manifest |
| Wallet `doAction` entrypoint | `RuntimeManifest.bootstrap.devWallet` |
| Native HOG address, transfer entrypoint, and calldata encoding | Existing CLI HOG helpers, extracted into the publishable signing/tooling layer |
| Block expiry | Current chain tip through the existing chain backend |
| Submission | Existing validated transaction-submission path |
| Receipt polling | Existing transaction-receipt backend |
| Raw amount | Validated supervisor option, default 10 HOG (`100000000` raw units at seven decimals) |

The local method is registered only for the full indexer-backed topology, where both submission and authoritative receipt lookup are available. Scheduler-only and storage-only topologies omit the optional method and return standard JSON-RPC `Method not found`.

The handler builds and signs the same dev-wallet HOG transfer as `groundhog airdrop`, submits it
through the in-process backend, and returns the shared `AirdropAttemptResponse`. A bounded
process-local map keyed by `(recipient, clientNonce)` preserves local retries. Creating an attempt,
claiming ownership of transaction construction, and recording the signed transaction happen
atomically for one key. Concurrent calls with the same key share the attempt and cannot construct
or submit a second transaction. The adapter retains the exact signed transaction and canonical hash
before submission so an ambiguous retry reuses the same bytes. It may evict terminal attempts to
stay within its configured capacity, but it must not evict pending or ambiguously submitted
attempts. Capacity and concurrency limits are resource bounds, not recipient, IP, or aggregate
airdrop quotas. The map is cleared on node restart.

The signing helper requires the runtime manifest's `networkHash`; it cannot sign with an implicit
or default network identity.

## Policy and Security

### Existing faucet policy remains authoritative

The faucet continues to own:

- the fixed raw HOG amount;
- recipient cooldown;
- maximum pending attempts per recipient;
- idempotency records;
- transaction submission and polling; and
- direct REST client/IP quota.

REST and RPC naturally share recipient and idempotency state because hosted RPC calls the same endpoint and store.

### Gateway quota override

The faucet sees the hosted gateway's IP, not the original RPC caller. Applying the default direct-client quota to that IP would pool all RPC users into one small allowance.

Extend the existing airdrop policy with optional per-IP quota overrides:

```toml
[[policy.airdrop.client_overrides]]
ip = "10.0.0.12"
quota_max = 10000
quota_window_secs = 3600
```

The faucet derives this identity from the actual socket peer and applies the override through the existing `AirdropClient(ClientIdentity::Ip(...))` quota key. The override is finite, deployment-configured, and shared by all users of that gateway. Recipient cooldown, pending-attempt, and idempotency checks remain unchanged.

The gateway does not forward end-user IP headers and does not introduce a second quota store. Callers cannot spoof a TCP peer address for an established HTTP connection. Faucet access should remain behind the existing trusted edge or private network.

If `trust_proxy_client_ip_headers` is enabled for direct REST traffic, deployment must prevent direct access around the canonicalizing proxy and strip caller-supplied forwarding headers at the edge. Strengthening this existing option with trusted proxy CIDRs is a faucet hardening follow-up, not part of the RPC facade.

### Deferred faucet hardening

This work preserves the documented single-process, in-memory faucet behavior. Restart-safe idempotency, exact-byte retries after ambiguous submission, multi-replica policy, aggregate outflow limits, and reserve management remain separate faucet hardening work. The RPC facade neither worsens nor claims to solve those existing limitations.

The hosted faucet retains accepted transactions with unresolved receipts and counts them toward its
bounded attempt capacity. If that capacity fills with ambiguous attempts, the faucet fails closed
for new keys while continuing to serve existing keys. Expiring those records would permit a second
transfer for the same idempotency key. Safe automatic recovery requires authoritative transaction
expiry reconciliation or restart-safe idempotency and remains part of the deferred hardening work.

## Error Mapping

The adapter decodes the structured faucet error code before considering HTTP status. Reuse existing JSON-RPC errors:

| Faucet code or adapter outcome | JSON-RPC error |
|---|---|
| `airdrop_invalid_client_nonce` | `InvalidParams` |
| `airdrop_network_mismatch` | `BackendUnavailable` |
| `airdrop_quota_exceeded`, `airdrop_recipient_pending_limit_exceeded`, `airdrop_already_pending` | `Overloaded` |
| `airdrop_transaction_failed`, `airdrop_attempt_terminal` | `TxRejected` |
| `faucet_policy_store_unavailable`, `airdrop_submit_amount_mismatch`, malformed success/error body | `BackendUnavailable` |
| `airdrop_submit_failed`, faucet unavailable, unknown faucet code, or HTTP 5xx without a recognized code | `BackendUnavailable` |
| Gateway-to-faucet timeout | `BackendTimeout` |
| Airdrop module not registered | Standard `Method not found` (`-32601`) |

`airdrop_attempt_state_invalid` and `airdrop_attempt_conflict` are treated as
`BackendUnavailable`; they indicate server-side state or race handling. `Overloaded` describes the
current call: it did not initiate new side-effecting execution, and retrying the same request with
the same `clientNonce` is safe. It does not imply that an earlier call for that logical attempt has
not submitted a transaction. A structured error code is mapped by the table regardless of HTTP
status, including HTTP 202. Log the bounded faucet code for diagnosis. Do not create a second public
error taxonomy or add untyped `error.data` in v1.

## Observability

Reuse existing per-method gateway metrics. For hosted airdrops, add only:

- hosted faucet request latency and outcome;
- pending and succeeded RPC response counts.

The local node supervisor has no metrics registry or exporter. Adding a second observability server solely for local airdrops is deferred rather than creating process-local counters that cannot be scraped. The faucet emits a bounded structured log when it selects a configured gateway quota override. A faucet metrics subsystem remains out of scope.

Logs must not include secret keys or full untrusted forwarding headers.

## Verification

### Network information

- Localnet returns the runtime manifest's `networkHash` and `genesisHash`.
- Hosted gateways return their configured `networkHash` and `genesisHash`.
- CLI hosted flows reject descriptors whose `networkHash` or `genesisHash` differs from the RPC endpoint.
- Identity is available while chain backends are degraded.
- Missing identity fails startup.
- A reachable faucet descriptor with a malformed or mismatched network ID, network hash, or genesis hash fails startup.
- An unreachable configured faucet starts degraded with the airdrop method registered and returns `BackendUnavailable` until validation succeeds.
- Concurrent requests share descriptor validation, and no request is forwarded before it succeeds.
- A descriptor found malformed or mismatched after degraded startup leaves execution disabled and readiness failed.

### Hosted airdrop

- The gateway sends the existing `/v1/airdrop` request shape with configured identity.
- Pending and succeeded responses round-trip without semantic changes.
- Sequential retries with the same `clientNonce` consume no additional quota event, and concurrent initial calls consume one quota event and initiate one submission.
- REST and RPC requests for one recipient share cooldown and pending state.
- Disabled deployments do not register the method.
- Faucet HTTP status and structured errors map to the documented JSON-RPC errors.
- Configured gateway source IPs receive the finite override through the existing client quota key.
- Unconfigured source IPs receive the default client quota.
- Forged forwarding headers do not affect the faucet's trusted gateway decision.
- RPC batches charge limits once per airdrop call.

### Local airdrop

- The local server derives and validates `dev-0` once at startup.
- Every local transaction input comes from the documented existing source.
- Airdrops use the validated local amount, defaulting to 10 HOG, and the existing transaction submission path.
- Quotas are disabled by default.
- Repeated local requests use the local adapter's process-local attempt map.
- Concurrent requests for one attempt construct and submit one exact signed transaction.
- Ambiguous retries reuse the recorded signed bytes and canonical transaction hash.
- The attempt map enforces configured resource bounds without evicting unresolved attempts.

### Shared contracts

- REST and RPC serialize the same `AirdropAttemptResponse` type.
- Pending responses require `retryAfterSeconds`, may include `txHash`, and omit committed fields.
- Succeeded responses require `txHash`, `height`, and `status`, and omit `retryAfterSeconds`.
- OpenRPC represents `AirdropAttemptResponse` as a tagged `pending` or `succeeded` union.
- Nonces over 32 bytes are rejected by the shared validator.
- OpenRPC contains both methods and their documented errors.
- `origin/main` exposes `groundhog_networkInfo` with required `networkHash` at wire format `3.2.0`.
- This change adds required `genesisHash` to that result and adds `groundhog_requestAirdrop`, producing wire format `4.0.0`.

## Implementation Sequence

1. Extend `NetworkInfo` with `genesisHash` and keep its RPC module always registered.
2. Promote the existing faucet airdrop response into a tagged union in `json-rpc-types`; add the RPC airdrop request type and optionally registered module.
3. Add gateway `network_hash` and `genesis_hash` startup identity; scope `network_id` to hosted faucet configuration, validate reachable descriptors, and preserve degraded startup when the faucet is unavailable.
4. Implement the hosted adapter by calling the existing `POST /v1/airdrop` endpoint.
5. Extract the current CLI HOG transaction helpers into the publishable signing/tooling layer and implement the process-local `dev-0` adapter.
6. Add finite per-IP quota overrides to the existing faucet client-quota configuration and policy lookup.
7. Update airdrop OpenRPC method/error mappings and snapshots; bump the wire format to `4.0.0`.
8. Hand the generated RPC contract to the separate TypeScript client workstream.

## Acceptance Criteria

- Every RPC endpoint returns its configured protocol network hash and genesis hash.
- Local and hosted development endpoints expose the same airdrop request and response contract.
- Hosted RPC uses the existing faucet REST API and policy store.
- Local RPC uses the current local-airdrop transaction helpers and `dev-0`.
- Mainnet and unconfigured endpoints do not register the airdrop method.
- REST and RPC share recipient, pending-attempt, and idempotency behavior.
- Proxied RPC users receive a finite deployment-configured gateway quota instead of the faucet's small direct-client quota.
- No second faucet API, durable store, identity subsystem, or new error taxonomy is introduced.
- The generated OpenRPC artifact fully describes both methods.
