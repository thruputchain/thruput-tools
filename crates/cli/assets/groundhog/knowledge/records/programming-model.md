+++
schema_version = 1
id = "groundhog.programming-model"
owner = "groundhog-rpc"
kind = "concept"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 20
+++
## Programming Model

All transactions considered for a block read the same frozen committed
snapshot. A transaction does not see another transaction's writes from that
block. Contract execution produces typed storage deltas rather than a bag of
arbitrary final writes.

After the root call finishes, the runtime checks whether its complete delta
batch can coexist with transactions already admitted to the proposal.
Compatible deltas can land together. A transaction with incompatible deltas is
excluded and returned to the mempool. Do not add retry loops inside contract
code to handle this exclusion.

Gas meters computation and bounds cross-contract call depth. It has no token
price and is not burned. Choose storage operations for correct conflict
semantics, not gas-price optimisation.

Declared root gas reserves scheduling capacity against the sender's free HOG;
target storage funding is separate. Use workload-tested budgets. A sufficient
first HOG credit can fund a recipient's balance-item deposit; a small first
credit may fail. Do not hard-code a universal deposit or require an airdrop for
every new address.

### Start with Balance, Then Choose by Behaviour

For contract authors, `Balance` is the practical starting point for ordinary
nonnegative amounts such as token balances, allowances, supply, deposits, and
credits. It is backed by Groundhog's mergeable integer storage, but presents a
zero-defaulting `u128` API that prevents negative values and absolute
replacement. Do not use raw `NNInt` for a balance unless the data model
genuinely needs its lower-level signed-delta or explicit-base operations.

- `Balance` is the ordinary type for balances, allowances, and supply. It uses
  `u128`, defaults missing state to zero, exposes increases and decreases, and
  does not expose absolute replacement or negative values.
- `NNInt` is the lower-level integer type for counters and state that needs
  signed deltas or an explicit base. Prefer `strict_inc` when execution must
  observe an immediate bound failure. Use `inc` only when deferring the bound
  check to reservation is intentional.
- `Map<K, V>` gives each key an independent conflict domain. The value type
  determines same-key behaviour.
- `Value<T>` stores typed configuration or other infrequently replaced values.
  It is usually wrong for hot counters or balances.
- `Vector<S>` serializes changes to its length. Do not assume concurrent pushes
  form a scalable shared log.
- `Bytestring` is raw byte storage for lower-level or infrequently replaced
  data. Prefer typed `Value<T>` when a typed value is available.
- `RCQ<T>` is the SDK's retro-causal queue. Within a block, submission order
  determines whether a consume sees another transaction's insert, and
  concurrent consumes conflict. Use it only when those semantics match the
  application.

For a token, use `Map<ContractAddress, Balance>` for per-account balances. For
a counter, use `NNInt`. Do not implement either as read, calculate, and replace
through `Value` or `Bytestring`.
