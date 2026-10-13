use thruput_testkit::AgentRng;

#[test]
fn choose_index_stays_in_range() {
    let mut rng = AgentRng::from_u64(42);
    for _ in 0..100 {
        let index = rng
            .choose_index(5)
            .expect("bug: nonzero length yields an index");
        assert!(index < 5);
    }
    assert_eq!(rng.choose_index(0), None);
}
