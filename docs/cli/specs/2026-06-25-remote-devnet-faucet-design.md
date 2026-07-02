# Remote Devnet Faucet and Network UX

**Date:** 2026-06-25
**Status:** Design. Decisions below were taken in a cross-repo brainstorm and
are ready for implementation planning. Spans `groundhog-rpc` (CLI + a new
faucet service), `hogd` (bootstrap-tool and dev-wallet changes now;
`kPubkeySignature` later), `chuckd` (public descriptor / manifest
sanitization), and `ops` (genesis faucet account, faucet deploy, key custody).

**Companions:**

- [`../../gateway/runtime-manifest.md`](../../gateway/runtime-manifest.md) —
  the private runtime manifest contract; the public descriptor is a distinct
  type covering only its non-secret subset (see §2), not a projection of it.
- `ops/docs/hosted-devnet-bootstrap-faucet.md` — the prior ops-side bootstrap
  and faucet design notes this supersedes for the CLI/service shape.

## Goal

Make a deployed "devnet" usable from the `groundhog` CLI: pick a network
(`--network devnet|local|testnet` or `--rpc-url`), get funded, create a wallet,
deploy, and call — **without publishing any bootstrap account private keys**.

Today every CLI write path (`wallet create`, `airdrop`, `deploy`, `call`)
assumes a local runtime manifest whose seed lets the CLI re-derive bootstrap
account private keys. That model cannot extend to a shared remote network: the
seed is the secret, and publishing it hands everyone the funded accounts. This
design replaces the secret-bearing local path, for remote networks, with a
**public network descriptor** (no secrets) plus a **hosted faucet service** that
holds one genesis-funded key. `groundhog wallet create` uses that service to
fund the deterministic future wallet address first, then triggers the sponsored
deploy/init step while pubkey-auth self-bootstrap is unavailable.

## Verified constraints (why the design looks like this)

These were confirmed by reading the backend, not inferred from names.

1. **A fresh keypair cannot author its own first transaction.** The
   `kPubkeySignature` auth claim is unimplemented — `auth_claim/verify.cc:204`
   returns `kAuthMalformed`. Transaction auth requires `AddressAuthData` already
   present at the invoked address (`auth/auth_server.cc:~277`), i.e. a deployed
   contract. So a developer cannot self-bootstrap from a merely-funded address.
