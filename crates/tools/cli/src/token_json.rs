use groundhog_json_rpc_types::ContractAddress;
use groundhog_json_rpc_types::events::PaginationCursor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::events_json::RawEventReport;

pub const TOKEN_TRANSFERS_JSON_SCHEMA_VERSION: u32 = 1;

/// Effective participant constraint for a token-transfer query.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TokenParticipantFilter {
    From {
        input: String,
        address: ContractAddress,
    },
    To {
        input: String,
        address: ContractAddress,
    },
    FromTo {
        from_input: String,
        from: ContractAddress,
        to_input: String,
        to: ContractAddress,
    },
    Account {
        input: String,
        address: ContractAddress,
    },
}

/// Semantic kind encoded by a canonical ERC20 transfer event.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TokenTransferKind {
    Mint,
    Burn,
    Transfer,
}

/// Decoding state for one signature-filtered transfer record.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TokenTransferDecodeStatus {
    Decoded,
    Malformed,
}

/// Canonical projection of one valid ERC20 transfer event.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodedTokenTransferReport {
    pub kind: TokenTransferKind,
    pub from: ContractAddress,
    pub to: ContractAddress,
    pub raw_amount: String,
}

/// One raw matching event plus its standard transfer projection.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenTransferRecordReport {
    #[serde(flatten)]
    pub raw: RawEventReport,
    pub decode_status: TokenTransferDecodeStatus,
    pub transfer: Option<DecodedTokenTransferReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_error: Option<String>,
}

/// Stable JSON data for one page of canonical token transfers.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenTransfersReport {
    pub schema_version: u32,
    pub profile: String,
    pub rpc_url: String,
    pub token_input: String,
    pub token_address: ContractAddress,
    pub participant_filter: Option<TokenParticipantFilter>,
    pub records: Vec<TokenTransferRecordReport>,
    pub cursor: PaginationCursor,
    pub latest_block: groundhog_json_rpc_types::BlockHeight,
    pub oldest_block: groundhog_json_rpc_types::BlockHeight,
}
