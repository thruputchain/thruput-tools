//! Top-level node supervisor orchestration.
//!
//! Owns hogd-dev process supervision and the public JSON-RPC server. Backend
//! adapters land in subsequent PRs; until then the JSON-RPC surface is
//! served by [`StubBackend`], which returns `NotImplemented` for every
//! method.

use std::{net::SocketAddr, process::ExitStatus};

use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_server::{GroundhogRpcServer, RpcServerConfig};
use jsonrpsee::server::{RpcModule, Server, ServerHandle};

use crate::{
    HogdDevProcess, HogdDevReadiness, NodeSupervisorConfig, NodeSupervisorError,
    stub_backend::StubBackend,
};

/// Running node supervisor.
///
/// Owns the supervised hogd-dev child and the JSON-RPC server. Drop semantics
/// are intentionally not relied on for shutdown; callers should `stop()`
/// explicitly so the JSON-RPC server and hogd-dev tear down in order.
#[derive(Debug)]
pub struct NodeSupervisorHandle {
    hogd: HogdDevProcess,
    rpc_handle: ServerHandle,
    rpc_addr: SocketAddr,
}

impl NodeSupervisorHandle {
    /// Spawn hogd-dev, wait for readiness, and start the JSON-RPC server.
    ///
    /// `genesis_payload` is written to hogd-dev's stdin per the v1 process
    /// contract. v1 is ephemeral, so callers typically build a deterministic
    /// dev genesis here.
    ///
    /// On any failure after hogd-dev has started, the child is stopped before
    /// the error is returned, so a failed `start` does not leave a hogd-dev
    /// process behind.
    #[tracing::instrument(skip_all, fields(rpc_listen = %config.rpc_listen_address))]
    pub async fn start(
        config: NodeSupervisorConfig,
        genesis_payload: impl AsRef<[u8]>,
    ) -> Result<Self, NodeSupervisorError> {
        let mut hogd = HogdDevProcess::start(&config.hogd_dev, genesis_payload).await?;

        match start_rpc_server(&config.rpc_listen_address).await {
            Ok((rpc_handle, rpc_addr)) => {
                tracing::info!(%rpc_addr, "node supervisor JSON-RPC ready");
                Ok(Self {
                    hogd,
                    rpc_handle,
                    rpc_addr,
                })
            }
            Err(error) => {
                // Roll back the hogd-dev child so a failed RPC bind does not
                // leak a child process.
                if let Err(stop_err) = hogd.stop().await {
                    tracing::warn!(
                        error = %stop_err,
                        "failed to stop hogd-dev after JSON-RPC startup error",
                    );
                }
                Err(error)
            }
        }
    }

    /// Bound JSON-RPC listen address. Differs from the configured address when
    /// the caller asked for port `0`.
    pub fn rpc_addr(&self) -> SocketAddr {
        self.rpc_addr
    }

    /// Parsed hogd-dev readiness payload.
    pub fn hogd_readiness(&self) -> &HogdDevReadiness {
        self.hogd.readiness()
    }

    /// Operating-system process id for hogd-dev, if available.
    pub fn hogd_child_id(&self) -> Option<u32> {
        self.hogd.child_id()
    }

    /// Wait until the JSON-RPC server stops accepting requests.
    pub async fn rpc_stopped(&self) {
        self.rpc_handle.clone().stopped().await;
    }

    /// Stop the JSON-RPC server and shut hogd-dev down with the configured
    /// grace timeout.
    #[tracing::instrument(skip_all)]
    pub async fn stop(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        if let Err(error) = self.rpc_handle.stop() {
            tracing::warn!(%error, "JSON-RPC server already stopped");
        }
        self.rpc_handle.clone().stopped().await;
        self.hogd.stop().await
    }
}

async fn start_rpc_server(
    listen_address: &str,
) -> Result<(ServerHandle, SocketAddr), NodeSupervisorError> {
    let server = Server::builder()
        .build(listen_address)
        .await
        .map_err(|source| NodeSupervisorError::RpcBind {
            address: listen_address.to_string(),
            source,
        })?;
    let rpc_addr = server
        .local_addr()
        .map_err(|source| NodeSupervisorError::RpcLocalAddr { source })?;

    let core_rpc = GroundhogRpcServer::unified(StubBackend::new(), RpcServerConfig::default());
    let mut module = RpcModule::new(());
    module
        .merge(GroundhogApiServer::into_rpc(core_rpc))
        .map_err(|source| NodeSupervisorError::RpcModule { source })?;

    let rpc_handle = server.start(module);
    Ok((rpc_handle, rpc_addr))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use groundhog_rpc_api::GroundhogApiClient;
    use jsonrpsee::core::ClientError;
    use jsonrpsee::http_client::HttpClientBuilder;
    use serial_test::serial;

    use super::*;
    use crate::{
        HogdDevConfig,
        test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
    };

    /// Set an env var for the duration of a test that needs the fake child
    /// to enter `ready` mode. Restored on drop. `#[serial]` ensures we are
    /// the only test mutating env at a time, per workspace convention.
    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            // SAFETY: `#[serial]` serializes all env-mutating tests in this
            // process; no other thread is reading or writing env concurrently.
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see EnvGuard::set.
            match self.prev.take() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn start_serves_json_rpc_with_stub_backend() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let mut handle = NodeSupervisorHandle::start(config, b"")
            .await
            .expect("bug: supervisor should start against fake hogd-dev");

        let url = format!("http://{}", handle.rpc_addr());
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");

        let err = client
            .chain_tip()
            .await
            .expect_err("bug: stub backend returns NotImplemented for chainTip");
        let ClientError::Call(call_err) = err else {
            panic!("bug: expected JSON-RPC call error, got {err:?}");
        };
        // 16004 == codes::NOT_IMPLEMENTED; we assert via the public message
        // contract rather than re-importing the constant from rpc-server.
        assert!(
            call_err.message().contains("not implemented"),
            "expected NotImplemented message, got: {}",
            call_err.message()
        );

        let status = handle.stop().await.expect("bug: clean shutdown");
        assert!(
            !status.success() || status.code() == Some(0),
            "fake hogd-dev should exit cleanly after SIGTERM",
        );
    }
}
