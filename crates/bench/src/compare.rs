//! Controlled trial comparisons; paths and ports are provenance, not controls.
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(clap::Args)]
pub(super) struct CompareArgs {
    /// Baseline trial files (repeat this option for repeats).
    #[arg(long, required = true)]
    baseline: Vec<PathBuf>,
    /// Candidate trial files (repeat this option for repeats).
    #[arg(long, required = true)]
    candidate: Vec<PathBuf>,
    /// Intentional experiment dimensions; all other material controls must match.
    #[arg(long, value_enum)]
    vary: Vec<Dimension>,
    /// Compare equivalent receipt shapes rather than the exact confirmed corpus.
    #[arg(long)]
    equivalent_corpus: bool,
    /// Emit the machine-readable comparison instead of Markdown.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Dimension {
    GatewayRevision,
    PoolSize,
    BuildSettings,
    Allocator,
    Logging,
}

pub(super) fn run(args: CompareArgs) -> Result<(), String> {
    let read = |paths: &[PathBuf]| -> Result<Vec<Value>, String> {
        paths
            .iter()
            .map(|path| {
                let file =
                    std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                use std::io::Read;
                let mut bytes = Vec::new();
                file.take(64 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if bytes.len() > 64 * 1024 * 1024 {
                    return Err("result exceeds 64 MiB".into());
                }
                let value: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                validate(&value)?;
                Ok(value)
            })
            .collect()
    };
    let baseline = read(&args.baseline)?;
    let candidate = read(&args.candidate)?;
    for group in [&baseline, &candidate] {
        let reference = controls_with_dimensions(&group[0], &args, &[]);
        for trial in group.iter().skip(1) {
            if controls_with_dimensions(trial, &args, &[]) != reference {
                return Err("repeats within a trial set have different material controls (including the varied dimension)".into());
            }
        }
    }
    let reference = controls(&baseline[0], &args);
    for value in baseline.iter().chain(&candidate) {
        let actual = controls(value, &args);
        if actual != reference {
            let mut differences = Vec::new();
            differences_at("", &reference, &actual, &mut differences);
            return Err(format!(
                "undeclared material differences: {}",
                differences.join(", ")
            ));
        }
    }
    let mut unknown = BTreeSet::new();
    unknown_at("controls", &reference, &mut unknown);
    let before = summarize(&baseline);
    let after = summarize(&candidate);
    let base_rate = before["throughput"]["median"]
        .as_f64()
        .ok_or("missing baseline throughput")?;
    let new_rate = after["throughput"]["median"]
        .as_f64()
        .ok_or("missing candidate throughput")?;
    let result = json!({
        "schemaVersion":1, "kind":"groundhog-bench-comparison",
        "variedDimensions":args.vary.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>(),
        "corpusPolicy":if args.equivalent_corpus {"equivalent-shape"} else {"exact"},
        "unknownControls":unknown,
        "baselineFiles":args.baseline,"candidateFiles":args.candidate,
        "baseline":before,"candidate":after,
        "throughputChangePercent":if base_rate > 0.0 { Some((new_rate/base_rate - 1.0)*100.0) } else {None},
        "cacheState":"not inferred; corpus validation warms reads before every trial",
    });
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "| Trial set | Repeats | Successful req/s median [min, max] | p50 µs | p95 µs | p99 µs | Errors | Backend rejected | Backend timeout | CPU s | RSS peak bytes |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        );
        for (name, row) in [("Baseline", &before), ("Candidate", &after)] {
            println!(
                "| {name} | {} | {} [{}, {}] | {} | {} | {} | {} | {} | {} | {} | {} |",
                row["repeats"],
                display(&row["throughput"]["median"]),
                display(&row["throughput"]["min"]),
                display(&row["throughput"]["max"]),
                display(&row["p50Us"]["median"]),
                display(&row["p95Us"]["median"]),
                display(&row["p99Us"]["median"]),
                row["errors"],
                display(&row["backendRejected"]["median"]),
                display(&row["backendTimeout"]["median"]),
                display(&row["cpuSeconds"]["median"]),
                display(&row["rssPeakBytes"]["max"])
            );
        }
        println!(
            "\nThroughput change: {}%. Unknown controls: {}.\nCache state is not inferred; corpus validation warms reads before every trial.\nBackend counters, gauge peaks and unavailable reasons are retained in JSON output.",
            display(&result["throughputChangePercent"]),
            result["unknownControls"]
        );
    }
    Ok(())
}

