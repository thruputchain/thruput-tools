use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::responses::EmittedEvent;
use thruput_json_rpc_types::responses::ReceiptExecution;
use thruput_json_rpc_types::transaction::TxHash;
use thruput_json_rpc_types::{BlockHeight, ContractAddress};

pub const TRANSFER_JSON_SCHEMA_VERSION: u32 = 5;

/// Stable JSON fields shared by every native HOG transfer outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransferReportCommon {
    pub schema_version: u32,
    pub profile: String,
    pub rpc_url: String,
    pub dry_run: bool,
    pub submitted: bool,
    pub sender: ContractAddress,
    pub sender_wallet: Option<String>,
    /// Signature scheme of the signing key: `ed25519` or `mldsa65`.
    pub signer_scheme: String,
    /// Raw encoded public key of the signing key in its scheme.
    pub signer_public_key: String,
    pub recipient_input: String,
    pub recipient: ContractAddress,
    pub token_address: ContractAddress,
    pub amount: String,
    pub raw_amount: String,
    pub tx_hash: TxHash,
}

/// Retained identity and evidence when submission or receipt waiting fails.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransferFailureReport {
    #[serde(flatten)]
    pub common: TransferReportCommon,
    pub network_hash: thruput_json_rpc_types::NetworkHash,
    pub submission: crate::native_rpc::SubmissionPhase,
    pub evidence: serde_json::Value,
}

/// Stable JSON data for a prepared native HOG transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransferDryRunReport {
    #[serde(flatten)]
    pub common: TransferReportCommon,
    #[serde(with = "thruput_json_rpc_types::decimal")]
    #[schemars(with = "thruput_json_rpc_types::decimal::Decimal<i64>")]
    pub expiry_height: i64,
    pub signed_transaction: HexBytes,
}

/// Stable JSON data for a committed native HOG transfer.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransferCommittedReport {
    #[serde(flatten)]
    pub common: TransferReportCommon,
    #[serde(with = "thruput_json_rpc_types::decimal")]
    #[schemars(with = "thruput_json_rpc_types::decimal::Decimal<BlockHeight>")]
    pub block_height: BlockHeight,
    pub execution: ReceiptExecution,
    pub events: Vec<EmittedEvent>,
    #[serde(with = "thruput_json_rpc_types::decimal")]
    #[schemars(with = "thruput_json_rpc_types::decimal::Decimal<u64>")]
    pub gas_used: u64,
    #[serde(flatten)]
    pub abort: crate::abort_json::AbortReport,
}

#[cfg(test)]
mod tests {
    use thruput_json_rpc_types::transaction::TxHash;
    use thruput_json_rpc_types::{base58::Base58, hex::HexBytes};
    use thruput_xdr::types::{ContractAddress, SHA256Hash};

    use thruput_json_rpc_types::responses::ReceiptExecution;

    use super::{
        TRANSFER_JSON_SCHEMA_VERSION, TransferCommittedReport, TransferDryRunReport,
        TransferReportCommon,
    };
    use crate::abort_json::AbortReport;

    #[test]
    fn committed_report_serializes_decoded_abort_fields() {
        let report = TransferCommittedReport {
            common: report_common(),
            block_height: thruput_json_rpc_types::BlockHeight::from(43),
            execution: ReceiptExecution::Abort {
                abort_data: HexBytes(vec![0x01]),
            },
            events: vec![],
            gas_used: 1_500,
            abort: AbortReport {
                abort_exception: Some("ERC20Error".into()),
                abort_exception_crate: Some("thruput-erc20-if".into()),
                abort_exception_version: Some("0.1".into()),
                abort_value: Some(serde_json::json!("TooLarge")),
                abort_decode_error: None,
            },
        };

        let value = serde_json::to_value(report).expect("bug: report should serialize");

        assert_eq!(value["abortException"], "ERC20Error");
        assert_eq!(value["abortExceptionCrate"], "thruput-erc20-if");
        assert_eq!(value["abortExceptionVersion"], "0.1");
        assert_eq!(value["abortValue"], "TooLarge");
        assert!(value.get("abortDecodeError").is_none());
    }

    #[test]
    fn committed_report_omits_absent_abort_fields() {
        let report = TransferCommittedReport {
            common: report_common(),
            block_height: thruput_json_rpc_types::BlockHeight::from(43),
            execution: ReceiptExecution::Return {
                return_data: HexBytes(vec![]),
            },
            events: vec![],
            gas_used: 1_500,
            abort: AbortReport::from_decode(None),
        };

        let value = serde_json::to_value(report).expect("bug: report should serialize");

        for absent in [
            "abortException",
            "abortExceptionCrate",
            "abortExceptionVersion",
            "abortValue",
            "abortDecodeError",
        ] {
            assert!(value.get(absent).is_none(), "{absent} should be omitted");
        }
    }

    fn report_common() -> TransferReportCommon {
        TransferReportCommon {
            schema_version: TRANSFER_JSON_SCHEMA_VERSION,
            profile: "local".into(),
            rpc_url: "http://127.0.0.1:9545".into(),
            dry_run: false,
            submitted: true,
            sender: Base58(ContractAddress::from([0x11; 32])),
            sender_wallet: Some("alice".into()),
            signer_scheme: "ed25519".into(),
            signer_public_key: thruput_encoding::encode(&[0x22; 32]),
            recipient_input: "alice".into(),
            recipient: Base58(ContractAddress::from([0x33; 32])),
            token_address: Base58(ContractAddress::from([0x44; 32])),
            amount: "1.5".into(),
            raw_amount: "15000000".into(),
            tx_hash: TxHash(Base58(SHA256Hash::from([0x55; 32]))),
        }
    }

    #[test]
    fn dry_run_report_serializes_stable_camel_case_shape() {
        let report = TransferDryRunReport {
            common: TransferReportCommon {
                schema_version: TRANSFER_JSON_SCHEMA_VERSION,
                profile: "local".into(),
                rpc_url: "http://127.0.0.1:9545".into(),
                dry_run: true,
                submitted: false,
                sender: Base58(ContractAddress::from([0x11; 32])),
                sender_wallet: Some("alice".into()),
                signer_scheme: "ed25519".into(),
                signer_public_key: thruput_encoding::encode(&[0x22; 32]),
                recipient_input: "alice".into(),
                recipient: Base58(ContractAddress::from([0x33; 32])),
                token_address: Base58(ContractAddress::from([0x44; 32])),
                amount: "1.5".into(),
                raw_amount: "15000000".into(),
                tx_hash: TxHash(Base58(SHA256Hash::from([0x55; 32]))),
            },
            expiry_height: 123,
            signed_transaction: HexBytes(vec![0xaa, 0xbb]),
        };

        let value = serde_json::to_value(report).expect("bug: report should serialize");

        assert_eq!(value["schemaVersion"], TRANSFER_JSON_SCHEMA_VERSION);
        assert_eq!(value["dryRun"], true);
        assert_eq!(value["submitted"], false);
        assert_eq!(value["senderWallet"], "alice");
        assert_eq!(value["recipientInput"], "alice");
        assert_eq!(value["rawAmount"], "15000000");
        assert_eq!(value["expiryHeight"], "123");
        assert_eq!(value["signedTransaction"], "0xaabb");
        for absent in [
            "height",
            "status",
            "events",
            "returnData",
            "abortData",
            "confirmation",
            "transactionStatus",
        ] {
            assert!(value.get(absent).is_none(), "{absent} should be omitted");
        }
    }
}
