use schemars::schema_for;

use thruput_json_rpc_types::block::{BlockInfo, ChainTip};
use thruput_json_rpc_types::bootstrap_manifest::BootstrapManifest;
use thruput_json_rpc_types::dev::{
    FrameOutputInfo, InvocationInfo, StorageDeltaInfo, SyscallInfo, SyscallStatusInfo, TraceInfo,
    TransactionDetail,
};
use thruput_json_rpc_types::events::{
    EmittedEvent, EventFilter, EventPage, EventQuery, EventTopicFilter, IdentifiedEvent,
};
use thruput_json_rpc_types::network_descriptor::NetworkDescriptor;
use thruput_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use thruput_json_rpc_types::state::{StorageItemInfo, StorageItemRead};
use thruput_json_rpc_types::transaction::{
    AddressTransactionQuery, TransactionRef, TransactionRefPage, TransactionStatus, TxHash,
};
use thruput_json_rpc_types::view::{RootInvocationOutput, ViewRequest, ViewResponse};

#[test]
fn schema_generation_succeeds() {
    // Core RPC types
    let _ = schema_for!(ChainTip);
    let _ = schema_for!(BlockInfo);
    let _ = schema_for!(TxHash);
    let _ = schema_for!(TransactionStatus);
    let _ = schema_for!(TransactionReceipt);
    let _ = schema_for!(ExecStatus);
    let _ = schema_for!(ViewRequest);
    let _ = schema_for!(ViewResponse);
    let _ = schema_for!(RootInvocationOutput);

    // Transaction history types
    let _ = schema_for!(AddressTransactionQuery);
    let _ = schema_for!(TransactionRef);
    let _ = schema_for!(TransactionRefPage);

    // Event types
    let _ = schema_for!(EmittedEvent);
    let _ = schema_for!(IdentifiedEvent);
    let _ = schema_for!(EventPage);
    let _ = schema_for!(EventTopicFilter);
    let _ = schema_for!(EventFilter);
    let _ = schema_for!(EventQuery);

    // State types
    let _ = schema_for!(StorageItemInfo);
    let _ = schema_for!(StorageItemRead);

    // Dev types
    let _ = schema_for!(TransactionDetail);
    let _ = schema_for!(StorageDeltaInfo);
    let _ = schema_for!(TraceInfo);
    let _ = schema_for!(InvocationInfo);
    let _ = schema_for!(SyscallInfo);
    let _ = schema_for!(SyscallStatusInfo);
    let _ = schema_for!(FrameOutputInfo);

    // Manifest types
    let _ = schema_for!(BootstrapManifest);
    let _ = schema_for!(NetworkDescriptor);
}
