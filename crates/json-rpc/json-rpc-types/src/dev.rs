use groundhog_trace::{
    HostFunctionStatusXdr, InvocationFrameTrace, SyscallTrace, SyscallTraceBody,
};
use groundhog_xdr::storage_item::{FlaggedStorageDelta, StorageDelta, StorageDeltaFlags};
use groundhog_xdr::transaction::{Invocation, InvocationFrameOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ContractAddress;
use crate::hex::{Hex0x, HexBytes};
use crate::receipt::TransactionReceipt;

/// Dev-only execution detail for a committed transaction.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransactionDetail {
    /// Full receipt retained within the heavy-data retention window.
    pub receipt: TransactionReceipt,
    /// Gas units consumed by the transaction.
    pub gas_consumed: u64,
    /// Configured gas limit from the transaction, if one was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas_limit: Option<u64>,
    /// Top-level storage writes applied by the committed transaction.
    pub storage_deltas: Vec<StorageDeltaInfo>,
    /// Full root trace, including child invocations when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceInfo>,
}

/// Readable storage delta plus raw encoded value for machine inspection.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageDeltaInfo {
    /// Contract address whose storage was modified.
    pub address: ContractAddress,
    /// XDR-encoded storage key bytes.
    pub key: HexBytes,
    /// Stable delta kind identifier.
    pub kind: String,
    /// Human-readable summary used by CLI and terminal output.
    pub summary: String,
    /// Raw delta flags bitset.
    pub flags: i32,
    /// XDR-encoded storage delta body.
    pub delta_xdr: HexBytes,
}

/// Serializable frame trace for local-node debugging.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TraceInfo {
    /// Invocation inputs for this frame.
    pub invocation: InvocationInfo,
    /// Ordered system calls executed by this frame.
    pub system_calls: Vec<SyscallInfo>,
    /// Storage writes accumulated by this frame.
    pub storage_writes: Vec<StorageDeltaInfo>,
    /// How the frame terminated.
    pub status: FrameOutputInfo,
}

/// RPC-facing invocation summary with hex-friendly byte fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InvocationInfo {
    pub address: ContractAddress,
    pub entrypoint: HexBytes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas_limit: Option<u64>,
    pub calldata: HexBytes,
}

/// Serializable syscall entry retained inside a frame trace.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyscallInfo {
    pub name: String,
    pub number: u64,
    pub gas_remaining_at_start: u64,
    pub gas_remaining_at_end: u64,
    pub status: SyscallStatusInfo,
    /// XDR-encoded syscall trace body, including typed inputs/outputs.
    pub body_xdr: HexBytes,
    /// Nested child trace for `invoke` syscalls, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_trace: Option<Box<TraceInfo>>,
}

/// Compact syscall completion status.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyscallStatusInfo {
    pub code: i32,
    pub label: String,
}

/// Frame completion status with raw buffers preserved as hex.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum FrameOutputInfo {
    Return { buffer: HexBytes },
    Abort { buffer: HexBytes },
    Fault,
}

impl From<&FlaggedStorageDelta> for StorageDeltaInfo {
    fn from(delta: &FlaggedStorageDelta) -> Self {
        Self {
            address: Hex0x(delta.addrkey.address),
            key: xdr_hex_bytes(&delta.addrkey.key),
            kind: storage_delta_kind(&delta.delta),
            summary: storage_delta_summary(delta),
            flags: delta.flags,
            delta_xdr: xdr_hex_bytes(&delta.delta),
        }
    }
}

impl From<FlaggedStorageDelta> for StorageDeltaInfo {
    fn from(delta: FlaggedStorageDelta) -> Self {
        Self::from(&delta)
    }
}

impl From<&InvocationFrameTrace> for TraceInfo {
    fn from(trace: &InvocationFrameTrace) -> Self {
        Self {
            invocation: InvocationInfo::from(&trace.invocation),
            system_calls: trace.system_calls.iter().map(SyscallInfo::from).collect(),
            storage_writes: trace
                .storage_writes
                .iter()
                .map(StorageDeltaInfo::from)
                .collect(),
            status: FrameOutputInfo::from(&trace.status),
        }
    }
}

impl From<InvocationFrameTrace> for TraceInfo {
    fn from(trace: InvocationFrameTrace) -> Self {
        Self::from(&trace)
    }
}

impl From<&Invocation> for InvocationInfo {
    fn from(invocation: &Invocation) -> Self {
        Self {
            address: Hex0x(invocation.invoked_address),
            entrypoint: HexBytes(invocation.entrypoint.into_inner().to_vec()),
            gas_limit: configured_gas_limit(invocation.gas_limit),
            calldata: HexBytes(invocation.calldata.0.clone()),
        }
    }
}

