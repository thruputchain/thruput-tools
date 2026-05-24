# Solana Ecosystem: DX Analysis for Groundhog

**Archive note:** Historical competitive analysis from 2026-04-08. This is not
the current CLI roadmap; use [`../../cli/roadmap.md`](../../cli/roadmap.md) for
active planning.

Analysis of the Solana developer experience — Anchor framework, Solana
CLI, solana-sdk (Rust), and @solana/kit (TypeScript) — with lessons
for the Groundhog CLI and SDK. Based on source-level review of
`solana-sdk` and `kit` repos plus ecosystem knowledge.

**Date:** 2026-04-08

---

## The Solana Developer Stack

Solana's tooling is a multi-layer stack, unlike Foundry's monolithic
approach:

| Layer | Tool | Groundhog Equivalent |
|-------|------|---------------------|
| **Framework** | Anchor (macros, account validation, IDL) | Groundhog SDK macros (`#[contract]`, `#[endpoint]`) |
| **CLI** | `solana` CLI + `anchor` CLI | `groundhog` CLI |
| **Build** | `cargo-build-sbf` (SBF/WASM target) | `groundhog build` (WASM target) |
| **Local node** | `solana-test-validator` | Nothing yet (Phase 2) |
| **Rust SDK** | `solana-sdk` (~117 crates) | `sdk_rs` |
| **JS/TS SDK** | `@solana/kit` (~57 packages) | Not started |
| **Testing** | `solana-program-test`, LiteSVM, Bankrun | `native_exec` TestEnvironment |
| **IDL/Codegen** | Anchor IDL + Codama renderers | Not started |

### Key Insight: Solana Has Two CLIs

Anchor and the Solana CLI serve different purposes:

- **`solana` CLI** — low-level node interaction: config, keygen,
  airdrop, deploy, transfer, balance, logs, validator management.
  Think of it as `cast` + `anvil` from Foundry.

- **`anchor` CLI** — project-level workflow: init, build, test,
  deploy, IDL management, workspace config. Think of it as `forge`
  from Foundry.

Groundhog's `groundhog` CLI is trying to be both. That's fine at
this stage (smaller surface area), but worth noting as the command
set grows.

---

## What Anchor Gets Right

### 1. Macros That Enforce Correctness

Anchor's macro system is its defining feature:

```rust
#[program]
mod my_program {
    pub fn initialize(ctx: Context<Initialize>, data: u64) -> Result<()> {
        ctx.accounts.my_account.data = data;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = user, space = 8 + 8)]
    pub my_account: Account<'info, MyAccount>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[account]
pub struct MyAccount {
    pub data: u64,
}
```

What the macros do automatically:
- **Account validation** — `#[account(init, payer, space)]`
  generates checks for ownership, discriminator, size, rent
- **Signer verification** — `Signer<'info>` enforces signature
- **Deserialization** — accounts are automatically deserialized
  from raw bytes into typed structs
- **Discriminator** — 8-byte discriminator prefix prevents
  instruction confusion attacks
- **Space calculation** — explicit, but `#[account]` generates
  the serialization

**Groundhog parallel:** The SDK's `#[contract]` and
`declare_mock_contract!()` macros serve a similar role. The
question is whether Groundhog's macros provide the same level of
"pit of success" safety that Anchor does.

### 2. IDL Generation and Client Codegen

Anchor generates an **IDL (Interface Definition Language)** JSON
file from your Rust program. This IDL:
- Describes all instructions, accounts, types, events, and errors
- Is used to generate TypeScript clients automatically
- Enables generic explorers and tools to interact with any program
- Lives on-chain (uploaded with `anchor idl init`)

The IDL flow: `anchor build` → IDL JSON → Codama renderer →
TypeScript client. One command, and your frontend has typed
bindings.

**Groundhog opportunity:** This is a clear gap. The SDK should
generate a machine-readable contract spec (ABI equivalent) from
the `#[contract]` macro, and the CLI/SDK should be able to
generate typed clients from it. `groundhog-spec-tool` exists in
the SDK repo — this may already be in progress.

### 3. Anchor.toml — Project Configuration

```toml
[features]
seeds = false
skip-lint = false

[programs.localnet]
my_program = "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS"

[programs.devnet]
my_program = "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS"

[registry]
url = "https://api.apr.dev"

[provider]
cluster = "Localnet"
wallet = "~/.config/solana/id.json"

[scripts]
test = "yarn run ts-mocha -p ./tsconfig.json -t 1000000 tests/**/*.ts"
```

Key pattern: **per-cluster program addresses**. The same project
deploys to different addresses on localnet vs devnet vs mainnet.
Configuration tells the framework which to use.

