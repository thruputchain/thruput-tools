use std::{fmt, mem::MaybeUninit};

/// Stream lowercase hex without allocating or initializing unused buffer bytes.
#[derive(Debug)]
pub(super) struct HexDisplay<'a>(pub(super) &'a [u8]);

impl fmt::Display for HexDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        f.write_str("0x")?;
        let mut encoded = [MaybeUninit::<u8>::uninit(); 512];
        for chunk in self.0.chunks(encoded.len() / 2) {
            let output = &mut encoded[..chunk.len() * 2];
            for (&byte, pair) in chunk.iter().zip(output.chunks_exact_mut(2)) {
                pair[0].write(DIGITS[usize::from(byte >> 4)]);
                pair[1].write(DIGITS[usize::from(byte & 0x0f)]);
            }
            // SAFETY: output has exactly two slots per input byte. The loop
            // above writes both slots of every pair before this reference is
            // created. MaybeUninit<u8> has u8's layout and alignment, and the
            // slice excludes the unused tail of encoded. No mutation occurs
            // while these bytes are borrowed by the formatter.
            let initialized = unsafe {
                std::slice::from_raw_parts(output.as_ptr().cast::<u8>(), output.len())
            };
            let text = std::str::from_utf8(initialized)
                .expect("bug: hex encoding contains only ASCII digits");
            f.write_str(text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    #[test]
    fn every_partial_chunk_matches_reference() {
        let input: Vec<u8> = (0..=255).cycle().take(1024).collect();
        for len in (0..=257).chain([511, 512, 513, 1023, 1024]) {
            let input = &input[..len];
            assert_eq!(
                HexDisplay(input).to_string(),
                format!("0x{}", hex::encode(input))
            );
        }
    }

    #[test]
    fn formatter_failure_stops_at_prefix_and_chunk_boundaries() {
        struct FailingWriter {
            remaining: usize,
            written: String,
        }
        impl fmt::Write for FailingWriter {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                if self.remaining == 0 {
                    return Err(fmt::Error);
                }
                self.remaining -= 1;
                self.written.push_str(s);
                Ok(())
            }
        }

        let input = [0xab; 513];
        for remaining in 0..=3 {
            let mut writer = FailingWriter {
                remaining,
                written: String::new(),
            };
            assert!(write!(writer, "{}", HexDisplay(&input)).is_err());
            let expected = if remaining == 0 {
                String::new()
            } else {
                format!("0x{}", "ab".repeat((remaining - 1) * 256))
            };
            assert_eq!(writer.written, expected);
        }
    }
}