2. **Wallet deployment must be sponsored, and the address is sponsor-derived.**
   `dev_wallet::create_wallet` (`built_contracts/.../dev_wallet/src/implementation.rs:97`)
   requires `kSourceAddressAuthData` (an existing wallet's signature), deploys
   the new wallet at `SHA256(sponsor_address || nonce || contract_hash)`, and
   bakes the owner key in via `initialize(owner)`. Today it also transfers
   `DEPLOY_STORAGE_TOKENS` into the future wallet address before deployment;
   this design moves that funding out of the contract and into the CLI/app
   wallet-create flow. Any existing wallet may sponsor deployment for an
   arbitrary owner pubkey — so a single faucet/sponsor wallet suffices for
   devnet until pubkey auth lands.
3. **Native HOG has fixed genesis supply and no mint.** A faucet is just a
   genesis-funded account; "topping up" means transferring from another
   genesis-funded account, or re-genesis.
4. **A balance can sit at an address before any contract is deployed there**
   (`FreeNativeTokenBalance` is address-keyed). That is useful immediately for
   pre-funding a deterministic future wallet address, but it is not enough for a
   fresh key to self-author the deployment until constraint 1 is lifted.
5. **Getting a funded account into genesis is already a solved mechanism.**
   `hog-hosted-bootstrap generate` builds the genesis config; chuckd loads it at
   height 0 and SCP externalizes block 1. `GenesisConfiguration.initial_config`
   (`protocol/genesis.x`) and `GenesisConfigurationBuilder::add_initial_config`
   already support arbitrary `(contract, nonce, entrypoint, calldata, amount)`
   allocations.
6. **But the bootstrap tool only emits seed-derived accounts.** Every account's
   private key comes from the seed string (`devnode/bootstrap_accounts.cc:74`,
   SHA256 over `seed + ":" + label`, with `groundhog-dev-accounts` published in
   source). There is no
   path today to inject an account whose public key is fixed in genesis but
   whose private key is supplied externally. Adding one is the single contained
   `hogd` change this design needs for P1.
7. **deploy and call already route through the user's wallet.** Both build a
   transaction whose `invoked_address` is `resolve_wallet_address(...)` and use
   `source_address_auth_claim()` (`deploy.rs:317,357`, `call.rs:290,295`). For a
   non-dev key, `resolve_wallet_address` returns the *pubkey-account* address
   (`account_resolution.rs:56`), which has no deployed contract and differs from
   the sponsor-derived address `wallet create` saves. This incoherence is not
   remote-specific — pubkey auth is unimplemented everywhere — so the binding
   must be fixed generally (see §5).
8. **chuckd, not hogd, emits the on-node runtime manifest — and it includes the
   seed.** `chuckd writeHostedRuntimeManifest` (`chuckd/cmd/hosted_bootstrap.go:267`)
   builds the manifest with `"bootstrapAccountDerivation"` (line 298) carried
   straight from bootstrap metadata, written via
   `--hosted-bootstrap-runtime-manifest-out`. chuckd already computes the
   bootstrap commit block hash (line 344), so it is the natural producer of a
   sanitized descriptor's `genesisHash`. The descriptor work is therefore not
   hogd-only.
9. **The public JSON-RPC API exposes no genesis hash.** `version` returns only
   `wireFormat` / `implementation` / `commitHash` (`version.rs`); there is no
   `networkInfo` method and no genesis field. A descriptor cached by `networkId`
   has no first-class live signal to detect a nightly re-genesis (see §2).

## Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Faucet key = dedicated custom-key account in genesis.** | Keeps the faucet key private (ops-held, not seed-derived) without abandoning the published seed for local dev. Needs a contained `hog-hosted-bootstrap` change (constraint 6). |
| D2 | **Prefund-then-sponsor now; `kPubkeySignature` is a separate later track.** | The mainnet-like UX is "fund the future wallet address, then deploy/init it." Devnet uses the faucet for funding and a faucet-held sponsor for deploy/init; mainnet later swaps the faucet funding step for an exchange/bridge/user transfer. The pubkey-account path is a real backend project gated on a replay-model decision (see §7), not "fill in the arm." |
| D3 | **Faucet exposed as a standalone HTTP service.** | Keeps the secret key out of the public RPC/gateway (which is a stateless read/submit front per gateway conventions), and lets ops deploy / rotate / rate-limit it independently. |
| D4 | **Networks are first-class; profiles layer on top.** | A network is "which endpoint + descriptor + faucet"; a profile is the user's working context. Conflating them (today's `--profile` alias `network`) blocks zero-config `--network devnet`. |
| D5 | **Descriptor fetched from the faucet/gateway and cached locally.** | The nightly devnet re-genesis changes genesis/dev-wallet hashes; a fetched descriptor always matches the live deployment, where baked-in values would go stale. |

## Architecture

### 1. Network model & flag precedence

Introduce built-in **networks**, distinct from profiles:

| Network | rpc default | faucet | bootstrap source |
|---|---|---|---|
| `local` | `http://127.0.0.1:9545` | none (local sponsor via `groundhog node`) | local runtime manifest (full, with seed — acceptable, it is local) |
| `devnet` | `http://44.236.180.135:8545` (DNS name later) | `https://faucet.devnet…` | public network descriptor (no secrets) |
| `testnet` | TBD | TBD | public network descriptor |

A **profile** (`[profile.<name>]` in `groundhog.toml`) holds `default_key`,
saved `contracts`, saved `wallets`, and a target `network = "devnet"`, with
optional `rpc_url` / `faucet_url` overrides.

Resolution precedence (high → low):

1. CLI flags: `--rpc-url`, `--faucet-url`, `--runtime-manifest`, `--network`.
2. Env: `GROUNDHOG_RPC_URL`, `GROUNDHOG_FAUCET_URL`, `GROUNDHOG_NETWORK`.
3. Profile (`groundhog.toml`).
4. Built-in network defaults.

`--network devnet` works with no `groundhog.toml`. The current
`--profile` alias `network` is retired (with a deprecation shim for one
release): `--network` selects a network; `--profile` selects context.

The existing loopback gate in `runtime_manifest.rs`
(`profile_rpc_allows_local_node_state`) generalizes to: **non-`local` networks
never attempt seed/dev-N derivation** — bootstrap goes to the faucet.

### 2. Public network descriptor

Two artifacts must stay separate (see Security §6):

- **Private operational runtime manifest** — what chuckd writes today
  (`bootstrapAccountDerivation` incl. `seed`, full `bootstrapAccounts`). Stays
  ops-only.
