#![cfg(unix)]
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rsi"))
        .env_clear()
        .env("HOME", root.parent().unwrap().join("home"))
        .current_dir(root.parent().unwrap())
        .args(["addon"])
        .args(arguments)
        .args(["--root", root.to_str().unwrap(), "--output", "json"])
        .output()
        .unwrap()
}
fn success(root: &Path, arguments: &[&str]) -> Value {
    let output = run(root, arguments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn source_commands_install_without_execution_or_activation_and_keep_selection_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let store = root.join("source-store");
    let manifest = root.join("addon.toml");
    fs::write(root.join("artifact.bin"), b"not executable native code").unwrap();
    fs::write(&manifest, format!("format = 1\nid = 'fixture.cli'\nplugin = 'fixture.native-cli'\ntarget = '{}'\nartifact = 'artifact.bin'\n", rsi::native_addon_target())).unwrap();
    let first = success(&store, &["install", "addon.toml"]);
    assert_eq!(first["revision"], "1");
    let rows = success(&store, &["list"]);
    assert_eq!(rows["installed"].as_array().unwrap().len(), 1);
    assert!(rows["enabled"].as_array().unwrap().is_empty());
    success(&store, &["enable", "fixture.cli"]);
    fs::write(
        root.join("artifact.bin"),
        b"different still not executable native code",
    )
    .unwrap();
    success(&store, &["install", manifest.to_str().unwrap()]);
    let rows = success(&store, &["list"]);
    assert_ne!(
        rows["installed"][0]["artifact_sha256"],
        rows["enabled"][0]["artifact_sha256"]
    );
    assert!(!run(&store, &["uninstall", "fixture.cli"]).status.success());
    success(&store, &["enable", "fixture.cli"]);
    let rows = success(&store, &["list"]);
    assert_eq!(rows["installed"], rows["enabled"]);
    success(&store, &["disable", "fixture.cli"]);
    success(&store, &["uninstall", "fixture.cli"]);
    assert!(
        success(&store, &["list"])["installed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read_dir(store.join("objects")).unwrap().count(), 2);
    assert!(!root.join("cache").exists());
    assert!(!root.join("state").exists());
}
#[test]
fn invalid_source_grammar_is_rejected_before_opening_a_store() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("never-created");
    for arguments in [
        vec!["enable"],
        vec!["list", "extra"],
        vec!["install"],
        vec!["disable", "a", "b"],
    ] {
        assert!(!run(&root, &arguments).status.success());
        assert!(!root.exists());
    }
    let help = Command::new(env!("CARGO_BIN_EXE_rsi"))
        .env_clear()
        .args(["addon", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("uninstall"));
}
