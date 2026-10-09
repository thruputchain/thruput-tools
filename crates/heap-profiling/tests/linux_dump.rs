#![cfg(all(target_os = "linux", feature = "jemalloc"))]

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[unsafe(export_name = "malloc_conf")]
pub static MALLOC_CONF: &[u8] = b"prof:true,prof_active:false,lg_prof_sample:0\0";

#[inline(never)]
fn retain_heap_profile_marker() -> Box<[u8]> {
    std::hint::black_box(vec![42; 8 * 1024 * 1024].into_boxed_slice())
}

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

#[tokio::test(flavor = "current_thread")]
async fn heap_dump_has_marker_and_executable_build_id() {
    // SAFETY: this is the only test, and no other thread accesses the environment.
    unsafe { std::env::set_var("GROUNDHOG_HEAP_PROFILING", "1") };
    assert!(
        groundhog_heap_profiling::initialize()
            .await
            .unwrap()
            .is_some()
    );
    let retained = retain_heap_profile_marker();
    let profile = groundhog_heap_profiling::dump_pprof().unwrap();
    std::hint::black_box(&retained);
    let fields = decode(&profile);
    let strings: Vec<_> = fields.iter().filter(|f| f.0 == 6).map(|f| f.2).collect();
    let marker = |index| {
        String::from_utf8_lossy(strings[usize::try_from(index).unwrap()])
            .contains("retain_heap_profile_marker")
    };
    assert!(
        strings
            .iter()
            .any(|s| String::from_utf8_lossy(s).contains("retain_heap_profile_marker"))
    );
    let functions: Vec<_> = fields
        .iter()
        .filter(|f| f.0 == 5)
        .map(|f| decode(f.2))
        .collect();
    assert!(!functions.is_empty());
    assert!(
        functions
            .iter()
            .any(|f| f.iter().any(|f| f.0 == 2 && marker(f.1)))
    );
    let executable = std::env::current_exe().unwrap();
    let path = executable.to_str().unwrap().as_bytes();
    assert!(fields.iter().filter(|f| f.0 == 3).any(|f| {
        let mapping = decode(f.2);
        mapping
            .iter()
            .any(|f| f.0 == 5 && strings[usize::try_from(f.1).unwrap()] == path)
            && mapping
                .iter()
                .any(|f| f.0 == 6 && !strings[usize::try_from(f.1).unwrap()].is_empty())
    }));
}
