# Gateway Roadmap

This roadmap is intentionally pragmatic. The goal is to get a solid testnet
gateway out first, while preserving the architectural seams needed for later
ingress hardening and commercial operator deployment.

## Principles

- v1 should establish the boundary, not the final policy
- keep transaction validity separate from admission policy
- keep public ingress outside validator internals
- avoid baking in assumptions that block future QoS / provider evolution

## Phase 1: Testnet v1

Goal: ship a dedicated public RPC gateway with the right architecture, even if
the ingress policy is intentionally minimal.

Scope:

- dedicated `hog-gateway` service as the only public ingress
- private-network connectivity from gateway to validator components
- submission goes directly to hogd scheduler (the configurable
  `submission_backend` was deleted once the direct path was settled)
- API surface for the initial read + submit methods
- explicit admission layer in the gateway, even if v1 policy is simple

Expected v1 admission policy:

- deterministic tx-hash dedupe
- basic per-IP and/or per-key rate limiting
- bounded queues
- overload shedding / backpressure
- metrics for submit rejects, queue depth, backend latency, and backend errors

Explicitly out of scope for v1:

- mainnet-grade fairness guarantees
- stake-weighted QoS
- prepaid admission capacity
- sophisticated anti-sybil economics
- commercial provider lanes

## Phase 2: Harden Official Gateway

Goal: keep the same architecture and user-facing RPC while making the official
gateway operationally stronger.

Likely additions:

- stronger authenticated lanes
- per-tenant quotas
- better overload behavior and queue partitioning
- internal policy split between anonymous traffic and trusted infrastructure
- improved observability and admission analytics
- WebSocket subscriptions

## Phase 3: Provider-Friendly Deployment Model

Goal: support the industry-standard shape where high-throughput users talk to
specialized RPC operators rather than directly to validator-run public
endpoints.

This is the QuickNode / Alchemy style model:

- official gateway remains one deployment
- third-party operators can run the same gateway boundary
- providers compete on reliability, caching, indexing, auth, and customer QoS
- validator internals remain private regardless of who operates the public edge

Implications:

- keep the public RPC contract stable
- keep ingress policy outside transaction validity
- make authentication and tenant identity pluggable at the gateway boundary

## Phase 4: Advanced Admission Policy

Goal: add stronger anti-spam / fairness controls if real usage requires them.

Possible directions:

- stake- or reputation-weighted admission classes
- sponsored ingress for dApps / wallets
- prepaid admission capacity or reserved ingress quota
- more explicit separation of anonymous, retail, and infrastructure lanes

These are long-term options, not prerequisites for testnet.

## Decision Heuristic

When in doubt:

- if it is needed to preserve the gateway boundary, include it in v1
- if it is mainly a policy optimization, leave a seam for it and defer it
- if it changes user-facing RPC semantics, be cautious
- if it only changes gateway admission behavior, it is a better candidate for
  later evolution
