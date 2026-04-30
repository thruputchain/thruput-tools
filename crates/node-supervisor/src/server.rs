//! Top-level node supervisor orchestration.
//!
//! Owns hogd-dev process supervision and the public JSON-RPC server. Backend
//! adapters land in subsequent PRs; until then the JSON-RPC surface is
//! served by [`StubBackend`], which returns `NotImplemented` for every
//! method.

use std::{future::Future, net::SocketAddr, process::ExitStatus};

use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_server::{GroundhogRpcServer, RpcServerConfig};
use jsonrpsee::server::{RpcModule, Server, ServerHandle};

use crate::{
    HogdDevProcess, HogdDevReadiness, NodeSupervisorConfig, NodeSupervisorError,
    stub_backend::StubBackend,
};

/// Why a [`NodeSupervisorHandle::stopped`] call returned.
///
/// Callers (CLI exit codes, supervisors-of-supervisors) need to distinguish
/// "hogd-dev died on us" from "we were asked to stop and hogd-dev is now
/// down because we asked it to". Both branches return an `ExitStatus`, but
/// the meaning is different.
#[derive(Debug)]
#[non_exhaustive]
pub enum SupervisorStop {
    /// hogd-dev exited on its own; the supervisor reacted by stopping the
    /// JSON-RPC server. The contained status is hogd-dev's own exit status.
    HogdExited(ExitStatus),
    /// The JSON-RPC server stopped (typically via an external `stop()`
    /// call), and the supervisor then shut hogd-dev down with the
    /// configured grace timeout. The contained status is hogd-dev's exit
    /// status from that shutdown.
    RpcStopped(ExitStatus),
    /// An external stop signal was received, and the supervisor then shut
    /// hogd-dev down with the configured grace timeout. The contained status
    /// is hogd-dev's exit status from that requested shutdown.
    StopRequested(ExitStatus),
}

