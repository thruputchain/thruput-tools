use std::fs;

#[test]
fn counter_app_guide_describes_the_generated_frontend_and_its_real_scripts() {
    let temp = tempfile::tempdir().unwrap();
    thruput_cli::scaffold::generate(temp.path(), "guide-app", Some("counter-app")).unwrap();
    let root = fs::read_to_string(temp.path().join("AGENTS.md")).unwrap();
    let web = fs::read_to_string(temp.path().join("web/AGENTS.md")).unwrap();
    let package: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("package.json")).unwrap()).unwrap();

    assert!(root.contains("web/AGENTS.md"));
    assert!(root.contains(&format!(
        "Scaffolded with Thruput {}, template `counter-app`",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(!root.contains("{{"));
    assert!(root.contains("local-node workflow"));
    assert!(web.starts_with("# guide-app frontend agent guide"));
    assert!(!web.contains("{{"));
    for script in package["scripts"].as_object().unwrap().keys() {
        assert!(
            web.contains(&format!("npm run {script}")),
            "missing script {script}"
        );
    }
    for path in [
        "web/main.tsx",
        "web/wallet.ts",
        "web/refresh-counter.ts",
        "scripts/thruput.mjs",
        "README.md",
    ] {
        assert!(temp.path().join(path).is_file());
    }
    assert!(web.contains("private key in memory"));
    assert!(web.contains("deploys a new counter"));
    assert!(web.contains("not a read-only check"));
    assert!(web.contains("`npm run dev` uses the saved deployment"));
    assert!(web.contains("Regenerate bindings"));
    assert!(web.contains("original transaction hash"));
}

#[test]
fn contract_only_templates_do_not_get_counter_app_guidance() {
    for template in [None, Some("counter")] {
        let temp = tempfile::tempdir().unwrap();
        thruput_cli::scaffold::generate(temp.path(), "guide-contract", template).unwrap();
        let guide = fs::read_to_string(temp.path().join("AGENTS.md")).unwrap();
        assert!(guide.contains(&format!("template `{}`", template.unwrap_or("minimal"))));
        assert!(!guide.contains("{{"));
        assert!(!guide.contains("## Counter application"));
        assert!(!guide.contains("web/AGENTS.md"));
        assert!(!temp.path().join("web").exists());
    }
}
