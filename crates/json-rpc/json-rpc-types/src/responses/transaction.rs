use super::{
    Deserialize, Deserializer, EmittedEvent, JsonSchema, ResponseData, Serialize, UnknownVariant,
    Value,
    fields::{height_schema, hex_schema, i32_schema, present, read_height, read_hex, u32_schema},
};
use crate::{BlockHeight, ContractAddress, hex::HexBytes, transaction::TxHash};

response_strings! {
/// A successful send acknowledges this admission attempt only.
Admission { Accepted("accepted", true) }
}

response_object! {
/// Authoritative admission acknowledgement, not commitment or execution.
SendTransactionResult {
    transaction_hash: TxHash,
    admission: Admission,
}
}

response_enum! {
/// Retained transaction observations. No negative status proves nonexecution.
TransactionStatus, "status" {
    Pending("pending", true) {
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
        #[schemars(with = "RecentAttemptFailure")]
        recent_failure: Option<RecentAttemptFailure>,
    },
    Committed("committed", true) {
        #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
        #[schemars(schema_with = "height_schema")]
        block_height: BlockHeight,
    },
    Retry("retry", true) {
        error: SchedulerError,
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
        #[schemars(with = "RecentAttemptFailure")]
        recent_failure: Option<RecentAttemptFailure>,
    },
    Dropped("dropped", true) {
        error: SchedulerError,
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
        #[schemars(with = "RecentAttemptFailure")]
        recent_failure: Option<RecentAttemptFailure>,
    },
    RecentFailure("recentFailure", true) {
        #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
        #[schemars(schema_with = "height_schema")]
        attempt_block_height: BlockHeight,
        cause: FailureCause,
    },
    Unknown("unknown", true) {},
    Other("other", false) {},
}
}

response_object! {
/// A recently indexed attempt failure, independent of the scheduler observation.
/// It need not describe the same or latest attempt. Absence does not prove success,
/// and its presence does not authorize resubmission or replacement.
RecentAttemptFailure {
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    attempt_block_height: BlockHeight,
    cause: FailureCause,
}
}

response_object! {
/// Scheduler subsystem and its unmodified numeric diagnostic.
SchedulerError {
    source: SchedulerErrorSource,
    #[schemars(schema_with = "i32_schema")]
    code: i32,
}
}

response_strings! {
/// Source of a scheduler diagnostic; unfamiliar names remain unsupported.
SchedulerErrorSource {
    Auth("auth", true), Scheduler("scheduler", true), Executor("executor", true), Other("other", false),
}
}

response_enum! {
/// Advisory failure from a recent proposal attempt.
FailureCause, "kind" {
    DbConflict("dbConflict", true) { conflict: ConflictDiagnostic },
    ContractRejected("contractRejected", true) {
        #[schemars(schema_with = "u32_schema")]
        contract_index: u32,
        code: ContractRejectCode,
    },
    Other("other", false) {},
}
}

response_object! {
/// Typed storage-conflict diagnostic, preserving the serialized key.
ConflictDiagnostic {
    address: ContractAddress,
    #[serde(deserialize_with = "read_hex")]
    #[schemars(schema_with = "hex_schema")]
    key: HexBytes,
    code: ConflictCode,
}
}

response_strings! {
/// Existing executor contract-rejection vocabulary.
ContractRejectCode {
    TooMany("tooMany", true), BlobTooLarge("blobTooLarge", true), NotDeployable("notDeployable", true),
    Duplicate("duplicate", true), MalformedArtifact("malformedArtifact", true),
    UnsupportedFormat("unsupportedFormat", true), UnsupportedAbi("unsupportedAbi", true),
    GasScheduleMismatch("gasScheduleMismatch", true), MemoryTooLarge("memoryTooLarge", true),
    CodeSizeInvalid("codeSizeInvalid", true), InitialMemoryInvalid("initialMemoryInvalid", true),
    DigestMismatch("digestMismatch", true), InvalidProgram("invalidProgram", true), Other("other", false),
}
}

response_strings! {
/// Existing storage-conflict vocabulary. These are diagnostics, not RPC errors.
ConflictCode {
    ExclusiveLock("exclusiveLock", true), TypeMismatch("typeMismatch", true), ValueMismatch("valueMismatch", true),
    BoundsExceeded("boundsExceeded", true), Overflow("overflow", true), ReplayProtection("replayProtection", true),
    QueueConsumer("queueConsumer", true), CapacityExceeded("capacityExceeded", true),
    InsufficientFunds("insufficientFunds", true), Other("other", false),
}
}

response_enum! {
/// Committed execution outcome. Return and abort bytes are required, even empty.
ReceiptExecution, "result" {
    Return("return", true) {
        #[serde(deserialize_with = "read_hex")]
        #[schemars(schema_with = "hex_schema")]
        return_data: HexBytes,
    },
    Abort("abort", true) {
        #[serde(deserialize_with = "read_hex")]
        #[schemars(schema_with = "hex_schema")]
        abort_data: HexBytes,
    },
    Fault("fault", true) {},
    NonDeterministicError("nonDeterministicError", true) {},
    FailedGasSponsorship("failedGasSponsorship", true) {},
    Other("other", false) {},
}
}

response_object! {
/// A committed receipt. Existence does not imply successful execution.
TransactionReceipt {
    transaction_hash: TxHash,
    #[serde(serialize_with = "crate::decimal::serialize", deserialize_with = "read_height")]
    #[schemars(schema_with = "height_schema")]
    block_height: BlockHeight,
    execution: ReceiptExecution,
    events: Vec<EmittedEvent>,
    /// Metered work, not a fee or token burn.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    gas_used: u64,
}
}

/// Null means a successful lookup produced no committed receipt.
pub type ReceiptResult = Option<TransactionReceipt>;
