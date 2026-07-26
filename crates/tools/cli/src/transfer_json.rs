use groundhog_json_rpc_types::events::EmittedEvent;
use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::receipt::ExecStatus;
use groundhog_json_rpc_types::transaction::TransactionStatus;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_json_rpc_types::{BlockHeight, ContractAddress, Ed25519PublicKey};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const TRANSFER_JSON_SCHEMA_VERSION: u32 = 1;

/// Stable JSON data for a prepared native HOG transfer.
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
    pub signer_public_key: Ed25519PublicKey,
    pub recipient_input: String,
    pub recipient: ContractAddress,
    pub token_address: ContractAddress,
    pub amount: String,
    pub raw_amount: String,
    pub tx_hash: TxHash,
}

/// Stable JSON data for a prepared native HOG transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransferDryRunReport {
    #[serde(flatten)]
    pub common: TransferReportCommon,
    pub expiry_height: i64,
    pub signed_transaction: HexBytes,
}

/// Stable JSON data for a committed native HOG transfer.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransferCommittedReport {
    #[serde(flatten)]
    pub common: TransferReportCommon,
    pub height: BlockHeight,
    pub status: ExecStatus,
    pub events: Vec<EmittedEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_data: Option<HexBytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_data: Option<HexBytes>,
}

/// Confirmation classification when no matching receipt is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TransferConfirmation {
    Invalid,
    Unavailable,
}

/// Stable JSON data for a submitted transfer without a matching receipt.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransferUnconfirmedReport {
    #[serde(flatten)]
    pub common: TransferReportCommon,
    pub confirmation: TransferConfirmation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_status: Option<TransactionStatus>,
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::hex::{Hex0x, HexBytes};
    use groundhog_json_rpc_types::transaction::TxHash;
    use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};

    use super::{TRANSFER_JSON_SCHEMA_VERSION, TransferDryRunReport, TransferReportCommon};

    #[test]
    fn dry_run_report_serializes_stable_camel_case_shape() {
        let report = TransferDryRunReport {
            common: TransferReportCommon {
                schema_version: TRANSFER_JSON_SCHEMA_VERSION,
                profile: "local".into(),
                rpc_url: "http://127.0.0.1:9545".into(),
                dry_run: true,
                submitted: false,
                sender: Hex0x(ContractAddress::from([0x11; 32])),
                sender_wallet: Some("alice".into()),
                signer_public_key: Hex0x(Ed25519PublicKey::from([0x22; 32])),
                recipient_input: "alice".into(),
                recipient: Hex0x(ContractAddress::from([0x33; 32])),
                token_address: Hex0x(ContractAddress::from([0x44; 32])),
                amount: "1.5".into(),
                raw_amount: "15000000".into(),
                tx_hash: TxHash(Hex0x(SHA256Hash::from([0x55; 32]))),
            },
            expiry_height: 123,
            signed_transaction: HexBytes(vec![0xaa, 0xbb]),
        };

        let value = serde_json::to_value(report).expect("bug: report should serialize");

        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["dryRun"], true);
        assert_eq!(value["submitted"], false);
        assert_eq!(value["senderWallet"], "alice");
        assert_eq!(value["recipientInput"], "alice");
        assert_eq!(value["rawAmount"], "15000000");
        assert_eq!(value["expiryHeight"], 123);
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
