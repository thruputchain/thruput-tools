use super::*;

#[tokio::test]
async fn shutdown_passes_separate_exporter_budget() {
    let (capture, provider, _) = test_support::subscriber();
    shutdown(Some(provider)).await;
    assert_eq!(
        *capture.shutdown_timeout.lock().unwrap(),
        Some(Duration::from_secs(1))
    );
}
