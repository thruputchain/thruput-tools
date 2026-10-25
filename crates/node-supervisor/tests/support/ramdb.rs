//! RAM database sizing for tests that start a real hog-devnode.

pub const MIB: u64 = 1024 * 1024;

/// hog-devnode divides its RAM database between the cores it may run on, so a
/// fixed size that suffices on a small runner runs out on a larger host,
/// failing genesis or a later commit with a system error. Allow 16 MiB per
/// available core, and never less than `floor`.
pub fn ramdb_bytes(floor: u64) -> u64 {
    let cores = std::thread::available_parallelism().map_or(8, std::num::NonZero::get);
    u64::try_from(cores)
        .unwrap_or(u64::MAX)
        .saturating_mul(16 * MIB)
        .max(floor)
}
