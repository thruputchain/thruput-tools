//! Binary allocator setup; profile control and conversion are shared.
pub use thruput_heap_profiling::{allow_remote_env, initialize, routes_for_listener};

#[cfg(all(target_os = "linux", feature = "heap-profiling"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// The allocator reads this before Rust startup.
#[cfg(all(target_os = "linux", feature = "heap-profiling"))]
#[unsafe(export_name = "malloc_conf")]
pub static MALLOC_CONF: &[u8] = b"prof:true,prof_active:false,lg_prof_sample:19\0";