- **Public network descriptor** — a *distinct* new type, never a projection of
  `RuntimeManifest`, so the seed cannot be serialized into it by accident. The
  `RuntimeManifest` type carries `bootstrap_account_derivation.seed: String`
  (`runtime_manifest.rs:93`); a stripped view of it is one missing-`#[serde(skip)]`
  away from a leak. Define the descriptor fresh.

The public descriptor (owned by `json-rpc-types`, since it crosses the RPC/CLI
boundary) contains only public fields: `descriptorVersion`, `networkId`,
`genesisHash`, `rpcUrl`, `faucetUrl`, `walletCreateSponsorAddress`,
`walletCreatePrefundRawAmount`, and the `devWallet` entrypoints
(`deployEntrypoint`, `doActionEntrypoint`, `createWalletEntrypoint`) +
`contentHash`. That is exactly what the CLI needs to derive and pre-fund a
future wallet address, build `deploy` / `do_action` transactions, and
label/verify the network — and nothing private.

- `local`: the existing local-node runtime manifest is used unchanged (the
  descriptor is a remote-network concern).
- remote: the descriptor is fetched (see §5) and cached at
  `~/.cache/groundhog/networks/<networkId>.json`. The CLI hard-refuses any
  seed-derivation on non-`local` networks regardless of descriptor contents.

**Freshness / re-genesis detection.** Because the API exposes no live genesis
hash (constraint 9), the descriptor needs an explicit refresh path:

- `GET <faucetUrl>/v1/network` is authoritative and supports HTTP `ETag` /
  conditional revalidation; the CLI revalidates the cached descriptor **before
  any signed or faucet-backed flow** (cheap 304 on the common path).
- On a `genesisHash` change between cached and freshly-fetched descriptor, the
  CLI invalidates the cache and surfaces "network was reset — refreshed
  descriptor" rather than signing against stale dev-wallet/entrypoint metadata.
- Optional future live cross-check: add a `networkInfo` method or include a
  block hash / genesis hash in block responses if a direct RPC confirmation is
  wanted. Today's `getBlockByNumber(0)` is not sufficient because `BlockInfo`
  does not include its own block hash.

### 3. Faucet HTTP service

A new small Rust service (own crate under `crates/`, reusing
`groundhog-signing` + `json-rpc-client`) holding the genesis-funded
faucet/sponsor wallet key (loaded from AWS Secrets Manager, never an argv):

- `POST /v1/create-wallet/prepare { ownerPubkey, deployNonce,
  walletContractHash, walletAddress, networkId, genesisHash, clientNonce,
  ownerSig }` → validates and reserves a wallet-create attempt without
  submitting a transaction. This endpoint verifies the same signed
  wallet-create intent as `/v1/create-wallet`, checks the `createWallet` quotas,
  address derivation, `networkId`, `genesisHash`, and dev-wallet hash, and
  returns `{ walletAddress, prefundRawAmount, preparedUntil }`. The CLI calls
  this before spending the pre-fund airdrop so an exhausted create quota cannot
  burn faucet funds for undeployable wallet addresses. It is idempotent on
  `(networkId, genesisHash, ownerPubkey, clientNonce)`.
- `POST /v1/create-wallet { ownerPubkey, deployNonce, walletContractHash,
  walletAddress, networkId, genesisHash, clientNonce, ownerSig }` → sponsored
  `create_wallet` via the faucet's wallet → waits for receipt →
  `{ walletAddress, txHash, height }`.
  - **Deploy/init only:** `createWallet` does not fund the wallet. The CLI/app
    must first fund the deterministic `walletAddress` with at least
    `walletCreatePrefundRawAmount`, using `/v1/airdrop` on devnet or an
    exchange/bridge/user transfer on mainnet. This is the key contract
    boundary change: move pre-funding out of `dev_wallet::create_wallet` and
    into the wallet-create UX.
  - **Address verification required:** the service rejects requests where
    `walletAddress != derive_deploy_address(walletCreateSponsorAddress,
    deployNonce, walletContractHash)` or `walletContractHash` does not match the
    live descriptor's dev-wallet `contentHash`.
  - **Proof-of-possession required:** `ownerSig` is an ed25519 signature by
    `ownerPubkey` over the canonical wallet-create intent (ownerPubkey +
    clientNonce + networkId + genesisHash + walletCreateSponsorAddress +
    deployNonce + walletContractHash + walletAddress). `networkId` and
    `genesisHash` are explicit request fields; the faucet rejects mismatches
    against the descriptor it is serving instead of inferring either value.
    Without it anyone could spend rate limit creating wallets for keys they
    don't hold. The CLI signs this with the owner key it already has.
  - **Idempotent:** `(networkId, genesisHash, ownerPubkey, clientNonce)` keys
    the request so retries return the same wallet instead of deploying a second
    one.
