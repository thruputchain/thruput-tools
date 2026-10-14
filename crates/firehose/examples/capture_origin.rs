//! Local-only fixture capture: archive a fresh node from its origin and write
//! each verified record as hex. `hog-firehose-store tail` is tip-only and
//! rejects a node that committed its bootstrap block before the tailer started,
//! so fixtures of historical heights come from this driver instead. It appends
//! through the same verifying store, using procedure 10 only.
use std::{path::Path, time::Duration};
use thruput_firehose::{
    client::{self, Client, Outcome},
    store::Store,
};
use thruput_indexer::EventIndexerPullResult;
use thruput_xdr::PROTOCOL_VERSION;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .ok_or("usage: capture_origin EVENT_INDEXER ARCHIVE OUT_DIR")?;
    let archive = Path::new(args.get(2).ok_or("missing archive")?);
    let out = Path::new(args.get(3).ok_or("missing output directory")?);
    std::fs::create_dir_all(archive)?;
    std::fs::create_dir_all(out)?;
    let mut store = Store::open(archive)?;
    let client = Client::connect(endpoint).await?;
    loop {
        // An initialized cursor parks at the tip; stop rather than wait.
        let pull = client.pull(PROTOCOL_VERSION, store.cursor(false));
        let Ok(result) = tokio::time::timeout(Duration::from_secs(3), pull).await else {
            break;
        };
        let result = result?;
        let outcome = client::outcome(&result);
        let EventIndexerPullResult::EventIndexerPullBlock { pulled_block } = result else {
            if !matches!(outcome, Outcome::CaughtUp) {
                return Err(format!("pull outcome {}", outcome.label()).into());
            }
            break;
        };
        store.append(&pulled_block)?;
        let payload = &pulled_block.block.payload;
        let events: usize = payload.event_logs.iter().map(|log| log.events.len()).sum();
        let height = store.height();
        std::fs::write(
            out.join(format!("{height}.hex")),
            hex::encode(xdrs::txs::encode(&pulled_block)?),
        )?;
        println!(
            "{}",
            serde_json::json!({"height": height, "transactions": payload.tx_bodies.len(), "events": events})
        );
    }
    println!("{}", serde_json::json!({"caughtUpHeight": store.height()}));
    Ok(())
}
