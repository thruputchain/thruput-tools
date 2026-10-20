//! Run both local listeners for qualification, without changing normal CLI serving.
//! Set `THRUPUT_DEVNODE_BINARY` to a compatible absolute hog-devnode binary path.

use thruput_json_rpc_server::RpcServerConfig;
use thruput_node_supervisor::server::qualification::NativeQualificationError;
use thruput_node_supervisor::{NodeSupervisorConfig, NodeSupervisorError, NodeSupervisorHandle};

#[derive(Debug, thiserror::Error)]
enum ExampleError {
    #[error(transparent)]
    Node(#[from] NodeSupervisorError),
    #[error(transparent)]
    Qualification(#[from] NativeQualificationError),
    #[error("failed to wait for Ctrl-C: {0}")]
    Signal(#[from] std::io::Error),
}

#[tokio::main]
async fn main() -> Result<(), ExampleError> {
    let node = NodeSupervisorHandle::start(
        NodeSupervisorConfig::default().with_rpc_listen_address("127.0.0.1:0"),
    )
    .await?;
    let result = run_candidate(&node).await;
    node.stop().await?;
    result
}

async fn run_candidate(node: &NodeSupervisorHandle) -> Result<(), ExampleError> {
    let candidate = node
        .start_native_qualification(RpcServerConfig::default())
        .await?;
    println!("wire13=http://{}", node.rpc_addr());
    println!("native=http://{}", candidate.rpc_addr());
    let signal = tokio::signal::ctrl_c().await;
    candidate.stop().await;
    signal?;
    Ok(())
}