- `POST /v1/airdrop { address, rawAmount, networkId, genesisHash,
  clientNonce }` → `do_action` HOG
  transfer (`0x63285b3523f9dc3f` on token `0x…01`) → `{ txHash, height }`.
  `rawAmount` is a decimal string in base HOG units; the CLI converts
  user-facing HOG decimals. Idempotent on `(networkId, genesisHash, address,
  clientNonce)`.
- `GET /v1/network` → the public network descriptor (§2), with `ETag`.
- `createWallet`: per-IP and per-pubkey rate limiting keyed on `ownerPubkey`.
  `airdrop`: per-IP and per-address rate limiting keyed on recipient `address`.
  Wallet-create pre-fund airdrops are issued only after
  `/v1/create-wallet/prepare` has accepted and reserved the create quota.
  Bounded amounts; structured-JSON errors.

The service is the only component holding the faucet key. It submits through the
same public RPC the CLI uses, so a successful response means the tx reached the
authoritative mempool path (per the gateway submit semantics in CLAUDE.md).

### 4. Genesis / ops changes

There is **no separate faucet smart contract**. The on-chain faucet account is a
normal genesis-funded `dev_wallet` — the same contract every account uses —
`initialize`d with the faucet *service's* public key. "Faucet logic" lives
entirely in the off-chain HTTP service (§3), which signs HOG transfers and the
temporary sponsored `create_wallet` deploy/init transaction with the faucet key.
The on-chain `dev_wallet::create_wallet` should stop transferring
`DEPLOY_STORAGE_TOKENS`; the CLI/app owns the pre-funding step before it asks
the sponsor to deploy and initialize the wallet contract.

- **`hogd`:** extend `hog-hosted-bootstrap generate` to accept **explicit custom
  wallet allocations from supplied public keys** (faucet, optional reserve),
  injecting each via `GenesisConfigurationBuilder::add_initial_config` with
  `initialize(pubkey)` calldata (mirroring the seed-derived loop in
  `bootstrap_accounts.cc:74`, but with a supplied pubkey + balance). It must
  carry **no private derivation material** for these accounts — only the public
  keys. Also update the dev-wallet `create_wallet` implementation so it
  deploys and initializes only; it must not transfer the create-wallet pre-fund.
- **`ops`:** generate the faucet keypair out-of-band; store the private key in
  Secrets Manager; pass only the public key to the bootstrap tool; enable hosted
  bootstrap for devnet; deploy the faucet service with read access to the
  secret. The published seed continues to fund the local-only dev-N accounts.
- **Top-up:** reserve → faucet transfer (operator-run, or a guarded faucet admin
  path), or simply re-genesis the disposable nightly devnet. No mint exists.

### 5. CLI command flows on a remote network

Fetch flow: when a command needs the descriptor on a remote network, the CLI
loads the cache, and on miss fetches `GET <faucetUrl>/v1/network` (or a gateway
endpoint) and caches it.

- **`wallet create`** → CLI revalidates the descriptor, loads the dev-wallet
  artifact/hash, chooses a deploy nonce, and derives the deterministic
  `walletAddress` from `walletCreateSponsorAddress`, the deploy nonce, and the
  dev-wallet `contentHash`. Before any faucet request, it persists an
  in-progress wallet-create attempt keyed by `(profile, key, walletName,
  networkId, genesisHash)` with `deployNonce`, `walletAddress`,
  `walletContractHash`, `airdropClientNonce`, and `createWalletClientNonce`.
  Retries must reuse that record so interruption after a successful pre-fund
  replays the same idempotent requests instead of generating a second airdrop.
  The record is cleared after the wallet is saved, and ignored if `genesisHash`
  or `walletContractHash` changes.

  It then prepares and funds that future address:
  - on devnet, `POST /v1/create-wallet/prepare { ownerPubkey, deployNonce,
    walletContractHash, walletAddress, networkId, genesisHash, clientNonce:
    createWalletClientNonce, ownerSig }`, followed by
    `POST /v1/airdrop { address: walletAddress, rawAmount:
    walletCreatePrefundRawAmount, networkId, genesisHash, clientNonce:
    airdropClientNonce }`;
  - on mainnet/testnet without a faucet, surface the same future address and wait
    for an exchange/bridge/user transfer before continuing.

  After funding, the CLI reuses the same proof-of-possession and
  `POST /v1/create-wallet { ownerPubkey, deployNonce, walletContractHash,
  walletAddress, networkId, genesisHash, clientNonce: createWalletClientNonce,
  ownerSig }` to trigger the sponsored deploy/init transaction; it saves the
  returned address under the key's wallet name in `[profile].wallets` and marks
  it the key's **active wallet**. The CLI still does not hold the sponsor key on
  remote networks, but it owns the wallet-create orchestration.