fn display(value: &Value) -> String {
    value
        .as_f64()
        .map_or_else(|| "unavailable".into(), |v| format!("{v:.2}"))
}

fn validate(v: &Value) -> Result<(), String> {
    if !v["target"]["runnerSupplied"]["diagnostic"].is_null() {
        return Err("diagnostic captures cannot be used for performance comparison".into());
    }
    if v["schemaVersion"] != 2 || v["kind"] != "groundhog-bench-result" {
        return Err("compare requires benchmark schemaVersion 2".into());
    }
    for key in [
        "configuration",
        "target",
        "platform",
        "measurement",
        "requests",
        "drained",
        "rates",
    ] {
        if !v[key].is_object() {
            return Err(format!("missing result {key}"));
        }
    }
    for pointer in [
        "/rates/successfulRequestsPerSecond",
        "/requests/failed",
        "/drained/failed",
    ] {
        if v.pointer(pointer)
            .and_then(Value::as_f64)
            .is_none_or(|n| !n.is_finite() || n < 0.0)
        {
            return Err(format!("invalid result {pointer}"));
        }
    }
    if !matches!(v["outcome"].as_str(), Some("passed" | "failed" | "empty")) {
        return Err("missing result outcome".into());
    }
    Ok(())
}

fn controls(v: &Value, args: &CompareArgs) -> Value {
    controls_with_dimensions(v, args, &args.vary)
}

fn controls_with_dimensions(v: &Value, args: &CompareArgs, dimensions: &[Dimension]) -> Value {
    let mut gateway = v["target"]["runnerSupplied"]["gateway"].clone();
    if let Some(fields) = gateway.as_object_mut() {
        fields.entry("logging").or_insert(Value::Null);
    }
    let mut version = v["target"]["gatewayVersion"].clone();
    if let Some(config) = gateway
        .get_mut("effectiveConfig")
        .and_then(Value::as_object_mut)
    {
        for key in [
            "listen_address",
            "observability_listen",
            "scheduler_address",
            "indexer_address",
            "db_gateway_address",
            "view_gateway_address",
        ] {
            if let Some(value) = config.get_mut(key)
                && value.is_string()
            {
                *value = json!("configured endpoint; see run provenance");
            }
        }
    }
    for dimension in dimensions {
        match dimension {
            Dimension::GatewayRevision => {
                remove(&mut gateway, &["artifactSha256", "commitHash", "gitDirty"]);
                if let Some(value) = version.get_mut("value") {
                    remove(
                        value,
                        &[
                            "commitHash",
                            "implementation",
                            "platformVersion",
                            "releaseVersion",
                            "bundleManifestDigest",
                        ],
                    );
                }
            }
            Dimension::PoolSize => {
                if let Some(config) = gateway.get_mut("effectiveConfig") {
                    remove(config, &["indexerPoolSize", "indexer_pool_size"]);
                }
            }
            Dimension::BuildSettings => {
                remove(
                    &mut gateway,
                    &["artifactSha256", "buildProfile", "features"],
                );
            }
            Dimension::Logging => remove(&mut gateway, &["logging"]),
            Dimension::Allocator => {
                remove(&mut gateway, &["artifactSha256"]);
                if let Some(config) = gateway.get_mut("effectiveConfig") {
                    remove(config, &["allocator"]);
                }
            }
        }
    }
    json!({
        "configuration":v["configuration"], "platform":v["platform"],
        "benchmarkBinarySha256":v["benchmarkBinarySha256"],
        "runtimeWorkerThreads":v["runtimeWorkerThreads"],
        "gateway":gateway, "gatewayVersion":version,
        "backend":v["target"]["runnerSupplied"]["backend"],
        "topology":v["target"]["runnerSupplied"]["topology"],
        "runtime": {"protocol":v["runtime"]["protocol"], "producer":v["runtime"]["producer"], "chainState":v["runtime"]["chainState"]},
        "measurementInclusion":v["measurement"]["inclusion"],
        "sampleIntervalMs":v["serverObservations"]["intervalMs"],
        "corpus":if v["corpus"].is_null() {Value::Null} else {json!({"count":v["corpus"]["count"], "identity":if args.equivalent_corpus {&v["corpus"]["shapeSha256"]} else {&v["corpus"]["sha256"]}})},
    })
}

