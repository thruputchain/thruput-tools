# Faucet Hardening Workstream

Date: 2026-07-08
Status: Draft after initial review
Related: GH-72, GH-183, GH-187

2026-08-24 update: Hosted CLI airdrops now use `groundhog_requestAirdrop`, and
the gateway-to-faucet request additionally carries `networkHash`. Direct
`POST /v1/airdrop` CLI guidance below is superseded by the network-info and
airdrop RPC design; web clients may continue using the REST endpoint with all
three network identity fields.

## Goal
Harden the hosted Devnet faucet before it is exposed to private-preview users.

PR #155 shipped the core faucet service, CLI integration, and web UI. The next workstream makes that flow safe and understandable under retries, abuse pressure, misconfiguration, and partial failures.

This document covers two related tracks:

- GH-183: faucet service policy controls, idempotency, duplicate-request behavior, and CLI retry semantics.
  
- GH-187: faucet web UI error handling and config hardening.
  
## Current State
The faucet service is functional but mostly stateless:

- `POST /v1/airdrop` signs and submits one faucet-funded transfer for each request.
  
- `POST /v1/create-wallet/prepare` derives a future wallet address but does not reserve quota or persist an attempt.
  
- `POST /v1/create-wallet` verifies owner proof and submits the sponsored wallet-create transaction.
  
- The CLI currently orchestrates remote wallet creation as `prepare -> airdrop -> create-wallet`.
  
- The web UI calls `/v1/network` on load and `/v1/airdrop` on submit.
  

This leaves three hardening gaps:

- Repeated airdrops can spend faucet funds without recipient, IP, or pending-request limits.
  
- Wallet-create retries can strand prefund funds if the CLI times out after prefunding but before it observes create success.
  
- Web UI users see poor errors when the API is down, misconfigured, or returns non-JSON error bodies.
  
## Design Change: Single Faucet-owned Wallet Create
The original design mirrored the eventual mainnet flow: derive a future wallet address, fund it externally, then ask a sponsor to create the wallet. For private-preview Devnet, the faucet is both the funder and sponsor, so keeping those steps split adds retry risk without much value.

This supersedes the original `prepare -> airdrop -> create-wallet` Devnet flow in `2026-06-25-remote-devnet-faucet-design.md`. The future mainnet shape can still expose a user-funded address before create, but private-preview Devnet should optimize for safe faucet-owned retries.

Wallet creation should move to a single focused faucet endpoint:

```text
POST /v1/create-wallet
```

The faucet owns the full state machine:

```text
validate owner proof and idempotency key
derive wallet address
reserve quota
prefund future wallet address
submit sponsored create-wallet transaction
return final or pending result
```

`/v1/airdrop` remains for funding existing addresses, but wallet creation should no longer depend on public `/v1/airdrop` as a sub-step.

This directly addresses Mark's GH-183 scenario: if the client times out after prefund, retrying the same wallet-create request must reuse the same attempt and return or continue the same result instead of generating a fresh deploy nonce and stranding funds at the old address.
## Policy State Model
Use in-memory policy state for private preview, but hide it behind a storage interface so a future database-backed implementation can replace it without rewriting handler logic. The private-preview deployment can assume one faucet instance for now.

Suggested abstraction:

```text
trait FaucetPolicyStore
  get/put airdrop attempts
  get/put wallet-create attempts
  check/increment quota counters
  expire old records
```

Initial implementation:

```text
InMemoryFaucetPolicyStore
  RwLock or Mutex protected maps
  periodic or opportunistic expiration
```

Future implementations can use SQLite, Redis, or Postgres if deployment later requires restart-safe idempotency or multi-instance faucet service replicas.

Policy logic must not hold a global store lock while submitting or polling RPC transactions. The store should atomically claim an attempt transition, such as `Prepared -> PrefundPending`, then release the lock before network I/O. Duplicate requests that arrive while another request owns the attempt return the current attempt state with retry guidance.

Client identity is initially IP-derived for private-preview quotas. If the faucet sits behind a reverse proxy or load balancer, the service must only trust forwarded client IP headers when explicitly configured with trusted proxy behavior; otherwise it should use the direct peer address. This prevents spoofed `X-Forwarded-For` values from bypassing per-client limits.