impl From<&SyscallTrace> for SyscallInfo {
    fn from(syscall: &SyscallTrace) -> Self {
        Self {
            name: syscall_name(&syscall.body).into(),
            number: u64::from(syscall.body.syscall_number().0),
            gas_remaining_at_start: syscall.gas_remaining_at_start,
            gas_remaining_at_end: syscall.gas_remaining_at_end,
            status: SyscallStatusInfo::from(&syscall.status),
            body_xdr: xdr_hex_bytes(&syscall.body),
            child_trace: syscall_child_trace(&syscall.body).map(|trace| Box::new(trace.into())),
        }
    }
}

impl From<&HostFunctionStatusXdr> for SyscallStatusInfo {
    fn from(status: &HostFunctionStatusXdr) -> Self {
        match status {
            HostFunctionStatusXdr::NoneOrRecoverable {
                syscall_return_code,
            } => Self {
                code: syscall_return_code.0,
                label: syscall_error_label(*syscall_return_code),
            },
            HostFunctionStatusXdr::Default { error } => Self {
                code: *error,
                label: format!("HostFnErrorCode({error})"),
            },
            _ => Self {
                code: -1,
                label: "HostFnErrorCode(unknown)".into(),
            },
        }
    }
}

impl From<&InvocationFrameOutput> for FrameOutputInfo {
    fn from(output: &InvocationFrameOutput) -> Self {
        match output {
            InvocationFrameOutput::Return { return_buffer } => Self::Return {
                buffer: HexBytes(return_buffer.0.clone()),
            },
            InvocationFrameOutput::Abort { abort_buffer } => Self::Abort {
                buffer: HexBytes(abort_buffer.0.clone()),
            },
            _ => Self::Fault,
        }
    }
}

pub fn configured_gas_limit(gas_limit: u64) -> Option<u64> {
    (gas_limit != u64::MAX).then_some(gas_limit)
}