**Groundhog parallel:** `groundhog.toml` profiles should support
per-profile contract addresses.

### 4. anchor test — The Full Loop

`anchor test` does everything:
1. Starts `solana-test-validator` (local node)
2. Builds the program (`anchor build`)
3. Deploys to localnet
4. Runs the test script (TypeScript by default)
5. Shuts down the validator

One command. Zero manual setup. The test script imports the
auto-generated client and interacts with the deployed program.

**Groundhog status:** `groundhog test` runs executor-backed WASM tests,
and `groundhog test --integration` now covers the one-command local-node
path for the first single-contract slice: start hogd-dev, deploy, run
`tests/integration.rs` with RPC environment variables, then shut down.

---

## solana-test-validator: The Local Node

Solana's equivalent of Foundry's Anvil:

```bash
solana-test-validator                    # start local node
solana-test-validator --reset            # fresh state
solana-test-validator --ledger ./test-ledger  # persist state
solana-test-validator --clone <address>  # clone account from mainnet
solana-test-validator --url <cluster>    # fork from cluster
solana-test-validator --bpf-program <addr> <path>  # preload program
solana-test-validator --account <addr> <json>       # preload account
solana-test-validator --slots-per-epoch 32          # fast epochs
solana-test-validator --ticks-per-slot 8            # fast blocks
```