### Chain Identity
Wallet-create attempts must be scoped to chain identity:

```text
networkId + genesisHash + ownerPublicKey + clientNonce
```

`networkId` identifies the named network. `genesisHash` identifies the specific reset of that network. Devnet can re-genesis, and old wallet-create attempts, addresses, tx hashes, and prefund state must not be reused after a reset.

For in-memory state this protects a long-running service across a network reset. For a future database store it is mandatory because old records may outlive the chain instance they were created on.
## Wallet-create State
Key:

```text
WalletCreateAttemptKey
  network_id
  genesis_hash
  owner_public_key
  client_nonce
```

Record:

```text
WalletCreateAttempt
  key
  deploy_nonce
  wallet_address
  wallet_contract_hash
  prefund_raw_amount
  status
  prefund_tx_hash optional
  prefund_height optional
  create_tx_hash optional
  create_height optional
  error_code optional
  created_at
  updated_at
  expires_at optional
```

Statuses:

```text
Prepared
PrefundPending
PrefundConfirmed
CreatePending
Created
FailedRetryable
FailedTerminal
Expired
```

Behavior:

- First request with a new key creates an attempt and derives one wallet address.
  
- Retry with the same key returns or continues the same attempt.
  
- Retry after client timeout must not create a second deploy nonce or wallet address.
  
- If prefund committed but create did not complete, retry continues with create for the same wallet address.
  
- If create committed, retry returns the created result.
  
- Prepared attempts that never prefunded may expire.
  
- Prefunded attempts should not be silently discarded; they are either retried to completion or logged as stranded-funds cases if they fail terminally before wallet creation.
  
## Airdrop State
Generic `/v1/airdrop` should also gain policy controls.

Idempotency key:

```text
networkId
genesisHash
recipient address
clientNonce
```

Client identity is tracked separately for quota enforcement; it is not part of the idempotency key. A retry should remain idempotent even if the caller's apparent IP changes between attempts.

Record:

```text
AirdropAttempt
  network_id
  genesis_hash
  recipient
  client_nonce
  client_identity
  raw_amount
  status
  tx_hash optional
  height optional
  error_code optional
  created_at
  updated_at
```

Statuses:

```text
Pending
Succeeded
FailedRetryable
FailedTerminal
```

Behavior:

- Only one pending airdrop per recipient.
  
- A retry with the same `clientNonce` should return or continue the same logical request.
  
- `clientNonce` is required. The service is not live yet, so there is no compatibility reason to preserve nonce-less airdrop requests.
  
- Airdrop amount remains fixed by server config.

This intentionally differs from the original design's `rawAmount` request field. For private preview, generic faucet airdrops should use the configured server amount only. A future public faucet can add bounded client-selected amounts if needed.
  
## Private-preview Policy Defaults
Policy values must be config-driven so private-preview limits can be changed after team discussion without a code change. Suggested defaults for invited-user Devnet:

```text
Airdrop
  per recipient cooldown: 12 hours
  per IP quota: 10 requests / hour
  max pending per recipient: 1

Wallet create
  per owner public key quota: 3 / day
  per IP quota: 20 / day
  max pending per owner public key: 1
  unprefunded attempt TTL: 15 minutes
  prefunded attempt retention: 24 hours minimum, preferably operator cleanup only
```

These defaults are intentionally moderate: enough to stop accidental double-spend and basic abuse, but not so strict that invited private-preview users are blocked during normal testing.
## API Contract Changes

These are pre-launch breaking changes. The faucet is not internet-reachable yet, so the hardening work can require `clientNonce`, `networkId`, and `genesisHash`, remove `prepare`, and change the wallet-create flow without compatibility shims.

### `POST /v1/create-wallet`
Request should include an idempotency nonce and enough chain identity to bind the request to the descriptor the client used:

```json
{
  "ownerPublicKey": "0x...",
  "deployNonce": "0x...",
  "clientNonce": "0x...",
  "ownerSignature": "0x...",
  "networkId": "devnet-preview",
  "genesisHash": "0x..."
}
```