/// Running node supervisor.
///
/// Owns the supervised hogd-dev child and the JSON-RPC server. The handle
/// does no proactive supervision in the background: callers are responsible
/// for awaiting [`NodeSupervisorHandle::stopped`] (the supervisor-level
/// "run until something ends" entry point) or driving [`NodeSupervisorHandle::stop`]
/// directly. Both consume the handle so a single instance cannot be torn
/// down twice.
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

    /// Wait until either hogd-dev exits or the JSON-RPC server stops, then
    /// tear the other side down.
    ///
    /// If hogd-dev dies after readiness, the public JSON-RPC facade keeps
    /// answering but every backend call would fail; this method observes
    /// that and shuts the facade down so the CLI sees a clean stop instead
    /// of silent malfunction. If the JSON-RPC server stops first (e.g. via
    /// an external [`NodeSupervisorHandle::stop`] call), hogd-dev is shut
    /// down with the configured grace timeout.
    ///
    /// Returns a [`SupervisorStop`] describing which side ended first.
    #[tracing::instrument(skip_all)]
    pub async fn stopped(mut self) -> Result<SupervisorStop, NodeSupervisorError> {
        self.run_until_inner(std::future::pending::<Result<(), NodeSupervisorError>>())
            .await
    }

    /// Wait until hogd-dev exits, the JSON-RPC server stops, or an external
    /// stop signal completes, then tear the remaining side down.
    ///
    /// This is the CLI-friendly variant of [`NodeSupervisorHandle::stopped`]:
    /// callers pass a future for Ctrl-C or another stop request without
    /// losing the ability to shut hogd-dev down cleanly.
    #[tracing::instrument(skip_all)]
    pub async fn run_until<F>(
        mut self,
        stop_signal: F,
    ) -> Result<SupervisorStop, NodeSupervisorError>
    where
        F: Future<Output = Result<(), NodeSupervisorError>>,
    {
        self.run_until_inner(stop_signal).await
    }

    async fn run_until_inner<F>(
        &mut self,
        stop_signal: F,
    ) -> Result<SupervisorStop, NodeSupervisorError>
    where
        F: Future<Output = Result<(), NodeSupervisorError>>,
    {
        let rpc_done = self.rpc_handle.clone().stopped();
        tokio::select! {
            result = self.hogd.stopped() => {
                let status = result?;
                tracing::warn!(?status, "hogd-dev exited; stopping JSON-RPC server");
                if let Err(error) = self.rpc_handle.stop() {
                    tracing::debug!(%error, "JSON-RPC server already stopped");
                }
                self.rpc_handle.clone().stopped().await;
                Ok(SupervisorStop::HogdExited(status))
            }
            () = rpc_done => {
                tracing::info!("JSON-RPC server stopped; stopping hogd-dev");
                let status = self.stop_inner().await?;
                Ok(SupervisorStop::RpcStopped(status))
            }
            result = stop_signal => {
                if let Err(error) = result {
                    tracing::warn!(%error, "stop signal failed; stopping node supervisor");
                    if let Err(stop_error) = self.stop_inner().await {
                        tracing::warn!(%stop_error, "failed to stop node supervisor after stop signal error");
                    }
                    return Err(error);
                }
                tracing::info!("stop signal received; stopping node supervisor");
                let status = self.stop_inner().await?;
                Ok(SupervisorStop::StopRequested(status))
            }
        }
    }

    /// Stop the JSON-RPC server and shut hogd-dev down with the configured
    /// grace timeout. Consumes the handle.
    #[tracing::instrument(skip_all)]
    pub async fn stop(mut self) -> Result<ExitStatus, NodeSupervisorError> {
        self.stop_inner().await
    }

    async fn stop_inner(&mut self) -> Result<ExitStatus, NodeSupervisorError> {
        if let Err(error) = self.rpc_handle.stop() {
            tracing::debug!(%error, "JSON-RPC server already stopped");
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

    tracing::warn!(
        "node supervisor is using StubBackend; \
         all JSON-RPC methods will return NOT_IMPLEMENTED until the hogd-dev \
         x23 adapters land (PR 4a/4b/4c)"
    );

    let core_rpc = GroundhogRpcServer::unified(StubBackend::new(), RpcServerConfig::default());
    let mut module = RpcModule::new(());
    // `merge` only fails on duplicate method names, which is a compile-time
    // invariant of the macro-generated `GroundhogApiServer` trait — the
    // module starts empty and gets one trait merged in.
    module
        .merge(GroundhogApiServer::into_rpc(core_rpc))
        .expect("bug: rpc-server methods register without conflicts");

    let rpc_handle = server.start(module);
    Ok((rpc_handle, rpc_addr))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use groundhog_rpc_api::GroundhogApiClient;
    use groundhog_rpc_api_types::error::codes;
    use jsonrpsee::core::ClientError;
    use jsonrpsee::http_client::HttpClientBuilder;
    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        HogdDevConfig,
        test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
    };

    /// Env var the fake hogd-dev reads to write its PID before printing
    /// readiness. Tests use this to verify cleanup via `kill(pid, 0)`.
    const FAKE_HOGD_DEV_PID_FILE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_PID_FILE";

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

    /// Poll `kill(pid, 0)` until ESRCH or until `deadline` passes. Returns
    /// `true` if the OS confirmed the process is gone within the deadline.
    async fn wait_pid_gone(pid: i32, deadline: Instant) -> bool {
        loop {
            // SAFETY: `kill(pid, 0)` does not deliver a signal; it only
            // tests whether `pid` is a known process. Always safe.
            let rc = unsafe { libc::kill(pid, 0) };
            if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn read_pid_file(path: &std::path::Path) -> Option<i32> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| raw.trim().parse().ok())
    }

    #[tokio::test]
    #[serial]
    async fn start_serves_json_rpc_with_stub_backend() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
        let pid_dir = TempDir::new().expect("bug: tempdir creates");
        let pid_path = pid_dir.path().join("hogd.pid");
        let _pid_guard = EnvGuard::set(
            FAKE_HOGD_DEV_PID_FILE_ENV,
            pid_path.to_str().expect("bug: tempdir path is utf-8"),
        );

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config, b"")
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
        assert_eq!(
            call_err.code(),
            codes::NOT_IMPLEMENTED,
            "stub backend should map to NOT_IMPLEMENTED ({}); got message: {}",
            codes::NOT_IMPLEMENTED,
            call_err.message()
        );

        let pid = read_pid_file(&pid_path).expect("bug: fake should have written pid");
        let status = handle.stop().await.expect("bug: clean shutdown");

        // Tighter than `!success() || code == 0`: a clean SIGTERM-handled
        // exit either returns code 0 or the SIGTERM signal. Anything else
        // (SIGKILL, abnormal exit) means the supervisor escalated past
        // graceful shutdown.
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let clean = status.code() == Some(0) || status.signal() == Some(libc::SIGTERM);
            assert!(
                clean,
                "fake hogd-dev should exit on code 0 or SIGTERM, got {status:?}"
            );
        }

        assert!(
            wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
            "fake hogd-dev pid {pid} still alive after stop()",
        );
    }

    #[tokio::test]
    #[serial]
    async fn run_until_stop_signal_error_stops_hogd_dev() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
        let pid_dir = TempDir::new().expect("bug: tempdir creates");
        let pid_path = pid_dir.path().join("hogd.pid");
        let _pid_guard = EnvGuard::set(
            FAKE_HOGD_DEV_PID_FILE_ENV,
            pid_path.to_str().expect("bug: tempdir path is utf-8"),
        );

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config, b"")
            .await
            .expect("bug: supervisor should start against fake hogd-dev");
        let pid = read_pid_file(&pid_path).expect("bug: fake should have written pid");

        let err = handle
            .run_until(async {
                Err(NodeSupervisorError::StopSignal {
                    source: std::io::Error::other("ctrl-c registration failed"),
                })
            })
            .await
            .expect_err("bug: stop signal error should be returned");
        assert!(
            matches!(err, NodeSupervisorError::StopSignal { .. }),
            "expected StopSignal error, got {err:?}",
        );
        assert!(
            wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
            "fake hogd-dev pid {pid} still alive after stop signal error",
        );
    }

    #[tokio::test]
    #[serial]
    async fn stopped_observes_hogd_dev_exit_and_tears_down_rpc() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready-then-exit");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address("127.0.0.1:0")
            .with_hogd_dev(hogd);
        let handle = NodeSupervisorHandle::start(config, b"")
            .await
            .expect("bug: supervisor should start against fake hogd-dev");

        // Capture the bound RPC address before consuming the handle.
        let rpc_addr = handle.rpc_addr();

        // The fake reports readiness then immediately exits with code 0.
        // `stopped()` should observe the child exit, tear down the RPC
        // server, and return HogdExited.
        let outcome = tokio::time::timeout(Duration::from_secs(5), handle.stopped())
            .await
            .expect("bug: stopped() should observe hogd-dev exit promptly")
            .expect("bug: stopped() should not error on clean child exit");

        match outcome {
            SupervisorStop::HogdExited(status) => {
                assert!(
                    status.success(),
                    "fake should exit cleanly with code 0, got {status:?}",
                );
            }
            SupervisorStop::RpcStopped(_) => {
                panic!("expected HogdExited, got RpcStopped — race lost the wrong way");
            }
            SupervisorStop::StopRequested(_) => {
                panic!("expected HogdExited, got StopRequested");
            }
        }

        // RPC facade should be torn down: a fresh connect attempt must fail.
        // (HttpClientBuilder::build is lazy; the call is what hits the wire.)
        let url = format!("http://{rpc_addr}");
        let client = HttpClientBuilder::default()
            .build(&url)
            .expect("bug: jsonrpsee http client builds");
        let err = client
            .chain_tip()
            .await
            .expect_err("bug: RPC facade should be down after hogd-dev exit");
        // Any client error here is correct (transport closed, refused, etc.).
        // We only assert it's not a successful call mapped to NotImplemented.
        assert!(
            !matches!(err, ClientError::Call(_)),
            "expected transport-level error after RPC shutdown, got JSON-RPC call error: {err:?}",
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_rolls_back_hogd_dev_when_rpc_bind_fails() {
        let _mode_guard = EnvGuard::set(FAKE_HOGD_DEV_MODE_ENV, "ready");
        let pid_dir = TempDir::new().expect("bug: tempdir creates");
        let pid_path = pid_dir.path().join("hogd.pid");
        let _pid_guard = EnvGuard::set(
            FAKE_HOGD_DEV_PID_FILE_ENV,
            pid_path.to_str().expect("bug: tempdir path is utf-8"),
        );

        // Occupy a loopback port so the supervisor's RPC bind hits EADDRINUSE.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: ephemeral bind succeeds");
        let occupied = listener
            .local_addr()
            .expect("bug: bound listener has local addr");

        let hogd = HogdDevConfig::default()
            .with_binary_path(fake_hogd_dev_binary())
            .with_startup_timeout(Duration::from_secs(5))
            .with_shutdown_grace(Duration::from_secs(2));
        let config = NodeSupervisorConfig::default()
            .with_rpc_listen_address(occupied.to_string())
            .with_hogd_dev(hogd);

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            NodeSupervisorHandle::start(config, b""),
        )
        .await
        .expect("bug: start should roll back and return promptly on rpc bind failure");

        let err = result.expect_err("bug: occupied port should fail rpc bind");
        assert!(
            matches!(err, NodeSupervisorError::RpcBind { .. }),
            "expected RpcBind, got {err:?}",
        );

        // The fake wrote its PID before the readiness line; if the rollback
        // path didn't actually reap the child, the OS would still know
        // about this pid.
        let pid =
            read_pid_file(&pid_path).expect("bug: fake should have written pid before readiness");
        assert!(
            wait_pid_gone(pid, Instant::now() + Duration::from_secs(2)).await,
            "rollback leaked fake hogd-dev pid {pid}",
        );

        drop(listener);
    }
}
