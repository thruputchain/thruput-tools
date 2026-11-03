# Thruput JSON-RPC

<!-- Generated from thruput-spec.json by tests/spec_snapshot.rs. Do not edit. -->

Wire format `14.2.0`. Requests use JSON-RPC 2.0 with named parameters. Fields marked `?` are optional.

This file lists all native methods. A server serves only the methods that its backends support; `getVersionInfo` lists them. For exact schemas, see [thruput-spec.json](thruput-spec.json).

## Methods

| Method | Summary |
| --- | --- |
| [`getVersionInfo`](#getversioninfo) | Describe compatibility and configured methods. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`. |
| [`getNetworkInfo`](#getnetworkinfo) | Read the signing network identity. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`. |
| [`getChainTip`](#getchaintip) | Read the committed chain tip. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`. |
| [`getChainConfig`](#getchainconfig) | Read the chain configuration and observation height. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`. |
| [`getServerLimits`](#getserverlimits) | Read configured request limits. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`. |
| [`sendTransaction`](#sendtransaction) | Submit a signed envelope for authoritative admission. |
| [`getTransactionStatus`](#gettransactionstatus) | Read retained transaction observations. |
| [`getTransactionReceipt`](#gettransactionreceipt) | Read indexed execution, or null when no receipt is available. |
| [`simulateInvocation`](#simulateinvocation) | Simulate an invocation without committing state. |
| [`simulateInvocations`](#simulateinvocations) | Simulate a batch against one state snapshot. |
| [`getStorage`](#getstorage) | Read serialized storage at an address. |
| [`getBalance`](#getbalance) | Read native token balance and its state height. |
| [`getRegisteredAuth`](#getregisteredauth) | Read registered authorization at an address. |
| [`getEvents`](#getevents) | Scan indexed events matching the supplied filters. |
| [`getTransactionsByRootAddress`](#gettransactionsbyrootaddress) | Read transactions whose root invocation targets the address, excluding internal calls and incoming transfers. |
| [`getBlockByHeight`](#getblockbyheight) | Read committed block metadata, or null when unavailable. |
| [`getTransactionsByBlock`](#gettransactionsbyblock) | Read a page of transactions in a committed block. |
| [`faucet_requestAirdrop`](#faucet_requestairdrop) | Request one faucet funding attempt. Receipt polling is a separate operation. |
| [`dev_fundAccount`](#dev_fundaccount) | Submit a local funding transfer. |
| [`dev_getPendingTransactionCount`](#dev_getpendingtransactioncount) | Read the local scheduler pending count. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`. |

### `getVersionInfo`

Describe compatibility and configured methods. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.

No parameters.

**Result:** [`VersionInfo`](#versioninfo)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32053 | `Overloaded` | `"overloaded"` |

### `getNetworkInfo`

Read the signing network identity. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.

No parameters.

**Result:** [`NetworkInfo`](#networkinfo)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32053 | `Overloaded` | `"overloaded"` |

### `getChainTip`

Read the committed chain tip. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.

No parameters.

**Result:** [`ChainTip`](#chaintip)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getChainConfig`

Read the chain configuration and observation height. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.

No parameters.

**Result:** [`ChainConfig`](#chainconfig)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getServerLimits`

Read configured request limits. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.

No parameters.

**Result:** [`ServerLimits`](#serverlimits)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32053 | `Overloaded` | `"overloaded"` |

### `sendTransaction`

Submit a signed envelope for authoritative admission.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `signedTransaction` | hex string | yes |  |

**Result:** [`SendTransactionResult`](#sendtransactionresult)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32000 | `TransactionInvalid` | `"transactionInvalid"` |
| -32001 | `TransactionRejected` | `"transactionRejected"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getTransactionStatus`

Read retained transaction observations.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `transactionHash` | [`TxHash`](#txhash) | yes |  |

**Result:** [`TransactionStatus`](#transactionstatus)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getTransactionReceipt`

Read indexed execution, or null when no receipt is available.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `transactionHash` | [`TxHash`](#txhash) | yes |  |

**Result:** [`TransactionReceipt`](#transactionreceipt) | null

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `simulateInvocation`

Simulate an invocation without committing state.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `calldata` | hex string | yes |  |
| `entrypoint` | hex string | yes |  |
| `assumedAuth` | [`AssumedAuth`](#assumedauth) | no |  |
| `caller` | [`Base58_ContractAddress`](#base58_contractaddress) | no |  |
| `gasLimit` | decimal string | no | Canonical decimal integer in the inclusive range 0 to 18446744073709551615. |
| `minStateBlockHeight` | decimal string | no | Fail-fast freshness floor; not a historical-state selector. |

**Result:** [`SimulationResult`](#simulationresult)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32012 | `StateHeightUnavailable` | `"stateHeightUnavailable"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `simulateInvocations`

Simulate a batch against one state snapshot.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `invocations` | [`Invocation`](#invocation)[] (at least 1) | yes |  |
| `minStateBlockHeight` | decimal string | no | Fail-fast freshness floor; not a historical-state selector. |

**Result:** [`SimulationResults`](#simulationresults)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32012 | `StateHeightUnavailable` | `"stateHeightUnavailable"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getStorage`

Read serialized storage at an address.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `key` | hex string | yes |  |

**Result:** [`StorageResult`](#storageresult)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getBalance`

Read native token balance and its state height.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |

**Result:** [`BalanceResult`](#balanceresult)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getRegisteredAuth`

Read registered authorization at an address.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |

**Result:** [`RegisteredAuthResult`](#registeredauthresult)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getEvents`

Scan indexed events matching the supplied filters.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `filters` | [`EventFilter`](#eventfilter)[] (1 to 5 items) | yes |  |
| `cursor` | hex string | no |  |
| `limit` | integer (uint32) | no | Omission selects the endpoint default; configured caps apply later. |
| `start` | [`HistoryStart`](#historystart) | no |  |

**Result:** [`EventPage`](#eventpage)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"`, `"invalidCursor"` |
| -32013 | `HistoryUnavailable` | `"historyUnavailable"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getTransactionsByRootAddress`

Read transactions whose root invocation targets the address, excluding internal calls and incoming transfers.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `cursor` | hex string | no |  |
| `limit` | integer (uint32) | no | Omission selects the endpoint default; configured caps apply later. |
| `start` | [`HistoryStart`](#historystart) | no |  |

**Result:** [`TransactionRefPage`](#transactionrefpage)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"`, `"invalidCursor"` |
| -32013 | `HistoryUnavailable` | `"historyUnavailable"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getBlockByHeight`

Read committed block metadata, or null when unavailable.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `blockHeight` | decimal string | yes | Canonical decimal string, from zero through `i64::MAX`. |

**Result:** [`BlockInfo`](#blockinfo) | null

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32013 | `HistoryUnavailable` | `"historyUnavailable"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `getTransactionsByBlock`

Read a page of transactions in a committed block.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `blockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `cursor` | hex string | no |  |
| `limit` | integer (uint32) | no | Omission selects the endpoint default; configured caps apply later. |
| `start` | [`BlockStart`](#blockstart) | no |  |

**Result:** [`TransactionRefPage`](#transactionrefpage)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"`, `"invalidCursor"` |
| -32013 | `HistoryUnavailable` | `"historyUnavailable"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `faucet_requestAirdrop`

Request one faucet funding attempt. Receipt polling is a separate operation.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `clientNonce` | hex string | yes |  |
| `recipient` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |

**Result:** [`AirdropAttempt`](#airdropattempt)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32030 | `FundingAttemptStopped` | `"fundingAttemptStopped"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `dev_fundAccount`

Submit a local funding transfer.

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `clientNonce` | hex string | yes |  |
| `faucet` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `rawAmount` | decimal string | yes | Positive canonical decimal amount in raw native token units. |
| `recipient` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |

**Result:** [`FundingSubmission`](#fundingsubmission)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32030 | `FundingAttemptStopped` | `"fundingAttemptStopped"` |
| -32031 | `LocalFundingUnresolved` | `"localFundingUnresolved"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

### `dev_getPendingTransactionCount`

Read the local scheduler pending count. Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.

No parameters.

**Result:** [`PendingTransactionCount`](#pendingtransactioncount)

| Code | [Error](#errors) | Data `kind` |
| --- | --- | --- |
| -32602 | `InvalidParams` | `"invalidParams"` |
| -32050 | `BackendUnavailable` | `"backendUnavailable"` |
| -32051 | `BackendTimeout` | `"backendTimeout"` |
| -32053 | `Overloaded` | `"overloaded"` |
| -32055 | `RateLimited` | `"rateLimited"` |
| -32054 | `BackendDataInvalid` | `"backendDataInvalid"` |

## Errors

The `kind` field identifies the shape of `data`.

| Code | Error | Message | Data `kind` | Data fields |
| --- | --- | --- | --- | --- |
| -32000 | `TransactionInvalid` | Invalid transaction | `"transactionInvalid"` | `admission`: `"notAttempted"`, `detail?`: [`ExecutorBoundDetail`](#executorbounddetail), `reason?`: [`TransactionInvalidReason`](#transactioninvalidreason), `transactionHash?`: [`TxHash`](#txhash) |
| -32001 | `TransactionRejected` | Transaction rejected | `"transactionRejected"` | `admission`: `"notAttempted"` \| `"rejected"`, `error?`: [`SchedulerError`](#schedulererror), `reason?`: [`TransactionRejectedReason`](#transactionrejectedreason), `transactionHash?`: [`TxHash`](#txhash) |
| -32012 | `StateHeightUnavailable` | Requested state height unavailable | `"stateHeightUnavailable"` | `observedStateBlockHeight`: decimal string, `requestedStateBlockHeight`: decimal string |
| -32013 | `HistoryUnavailable` | History unavailable | `"historyUnavailable"` | `latestIndexedBlockHeight?`: decimal string, `oldestIndexedBlockHeight?`: decimal string, `reason?`: [`HistoryUnavailableReason`](#historyunavailablereason) |
| -32030 | `FundingAttemptStopped` | Funding attempt stopped | `"fundingAttemptStopped"` | fields of [`FundingAttemptStoppedData`](#fundingattemptstoppeddata) |
| -32031 | `LocalFundingUnresolved` | Local funding unresolved | `"localFundingUnresolved"` | `cause?`: [`FundingCause`](#fundingcause), `signedTransaction`: hex string, `transactionHash`: [`TxHash`](#txhash) |
| -32050 | `BackendUnavailable` | Backend unavailable | `"backendUnavailable"` | `admission?`: [`AttemptAdmission`](#attemptadmission), `error?`: [`SchedulerError`](#schedulererror), `retryAfterMs?`: [`DecimalU64`](#decimalu64), `serviceCode?`: [`ServiceCode`](#servicecode), `transactionHash?`: [`TxHash`](#txhash) |
| -32051 | `BackendTimeout` | Backend timeout | `"backendTimeout"` | `admission?`: [`AttemptAdmission`](#attemptadmission), `retryAfterMs?`: [`DecimalU64`](#decimalu64), `transactionHash?`: [`TxHash`](#txhash) |
| -32053 | `Overloaded` | Temporary refusal | `"overloaded"` | `admission?`: [`AttemptAdmission`](#attemptadmission), `error?`: [`SchedulerError`](#schedulererror), `retryAfterMs?`: [`DecimalU64`](#decimalu64), `transactionHash?`: [`TxHash`](#txhash) |
| -32054 | `BackendDataInvalid` | Invalid backend data | `"backendDataInvalid"` | `admission?`: [`AttemptAdmission`](#attemptadmission), `observation?`: [`ReceiptObservation`](#receiptobservation), `reason?`: [`BackendDataInvalidReason`](#backenddatainvalidreason), `transactionHash?`: [`TxHash`](#txhash) |
| -32055 | `RateLimited` | Rate limited | `"rateLimited"` | `retryAfterMs?`: [`DecimalU64`](#decimalu64) |
| -32602 | `InvalidParams` | Invalid params | `"invalidCursor"` | `path?`: string, `reason?`: [`InvalidCursorReason`](#invalidcursorreason) |
| -32602 | `InvalidParams` | Invalid params | `"invalidParams"` | `path?`: string, `reason?`: [`InvalidParamsReason`](#invalidparamsreason) |

## Types

### `Admission`

A successful send acknowledges this admission attempt only.

Type: `"accepted"`

### `AirdropAttempt`

Result of one faucet call. Pending does not establish admission; callers can poll a supplied transaction hash without repeating the faucet operation.

| `attemptStatus` | Other fields |
| --- | --- |
| `"pending"` | `rawAmount`: decimal string, `recipient`: [`Base58_ContractAddress`](#base58_contractaddress), `retryAfterSeconds`: [`DecimalU64`](#decimalu64), `transactionHash?`: [`TxHash`](#txhash) |
| `"succeeded"` | `blockHeight`: decimal string, `execution`: [`AirdropExecution`](#airdropexecution), `rawAmount`: decimal string, `recipient`: [`Base58_ContractAddress`](#base58_contractaddress), `transactionHash`: [`TxHash`](#txhash) |

### `AirdropExecution`

Successful committed execution reported by the faucet. Known failed execution cannot be interpreted as a succeeded funding attempt.

| `result` | Other fields |
| --- | --- |
| `"return"` | `returnData`: hex string |

### `AscendingAnchor`

An ascending walk starts at retained beginning or an inclusive height.

| `kind` | Other fields |
| --- | --- |
| `"atBeginning"` | none |
| `"atHeight"` | `blockHeight`: decimal string |

### `AssumedAuth`

Assumed authorization for simulation; no signature is verified.

| `kind` | Other fields |
| --- | --- |
| `"pubkeySignature"` | `accountNonce`: decimal string, `pubkeyHash`: [`Base58_PublicKeyHash`](#base58_publickeyhash) |
| `"sourceAddress"` | `allowedKeys`: integer (uint32), `requiredWeight`: integer (uint32) |

### `AuthConfiguration`

Registered signer slots and threshold. Slot IDs need not be contiguous.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `requiredWeight` | integer (uint32) | yes |  |
| `signers` | [`AuthSigner`](#authsigner)[] | yes |  |

### `AuthSigner`

One registered signer; its key ID is not its array position.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `keyId` | integer (uint32) | yes |  |
| `maxWeight` | integer (uint32) | yes |  |
| `publicKey` | [`PublicKey`](#publickey) | yes |  |

### `BalanceResult`

Free native-token base units; storage collateral is separate.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `rawBalance` | decimal string | yes | Canonical nonnegative signed-128 decimal amount. |
| `stateBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |

### `Base58_ContractAddress`

Unprefixed, case-sensitive Bitcoin/Solana base58; must decode to exactly 32 bytes. Encoded length and alphabet alone do not validate decoded length.

Type: string

Constraints: length 32 to 45, pattern `^[1-9A-HJ-NP-Za-km-z]+$`

### `Base58_Ed25519PublicKey`

Unprefixed, case-sensitive Bitcoin/Solana base58; must decode to exactly 32 bytes. Encoded length and alphabet alone do not validate decoded length.

Type: string

Constraints: length 32 to 45, pattern `^[1-9A-HJ-NP-Za-km-z]+$`

### `Base58_PublicKeyHash`

Unprefixed, case-sensitive Bitcoin/Solana base58; must decode to exactly 32 bytes. Encoded length and alphabet alone do not validate decoded length.

Type: string

Constraints: length 32 to 45, pattern `^[1-9A-HJ-NP-Za-km-z]+$`

### `Base58_SHA256Hash`

Unprefixed, case-sensitive Bitcoin/Solana base58; must decode to exactly 32 bytes. Encoded length and alphabet alone do not validate decoded length.

Type: string

Constraints: length 32 to 45, pattern `^[1-9A-HJ-NP-Za-km-z]+$`

### `Base58_ValidatorID`

Unprefixed, case-sensitive Bitcoin/Solana base58; must decode to exactly 32 bytes. Encoded length and alphabet alone do not validate decoded length.

Type: string

Constraints: length 32 to 45, pattern `^[1-9A-HJ-NP-Za-km-z]+$`

### `BlockInfo`

Complete committed block metadata, preserving the existing consensus fields. Identity, coverage and request-height checks belong to the block adapter.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `blockHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `blockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `blockTimepoint` | [`DecimalTimePoint`](#decimaltimepoint) | yes |  |
| `configHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `eventsHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `networkHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `prevBlockHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `proposalTxsHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `proposer` | [`Base58_ValidatorID`](#base58_validatorid) | yes |  |
| `protocol` | integer (uint32) | yes |  |
| `storageHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `txsHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `upgrades` | hex string[] | yes |  |

### `BlockStart`

Initial exact-block position has a direction, with no range anchor.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `order` | [`IterationOrder`](#iterationorder) | yes |  |

The server rejects unknown fields.

### `ChainConfig`

Stated chain rules and optional authoritative execution provenance.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `execution` | [`ExecutionConfig`](#executionconfig) | no |  |
| `stated` | [`ConfigLimits`](#configlimits) | yes |  |

### `ChainTip`

Committed tip identity, also used for configuration observation provenance.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `blockHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `blockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |

### `ConfigLimits`

Explicit configuration values. Omission means unavailable or unspecified.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `maxAuthTokenDepth` | integer (uint32) | no |  |
| `maxBytestringLen` | integer (uint32) | no |  |
| `maxContractSizeBytes` | integer (uint32) | no |  |
| `maxCrossInvokeDepth` | integer (uint32) | no |  |
| `maxNewContractsPerTx` | integer (uint32) | no |  |
| `maxRv32CodeBytes` | integer (uint32) | no |  |
| `maxRv32MemoryBytes` | integer (uint32) | no |  |
| `maxSavepointDepth` | integer (uint32) | no |  |
| `maxSha256Buffers` | integer (uint32) | no |  |
| `maxTxGas` | [`DecimalU64`](#decimalu64) | no |  |

### `ConflictCode`

Existing storage-conflict vocabulary. These are diagnostics, not RPC errors.

Type: `"exclusiveLock"` | `"typeMismatch"` | `"valueMismatch"` | `"boundsExceeded"` | `"overflow"` | `"replayProtection"` | `"queueConsumer"` | `"capacityExceeded"` | `"insufficientFunds"` | `"other"`

### `ConflictDiagnostic`

Typed storage-conflict diagnostic, preserving the serialized key.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `code` | [`ConflictCode`](#conflictcode) | yes |  |
| `key` | hex string | yes |  |

### `ContractRejectCode`

Existing executor contract-rejection vocabulary.

Type: `"tooMany"` | `"blobTooLarge"` | `"notDeployable"` | `"duplicate"` | `"malformedArtifact"` | `"unsupportedFormat"` | `"unsupportedAbi"` | `"gasScheduleMismatch"` | `"memoryTooLarge"` | `"codeSizeInvalid"` | `"initialMemoryInvalid"` | `"digestMismatch"` | `"invalidProgram"` | `"other"`

### `DecimalTimePoint`

Canonical decimal integer in the inclusive range 0 to 18446744073709551615.

Type: decimal string

Constraints: value 0 to 18446744073709551615, length at most 20, pattern `^(0|[1-9][0-9]*)(?![\s\S])`

### `DecimalU64`

Canonical decimal integer in the inclusive range 0 to 18446744073709551615.

Type: decimal string

Constraints: value 0 to 18446744073709551615, length at most 20, pattern `^(0|[1-9][0-9]*)(?![\s\S])`

### `DescendingAnchor`

A descending walk starts at retained end or an inclusive height.

| `kind` | Other fields |
| --- | --- |
| `"atEnd"` | none |
| `"atHeight"` | `blockHeight`: decimal string |

### `EmittedEvent`

Parsed event with a typed emitter address and raw topic/data bytes.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `data` | hex string | yes |  |
| `topics` | hex string[] (at most 3) | yes |  |

### `EventFilter`

Address membership and topic predicates for one event filter.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `addresses` | [`Base58_ContractAddress`](#base58_contractaddress)[] (1 to 5 items) | yes |  |
| `topics` | [`TopicFilter`](#topicfilter) | no |  |

The server rejects unknown fields.

### `EventPage`

Event page. Empty nonterminal pages are valid; bounds are not checkpoints.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `events` | [`IdentifiedEvent`](#identifiedevent)[] | yes |  |
| `latestIndexedBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `nextCursor` | hex string \| null | yes |  |
| `oldestIndexedBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |

### `ExecutionConfig`

Limits and storage prices read with one tip. Does not guarantee later rules.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `configHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |
| `effectiveLimits` | [`ConfigLimits`](#configlimits) | yes |  |
| `observedTip` | [`ChainTip`](#chaintip) | yes |  |
| `storageCosts` | [`StorageCosts`](#storagecosts) | yes |  |

### `FailureCause`

Advisory failure from a recent proposal attempt.

| `kind` | Other fields |
| --- | --- |
| `"dbConflict"` | `conflict`: [`ConflictDiagnostic`](#conflictdiagnostic) |
| `"contractRejected"` | `code`: [`ContractRejectCode`](#contractrejectcode), `contractIndex`: integer (uint32) |
| `"other"` | none |

### `FundingSubmission`

Retained local funding submission or reconciliation evidence. Inspect its receipt before asserting that the transfer succeeded.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `rawAmount` | decimal string | yes | Positive canonical decimal raw amount, at most 340282366920938463463374607431768211455. |
| `recipient` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `signedTransaction` | hex string | yes |  |
| `transactionHash` | [`TxHash`](#txhash) | yes |  |

### `HistoryStart`

Initial range-history position. Direction determines the valid anchors.

| `order` | Other fields |
| --- | --- |
| `"ascending"` | `anchor`: [`AscendingAnchor`](#ascendinganchor) |
| `"descending"` | `anchor`: [`DescendingAnchor`](#descendinganchor) |

### `IdentifiedEvent`

Event identity within one chain instance.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `blockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `event` | [`EmittedEvent`](#emittedevent) | yes |  |
| `sequenceNumber` | integer (uint32) | yes |  |
| `transactionHash` | [`TxHash`](#txhash) | yes |  |

### `Invocation`

One batch invocation; freshness belongs to the whole batch.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `assumedAuth` | [`AssumedAuth`](#assumedauth) | no |  |
| `calldata` | hex string | yes |  |
| `caller` | [`Base58_ContractAddress`](#base58_contractaddress) | no |  |
| `entrypoint` | hex string | yes |  |
| `gasLimit` | decimal string | no | Canonical decimal integer in the inclusive range 0 to 18446744073709551615. |

The server rejects unknown fields.

### `InvocationOutput`

Invocation outcome. This does not validate a signed transaction envelope.

| `result` | Other fields |
| --- | --- |
| `"return"` | `returnData`: hex string |
| `"abort"` | `abortData`: hex string |
| `"fault"` | none |

### `InvocationResult`

One invocation's outcome within a shared-snapshot batch.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `events` | [`EmittedEvent`](#emittedevent)[] | yes |  |
| `gasUsed` | [`DecimalU64`](#decimalu64) | yes |  |
| `output` | [`InvocationOutput`](#invocationoutput) | yes |  |

### `IterationOrder`

Direction to walk a paginated event stream.

Type: `"ascending"` | `"descending"`

### `NetworkInfo`

Signing identity; this is not a chain-reset instance identifier.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `networkHash` | [`Base58_SHA256Hash`](#base58_sha256hash) | yes |  |

### `PendingTransactionCount`

This local runtime's pending pool observation, not a network-wide count.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `count` | [`DecimalU64`](#decimalu64) | yes |  |

### `PublicKey`

Implemented registered-key projection. Pubkey PQ transactions do not imply registered PQ support; an unknown scheme is unusable by a signer.

| `scheme` | Other fields |
| --- | --- |
| `"ed25519"` | `key`: [`Base58_Ed25519PublicKey`](#base58_ed25519publickey) |
| `"unknown"` | `schemeId`: integer |

### `ReceiptExecution`

Committed execution outcome. Return and abort bytes are required, even empty.

| `result` | Other fields |
| --- | --- |
| `"return"` | `returnData`: hex string |
| `"abort"` | `abortData`: hex string |
| `"fault"` | none |
| `"nonDeterministicError"` | none |
| `"failedGasSponsorship"` | none |
| `"other"` | none |

### `RecentAttemptFailure`

A recently indexed attempt failure, independent of the scheduler observation. It need not describe the same or latest attempt. Absence does not prove success, and its presence does not authorize resubmission or replacement.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `attemptBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `cause` | [`FailureCause`](#failurecause) | yes |  |

### `RegisteredAuthResult`

Registered signers at a snapshot. Null does not imply account absence and does not recover the key or scheme of a pubkey-derived account.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `address` | [`Base58_ContractAddress`](#base58_contractaddress) | yes |  |
| `configuration` | [`AuthConfiguration`](#authconfiguration) \| null | yes |  |
| `stateBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |

### `SchedulerError`

Scheduler subsystem and its unmodified numeric diagnostic.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `code` | integer | yes |  |
| `source` | [`SchedulerErrorSource`](#schedulererrorsource) | yes |  |

### `SchedulerErrorSource`

Source of a scheduler diagnostic; unfamiliar names remain unsupported.

Type: `"auth"` | `"scheduler"` | `"executor"` | `"other"`

### `SendTransactionResult`

Authoritative admission acknowledgement, not commitment or execution.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `admission` | [`Admission`](#admission) | yes |  |
| `transactionHash` | [`TxHash`](#txhash) | yes |  |

### `ServerLimits`

Configured per-endpoint request budgets, not a complete ingress-limit list.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `maxCursorBytes` | [`DecimalU64`](#decimalu64) | yes |  |
| `maxEventsPerPage` | integer (uint32) | yes |  |
| `maxSimulationCalldataBytes` | [`DecimalU64`](#decimalu64) | yes |  |
| `maxSimulationGas` | [`DecimalU64`](#decimalu64) | yes |  |
| `maxTransactionsPerPage` | integer (uint32) | yes |  |
| `simulationBatch` | [`SimulationBatchLimits`](#simulationbatchlimits) | no |  |

### `SimulationBatchLimits`

Aggregate budgets for a shared-snapshot simulation batch.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `maxBackendResponseBytes` | integer (uint32) | yes |  |
| `maxCalldataBytes` | integer (uint32) | yes |  |
| `maxGas` | [`DecimalU64`](#decimalu64) | yes |  |
| `maxInvocations` | integer (uint32) | yes |  |
| `timeoutMs` | [`DecimalU64`](#decimalu64) | yes |  |

### `SimulationResult`

Simulation at committed state H, executed with context H+1.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `events` | [`EmittedEvent`](#emittedevent)[] | yes |  |
| `gasUsed` | [`DecimalU64`](#decimalu64) | yes | Metered work, not a fee estimate. |
| `output` | [`InvocationOutput`](#invocationoutput) | yes |  |
| `stateBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |

### `SimulationResults`

Input-ordered results from independent invocations at the same snapshot.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `results` | [`InvocationResult`](#invocationresult)[] | yes |  |
| `stateBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |

### `StorageCosts`

Storage collateral in native-token base units. Each decimal amount retains the width of its protocol field; these prices are unrelated to gas burning.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `authPerKeyCost` | [`DecimalU64`](#decimalu64) | yes |  |
| `balanceCost` | [`DecimalU64`](#decimalu64) | yes |  |
| `baseContractDeploymentCost` | [`DecimalU64`](#decimalu64) | yes |  |
| `baseDataItemCost` | [`DecimalU64`](#decimalu64) | yes |  |
| `bytestringCostPerByte` | decimal string | yes | Canonical unsigned-32 decimal amount. |
| `contractDeploymentCostPerByte` | [`DecimalU64`](#decimalu64) | yes |  |
| `rcqCostPerEntry` | [`DecimalU64`](#decimalu64) | yes |  |
| `replayCacheCostPerEntry` | [`DecimalU64`](#decimalu64) | yes |  |

### `StorageItem`

Known storage projections. Unsupported variants never imply item absence.

| `kind` | Other fields |
| --- | --- |
| `"nonnegativeInt"` | `value`: decimal string |
| `"bytestring"` | `bytes`: hex string |
| `"authData"` | none |
| `"retrocausalQueue"` | none |
| `"replayCacheEntry"` | none |
| `"deployedContract"` | `contractType`: integer, `hash`: [`Base58_SHA256Hash`](#base58_sha256hash), `size`: integer (uint32) |
| `"unknown"` | `typeId`: integer |

### `StorageResult`

Storage at a committed snapshot. The item member is required, even if null.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `item` | [`StorageItem`](#storageitem) \| null | yes |  |
| `stateBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |

### `TopicFilter`

All topic positions must match; any listed value can match within a position. Omission and empty lists are equivalent wildcards.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `topic0` | hex string[] (at most 16) | no |  |
| `topic1` | hex string[] (at most 16) | no |  |
| `topic2` | hex string[] (at most 16) | no |  |

The server rejects unknown fields.

### `TransactionReceipt`

A committed receipt. Existence does not imply successful execution.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `blockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `events` | [`EmittedEvent`](#emittedevent)[] | yes |  |
| `execution` | [`ReceiptExecution`](#receiptexecution) | yes |  |
| `gasUsed` | [`DecimalU64`](#decimalu64) | yes | Metered work, not a fee or token burn. |
| `transactionHash` | [`TxHash`](#txhash) | yes |  |

### `TransactionRef`

Committed transaction reference; contains neither its body nor execution.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `blockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `transactionHash` | [`TxHash`](#txhash) | yes |  |

### `TransactionRefPage`

References for root-address history or one exact block. Request context determines ordering, membership and the meaning of the serving bounds.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `latestIndexedBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `nextCursor` | hex string \| null | yes |  |
| `oldestIndexedBlockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `transactions` | [`TransactionRef`](#transactionref)[] | yes |  |

### `TransactionStatus`

Retained transaction observations. No negative status proves nonexecution.

| `status` | Other fields |
| --- | --- |
| `"pending"` | `recentFailure?`: [`RecentAttemptFailure`](#recentattemptfailure) |
| `"committed"` | `blockHeight`: decimal string |
| `"retry"` | `error`: [`SchedulerError`](#schedulererror), `recentFailure?`: [`RecentAttemptFailure`](#recentattemptfailure) |
| `"dropped"` | `error`: [`SchedulerError`](#schedulererror), `recentFailure?`: [`RecentAttemptFailure`](#recentattemptfailure) |
| `"recentFailure"` | `attemptBlockHeight`: decimal string, `cause`: [`FailureCause`](#failurecause) |
| `"unknown"` | none |
| `"other"` | none |

### `TxHash`

Canonical transaction hash: `sha256(xdr_serialize(signed_tx.tx))`. Covers the inner Transaction body (not signatures), matching the scheduler's lookup key and the indexer's primary key.

Serializes as an unprefixed, case-sensitive base58 string encoding 32 bytes.

Type: [`Base58_SHA256Hash`](#base58_sha256hash)

### `VersionInfo`

Native wire compatibility and configured capability metadata. Diagnostic release fields are not codec selectors and are omitted when unavailable.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `bundleManifestDigest` | string | no |  |
| `commitHash` | string | no |  |
| `implementation` | string | yes |  |
| `platformVersion` | string | no |  |
| `releaseVersion` | string | no |  |
| `supportedMethods` | string[] | yes |  |
| `wireFormat` | string | yes |  |

## Error data types

These types occur only in error `data`.

### `AttemptAdmission`

Evidence for this submission attempt, never proof about an earlier attempt.

Type: `"notAttempted"` | `"rejected"` | `"unknown"`

### `BackendDataInvalidReason`

Evidence-backed refinements; omission conveys no finer classification.

Type: `"invalidEncoding"` | `"invalidCardinality"` | `"unsupportedValue"` | `"inconsistentResult"`

### `ExecutorBoundDetail`

Existing executor-bound diagnostic fields, with native numeric constraints.

| `reason` | Other fields |
| --- | --- |
| `"tooManyContracts"` | `attached`: integer (uint32), `max`: integer (uint32) |
| `"contractTooLarge"` | `index`: integer (uint32), `maxBytes`: integer (uint32), `sizeBytes`: integer (uint32) |
| `"duplicateContract"` | `firstIndex`: integer (uint32), `index`: integer (uint32) |
| `"rv32MemoryTooLarge"` | `index`: integer (uint32), `maxBytes`: integer (uint32), `memoryBytes`: integer (uint32) |
| `"rv32CodeTooLarge"` | `codeBytes`: integer (uint32), `index`: integer (uint32), `maxBytes`: integer (uint32) |
| `"gasLimitTooHigh"` | `gasLimit`: [`DecimalU64`](#decimalu64), `maxGas`: [`DecimalU64`](#decimalu64) |

### `FundingAttemptStoppedData`

Retained funding evidence. A stop does not establish nonexecution.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `receipt` | [`TransactionReceipt`](#transactionreceipt) | no |  |
| `serviceCode` | [`ServiceCode`](#servicecode) | no |  |
| `signedTransaction` | hex string | no |  |
| `transactionHash` | [`TxHash`](#txhash) | no |  |

### `FundingCause`

Recognized stop retained inside unresolved local funding evidence.

| `kind` | Other fields |
| --- | --- |
| `"fundingAttemptStopped"` | fields of [`FundingAttemptStoppedData`](#fundingattemptstoppeddata) |

### `HistoryUnavailableReason`

Evidence-backed refinements; omission conveys no finer classification.

Type: `"outsideRetention"`

### `InvalidCursorReason`

Evidence-backed refinements; omission conveys no finer classification.

Type: `"malformed"` | `"unsupportedVersion"` | `"queryMismatch"`

### `InvalidParamsReason`

Evidence-backed refinements; omission conveys no finer classification.

Type: `"missingField"` | `"unknownField"` | `"invalidType"` | `"invalidFormat"` | `"outOfRange"` | `"conflictingFields"` | `"invalidValue"`

### `ReceiptObservation`

Independently validated commitment identity; no partial receipt payload.

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `blockHeight` | decimal string | yes | Canonical decimal block height in the inclusive range 0 to 9223372036854775807. |
| `transactionHash` | [`TxHash`](#txhash) | yes |  |

### `ServiceCode`

Bounded service diagnostic, not a message or a recovery decision. The native contract limits producer codes to 128 Unicode scalar values.

Type: string

Constraints: length 1 to 128

### `TransactionInvalidReason`

Evidence-backed refinements; omission conveys no finer classification.

Type: `"invalidEncoding"` | `"malformedTransaction"` | `"invalidAuthEnvelope"` | `"transactionTooLarge"` | `"executorBoundViolation"`

### `TransactionRejectedReason`

Evidence-backed refinements; omission conveys no finer classification.

Type: `"expired"` | `"authenticationRejected"`