- **`airdrop`** → `POST /v1/airdrop { address, rawAmount, networkId,
  genesisHash, clientNonce }`.
  Recipient resolution reuses the existing saved-wallet / `0x…` logic in
  `resolve_airdrop_recipient`; CLI `--amount` remains a HOG decimal and is
  converted to `rawAmount` before submission.
- **`deploy` / `call`** → bind to the key's **saved wallet, on every network**
  (not only remote). The rule: if the active key has a saved wallet, use it as
  the `invoked_address`; dev-N keys keep the existing manifest wallet path; a
  non-dev key without a saved wallet errors with "run `groundhog wallet create`"
  until `kPubkeySignature` (§7) lands. Default wallet name == key name; explicit
  selection via `--wallet` / a profile `active_wallet`. Dev-wallet `deploy` /
  `do_action` entrypoints come from the descriptor on remote (from the manifest
  on local) instead of the hardcoded `WALLET_DO_ACTION_ENTRYPOINT` constant. Auth
  stays `kSourceAddressAuthData` signed by the wallet owner's key.
- **`balance` / `view` / `storage`** → read-only RPC; already work against any
  `rpc_url`. They only need network/endpoint resolution (§1).

**Remote `--dry-run`.** Local dry-run builds a *signed* sponsor transaction
because the local seed is available; the faucet key is not on the client, so
remote faucet-backed `wallet create` / `airdrop` cannot produce a signed tx.
Remote dry-run instead prints the **faucet request plan** it would send (target
endpoints, resolved owner pubkey / recipient / amount, persisted attempt key,
and idempotency nonces) clearly marked "not submitted; faucet signs
server-side." Dry-run must never produce a live reusable `ownerSig`; it signs a
distinct dry-run domain that the faucet rejects for live submission. A future
non-reserving validate endpoint can turn this into a real server-side check.
`deploy` / `call` dry-run is unaffected — those are signed by the user's key and
still produce a signed tx on any network.

### 6. Security considerations

