use super::*;
use std::{future::Future, os::unix::fs::PermissionsExt, task::Poll};

#[allow(dead_code)]
#[path = "../../../src/test_support.rs"]
mod test_support;

// A failed regression must not leave its deliberately uncooperative process alive.
struct ProcessGuard(Option<i32>);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        // SAFETY: the supervisor creates a process group whose ID is its child PID.
        if let Some(pid) = self.0 {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
}

fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 checks this child PID without sending a signal.
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn cancelled_stop(gateway: bool) {
    let directory = tempfile::tempdir().unwrap();
    let binary = test_support::fake_hogd_dev_binary();
    let wrapper = directory.path().join("ignore-term-runtime");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport {}=ignore-term\nexec '{}' \"$@\"\n",
            test_support::FAKE_HOGD_DEV_MODE_ENV,
            binary.to_str().unwrap().replace('\'', "'\\''"),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_block_driver(BlockDriverConfig::manual())
        .with_hogd_dev(
            HogdDevConfig::default()
                .with_binary_path(wrapper)
                .with_data_dir(directory.path().join("data"))
                .with_shutdown_grace(Duration::from_millis(200)),
        );
    let node = NodeSupervisorHandle::start(config.clone()).await.unwrap();
    let pid = i32::try_from(node.hogd_child_id().unwrap()).unwrap();
    let mut guard = ProcessGuard(Some(pid));
    let gateway = gateway.then(|| {
        tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    });
    let gateway_pid = gateway
        .as_ref()
        .map(|child| i32::try_from(child.id().unwrap()).unwrap());
    let mut fixture = Fixture {
        directory,
        config,
        node: Some(node),
        gateway,
        starts: 0,
        shutdown: None,
    };

    // Poll shutdown into its first wait, then cancel exactly as timeout drops it.
    // The current-thread executor keeps the new shutdown task from racing this poll.
    {
        let mut stop = std::pin::pin!(fixture.stop());
        std::future::poll_fn(|cx| {
            assert!(stop.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    assert!(process_exists(pid), "runtime must still need cleanup");
    let error = tokio::time::timeout(Duration::from_secs(5), fixture.stop())
        .await
        .expect("final cleanup must finish")
        .expect_err("SIGKILL must not count as a clean snapshot shutdown");
    assert!(
        error
            .to_string()
            .contains("runtime did not shut down cleanly")
    );
    assert!(!process_exists(pid), "cleanup must reap the runtime");
    if let Some(pid) = gateway_pid {
        assert!(!process_exists(pid), "cleanup must reap the gateway");
    }
    // Cleanup after a completed stop must remain harmless.
    fixture.stop().await.unwrap();
    guard.0 = None;
}

#[tokio::test]
async fn cancelled_stop_reaps_persistent_runtime_that_ignores_sigterm() {
    cancelled_stop(false).await;
}

#[tokio::test]
async fn cancelled_stop_reaps_gateway_and_persistent_runtime() {
    cancelled_stop(true).await;
}
