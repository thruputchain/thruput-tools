//! Seed/import confirmed records outside the measurement window.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thruput_json_rpc_types::hex::sha256_prefixed_hex;

use serde::{Deserialize, Serialize};
use thruput_json_rpc_types::{
    NetworkHash,
    network_info::NetworkInfo,
    receipt::TransactionReceipt,
    runtime_manifest::RuntimeManifest,
    transaction::{TransactionStatus, TxHash},
};
use tokio::io::AsyncReadExt;

use crate::{
    BenchError,
    cli::Cli,
    request::request_value,
    workload::{generate_tx_batch, transaction_inputs_for_method},
};

const MAX_CORPUS_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_SEED_COUNT: usize = 100_000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HashCorpus {
    schema_version: u32,
    network_hash: NetworkHash,
    hashes: Vec<TxHash>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ConfirmedRecord {
    pub(super) hash: TxHash,
    pub(super) height: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CorpusIdentity {
    pub(super) count: usize,
    pub(super) sha256: String,
    pub(super) shape_sha256: String,
    pub(super) network_hash: NetworkHash,
    pub(super) seed_nonce_start: Option<u64>,
    pub(super) fixture: Option<String>,
}

#[derive(Debug)]
pub(super) struct ConfirmedCorpus {
    pub(super) identity: CorpusIdentity,
    pub(super) records: Vec<ConfirmedRecord>,
}

pub(super) async fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, BenchError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|source| BenchError::File {
            path: path.display().to_string(),
            source,
        })?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| BenchError::File {
            path: path.display().to_string(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(BenchError::InvalidConfiguration(format!(
            "{} exceeds {limit} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

pub(super) async fn rpc(
    client: &reqwest::Client,
    cli: &Cli,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, BenchError> {
    let body = serde_json::json!({"jsonrpc":"2.0", "id":0, "method":format!("groundhog_{method}"), "params":params});
    request_value(
        client.post(&cli.url).json(&body).send(),
        0,
        Duration::from_millis(cli.request_timeout_ms),
        cli.max_response_bytes,
    )
    .await
    .map_err(|kind| BenchError::Setup(format!("{method}: {kind:?}")))
}

fn validate_file(corpus: &HashCorpus) -> Result<(), BenchError> {
    if corpus.schema_version != 1
        || corpus.hashes.is_empty()
        || corpus.hashes.len() > MAX_SEED_COUNT
    {
        return Err(BenchError::Setup(
            "corpus requires schemaVersion 1 and 1..=100000 hashes".into(),
        ));
    }
    let unique: HashSet<_> = corpus.hashes.iter().collect();
    if unique.len() != corpus.hashes.len() {
        return Err(BenchError::Setup("corpus contains duplicate hashes".into()));
    }
    Ok(())
}

pub(super) async fn prepare(
    cli: &Cli,
    manifest: Option<&RuntimeManifest>,
    client: &reqwest::Client,
) -> Result<ConfirmedCorpus, BenchError> {
    let mut progress = "reading network identity".to_owned();
    tokio::time::timeout(
        Duration::from_secs(cli.setup_timeout_secs),
        prepare_inner(cli, manifest, client, &mut progress),
    )
    .await
    .map_err(|_| {
        BenchError::Setup(format!(
            "seed/import validation deadline exceeded while {progress}; no benchmark was measured"
        ))
    })?
}

async fn prepare_inner(
    cli: &Cli,
    manifest: Option<&RuntimeManifest>,
    client: &reqwest::Client,
    progress: &mut String,
) -> Result<ConfirmedCorpus, BenchError> {
    let network: NetworkInfo =
        serde_json::from_value(rpc(client, cli, "networkInfo", serde_json::json!([])).await?)
            .map_err(|error| BenchError::Setup(format!("invalid networkInfo: {error}")))?;
    if let Some(manifest) = manifest
        && network.network_hash.as_ref()
            != manifest
                .network_hash
                .to_network_hash()
                .into_inner()
                .as_ref()
    {
        return Err(BenchError::Setup(
            "runtime manifest and RPC target have different network hashes".into(),
        ));
    }
    let mut nonce_start = None;
    let corpus = if let Some(path) = &cli.hashes_file {
        let corpus: HashCorpus =
            serde_json::from_slice(&read_bounded(path, MAX_CORPUS_BYTES).await?)
                .map_err(|error| BenchError::Setup(format!("invalid hashes file: {error}")))?;
        validate_file(&corpus)?;
        if corpus.network_hash != network.network_hash {
            return Err(BenchError::Setup(
                "corpus and RPC target have different network hashes".into(),
            ));
        }
        corpus
    } else {
        let count = cli.seed_count.ok_or_else(|| {
            BenchError::InvalidConfiguration(
                "confirmed reads need --hashes-file or --seed-count".into(),
            )
        })?;
        let manifest = manifest.ok_or(BenchError::MissingRuntimeManifest)?;
        let accounts = transaction_inputs_for_method(
            "sendRawTransaction",
            Some(manifest),
            cli.accounts,
            cli.account_seed.as_deref(),
        )?;
        let first_nonce = match cli.seed_nonce_start {
            Some(value) => value,
            None => u64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| BenchError::Setup(error.to_string()))?
                    .as_nanos(),
            )
            .map_err(|_| BenchError::Setup("current time does not fit a seed nonce".into()))?,
        };
        nonce_start = Some(first_nonce);
        let network_hash = manifest.network_hash.to_network_hash();
        *progress = format!("generating {count} seed transactions");
        let batch = tokio::task::spawn_blocking(move || {
            generate_tx_batch(count, &accounts, &network_hash, first_nonce)
        })
        .await
        .map_err(|source| BenchError::WorkerJoin { source })??;
        let mut hashes = Vec::with_capacity(count);
        for (index, tx) in batch.into_iter().enumerate() {
            *progress = format!("submitting seed {} of {count}", index + 1);
            let value = rpc(
                client,
                cli,
                "sendRawTransaction",
                serde_json::json!([tx.hex]),
            )
            .await?;
            let hash: TxHash = serde_json::from_value(value)
                .map_err(|error| BenchError::Setup(format!("invalid submission hash: {error}")))?;
            if hash != tx.hash {
                return Err(BenchError::Setup(
                    "submission returned a different transaction hash".into(),
                ));
            }
            hashes.push(hash);
        }
        HashCorpus {
            schema_version: 1,
            network_hash: network.network_hash,
            hashes,
        }
    };
    let mut records = Vec::with_capacity(corpus.hashes.len());
    let mut shapes = Vec::with_capacity(corpus.hashes.len());
    for (index, hash) in corpus.hashes.iter().enumerate() {
        *progress = format!("confirming hash {} of {}", index + 1, corpus.hashes.len());
        loop {
            let status: TransactionStatus = serde_json::from_value(
                rpc(
                    client,
                    cli,
                    "getTransactionStatus",
                    serde_json::json!([hash]),
                )
                .await?,
            )
            .map_err(|error| BenchError::Setup(format!("invalid status: {error}")))?;
            if let TransactionStatus::Committed { height } = status {
                let receipt: Option<TransactionReceipt> = serde_json::from_value(
                    rpc(
                        client,
                        cli,
                        "getTransactionReceipt",
                        serde_json::json!([hash]),
                    )
                    .await?,
                )
                .map_err(|error| BenchError::Setup(format!("invalid receipt: {error}")))?;
                let Some(receipt) = receipt else {
                    *progress = format!("waiting for committed hash {hash:?} to be indexed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                if receipt.tx_hash != *hash || receipt.height != height {
                    return Err(BenchError::Setup(
                        "receipt hash/height differs from committed status".into(),
                    ));
                }
                shapes.push(serde_json::to_string(&serde_json::json!({
                    "status": receipt.status,
                    "returnBytes": receipt.return_data.as_ref().map(|bytes| bytes.0.len()),
                    "abortBytes": receipt.abort_data.as_ref().map(|bytes| bytes.0.len()),
                    "events": receipt.events.iter().map(|event| (event.topics.len(), event.data.0.len())).collect::<Vec<_>>()
                })).expect("bug: receipt shape serializes"));
                records.push(ConfirmedRecord {
                    hash: *hash,
                    height: height.into_inner(),
                });
                break;
            }
            if cli.hashes_file.is_some()
                || matches!(
                    status,
                    TransactionStatus::Invalid { .. }
                        | TransactionStatus::Retry { .. }
                        | TransactionStatus::Dropped { .. }
                        | TransactionStatus::Other
                )
            {
                return Err(BenchError::Setup(format!(
                    "hash is not committed: {status:?}"
                )));
            }
            let label = match status {
                TransactionStatus::Pending => "pending",
                TransactionStatus::RecentFailure { .. } => "recentFailure",
                _ => "unknown",
            };
            *progress = format!(
                "confirming hash {} of {} (last status: {label})",
                index + 1,
                corpus.hashes.len()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let bytes = serde_json::to_vec(&corpus).expect("bug: corpus serializes");
    // Refuse replacement, including through symlinks. A failed setup never publishes a corpus.
    if let Some(path) = &cli.seed_output {
        use std::io::Write;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| BenchError::File {
                path: path.display().to_string(),
                source,
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|source| BenchError::File {
                path: path.display().to_string(),
                source,
            })?;
        file.persist_noclobber(path)
            .map_err(|error| BenchError::File {
                path: path.display().to_string(),
                source: error.error,
            })?;
    }
    shapes.sort();
    Ok(ConfirmedCorpus {
        identity: CorpusIdentity {
            count: records.len(),
            sha256: sha256_prefixed_hex(&bytes),
            shape_sha256: sha256_prefixed_hex(
                &serde_json::to_vec(&shapes).expect("bug: shape list serializes"),
            ),
            network_hash: corpus.network_hash,
            seed_nonce_start: nonce_start,
            fixture: cli
                .seed_count
                .map(|_| "native-wallet-hog-balance-v1".into()),
        },
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn corpus_rejects_unknown_version_empty_or_duplicate_hashes() {
        let hash = thruput_json_rpc_types::NetworkHashText::from([0xaa; 32]).to_string();
        for (version, hashes) in [
            (2, vec![hash.clone()]),
            (1, vec![]),
            (1, vec![hash.clone(), hash.clone()]),
        ] {
            let corpus: HashCorpus = serde_json::from_value(
                json!({"schemaVersion":version,"networkHash":hash,"hashes":hashes}),
            )
            .unwrap();
            assert!(validate_file(&corpus).is_err());
        }
        let corpus: HashCorpus =
            serde_json::from_value(json!({"schemaVersion":1,"networkHash":hash,"hashes":[hash]}))
                .unwrap();
        assert!(validate_file(&corpus).is_ok());
    }
}
