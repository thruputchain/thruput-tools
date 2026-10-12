use thruput_sim::nonce::*;

#[test]
fn namespace_uses_high_32_bits() {
    let namespace = NonceNamespace::new(0x1234_5678).unwrap();

    assert_eq!(namespace.make_nonce(0), 0x1234_5678_0000_0000);
    assert_eq!(namespace.make_nonce(0x90ab_cdef), 0x1234_5678_90ab_cdef);
}

#[test]
fn rejects_namespace_that_does_not_fit_high_32_bits() {
    let err = NonceNamespace::new(u64::from(u32::MAX) + 1)
        .expect_err("namespace should be limited to u32");

    assert!(err.to_string().contains("exceeds"));
}

#[test]
fn generated_namespace_is_stable_for_same_run_identity() {
    let a = NonceNamespace::generate("seed", "scenario", "run-1");
    let b = NonceNamespace::generate("seed", "scenario", "run-1");
    let c = NonceNamespace::generate("seed", "scenario", "run-2");

    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn generated_namespace_changes_across_run_ids() {
    let namespaces: std::collections::BTreeSet<_> = ["run-1", "run-2", "run-3"]
        .into_iter()
        .map(|run_id| NonceNamespace::generate("seed", "scenario", run_id).get())
        .collect();

    assert_eq!(namespaces.len(), 3);
}