- **Two artifacts, kept explicitly separate.** The private operational hosted
  bootstrap metadata / runtime manifest (chuckd's output) may contain
  seed-derived account material for ops use. The public network descriptor must
  **never** contain the seed or any derivable private-key material. These are
  distinct types (§2), not two views of one type, precisely so the boundary
  can't erode.
- The faucet key lives only in the faucet service (Secrets Manager); never in
  the descriptor, the CLI, argv, or the gateway.
- `createWallet` requires owner proof-of-possession, so callers can't burn rate
  limit on keys they don't hold. Both `createWallet` and `airdrop` bind
  `networkId` and `genesisHash` into request validation and idempotency (§3),
  so retries do not double-create wallets or double-send an airdrop, and a
  nightly re-genesis cannot replay or return stale pre-reset results.
- The wallet-create flow persists its deploy nonce and faucet idempotency
  nonces before the pre-fund airdrop. A crash or rate-limit response after
  funding resumes the same attempt instead of deriving a new address or spending
  a second airdrop.
- Faucet abuse is otherwise bounded by rate limits and per-request caps; devnet
  is disposable, so the blast radius is a re-genesis.
- Cached descriptors are keyed by `networkId`; a `genesisHash` mismatch between
  cache and a live node surfaces as a clear "network reset — refresh descriptor"
  error rather than silent signing against stale metadata.

### 7. `kPubkeySignature` future track (deferred)

Implementing pubkey accounts would let a funded fresh keypair self-author its
first transaction, removing the temporary sponsored create step. The
wallet-create UX remains prefund-then-deploy/init; only the sponsor/relayer
piece changes. Scope assessment (~3–4 weeks; deep because of one decision, not
volume):

- **Localized:** the verify arm itself (~40 lines: verify ed25519 sig, derive
  address, compare); fee charging from `FreeNativeTokenBalance` already works
  for a bare funded address; deploy-from-pubkey-account already works (deploy
  address derives from the caller regardless of whether it is a contract).
- **Medium:** the auth pipeline assumes stored `AddressAuthData` at the source
  (`auth_server.cc:~188`) — needs a two-path fork (pubkey-derived vs stored).
- **Deep (the real fork):** replay protection is a per-address `Uniquifier`
  cache stored *in the account's own contract storage* (`replay_cache.h`); a
  bare pubkey account has none, so consumed nonces need a new shared structure +
  cost-model entry. This is a protocol/state-model decision and is almost
  certainly why the auth-simplification work left the arm a "placeholder only."
- **Also:** C++ has the `PubkeyAccountPreimage` struct but no derivation
  function; the Rust signing crate (`signing/src/account.rs`) is the only
  implementation. A byte-for-byte C++ match + cross-repo parity test is required.

When it lands, the funding plumbing here is unchanged — the faucet becomes pure
funding on devnet, while mainnet relies on normal incoming funds to the future
wallet address.

## Phasing

- **P0 — Network UX foundation (groundhog-rpc only).** First-class networks,
  precedence chain, `--rpc-url`/`--faucet-url`/`--network`, the distinct public
  descriptor type + fetch/cache/ETag refresh, the manifest/descriptor type
  separation, and the **general** saved-wallet binding fix (local + remote) so
  read + signed-through-wallet flows cohere against any node. Faucet not yet
  required (can target an operator-funded wallet manually for testing).
- **P1 — Usable faucet.** `hog-hosted-bootstrap` custom faucet/reserve wallet
  allocation (supplied pubkeys); chuckd's public-descriptor / sanitization path
  (emit a sanitized descriptor, or leave descriptor serving to the faucet); the
  dev-wallet `create_wallet` no-prefund contract change; the faucet HTTP
  service (with proof-of-possession + network/genesis-scoped idempotency and
  wallet-create prepare); CLI faucet
  client; ops wiring + key custody; and remote `wallet create` / `airdrop` via
  the faucet. This is the milestone that makes devnet self-serve.
- **P2 — Pubkey accounts (separate hogd track).** `kPubkeySignature`, the replay
  model, and C++/Rust derivation parity tests per §7; the sponsored create step
  collapses away, leaving faucet funding only on devnet.

## Cross-repo work summary

| Repo | Work |
|---|---|
| `groundhog-rpc` | Network model + precedence; distinct public descriptor type + fetch/cache/ETag; general saved-wallet binding fix; faucet HTTP service crate; faucet client and prefund-then-create wallet orchestration in CLI. |
| `hogd` | `hog-hosted-bootstrap` custom faucet (+ optional reserve) wallet allocation from supplied pubkeys; remove the pre-fund transfer from dev-wallet `create_wallet`. Later: `kPubkeySignature` (P2). |
| `ops` | Generate/store faucet key (Secrets Manager); enable hosted bootstrap with the faucet pubkey; deploy faucet service; devnet DNS name. |
| `chuckd` | Emit a sanitized public descriptor (or expose the `genesisHash` / dev-wallet fields a descriptor needs), since it writes the on-node manifest with `bootstrapAccountDerivation` today (`hosted_bootstrap.go:267`). Or leave descriptor serving to the faucet and keep chuckd's manifest ops-only. |

## Open questions / follow-ups

- **Reserve account in P1, or fund the faucet huge + rely on re-genesis?**
  Lean: single large faucet account for P1; add a cold reserve only if devnet
  becomes long-lived.
- **Who produces the sanitized descriptor content?** Serving is settled (faucet
  `/v1/network`, ETag). Open: does chuckd emit a sanitized descriptor at
  bootstrap commit (it already has `genesisHash` and the dev-wallet fields), and
  the faucet just serve that file? Or does the faucet assemble the descriptor
  from its own config + a read-only RPC probe? Lean: chuckd emits the sanitized
  descriptor, faucet serves it — single source of truth tied to the actual
  genesis. A gateway-served descriptor stays a future option for faucet-less
  read-only consumers.
- **`active_wallet` selection ergonomics** when a key has multiple named wallets
  (per the existing multi-wallet-per-key support) — default to key-name wallet;
  confirm the explicit-selection surface (`--wallet` vs profile field).
- **DNS / TLS for devnet** RPC and faucet endpoints (currently a raw Elastic
  IP over http).