fn storage_delta_kind(delta: &StorageDelta) -> String {
    match delta {
        StorageDelta::kDeltaTypeBytestringSet { .. } => "bytestringSet".into(),
        StorageDelta::kDeltaTypeNonnegativeIntSetAdd { .. } => "nonnegativeIntSetAdd".into(),
        StorageDelta::kDeltaTypeDelete => "delete".into(),
        StorageDelta::kDeltaTypeRetrocausalQueue { .. } => "retrocausalQueue".into(),
        StorageDelta::kDeltaTypeNonnegativeIntAdd { .. } => "nonnegativeIntAdd".into(),
        StorageDelta::kDeltaTypeAuthDataChange { .. } => "authDataChange".into(),
        StorageDelta::kDeltaTypeDeployContract { .. } => "deployContract".into(),
        StorageDelta::kDeltaTypeConsumeUniquifier { .. } => "consumeUniquifier".into(),
        other => format!("storageDelta({})", other.r#type().0),
    }
}

fn storage_delta_summary(delta: &FlaggedStorageDelta) -> String {
    let body = match &delta.delta {
        StorageDelta::kDeltaTypeBytestringSet { bytestring } => {
            format!("BytestringSet -> 0x{}", hex::encode(&bytestring.0))
        }
        StorageDelta::kDeltaTypeNonnegativeIntSetAdd { nnint } => format!(
            "NonnegativeIntSetAdd set={} add={}",
            format_i128_xdr(&nnint.set_value),
            format_i128_xdr(&nnint.add_value),
        ),
        StorageDelta::kDeltaTypeNonnegativeIntAdd { nnint_add } => {
            format!("NonnegativeIntAdd {:+}", int128_to_i128(nnint_add))
        }
        StorageDelta::kDeltaTypeDelete => "Delete".to_string(),
        StorageDelta::kDeltaTypeRetrocausalQueue { retrocausal_queue } => {
            format!("RetrocausalQueue -> {}", xdr_hex_string(retrocausal_queue))
        }
        StorageDelta::kDeltaTypeAuthDataChange { auth_data_change } => {
            format!("AuthDataChange -> {}", xdr_hex_string(auth_data_change))
        }
        StorageDelta::kDeltaTypeDeployContract {
            deployed_contract_hash,
        } => format!(
            "DeployContract -> 0x{}",
            hex::encode(deployed_contract_hash.hash.as_ref())
        ),
        StorageDelta::kDeltaTypeConsumeUniquifier {
            consume_uniquifier_args,
        } => format!(
            "ConsumeUniquifier nonce={} expiry={}",
            consume_uniquifier_args.uniquifier.nonce,
            *consume_uniquifier_args.uniquifier.block_expiry_height,
        ),
        other => format!(
            "StorageDelta({}) -> {}",
            other.r#type().0,
            xdr_hex_string(other)
        ),
    };

    if delta.flags == StorageDeltaFlags::None.0 {
        body
    } else {
        format!("{body} [{}]", storage_delta_flags_label(delta.flags))
    }
}

fn syscall_name(body: &SyscallTraceBody) -> &'static str {
    match body {
        SyscallTraceBody::kSHA256 { .. } => "sha256",
        SyscallTraceBody::kGetMsgSender { .. } => "getMsgSender",
        SyscallTraceBody::kGetCalldataLen { .. } => "getCalldataLen",
        SyscallTraceBody::kGetCalldata { .. } => "getCalldata",
        SyscallTraceBody::kConsoleLog { .. } => "consoleLog",
        SyscallTraceBody::kInvoke { .. } => "invoke",
        SyscallTraceBody::kReturn { .. } => "return",
        SyscallTraceBody::kGetReturnedBuffer { .. } => "getReturnedBuffer",
        SyscallTraceBody::kExit { .. } => "exit",
        SyscallTraceBody::kGetDeployAddress { .. } => "getDeployAddress",
        SyscallTraceBody::kDeployContract { .. } => "deployContract",
        SyscallTraceBody::kGetSourceTransactionHash { .. } => "getSourceTransactionHash",
        SyscallTraceBody::kVerifySignature { .. } => "verifySignature",
        SyscallTraceBody::kBytestringGetLen { .. } => "bytestringGetLen",
        SyscallTraceBody::kBytestringGet { .. } => "bytestringGet",
        SyscallTraceBody::kBytestringSet { .. } => "bytestringSet",
        SyscallTraceBody::kNonnegativeIntGet { .. } => "nonnegativeIntGet",
        SyscallTraceBody::kNonnegativeIntSetAdd { .. } => "nonnegativeIntSetAdd",
        SyscallTraceBody::kDeleteObject { .. } => "deleteObject",
        SyscallTraceBody::kGetCurrentAddress { .. } => "getCurrentAddress",
        SyscallTraceBody::kRetrocausalQueueInsert { .. } => "retrocausalQueueInsert",
        SyscallTraceBody::kRetrocausalQueueConsume { .. } => "retrocausalQueueConsume",
        SyscallTraceBody::kGetTransactionStatistics { .. } => "getTransactionStatistics",
        SyscallTraceBody::kChangePublicKeyRegistration { .. } => "changePublicKeyRegistration",
        SyscallTraceBody::kLogEvent { .. } => "logEvent",
        SyscallTraceBody::kGetAuthClaim { .. } => "getAuthClaim",
        SyscallTraceBody::kSHA256Update { .. } => "sha256Update",
        SyscallTraceBody::kSHA256Finalize { .. } => "sha256Finalize",
        SyscallTraceBody::kWriteAddressRequiredWeight { .. } => "writeAddressRequiredWeight",
        SyscallTraceBody::kGetBlockHeight { .. } => "getBlockHeight",
        SyscallTraceBody::kRetrocausalQueuePeekLen { .. } => "retrocausalQueuePeekLen",
        SyscallTraceBody::kMintAuthToken { .. } => "mintAuthToken",
        SyscallTraceBody::kConsumeAuthToken { .. } => "consumeAuthToken",
        _ => "unknown",
    }
}

fn syscall_child_trace(body: &SyscallTraceBody) -> Option<&InvocationFrameTrace> {
    match body {
        SyscallTraceBody::kInvoke { invoke } => invoke
            .outputs
            .as_ref()
            .and_then(|outputs| outputs.child_trace.as_ref()),
        _ => None,
    }
}

fn xdr_hex_bytes<T>(value: &T) -> HexBytes
where
    T: xdrs::WriteXDR,
{
    let bytes = xdrs::txs::encode(value).expect("bug: generated XDR type should encode");
    HexBytes(bytes)
}

fn xdr_hex_string<T>(value: &T) -> String
where
    T: xdrs::WriteXDR,
{
    format!("0x{}", hex::encode(&xdr_hex_bytes(value).0))
}

pub fn format_gas_summary(consumed: u64, gas_limit: Option<u64>) -> String {
    let consumed = format_number(consumed);
    match gas_limit {
        Some(limit) => format!("{consumed} / {}", format_number(limit)),
        None => consumed,
    }
}

fn format_number(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(ch);
    }
    formatted
}

pub fn format_i128_xdr(value: &groundhog_xdr::types::int128_xdr) -> String {
    int128_to_i128(value).to_string()
}

pub fn int128_to_i128(value: &groundhog_xdr::types::int128_xdr) -> i128 {
    (i128::from(value.highbits) << 64) | i128::from(value.lowbits)
}

