use std::time::Duration;

use groundhog_testkit::clock::Clock;

#[tokio::test]
async fn sleep_waits_until_the_clock_advances() {
    let clock = Clock::new();
    let sleeper_clock = clock.clone();
    let sleeper = tokio::spawn(async move {
        sleeper_clock.sleep_ms(100).await;
        sleeper_clock.now_ms()
    });

    // Let the sleeper register its deadline, then confirm it is still
    // pending because virtual time has not moved.
    tokio::task::yield_now().await;
    assert!(
        !sleeper.is_finished(),
        "sleeper resolved before the clock advanced"
    );

    clock.advance_ms(100);

    let woke_at = tokio::time::timeout(Duration::from_millis(500), sleeper)
        .await
        .expect("sleeper resolves once the clock passes its deadline")
        .expect("sleeper task did not panic");
    assert_eq!(woke_at, 100);
}

#[tokio::test]
async fn zero_duration_sleep_returns_immediately() {
    let clock = Clock::new();
    clock.sleep_ms(0).await;
    assert_eq!(clock.now_ms(), 0);
}
