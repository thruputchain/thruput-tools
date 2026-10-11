//! Groundhog JSON-RPC benchmark entry point.

#[tokio::main]
async fn main() {
    groundhog_bench::run_cli().await;
}
