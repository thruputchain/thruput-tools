//! Groundhog JSON-RPC benchmark entry point.

#[tokio::main]
async fn main() {
    thruput_bench::run_cli().await;
}
