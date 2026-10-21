//! Start the normal local RPC endpoint with a real runtime.
//! Set `THRUPUT_DEVNODE_BINARY` to a compatible absolute hog-devnode binary path.

use thruput_node_supervisor::{NodeSupervisorConfig, NodeSupervisorError, NodeSupervisorHandle};

#[derive(Debug, thiserror::Error)]
enum ExampleError {
    #[error(transparent)]
    Node(#[from] NodeSupervisorError),
    #[error("failed to wait for Ctrl-C: {0}")]
    Signal(#[from] std::io::Error),
}

#[tokio::main]
async fn main() -> Result<(), ExampleError> {
    let node = NodeSupervisorHandle::start(
        NodeSupervisorConfig::default().with_rpc_listen_address("127.0.0.1:0"),
    )
    .await?;
    println!("rpc=http://{}", node.rpc_addr());
    let result = tokio::signal::ctrl_c().await;
    node.stop().await?;
    result?;
    Ok(())
}