Key capabilities:
- **Account cloning** from mainnet/devnet (like Foundry's forking)
- **Program preloading** — deploy programs at startup without
  transactions
- **State persistence** via ledger directory
- **Time manipulation** — configurable slot/tick timing
- **Warp slot** — `solana-test-validator --warp-slot <slot>`

**Comparison to Anvil:**

| Feature | Anvil | solana-test-validator |
|---------|-------|---------------------|
| Start time | ~1s | ~3-5s |
| State persistence | JSON dump/load | Ledger directory |
| Forking | Any RPC, any block | Account cloning |
| Auto-mining | Yes | Yes (default) |
| Time manipulation | evm_increaseTime | Warp slot |
| Pre-funded accounts | Deterministic | Airdrop needed |
| Programmatic control | Full RPC | Limited |

`solana-test-validator` is heavier than Anvil (it's a full
validator) but more realistic. The main DX friction is startup
time and the need to airdrop SOL to test accounts.

**Groundhog opportunity:** Since Groundhog has instant finality and
no gas fees, the local node can be even lighter than both. No
airdrop step needed. Pre-funded accounts by default.

---

## solana-sdk: The Modular Rust SDK

The `solana-sdk` workspace has ~117 crates, decomposed from the
original monolithic `solana-sdk` crate. Key crates:

| Crate | Purpose |
|-------|---------|
| `solana-program` | On-chain program primitives |
| `solana-account` | Account types |
| `solana-pubkey` | Public key / address |
| `solana-keypair` | Key generation |
| `solana-instruction` | Instruction building |
| `solana-transaction` | Transaction construction |
| `solana-message` | Transaction message encoding |
| `solana-signature` | Signature types |
| `solana-signer` | Signer trait |
| `solana-cpi` | Cross-program invocation |
| `solana-borsh` | Borsh serialization |
| `solana-hash` | Hash types |
| `solana-clock` | Slot/epoch timing |
| `solana-rent` | Rent calculation |
| `solana-sysvar` | System variables |
| `solana-system-interface` | System program interface |

**The decomposition lesson:** Solana's SDK was originally one giant
crate. Breaking it into ~117 focused crates improved compile times,
allowed on-chain programs to depend on only what they need, and
made the dependency graph explicit. They did this migration over
2024-2025.

**Groundhog parallel:** The SDK already has a multi-crate
structure (native_exec, macros, groundhog-sys, etc.). The key
lesson is: keep the on-chain contract dependencies minimal
(affects WASM size) and separate from the off-chain/test
dependencies.

---

## @solana/kit: The Frontend SDK

Kit (formerly @solana/web3.js 2.0) is a complete rewrite of the
Solana JavaScript SDK. It has ~57 packages and zero third-party
dependencies.

### Key Design Decisions

**1. Functional, not object-oriented**

Old web3.js:
```ts
const connection = new Connection("http://localhost:8899");
const balance = await connection.getBalance(publicKey);
```

Kit:
```ts
const rpc = createSolanaRpc("http://localhost:8899");
const balance = await rpc.getBalance(address).send();
```

No classes. Pure functions. Everything tree-shakeable.

**2. pipe() for transaction building**

```ts
const message = pipe(
    createTransactionMessage({ version: 0 }),
    tx => setTransactionMessageFeePayer(signer.address, tx),
    tx => setTransactionMessageLifetimeUsingBlockhash(blockhash, tx),
    tx => appendTransactionMessageInstruction(transferIx, tx),
);
```

Immutable data, functional composition, type inference carries
through the entire pipeline. Each function narrows the type.

**3. Branded/nominal types**

```ts
type Address = Brand<string, "Address">;
type Signature = Brand<string, "Signature">;
type Lamports = Brand<bigint, "Lamports">;
```

You can't accidentally pass a signature where an address is
expected, even though both are strings at runtime. Zero runtime
cost.

**4. Composable transports**

The RPC client is built from composable layers:
- Transport (HTTP, custom)
- Round-robin, failover, rate-limited transports
- Subscription channels (WebSocket, custom)

Developers can swap any layer. The default works out of the box.

**5. Instruction plans and transaction planning**

Kit has a sophisticated system for automatically splitting
complex operations across multiple transactions when they exceed
Solana's transaction size limit:

```ts
const planner = createTransactionPlanner({
    createTransactionMessage: () => pipe(
        createTransactionMessage({ version: 0 }),
        msg => setTransactionMessageFeePayerSigner(signer, msg),
    ),
});
const plan = await planner(instructionPlan);
```

This handles sequential vs parallel instruction ordering,
message packing, and automatic transaction splitting.

**6. Coded errors with production stripping**

```ts
// Development: "Transaction is missing signatures for addresses: [...]"
// Production: "Error #42" (message stripped for bundle size)
// Decode: npx @solana/errors decode -- 42
```

All error messages have numeric codes. In production builds,
human-readable messages are removed. A CLI decoder restores them.

### Statistics (from README)

| Metric | web3.js 1.x | Kit | Change |
|--------|-------------|-----|--------|
| Full library size | 81 KB | 57.5 KB | -29% |
| Transfer app bundle | 111 KB | 23.9 KB | **-78%** |
| Crypto performance | 700 ops/s | 7000 ops/s | **+900%** |

**Groundhog lesson:** When the time comes for a JS/TS client SDK,
Kit's architecture is the gold standard:
- Functional composition, not classes
- Tree-shakeable from day one
- Branded types for safety
- Composable transports
- Automatic transaction planning

But Groundhog doesn't need this yet. The RPC gateway's
`#[rpc(client)]` trait already generates a Rust client. A JS
client SDK is a post-launch concern.

---

## Solana's DX Journey: What Changed

### Before (2022-2023 pain points)
- Monolithic `solana-sdk` crate — slow compiles, huge dependency
  tree for on-chain programs
- Monolithic `@solana/web3.js` — huge bundle, can't tree-shake
- `solana-program-test` was slow and flaky
- BPF target with custom LLVM fork — fragile build toolchain
- Account model confusion — rent, PDAs, space calculation are
  genuinely hard concepts for new developers

### After (2024-2026 improvements)
- **SDK decomposition** — 117 focused crates, faster compiles
- **Kit rewrite** — functional, tree-shakeable, zero deps
- **LiteSVM** — fast in-process Solana VM for testing (no
  validator needed). Similar concept to Foundry's in-process EVM.
- **SBF target** — standardized Solana Bytecode Format, more
  stable than BPF
- **Agave validator** — Anza (spun out from Solana Labs) now
  maintains the validator. More focused development.
- **Codama** — Universal IDL renderer, generates clients for
  any language from Anchor IDL or Shank annotations
- **Plattools** — Pre-built Solana platform tools, faster setup
- **Better docs** — Solana Cookbook, updated developer guides

### What's still hard
- Account model is inherently complex (rent, PDAs, space)
- Transaction size limit (1232 bytes) forces instruction splitting
- Compute unit estimation is guesswork
- Local validator is heavy compared to Anvil
- Two CLIs (solana + anchor) with overlapping concerns
- Program upgrades require careful authority management

---

## Patterns Groundhog Should Adopt

### 1. One-Command Test Loop (from Anchor)

`anchor test` = build + start local node + deploy + run tests +
shutdown. Groundhog's v1 equivalent is `groundhog test --integration`
for `tests/integration.rs`; the remaining parity work is richer output,
watch/rerun support, persistence-aware runs, and multi-contract
orchestration.

### 2. IDL / Contract Spec Generation (from Anchor)

The SDK should generate a machine-readable contract specification
from `#[contract]` macros. This enables:
- Auto-generated client SDKs (Rust and eventually JS/TS)
- Block explorer interaction
- Generic `groundhog call` without knowing the ABI in advance
- `groundhog inspect <address>` — show endpoints, types, events

The `groundhog-spec-tool` in the SDK repo may already be heading
here. Wire it into the CLI.

### 3. Per-Profile Contract Addresses (from Anchor.toml)

```toml
# groundhog.toml
[profile.local]
rpc_url = "http://localhost:9545"
counter = "0x1234..."

[profile.testnet]
rpc_url = "https://testnet.groundhog.network"
counter = "0xabcd..."
```

### 4. Account/State Cloning (from solana-test-validator)

When the local node exists, `groundhog node --clone <address>`
should be able to clone contract state from testnet. This
lets developers test against real data locally.

### 5. Functional Client SDK Design (from Kit)

When it's time for a JS/TS client SDK:
- Functional composition, not classes
- `pipe()` for transaction building
- Tree-shakeable from day one
- Branded types for addresses, hashes, amounts
- Composable RPC transports

### 6. Coded Error System (from Kit)

Kit's error system (numeric codes, production-strippable messages,
CLI decoder) is elegant. The RPC gateway's `api-types/src/error.rs`
already uses numeric error code ranges. Extend this pattern to
the CLI and SDK.