The server should validate:

- `networkId` matches the live descriptor.
  
- `genesisHash` matches the live descriptor.
  
- `deployNonce` is 32 bytes.
  
- `clientNonce` is non-empty random client entropy.
  
- `ownerSignature` verifies the canonical wallet-create proof for the owner key, `clientNonce`, network identity, faucet sponsor address, wallet contract hash, deploy nonce, and derived wallet address.
  
- The derived wallet address matches the stored or newly derived attempt.
  
- Policy allows this attempt or this is a retry of an existing attempt.
  

Response for success or already-created retry:

```json
{
  "ownerPublicKey": "0x...",
  "deployNonce": "0x...",
  "clientNonce": "0x...",
  "walletAddress": "0x...",
  "prefundRawAmount": "200000000",
  "prefundTxHash": "0x...",
  "prefundHeight": 123,
  "createTxHash": "0x...",
  "createHeight": 124,
  "status": { "result": "ok" }
}
```

Response for an in-progress retry can be either `202 Accepted` or `200 OK` with an explicit pending status. Prefer `202 Accepted` if the handler chooses not to wait for completion:

```json
{
  "ownerPublicKey": "0x...",
  "deployNonce": "0x...",
  "clientNonce": "0x...",
  "walletAddress": "0x...",
  "attemptStatus": "createPending",
  "retryAfterSeconds": 5
}
```

The request does not include `walletAddress` or `walletContractHash`. The faucet derives both from the live descriptor and `deployNonce`, then binds the derived values into the owner proof. This keeps the single endpoint compact while preserving the original design's proof-of-intent boundary.

Wallet-create prefunding and ordinary airdrops are separate policy values. Wallet creation currently transfers `200000000` raw units so the new wallet can pay its own creation costs and still fund a first contract deploy. A normal airdrop remains `100000000` raw units.

The signing helper will need a new proof-message shape or version because the current shipped proof does not include `clientNonce`, `networkId`, or wallet address.

If a retry reuses the same wallet-create key but sends a different deploy nonce, owner key, or signature payload, the faucet should reject it as an idempotency conflict rather than creating or mutating a second attempt.

### `POST /v1/create-wallet/prepare`
Remove this endpoint as part of the hardening workstream. The faucet is not live yet, and the single `POST /v1/create-wallet` endpoint replaces the prepare/prefund/create split.
### `POST /v1/airdrop`
Request:

```json
{
  "recipient": "0x...",
  "clientNonce": "0x...",
  "networkId": "devnet-preview",
  "genesisHash": "0x..."
}
```

`clientNonce`, `networkId`, and `genesisHash` are required. The server rejects mismatched chain identity.

Response:

```json
{
  "recipient": "0x...",
  "rawAmount": "100000000",
  "txHash": "0x...",
  "height": 123,
  "status": { "result": "ok" }
}
```

An idempotent retry of an already-succeeded airdrop returns the same success payload. A duplicate request while the original airdrop is still pending returns a pending response with `retryAfterSeconds` rather than submitting a second transfer.

If a retry reuses the same airdrop idempotency key but sends conflicting request data, the faucet should reject it as an idempotency conflict.

## Structured Error Contract
All faucet errors should use the same JSON shape when the handler runs:

```json
{
  "error": {
    "code": "rate_limited",
    "message": "Airdrop limit reached for this recipient.",
    "retryAfterSeconds": 3600
  }
}
```

Recommended codes:

```text
invalid_request
idempotency_conflict
network_mismatch
owner_proof_invalid
rate_limited
request_pending
quota_exceeded
attempt_expired
airdrop_transaction_failed
airdrop_submit_failed
wallet_create_transaction_failed
wallet_create_submit_failed
service_unavailable
```

HTTP status guidance:

```text
400 invalid_request, idempotency_conflict, network_mismatch, attempt_expired
401 owner_proof_invalid
409 request_pending when a duplicate is already running and cannot be joined
429 rate_limited, quota_exceeded
502 transaction failed or RPC submit/poll failure
503 service unavailable or policy store unavailable
```