fn remove(value: &mut Value, keys: &[&str]) {
    if let Some(map) = value.as_object_mut() {
        for key in keys {
            map.remove(*key);
        }
    }
}
fn unknown_at(path: &str, v: &Value, result: &mut BTreeSet<String>) {
    match v {
        Value::Null => {
            result.insert(path.into());
        }
        Value::Object(map) => {
            if map.get("availability").and_then(Value::as_str) == Some("unavailable") {
                result.insert(path.into());
            } else {
                for (key, value) in map {
                    unknown_at(&format!("{path}.{key}"), value, result);
                }
            }
        }
        _ => {}
    }
}
fn differences_at(path: &str, a: &Value, b: &Value, result: &mut Vec<String>) {
    if a == b {
        return;
    }
    if let (Some(a), Some(b)) = (a.as_object(), b.as_object()) {
        for key in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
            differences_at(
                &format!("{path}/{key}"),
                a.get(key).unwrap_or(&Value::Null),
                b.get(key).unwrap_or(&Value::Null),
                result,
            );
        }
    } else {
        result.push(path.into());
    }
}
fn spread(values: Vec<f64>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut values = values;
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let median = if n.is_multiple_of(2) {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    };
    json!({"min":values[0],"median":median,"max":values[n-1],"samples":n})
}
fn backend_counter(trials: &[Value], outcome: &str) -> Value {
    let counts: Option<Vec<f64>> = trials
        .iter()
        .map(|v| {
            let map = v
                .pointer("/serverObservations/counterDeltas/value")?
                .as_object()?;
            let series: Vec<_> = map
                .iter()
                .filter(|(key, _)| key.starts_with("groundhog_gateway_backend_requests_total{"))
                .collect();
            if series.is_empty() {
                return None;
            }
            series
                .into_iter()
                .filter(|(key, _)| key.contains(&format!("outcome=\"{outcome}\"")))
                .map(|(_, value)| value.as_f64())
                .sum()
        })
        .collect();
    counts.map_or(Value::Null, spread)
}