---

## Where Groundhog Has Natural Advantages

### vs Solana's Complexity

| Solana Pain Point | Groundhog Reality |
|-------------------|-------------------|
| Account model (rent, PDAs, space calculation) | Simpler storage model (NNInt, Map, Bytestring, RCQ) |
| Transaction size limit (1232 bytes) | No artificial limit (TBD but likely larger) |
| Compute unit estimation | Storage tokens, not gas auctions |
| Two CLIs with overlapping concerns | One CLI |
| Program upgrade authority management | TBD but can be simpler |
| Slow test validator startup | Can be instant (in-memory executor) |
| Must airdrop SOL for testing | No gas fees — pre-funded by default |

### vs Solana's Testing

Solana has three testing approaches with different tradeoffs:
1. `solana-program-test` — in-process BanksClient (fast, limited)
2. LiteSVM — in-process VM (faster, newer)
3. `solana-test-validator` — full validator (slow, realistic)

Groundhog can unify all three levels with one executor
(`native_exec`) running in different modes:
- **In-process** — `TestEnvironment` for unit tests (already works)
- **Local RPC** — same executor behind a JSON-RPC interface
- **Remote** — real network

Same semantics at every level. This is a genuine differentiator
if executed well.

---

## Impact on Groundhog Roadmap

### Reinforces existing priorities

The Solana analysis **strongly reinforces** the Phase 2 priority
(local dev node) and adds emphasis to:

1. **Contract spec / IDL generation** — should be in Phase 1 or
   early Phase 2. Needed for `groundhog call` to work without
   hardcoded endpoint IDs, and for future client SDK generation.

2. **One-command integration test** — Phase 3 testing DX should
   include `groundhog test --integration` that orchestrates the
   full deploy-and-call loop against the local node.

3. **Per-profile addresses** — `groundhog.toml` should support
   contract address mappings per profile from day one.

### New items to consider

4. **`groundhog inspect`** — given a deployed address, show the
   contract's spec (endpoints, types, events). Anchor has
   `anchor idl fetch`. This is valuable for debugging and
   exploration.

5. **State cloning for local node** — `groundhog node --clone
   <addr> --from testnet` to replicate contract state locally.
   Not Phase 2 but a natural Phase 3 addition.

6. **Client SDK codegen** — eventually, generate typed clients
   from the contract spec. Start with Rust (already have the
   `*Client` types in templates), later add TypeScript. This
   is post-launch but should influence the spec format design.

### Additional details from deep-dive

7. **`groundhog logs`** — Solana's `solana logs <program-id>`
   streams transaction logs in real time, filtered by program.
   This is one of their most-used debugging tools. When the
   local node exists, streaming contract events and execution
   traces should be first-class.

8. **Reproducible builds and verification from day one** —
   Solana added `solana-verify` and Anchor added
   `anchor verify` after the fact. `groundhog build` should
   produce deterministic WASM, and `groundhog verify` should
   confirm a deployment matches source. Easier to do now than
   retrofit.

9. **Don't fragment the framework landscape** — Solana's
   proliferation of frameworks (Native vs Anchor vs Seahorse
   vs Poseidon) creates genuine confusion about which to learn.
   Groundhog having one SDK, one macro system, one test pattern
   is a significant advantage worth protecting.

### Does NOT change

- Phase 0 and Phase 1 are correct as-is
- The local node architecture (same executor, different modes)
  is validated by Solana's pain with three different test levels
- The decision to keep one CLI (not splitting into separate
  binaries) is correct for now
