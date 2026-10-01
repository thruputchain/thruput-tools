use std::{collections::BTreeMap, io};

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed jemalloc heap profile or process mappings",
    )
}

fn number(text: &str, radix: u32) -> io::Result<u64> {
    let text = if radix == 16 {
        text.strip_prefix("0x").unwrap_or(text)
    } else {
        text
    };
    if text.is_empty() || !text.chars().all(|c| c.is_digit(radix)) {
        return Err(invalid());
    }
    u64::from_str_radix(text, radix).map_err(|_| invalid())
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push(((value & 127) | 128).to_le_bytes()[0]);
        value >>= 7;
    }
    out.push(value.to_le_bytes()[0]);
}

fn integer(out: &mut Vec<u8>, field: u64, value: u64) {
    varint(out, field << 3);
    varint(out, value);
}

fn message(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    varint(out, (field << 3) | 2);
    varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

struct Mapping {
    start: u64,
    limit: u64,
    offset: u64,
    filename: u64,
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn rounded(value: f64) -> io::Result<u64> {
    if !value.is_finite() || value >= i64::MAX as f64 {
        return Err(invalid());
    }
    Ok(value.round() as u64)
}

/// Convert `heap_v2` text to uncompressed profile.proto, using the originating process's maps.
/// Applies jeprof's Poisson correction and caller-PC adjustment; emits no fabricated symbols.
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub fn jemalloc_to_pprof(dump: &str, maps: &str) -> io::Result<Vec<u8>> {
    let mut lines = dump.lines();
    let period = number(
        lines
            .next()
            .and_then(|s| s.strip_prefix("heap_v2/"))
            .ok_or_else(invalid)?,
        10,
    )?;
    if period == 0 {
        return Err(invalid());
    }
    let mut strings = vec!["", "inuse_objects", "count", "inuse_space", "bytes"];
    let mut mappings = Vec::new();
    for line in maps.lines() {
        let mut rest = line;
        let mut fields = Vec::new();
        for _ in 0..5 {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            if end == 0 {
                return Err(invalid());
            }
            fields.push(&rest[..end]);
            rest = &rest[end..];
        }
        let (start, limit) = fields[0].split_once('-').ok_or_else(invalid)?;
        let start = number(start, 16)?;
        let limit = number(limit, 16)?;
        if start >= limit {
            return Err(invalid());
        }
        strings.push(rest.trim_start());
        mappings.push(Mapping {
            start,
            limit,
            offset: number(fields[2], 16)?,
            filename: (strings.len() - 1) as u64,
        });
    }
    let mut profile = Vec::new();
    for (kind, unit) in [(1, 2), (3, 4)] {
        let mut value = Vec::new();
        integer(&mut value, 1, kind);
        integer(&mut value, 2, unit);
        message(&mut profile, 1, &value);
    }
    let mut locations = BTreeMap::new();
    let mut stack = Vec::new();
    let mut consumed = true;
    for line in lines {
        if line == "MAPPED_LIBRARIES:" {
            break;
        }
        if let Some(frames) = line.strip_prefix("@ ") {
            if !consumed {
                return Err(invalid());
            }
            stack.clear();
            consumed = false;
            for address in frames.split_whitespace() {
                let mut pc = number(address, 16)?;
                if pc == 0 {
                    return Err(invalid());
                }
                if !stack.is_empty() {
                    pc -= 1;
                }
                let next = locations.len() as u64 + 1;
                stack.push(*locations.entry(pc).or_insert(next));
            }
            if stack.is_empty() {
                return Err(invalid());
            }
        } else {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.first() != Some(&"t*:") {
                continue;
            }
            if !consumed && fields.len() != 5 {
                return Err(invalid());
            }
            if stack.is_empty() {
                continue;
            }
            if consumed || fields.len() != 5 {
                return Err(invalid());
            }
            consumed = true;
            let sampled_count = number(fields[1].strip_suffix(':').ok_or_else(invalid)?, 10)?;
            let sampled_bytes = number(fields[2], 10)?;
            number(
                fields[3]
                    .strip_prefix('[')
                    .and_then(|s| s.strip_suffix(':'))
                    .ok_or_else(invalid)?,
                10,
            )?;
            number(fields[4].strip_suffix(']').ok_or_else(invalid)?, 10)?;
            if (sampled_count == 0) != (sampled_bytes == 0) {
                return Err(invalid());
            }
            if sampled_count == 0 {
                continue;
            }
            let probability =
                -(-(sampled_bytes as f64) / sampled_count as f64 / period as f64).exp_m1();
            let bytes = rounded(sampled_bytes as f64 / probability)?;
            let count = rounded(bytes as f64 * sampled_count as f64 / sampled_bytes as f64)?;
            let mut sample = Vec::new();
            for id in &stack {
                integer(&mut sample, 1, *id);
            }
            integer(&mut sample, 2, count);
            integer(&mut sample, 2, bytes);
            message(&mut profile, 2, &sample);
        }
    }
    if !consumed {
        return Err(invalid());
    }
    for (index, m) in mappings.iter().enumerate() {
        let mut encoded = Vec::new();
        for (field, value) in [
            (1, index as u64 + 1),
            (2, m.start),
            (3, m.limit),
            (4, m.offset),
            (5, m.filename),
        ] {
            integer(&mut encoded, field, value);
        }
        message(&mut profile, 3, &encoded);
    }
    for (pc, id) in locations {
        let mut location = Vec::new();
        integer(&mut location, 1, id);
        if let Some(index) = mappings.iter().position(|m| pc >= m.start && pc < m.limit) {
            integer(&mut location, 2, index as u64 + 1);
        }
        integer(&mut location, 3, pc);
        message(&mut profile, 4, &location);
    }
    for value in strings {
        message(&mut profile, 6, value.as_bytes());
    }
    integer(&mut profile, 14, 3);
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(input: &mut &[u8]) -> u64 {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let byte = input[0];
            *input = &input[1..];
            value |= u64::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return value;
            }
        }
        panic!("invalid varint");
    }

    fn decode(mut input: &[u8]) -> Vec<(u64, u64, &[u8])> {
        let mut fields = Vec::new();
        while !input.is_empty() {
            let key = read(&mut input);
            match key & 7 {
                0 => fields.push((key >> 3, read(&mut input), &[][..])),
                2 => {
                    let size = usize::try_from(read(&mut input)).unwrap();
                    fields.push((key >> 3, 0, &input[..size]));
                    input = &input[size..];
                }
                _ => panic!("unsupported wire type"),
            }
        }
        fields
    }

    #[test]
    fn captured_fixture() {
        let profile = jemalloc_to_pprof(include_str!("../testdata/jemalloc.heap"), "").unwrap();
        let fields = decode(&profile);
        let samples: Vec<_> = fields.iter().filter(|f| f.0 == 2).collect();
        assert!(!samples.is_empty());
        for sample in samples {
            assert!(!decode(sample.2).is_empty());
        }
    }

    #[test]
    fn sampling_locations_and_mappings() {
        let profile = jemalloc_to_pprof(
            "heap_v2/524288\n  t*: 99: 999 [0: 0]\n@ 0x1011 0x1021\n  t*: 1: 524288 [0: 0]\n  t0: 1: 524288 [0: 0]\n@ 0x1011\n  t*: 0: 0 [0: 0]\nMAPPED_LIBRARIES:\n",
            "1000-2000 r-xp 00001000 00:00 1 /tmp/a binary\n").unwrap();
        let fields = decode(&profile);
        for (tag, count) in [(1, 2), (2, 1), (3, 1), (4, 2), (6, 6)] {
            assert_eq!(fields.iter().filter(|f| f.0 == tag).count(), count);
        }
        let sample = decode(fields.iter().find(|f| f.0 == 2).unwrap().2);
        assert_eq!(
            sample.iter().map(|f| (f.0, f.1)).collect::<Vec<_>>(),
            [(1, 1), (1, 2), (2, 2), (2, 829_411)]
        );
        let mapping = decode(fields.iter().find(|f| f.0 == 3).unwrap().2);
        assert_eq!(
            mapping.iter().map(|f| f.1).collect::<Vec<_>>(),
            [1, 0x1000, 0x2000, 0x1000, 5]
        );
        for field in fields.iter().filter(|f| f.0 == 4) {
            let location = decode(field.2);
            assert_eq!(location.len(), 3);
            assert_eq!(location[1].1, 1);
            assert!([0x1011, 0x1020].contains(&location[2].1));
        }
        let strings: Vec<_> = fields.iter().filter(|f| f.0 == 6).map(|f| f.2).collect();
        assert!(strings[0].is_empty());
        assert_eq!(strings[5], b"/tmp/a binary");
        assert!(!fields.iter().any(|f| f.0 == 5));
        assert_eq!(fields.last().unwrap().1, 3);
    }

    #[test]
    fn malformed_input() {
        for dump in [
            "heap_v1/1\n",
            "heap_v2/0\n",
            "heap_v2/1\n@ 0x1\n",
            "heap_v2/1\n@ 0x0\n",
            "heap_v2/1\n@ 0x1\n t*: 0: 1 [0: 0]\n",
            "heap_v2/1\n@ 0x1\n t*: 1: 1 [0: 0]\n t*: 1: 1 [0: 0]\n",
            "heap_v2/1\n@ \n",
            "heap_v2/1\n@ 0x1\n@ 0x2\n",
            "heap_v2/1\n@ 0x1\n t*: 1: 18446744073709551615 [0: 0]\n",
        ] {
            assert!(jemalloc_to_pprof(dump, "").is_err(), "accepted {dump}");
        }
        for maps in ["garbage", "2-1 r-xp 0 00:00 1", "1-2 r-xp xyz 00:00 1"] {
            assert!(jemalloc_to_pprof("heap_v2/1\n", maps).is_err());
        }
    }
}
