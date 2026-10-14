//! Reproducible public-payload benchmark. Run in release mode; no wallet data.
use std::time::Instant;

fn main() -> Result<(), thruput_encoding::Error> {
    for size in [32, 64, 1_024, 16_384, 65_536, 262_144] {
        let bytes: Vec<u8> = (0u8..251).cycle().take(size).collect();
        let start = Instant::now();
        let encoded = thruput_encoding::encode(&bytes);
        let encode_time = start.elapsed();
        let start = Instant::now();
        let decoded = thruput_encoding::decode(&encoded, bytes.len())?;
        let decode_time = start.elapsed();
        assert_eq!(bytes, decoded);
        println!("{size} bytes: encode={encode_time:?}, decode={decode_time:?}");
    }
    Ok(())
}
