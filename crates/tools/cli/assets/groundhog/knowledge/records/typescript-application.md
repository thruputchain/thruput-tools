+++
schema_version = 1
id = "groundhog.typescript-application"
owner = "groundhog-rpc"
kind = "workflow"
audience = ["coding-agent"]
components = ["groundhog-cli"]
bootstrap_order = 50
+++
## TypeScript Application Path

Use the package's public exports and generated binding, not files below its
`src/` directory or dependencies below `node_modules`.

The application flow is:

1. Create a cluster-aware Groundhog client.
2. Create or load an account and signer appropriate for that network.
3. Bind the account to a wallet client.
4. Build an application invocation with the generated method descriptor.
5. Execute the invocation through the wallet.
6. Wait for the receipt.
7. Decode the wallet root outcome, then decode child return data when needed.

Generated methods sit under their interface's major, which the app selects once:

```ts
import { Counter as CounterVersions } from "./counter";
import { DefaultWallet as DefaultWalletVersions } from "@groundhogchain/client/contracts";

const Counter = CounterVersions.v0_1;
const DefaultWallet = DefaultWalletVersions.v0_1;
```

These constants hold only values, so name generated types by their full path,
as in `CounterVersions.v0_1.GetResult`.

A read-only call goes through the generated descriptor:

```ts
const result = await Counter.get.view({
  client,
  contract,
  caller,
});
```

A state-changing call is built as an invocation and executed through a wallet:

```ts
const invocation = Counter.increment.invocation(contract, callGas, {
  amount: 1n,
});

const txHash = await wallet.execute({
  invocation,
  gasLimit: walletGas,
});

const receipt = await wallet.waitForTransactionReceipt(txHash);
```

Do not submit `Counter.increment` directly as the transaction root. A pubkey
account normally uses DefaultWallet. Local bootstrap source-address accounts
normally use DevWallet with the local-development adapter. Custom smart
wallets provide their own root invocation adapter.

For a DefaultWallet transaction, first decode the root wallet receipt:

```ts
const root = DefaultWallet.execute.decodeReceiptOutcome(receipt);
```

If the wallet returned raw child values, select the child corresponding to the
application invocation and decode it with that method's generated
`decodeReturn`. The receipt does not contain enough TypeScript method identity
for a generic decoder to choose the child descriptor automatically.

Submission, commitment, and successful contract execution are separate facts.
Check the typed receipt outcome before treating a state-changing operation as
successful. For dependent flows such as an airdrop followed by a transfer,
verify the first execution result before submitting the second operation.
