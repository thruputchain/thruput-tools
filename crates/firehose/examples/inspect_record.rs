//! Offline inspection of one exact `EventIndexerPulledBlock` per file.
use std::{collections::BTreeMap, path::PathBuf};

use clap::Parser;
use serde_json::{Value, json};
use thruput_firehose::Error;
use thruput_indexer::EventIndexerPulledBlock;
use thruput_xdr::exec_result::ExecResult;

#[derive(Debug, Parser)]
#[command(about = "Inspect archived firehose XDR records without a node")]
struct Args {
    /// Emit one JSON object per file (the default).
    #[arg(long, conflicts_with = "text")]
    json: bool,
    /// Emit an indented, readable report.
    #[arg(long)]
    text: bool,
    /// Files containing exactly one `EventIndexerPulledBlock` each.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

fn encode<T: xdrs::WriteXDR>(value: &T) -> Result<Vec<u8>, Error> {
    xdrs::txs::encode(value).map_err(|e| Error::Xdr(e.to_string()))
}

fn encoded_len<T: xdrs::WriteXDR>(value: &T) -> Result<usize, Error> {
    Ok(encode(value)?.len())
}

fn hex<T: xdrs::WriteXDR>(value: &T) -> Result<String, Error> {
    Ok(format!("0x{}", hex::encode(encode(value)?)))
}

fn increment<K: Ord>(counts: &mut BTreeMap<K, usize>, key: K) {
    *counts.entry(key).or_default() += 1;
}

// Numeric keys stay numeric while sorting, including tie breaks. Wide values
// become decimal strings only at the JSON boundary.
fn top<K: Ord + ToString>(counts: BTreeMap<K, usize>, limit: usize) -> Vec<Value> {
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter()
        .take(limit)
        .map(|(value, count)| json!({"value": value.to_string(), "count": count}))
        .collect()
}

fn transaction_report(
    body: &thruput_xdr::transaction::ProposedTransaction,
    log: &thruput_xdr::event::TxEventLog,
    index: usize,
    kind: &str,
) -> Result<Value, Error> {
    let signed = &body.signed_tx;
    let tx = &signed.tx;
    let signatures = signed
        .signatures
        .iter()
        .map(|signature| {
            Ok(json!({
                "key_id": signature.public_key_id,
                "scheme": signature.signature.scheme().to_string(),
                "encoded_bytes": encoded_len(&signature.signature)?,
                "decorated_bytes": encoded_len(signature)?
            }))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(json!({
            "index": index,
            "tx_hash": hex(&log.tx_hash)?,
            "kind": kind,
            "contract_id": hex(&tx.invocation.invoked_address)?,
            "selector": hex(&tx.invocation.entrypoint)?,
            "nonce": tx.uniquifier.nonce.to_string(),
            "gas_limit": tx.invocation.gas_limit.to_string(),
            "expiry_height": tx.uniquifier.block_expiry_height.to_string(),
            "deployment_count": tx.contracts_to_deploy.len(),
            "encoded_bytes": encoded_len(body)?,
            "signed_transaction_bytes": encoded_len(signed)?,
            "auth": {"variant": tx.auth_claim.claim_type().to_string(),
                     "encoded_bytes": encoded_len(&tx.auth_claim)?},
            "signatures": signatures,
            "outcome": log.exec_result.to_string(),
            "success": log.exec_result == ExecResult::ExecResultOK,
            "root_output_variant": log.root_output.status_code().to_string()
    }))
}

fn inspect(bytes: &[u8]) -> Result<Value, Error> {
    // The same decoder used by store recovery rejects trailing bytes as well.
    let record: EventIndexerPulledBlock =
        xdrs::txs::decode(bytes).map_err(|e| Error::Xdr(e.to_string()))?;
    let payload = &record.block.payload;
    if payload.tx_bodies.len() != payload.event_logs.len() {
        return Err(Error::Invalid("event/body cardinality mismatch".into()));
    }
    let mut sizes = Vec::new();
    let mut size_counts = BTreeMap::new();
    let mut kinds = BTreeMap::new();
    let mut contracts = BTreeMap::new();
    let mut functions = BTreeMap::new();
    let mut gas = BTreeMap::new();
    let mut expiries = BTreeMap::new();
    let mut transactions = Vec::new();
    for (index, (body, log)) in payload
        .tx_bodies
        .iter()
        .zip(payload.event_logs.iter())
        .enumerate()
    {
        let signed = &body.signed_tx;
        let tx = &signed.tx;
        // The archive batch contains ProposedTransaction, not just Transaction.
        let size = encoded_len(body)?;
        sizes.push(size);
        increment(&mut size_counts, size);
        let kind = if tx.contracts_to_deploy.is_empty() {
            "contract_invoke"
        } else {
            "contract_invoke_with_deployments"
        };
        increment(&mut kinds, kind);
        let contract = hex(&tx.invocation.invoked_address)?;
        let selector = hex(&tx.invocation.entrypoint)?;
        increment(&mut contracts, contract.clone());
        increment(&mut functions, format!("{contract}:{selector}"));
        increment(&mut gas, tx.invocation.gas_limit);
        increment(&mut expiries, *tx.uniquifier.block_expiry_height);
        transactions.push(transaction_report(body, log, index, kind)?);
    }
    sizes.sort_unstable();
    let distribution = if sizes.is_empty() {
        Value::Null
    } else {
        json!({"min": sizes[0], "p50": sizes[(sizes.len() - 1) / 2],
               "max": sizes[sizes.len() - 1]})
    };
    let mut event_contracts = BTreeMap::new();
    let mut topics = BTreeMap::new();
    let mut event_count = 0;
    let mut success = 0;
    let mut outcomes = BTreeMap::new();
    for log in payload.event_logs.iter() {
        success += usize::from(log.exec_result == ExecResult::ExecResultOK);
        increment(&mut outcomes, log.exec_result.to_string());
        for event in log.events.iter() {
            event_count += 1;
            increment(&mut event_contracts, hex(&event.contract_address)?);
            let topic_bytes = event.num_topics as usize * 32;
            if event.num_topics > thruput_xdr::event::kMaxTopics
                || topic_bytes > event.serialized_event.len()
            {
                return Err(Error::Invalid("invalid event topic count/length".into()));
            }
            for topic in event.serialized_event[..topic_bytes].chunks_exact(32) {
                increment(&mut topics, format!("0x{}", hex::encode(topic)));
            }
        }
    }
    Ok(json!({
        "height": payload.consensus_value.proposal_header.block_height.to_string(),
        "consensus_hash": hex(&record.block.identity.consensus_hash)?,
        "origin": {"protocol": std::str::from_utf8(&record.origin.protocol)
                       .map_or_else(|_| format!("0x{}", hex::encode(record.origin.protocol)), str::to_owned),
                   "first_height": record.origin.first_height.to_string()},
        "record_bytes": bytes.len(),
        "bytes": {"consensus": encoded_len(&payload.consensus_value)?,
                  "event_logs": encoded_len(&payload.event_logs)?,
                  "transaction_bodies": encoded_len(&payload.tx_bodies)?},
        "transaction_count": sizes.len(),
        "transaction_size_distribution": distribution,
        "transaction_size_histogram_top10": top(size_counts, 10),
        "by_kind": top(kinds, usize::MAX),
        "by_contract_top10": top(contracts, 10),
        "by_function_top10": top(functions, 10),
        "gas_limit_histogram_top5": top(gas, 5),
        "expiry_heights_top5": top(expiries, 5),
        "transactions": transactions,
        "events": {"count": event_count, "by_contract_top10": top(event_contracts, 10),
                   "by_topic_top10": top(topics, 10)},
        "outcomes": {"success": success, "failure": sizes.len() - success,
                     "by_result": top(outcomes, usize::MAX)},
        "notes": [
            "No explicit sender/source account exists in Transaction or Invocation at XDR 0.54.1; sender counts, shares and per-sender nonce ranges are unavailable. Auth claims do not name a source address.",
            "Transaction is a struct, not an operation union: all transactions invoke a contract, optionally carrying contracts_to_deploy. Native transfers cannot be distinguished without contract-specific calldata knowledge.",
            "No fee limit or function/event names are stored. Selectors and topics are reported as hex; function groups use contract:selector and topic counts include every topic position.",
            "Size distribution uses encoded ProposedTransaction bytes, including signatures and proposal data; batch byte totals also include the XDR array length. p50 is the nearest-rank median (lower middle for even counts).",
            "Signature encoded_bytes includes the scheme discriminator; decorated_bytes also includes key_id. Inspection decodes records but does not verify commitments or signatures."
        ]
    }))
}

fn print_text(value: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if value.is_object() || value.is_array() {
                    println!("{pad}{key}:");
                    print_text(value, indent + 2);
                } else {
                    println!("{pad}{key}: {value}");
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                println!("{pad}-");
                print_text(item, indent + 2);
            }
        }
        Value::String(s) => println!("{pad}{s}"),
        _ => println!("{pad}{value}"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut failed = false;
    for path in args.paths {
        let result = std::fs::read(&path)
            .map_err(Error::from)
            .and_then(|b| inspect(&b));
        match result {
            Ok(mut report) => {
                report["file"] = json!(path);
                if args.text {
                    print_text(&report, 0);
                } else {
                    println!("{}", serde_json::to_string(&report)?);
                }
            }
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(text: &str) -> Vec<u8> {
        hex::decode(text.split_whitespace().collect::<String>()).unwrap()
    }

    #[test]
    fn live_fixtures_report_bytes_auth_events_and_outcomes() {
        let bootstrap = inspect(&fixture(include_str!("../tests/fixtures/bootstrap.hex"))).unwrap();
        assert_eq!(bootstrap["record_bytes"], 1096);
        assert_eq!(
            bootstrap["bytes"],
            json!({"consensus":288,"event_logs":504,"transaction_bodies":200})
        );
        assert_eq!(bootstrap["events"]["count"], 4);
        assert_eq!(bootstrap["outcomes"]["success"], 1);
        assert_eq!(bootstrap["transactions"][0]["deployment_count"], 0);
        assert_eq!(
            bootstrap["transactions"][0]["auth"]["variant"],
            "AuthClaimType::kSourceAddressAuthData"
        );
        assert_eq!(
            bootstrap["transactions"][0]["gas_limit"],
            u64::MAX.to_string()
        );
        let hundred = inspect(&fixture(include_str!(
            "../tests/fixtures/hundred-transactions.hex"
        )))
        .unwrap();
        assert_eq!(hundred["transaction_count"], 100);
        assert_eq!(
            hundred["transaction_size_distribution"],
            json!({"min":324,"p50":324,"max":324})
        );
        assert_eq!(hundred["outcomes"]["success"], 100);
        assert_eq!(hundred["events"]["count"], 0);
        assert_eq!(
            hundred["transactions"][0]["signatures"][0]["encoded_bytes"],
            68
        );
    }

    #[test]
    fn empty_block_and_exact_record_boundaries() {
        let bytes = fixture(include_str!("../tests/fixtures/bootstrap.hex"));
        let mut block: EventIndexerPulledBlock = xdrs::txs::decode(&bytes).unwrap();
        block.block.payload.tx_bodies.0.clear();
        assert!(inspect(&encode(&block).unwrap()).is_err());
        block.block.payload.event_logs.0.clear();
        let report = inspect(&encode(&block).unwrap()).unwrap();
        assert_eq!(report["transaction_count"], 0);
        assert!(report["transaction_size_distribution"].is_null());
        let mut trailing = bytes.clone();
        trailing.extend_from_slice(&[0; 4]);
        assert!(inspect(&trailing).is_err());
        assert!(inspect(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn histogram_orders_by_frequency_then_numeric_value() {
        assert_eq!(
            top(BTreeMap::from([(100, 2), (20, 2), (5, 3)]), 2),
            vec![
                json!({"value":"5","count":3}),
                json!({"value":"20","count":2})
            ]
        );
    }
}