fn summarize(trials: &[Value]) -> Value {
    let stats = |path: &str| {
        let values: Option<Vec<_>> = trials
            .iter()
            .map(|v| v.pointer(path).and_then(Value::as_f64))
            .collect();
        values.map_or(Value::Null, spread)
    };
    json!({"repeats":trials.len(),
        "throughput":stats("/rates/successfulRequestsPerSecond"),
        "p50Us":stats("/successfulLatencyUs/p50"),"p95Us":stats("/successfulLatencyUs/p95"),"p99Us":stats("/successfulLatencyUs/p99"),
        "cpuSeconds":stats("/serverObservations/cpuSeconds/value"),"rssPeakBytes":stats("/serverObservations/rssPeakBytes/value"),
        "backendRejected":backend_counter(trials,"rejected"), "backendTimeout":backend_counter(trials,"timeout"),
        "errors":trials.iter().map(|v| v["requests"]["failed"].as_u64().unwrap_or(0)+v["drained"]["failed"].as_u64().unwrap_or(0)).sum::<u64>(),
        "outcomes":trials.iter().map(|v| &v["outcome"]).collect::<Vec<_>>(),
        "serverObservations":trials.iter().map(|v| &v["serverObservations"]).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args() -> CompareArgs {
        CompareArgs {
            baseline: vec![],
            candidate: vec![],
            vary: vec![],
            equivalent_corpus: false,
            json: false,
        }
    }
    #[test]
    fn paths_are_provenance_but_pool_size_requires_declaration() {
        let a = json!({"run":{"url":"http://a"},"target":{"runnerSupplied":{"gateway":{"effectiveConfig":{"indexerPoolSize":4}}}}});
        let mut b = a.clone();
        b["run"]["url"] = json!("http://b");
        assert_eq!(controls(&a, &args()), controls(&b, &args()));
        b["target"]["runnerSupplied"]["gateway"]["effectiveConfig"]["indexerPoolSize"] = json!(8);
        assert_ne!(controls(&a, &args()), controls(&b, &args()));
        let mut args = args();
        args.vary.push(Dimension::PoolSize);
        assert_eq!(controls(&a, &args), controls(&b, &args));
    }
    #[test]
    fn revision_changes_keep_wire_format_and_repeat_identity_controlled() {
        let a = json!({"target":{"gatewayVersion":{"availability":"observed","value":{"wireFormat":"3.0.0","implementation":"1","commitHash":"a"}}}});
        let mut b = a.clone();
        b["target"]["gatewayVersion"]["value"]["implementation"] = json!("2");
        b["target"]["gatewayVersion"]["value"]["commitHash"] = json!("b");
        let mut args = args();
        args.vary.push(Dimension::GatewayRevision);
        assert_eq!(controls(&a, &args), controls(&b, &args));
        assert_ne!(
            controls_with_dimensions(&a, &args, &[]),
            controls_with_dimensions(&b, &args, &[])
        );
        b["target"]["gatewayVersion"]["value"]["wireFormat"] = json!("4.0.0");
        assert_ne!(controls(&a, &args), controls(&b, &args));
    }

    #[test]
    fn diagnostic_results_cannot_be_compared() {
        let value = json!({"target":{"runnerSupplied":{"diagnostic":"trace"}}});
        assert!(validate(&value).unwrap_err().contains("diagnostic"));
    }

    #[test]
    fn logging_is_controlled_and_legacy_policy_is_unknown() {
        let a = json!({"target":{"runnerSupplied":{"gateway":{}}}});
        let mut b = a.clone();
        b["target"]["runnerSupplied"]["gateway"]["logging"] = Value::Null;
        assert_eq!(controls(&a, &args()), controls(&b, &args()));
        b["target"]["runnerSupplied"]["gateway"]["logging"] =
            json!({"filter":"warn","sink":"file","spanEvents":"none"});
        assert_ne!(controls(&a, &args()), controls(&b, &args()));
        let mut args = args();
        args.vary.push(Dimension::Logging);
        assert_eq!(controls(&a, &args), controls(&b, &args));
        assert_ne!(
            controls_with_dimensions(&a, &args, &[]),
            controls_with_dimensions(&b, &args, &[])
        );
    }

    #[test]
    fn corpus_equivalence_is_explicit() {
        let a = json!({"corpus":{"count":1,"sha256":"a","shapeSha256":"shape"}});
        let b = json!({"corpus":{"count":1,"sha256":"b","shapeSha256":"shape"}});
        assert_ne!(controls(&a, &args()), controls(&b, &args()));
        let mut args = args();
        args.equivalent_corpus = true;
        assert_eq!(controls(&a, &args), controls(&b, &args));
    }
    #[test]
    fn spread_retains_repeat_variation() {
        assert_eq!(
            spread(vec![4.0, 1.0, 3.0, 2.0]),
            json!({"min":1.0,"median":2.5,"max":4.0,"samples":4})
        );
    }
}