fn syscall_error_label(code: groundhog_xdr::syscall_error::SyscallErrorCode) -> String {
    match code.0 {
        0 => "SyscallErrorCode::kSyscallErrorCodeNone".into(),
        1 => "SyscallErrorCode::kSyscallErrorCodeFault".into(),
        2 => "SyscallErrorCode::kSyscallErrorCodeNoSyscall".into(),
        3 => "SyscallErrorCode::kSyscallErrorCodeIsRootTransaction".into(),
        4 => "SyscallErrorCode::kSyscallErrorCodeReentrance".into(),
        5 => "SyscallErrorCode::kSyscallErrorCodeContractAlreadyDeployed".into(),
        6 => "SyscallErrorCode::kSyscallErrorCodeNoReturnBufferAvailable".into(),
        7 => "SyscallErrorCode::kSyscallErrorCodeInvalidSyscallInput".into(),
        8 => "SyscallErrorCode::kSyscallErrorCodeStorageKeyNotExist".into(),
        9 => "SyscallErrorCode::kSyscallErrorCodeNoRetrocausalQueueEntry".into(),
        -1 => "SyscallErrorCode::kSyscallErrorCodeDeterministicWasm".into(),
        -2 => "SyscallErrorCode::kSyscallErrorCodeFrameReverted".into(),
        other => format!("SyscallErrorCode({other})"),
    }
}

fn storage_delta_flags_label(flags: i32) -> String {
    match flags {
        0 => "StorageDeltaFlags::None".into(),
        1 => "StorageDeltaFlags::ExclusiveLock".into(),
        other => format!("StorageDeltaFlags({other})"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FrameOutputInfo, InvocationInfo, StorageDeltaInfo, SyscallInfo, TraceInfo,
        TransactionDetail,
    };
    use crate::BlockHeight;
    use crate::events::EmittedEvent;
    use crate::hex::{Hex0x, HexBytes, Topic};
    use crate::receipt::{ExecStatus, TransactionReceipt};
    use crate::transaction::TxHash;
    use serde_json::Value;

    #[test]
    fn transaction_detail_serializes_hex_and_camel_case() {
        let address = groundhog_xdr::types::ContractAddress::from([0x11; 32]);
        let detail = TransactionDetail {
            receipt: TransactionReceipt {
                tx_hash: TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x22; 32]))),
                height: BlockHeight::from(7),
                status: ExecStatus::Ok,
                events: vec![EmittedEvent {
                    contract_address: Hex0x(address),
                    topics: vec![Topic([0x33; 32])],
                    data: HexBytes(vec![0x44]),
                }],
                return_data: Some(HexBytes(vec![0x55])),
                abort_data: None,
            },
            gas_consumed: 12,
            gas_limit: Some(99),
            storage_deltas: vec![StorageDeltaInfo {
                address: Hex0x(address),
                key: HexBytes(vec![0x66]),
                kind: "bytestringSet".into(),
                summary: "BytestringSet -> 0x77".into(),
                flags: 0,
                delta_xdr: HexBytes(vec![0x77]),
            }],
            trace: Some(TraceInfo {
                invocation: InvocationInfo {
                    address: Hex0x(address),
                    entrypoint: HexBytes(vec![0x88; 8]),
                    gas_limit: Some(99),
                    calldata: HexBytes(vec![0x99]),
                },
                system_calls: vec![SyscallInfo {
                    name: "logEvent".into(),
                    number: 23,
                    gas_remaining_at_start: 100,
                    gas_remaining_at_end: 90,
                    status: super::SyscallStatusInfo {
                        code: 0,
                        label: "SyscallErrorCode::kSyscallErrorCodeNone".into(),
                    },
                    body_xdr: HexBytes(vec![0xaa]),
                    child_trace: None,
                }],
                storage_writes: Vec::new(),
                status: FrameOutputInfo::Return {
                    buffer: HexBytes(vec![0xbb]),
                },
            }),
        };

        let value: Value = serde_json::to_value(&detail).expect("bug: detail should serialize");
        assert_eq!(value["gasConsumed"], 12);
        assert_eq!(value["gasLimit"], 99);
        assert_eq!(value["storageDeltas"][0]["kind"], "bytestringSet");
        assert_eq!(value["storageDeltas"][0]["key"], "0x66");
        assert_eq!(
            value["trace"]["invocation"]["entrypoint"],
            "0x8888888888888888"
        );
        assert_eq!(value["trace"]["systemCalls"][0]["bodyXdr"], "0xaa");
        assert_eq!(value["trace"]["status"]["result"], "return");
        assert_eq!(value["trace"]["status"]["buffer"], "0xbb");
        assert!(value.get("gas_consumed").is_none());
        assert!(value["trace"].get("system_calls").is_none());
    }
}
