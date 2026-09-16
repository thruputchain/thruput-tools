//! Opt-in microbenchmark: `cargo bench -p groundhog-json-rpc-types --bench hex_serialization`
//! Emits each trial as CSV. Reuses output storage; excludes transport and allocation cost.
//! Candidate imports the production formatter; baseline is the merged #384 implementation.
use std::{fmt, hint::black_box, time::Instant};

use serde::{Serialize, Serializer, ser::SerializeSeq};

#[path = "../src/hex/display.rs"]
mod display;
use display::HexDisplay;

struct Baseline<'a>(&'a [u8]);

impl fmt::Display for Baseline<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("0x")?;
        let mut encoded = [0u8; 512];
        for chunk in self.0.chunks(encoded.len() / 2) {
            let output = &mut encoded[..chunk.len() * 2];
            hex::encode_to_slice(chunk, output)
                .expect("bug: hex output is exactly twice the input length");
            let text =
                std::str::from_utf8(output).expect("bug: hex encoding contains only ASCII digits");
            f.write_str(text)?;
        }
        Ok(())
    }
}

// Control for the encoder change: identical digit loop with a zeroed buffer.
struct Zeroed<'a>(&'a [u8]);
impl fmt::Display for Zeroed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        f.write_str("0x")?;
        let mut encoded = [0u8; 512];
        for chunk in self.0.chunks(encoded.len() / 2) {
            let output = &mut encoded[..chunk.len() * 2];
            for (&byte, pair) in chunk.iter().zip(output.chunks_exact_mut(2)) {
                pair[0] = DIGITS[usize::from(byte >> 4)];
                pair[1] = DIGITS[usize::from(byte & 0x0f)];
            }
            let text =
                std::str::from_utf8(output).expect("bug: hex encoding contains only ASCII digits");
            f.write_str(text)?;
        }
        Ok(())
    }
}

struct Encoded<D>(D);
impl<D: fmt::Display> Serialize for Encoded<D> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

struct Fields<'a, D>(&'a D, usize);
impl<D: fmt::Display> Serialize for Fields<'_, D> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.1))?;
        for _ in 0..self.1 {
            seq.serialize_element(&Encoded(self.0))?;
        }
        seq.end()
    }
}

#[derive(Clone, Copy)]
enum Workload {
    Format,
    Json,
    HashArray,
}

fn run<D: fmt::Display>(
    value: &D,
    workload: Workload,
    iterations: usize,
    text: &mut String,
    json: &mut Vec<u8>,
) -> u128 {
    let started = Instant::now();
    for _ in 0..iterations {
        match workload {
            Workload::Format => {
                text.clear();
                fmt::write(text, format_args!("{}", black_box(value)))
                    .expect("bug: String formatting failed");
                black_box(&*text);
            }
            Workload::Json => {
                json.clear();
                serde_json::to_writer(&mut *json, &Encoded(black_box(value)))
                    .expect("bug: JSON encoding failed");
                black_box(&*json);
            }
            Workload::HashArray => {
                json.clear();
                serde_json::to_writer(&mut *json, &Fields(black_box(value), 32))
                    .expect("bug: JSON encoding failed");
                black_box(&*json);
            }
        }
    }
    started.elapsed().as_nanos()
}

fn main() {
    // cargo test --all-targets may launch a harness=false bench with --test.
    let smoke = std::env::args().any(|arg| arg == "--test");
    println!("workload,bytes,variant,trial,iterations,elapsed_ns");
    for (workload, name, sizes) in [
        (
            Workload::Format,
            "format",
            &[0, 32, 256, 257, 2048, 65536][..],
        ),
        (Workload::Json, "json", &[0, 32, 256, 257, 2048, 65536][..]),
        (Workload::HashArray, "hash_array_32", &[32][..]),
    ] {
        for &size in sizes {
            let input: Vec<u8> = (0..=255).cycle().take(size).collect();
            let baseline = Baseline(&input);
            let zeroed = Zeroed(&input);
            let candidate = HexDisplay(&input);
            let expected = format!("0x{}", hex::encode(&input));
            assert_eq!(baseline.to_string(), expected);
            assert_eq!(zeroed.to_string(), expected);
            assert_eq!(candidate.to_string(), expected);
            let expected_json = serde_json::to_vec(&Encoded(&baseline))
                .expect("bug: baseline JSON encoding failed");
            assert_eq!(
                serde_json::to_vec(&Encoded(&candidate))
                    .expect("bug: candidate JSON encoding failed"),
                expected_json
            );
            assert_eq!(
                serde_json::to_vec(&Encoded(&zeroed)).expect("bug: control JSON encoding failed"),
                expected_json
            );
            if smoke {
                continue;
            }
            let mut text = String::with_capacity(2 * size + 4);
            let mut json = Vec::with_capacity((2 * size + 5) * 32 + 2);
            // Warm all variants, then use the same iteration count per paired trial.
            let pilot = run(&baseline, workload, 1000, &mut text, &mut json).max(1);
            run(&zeroed, workload, 1000, &mut text, &mut json);
            run(&candidate, workload, 1000, &mut text, &mut json);
            let iterations = usize::try_from((50_000_000 * 1000 / pilot).clamp(1, 10_000_000))
                .expect("bug: bounded iteration count does not fit usize");
            for trial in 0..9 {
                // Alternate forward/reverse order; never run two benchmarks concurrently.
                let order = if trial % 2 == 0 { [0, 1, 2] } else { [2, 1, 0] };
                for variant in order {
                    let elapsed = match variant {
                        0 => run(&baseline, workload, iterations, &mut text, &mut json),
                        1 => run(&zeroed, workload, iterations, &mut text, &mut json),
                        _ => run(&candidate, workload, iterations, &mut text, &mut json),
                    };
                    let variant = ["baseline", "zeroed_control", "maybe_uninit"][variant];
                    println!("{name},{size},{variant},{trial},{iterations},{elapsed}");
                }
            }
        }
    }
}
