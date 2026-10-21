//! Linux HTTP clients must verify certificates with rustls so the `rustls-webpki` distrust list
//! applies. OpenSSL through `native-tls` trusts any chain to a root in the system bundle and does
//! not check revocation, so an outdated `ca-certificates` package would accept distrusted CAs.

use std::path::Path;
use std::process::Command;

const LINUX_TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

fn cargo_tree(target: &str, args: &[&str]) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO"))
        .current_dir(workspace)
        .args(["tree", "--workspace", "--locked", "--target", target])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo tree failed for {target}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn linux_graph_excludes_native_tls() {
    for target in LINUX_TARGETS {
        let dependents = cargo_tree(
            target,
            &["--edges", "normal,build", "--invert", "native-tls"],
        );
        assert!(
            dependents.trim().is_empty(),
            "native-tls is linked on {target}:\n{dependents}"
        );
    }
}

#[test]
fn linux_reqwest_uses_rustls_with_native_roots() {
    for target in LINUX_TARGETS {
        let features = cargo_tree(
            target,
            &["--edges", "features", "--invert", "reqwest", "--depth", "1"],
        );
        assert!(
            features.contains(r#"reqwest feature "rustls-tls-native-roots""#),
            "reqwest lacks rustls-tls-native-roots on {target}:\n{features}"
        );
    }
}
