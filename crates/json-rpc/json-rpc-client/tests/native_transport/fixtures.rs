//! Independent JSON examples: the catalog supplies types, not expected wire data.
use serde_json::{Value, json};

pub fn address() -> String {
    "11111111111111111111111111111111".into()
}

pub fn invocation() -> Value {
    json!({"address": address(), "entrypoint": "0x0102030405060708", "calldata": "0x"})
}

pub fn request(method: &str) -> Value {
    match method {
        "sendTransaction" => json!({"signedTransaction": "0x0102"}),
        "getTransactionStatus" | "getTransactionReceipt" | "getTransactionInput" => {
            json!({"transactionHash": address()})
        }
        "simulateInvocation" => invocation(),
        "simulateInvocations" => json!({"invocations": [invocation()]}),
        "getStorage" => json!({"address": address(), "key": "0x0102"}),
        "getBalance" | "getRegisteredAuth" | "getContractDeployment" => {
            json!({"address": address()})
        }
        "getContractArtifact" => json!({"hash": address()}),
        "getEffectiveContract" => json!({"address": address(), "context": "root"}),
        "getEvents" => {
            json!({"filters": [{"addresses": [address()]}], "start": {"order": "ascending", "anchor": {"kind": "atBeginning"}}})
        }
        "getTransactionsByRootAddress" => {
            json!({"address": address(), "start": {"order": "descending", "anchor": {"kind": "atHeight", "blockHeight": "9007199254740993"}}})
        }
        "getBlockByHeight" => json!({"blockHeight": "812"}),
        "getTransactionsByBlock" => json!({"blockHeight": "812", "start": {"order": "ascending"}}),
        "faucet_requestAirdrop" => json!({"recipient": address(), "clientNonce": "0x0102"}),
        "dev_fundAccount" => {
            json!({"recipient": address(), "rawAmount": "340282366920938463463374607431768211455", "clientNonce": "0x0102", "faucet": address()})
        }
        "getVersionInfo"
        | "getNetworkInfo"
        | "getChainTip"
        | "getChainConfig"
        | "getServerLimits"
        | "dev_getPendingTransactionCount" => json!({}),
        _ => panic!("no request fixture for {method}"),
    }
}

pub fn response(method: &str) -> Value {
    let event = json!({"address": address(), "topics": [format!("0x{}", "01".repeat(32))], "data": "0x0102"});
    let output = json!({"result": "return", "returnData": "0x0102"});
    match method {
        "getTransactionInput" => json!({"status": "unavailable"}),
        "getContractDeployment" => {
            json!({"address": address(), "stateBlockHeight": "42", "deployment": null})
        }
        "getContractArtifact" => json!({"hash": address(), "contractXdr": "0x0000000300000000"}),
        "getEffectiveContract" => {
            json!({"address": address(), "context": "root", "stateBlockHeight": "42", "selection": null})
        }
        "getVersionInfo" => {
            json!({"wireFormat": "14.0.0", "supportedMethods": ["incorrectServiceValue"], "implementation": "transport-test"})
        }
        "getNetworkInfo" => json!({"networkHash": address()}),
        "getChainTip" => json!({"blockHeight": "9007199254740993", "blockHash": address()}),
        "getChainConfig" => json!({"stated": {"maxCrossInvokeDepth": 64}}),
        "getServerLimits" => {
            json!({"maxSimulationGas": "18446744073709551615", "maxSimulationCalldataBytes": "4096", "maxEventsPerPage": 100, "maxTransactionsPerPage": 100, "maxCursorBytes": "4096"})
        }
        "sendTransaction" => json!({"transactionHash": address(), "admission": "accepted"}),
        "getTransactionStatus" => json!({"status": "committed", "blockHeight": "9007199254740993"}),
        "getTransactionReceipt" => {
            json!({"transactionHash": address(), "blockHeight": "9007199254740993", "execution": output, "events": [event], "gasUsed": "18446744073709551615"})
        }
        "simulateInvocation" => {
            json!({"stateBlockHeight": "9007199254740993", "output": output, "events": [event], "gasUsed": "18446744073709551615"})
        }
        "simulateInvocations" => {
            json!({"stateBlockHeight": "9007199254740993", "results": [{"output": output, "events": [event], "gasUsed": "18446744073709551615"}]})
        }
        "getStorage" => {
            json!({"stateBlockHeight": "9007199254740993", "item": {"kind": "bytestring", "bytes": "0x0102"}})
        }
        "getBalance" => {
            json!({"address": address(), "stateBlockHeight": "9007199254740993", "rawBalance": "170141183460469231731687303715884105727"})
        }
        "getRegisteredAuth" => {
            json!({"address": address(), "stateBlockHeight": "9007199254740993", "configuration": null})
        }
        "getEvents" => {
            json!({"events": [{"transactionHash": address(), "blockHeight": "812", "sequenceNumber": 0, "event": event}], "nextCursor": "0x0102", "oldestIndexedBlockHeight": "0", "latestIndexedBlockHeight": "9007199254740993"})
        }
        "getTransactionsByRootAddress" | "getTransactionsByBlock" => {
            json!({"transactions": [{"transactionHash": address(), "blockHeight": "812"}], "nextCursor": null, "oldestIndexedBlockHeight": "0", "latestIndexedBlockHeight": "9007199254740993"})
        }
        "getBlockByHeight" => {
            json!({"blockHash": address(), "protocol": 4, "networkHash": address(), "configHash": address(), "proposer": address(), "blockTimepoint": "1700000000000000000", "blockHeight": "812", "txsHash": address(), "storageHash": address(), "eventsHash": address(), "prevBlockHash": address(), "proposalTxsHash": address(), "upgrades": ["0x0102"]})
        }
        "faucet_requestAirdrop" => {
            json!({"attemptStatus": "pending", "recipient": address(), "rawAmount": "1", "transactionHash": address(), "retryAfterSeconds": "1"})
        }
        "dev_fundAccount" => {
            json!({"recipient": address(), "rawAmount": "1", "transactionHash": address(), "signedTransaction": "0x0102"})
        }
        "dev_getPendingTransactionCount" => json!({"count": "18446744073709551615"}),
        _ => panic!("no result fixture for {method}"),
    }
}
