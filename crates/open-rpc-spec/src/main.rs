fn main() {
    let spec = groundhog_open_rpc_spec::build_spec();
    println!(
        "{}",
        serde_json::to_string_pretty(&spec).expect("bug: spec JSON serialization cannot fail")
    );
}
