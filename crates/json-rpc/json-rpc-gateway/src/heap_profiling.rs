//! Binary allocator setup; profile control and conversion are shared.
pub use groundhog_heap_profiling::{initialize, routes};

#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// The allocator reads this before Rust startup.
#[cfg(target_os = "linux")]
#[unsafe(export_name = "malloc_conf")]
pub static MALLOC_CONF: &[u8] = b"prof:true,prof_active:false,lg_prof_sample:19\0";