Include `retryAfterSeconds` whenever the caller can safely retry later.
## CLI Changes
The CLI should stop orchestrating remote wallet create as `prepare -> airdrop -> create-wallet`.

New remote wallet-create flow:

```text
resolve profile/backend
fetch /v1/network
load owner key
load or create local wallet-create attempt record
derive deployNonce and clientNonce once
sign wallet-create proof
POST /v1/create-wallet
save returned wallet binding
clear local attempt record
```

Local attempt persistence is still useful even with a single endpoint because it lets CLI retries reuse the same deploy nonce and client nonce after local interruption.

Suggested local attempt key:

```text
profile name
key name
wallet name
networkId
genesisHash
```

Suggested local record:

```text
owner_public_key
deploy_nonce
client_nonce
wallet_address optional
wallet_contract_hash
created_at
updated_at
```

Behavior:

- Create the local attempt before the first faucet request.
  
- Reuse it on retry when network identity and wallet contract hash still match.
  
- Ignore or delete it when `genesisHash` or wallet contract hash changes.
  
- Clear it after the wallet binding is saved.
  
- If the faucet returns `request_pending` or `202 Accepted`, surface a clear retry message.
  
Remote `airdrop` must also include the descriptor's `networkId` and `genesisHash` plus a generated `clientNonce`. To make timeout retries exact rather than best effort, the CLI should persist a small airdrop attempt record before the request and clear it after a successful response.

Suggested local airdrop attempt key:

```text
profile name
recipient address
networkId
genesisHash
```

Suggested local airdrop record:

```text
client_nonce
created_at
updated_at
```

## Web UI Hardening
GH-187 scope should be handled as a separate UI-focused PR unless it naturally lands with shared API error helpers.

Required changes:

- Normalize `VITE_GROUNDHOG_FAUCET_API_BASE` by stripping trailing slashes.
  
- Detect unset or bad API base in production instead of silently fetching the static host's `index.html`.
  
- Validate `/v1/network` response shape before treating it as a descriptor. At minimum require `networkId`, `networkHash`, `genesisHash`, `nativeHog.decimals`, and `walletCreate.prefundRawAmount`.
  
- Parse API responses defensively:
  
  - check `response.ok` and `Content-Type` before JSON parsing;
    
  - handle `text/plain` Axum extractor errors;
    
  - handle HTML proxy errors;
    
  - handle JSON `null` or malformed error bodies.
    
- Add persistent descriptor-load error state with a retry button. The form must not stay permanently disabled with only a transient toast.
  
- Keep post-airdrop formatting outside the request failure path. If `formatHogAmount` fails after a committed airdrop, show the tx success with raw amount fallback rather than a false request failure.

- Generate and send `clientNonce`, `networkId`, `networkHash`, and `genesisHash` with `/v1/airdrop` requests. The UI should validate all three network identity fields from `/v1/network` before enabling a request.
  
## Implementation Plan
Recommended PR split:

1. Faucet policy store and structured errors.
  
2. Single-endpoint wallet-create state machine and CLI flow update.
  
3. Generic airdrop policy/idempotency controls.
  
4. Web UI error/config hardening for GH-187.
  
5. Docs/runbook updates for private-preview limits and retry behavior.
  

The first two can be combined if the policy store is small. The UI hardening should stay independent if possible because it has different test surface and review concerns.
## Acceptance Criteria
GH-183:

- Airdrop requests are limited by recipient and client identity.
  
- Duplicate pending airdrop requests do not double-spend accidentally.
  
- Wallet-create retries do not generate a new deploy nonce, derive a new address, or double-spend prefund funds.
  
- A client timeout after prefund can be retried safely with the same logical request.
  
- Policy errors are structured and distinguishable from validation, RPC, and on-chain failures.
  
- Policy defaults are configurable and documented.

- Tests cover validation, rate limits, cooldowns, idempotent retries, and duplicate pending-request behavior.


GH-187:

- API down, misconfigured API base, non-JSON errors, and descriptor shape mismatches produce actionable messages.
  
- The web UI can retry descriptor load without a page refresh.
  
- A committed airdrop is not reported as failed because amount formatting failed afterward.
