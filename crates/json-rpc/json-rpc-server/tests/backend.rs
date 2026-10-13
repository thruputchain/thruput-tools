use std::time::Duration;

use thruput_json_rpc_server::backend::RpcRequestContext;

#[test]
fn cap_from_start_does_not_reset_after_elapsed_work() {
    let context = RpcRequestContext::new(Some(Duration::from_secs(10)));
    let expected = context
        .started_at()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline should fit");

    let capped = context.capped_from_start(Duration::from_secs(5));

    assert_eq!(capped.started_at(), context.started_at());
    assert_eq!(capped.deadline(), Some(expected));
}
