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
    fs::write(&manifest, format!("format = 2\nscope = 'agent'\nid = 'fixture.cli'\nplugin = 'fixture.native-cli'\ntarget = '{}'\nartifact = 'artifact.bin'\n", rsi::native_addon_target())).unwrap();
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
fn unsupported_store_format_is_reported_without_rewriting_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let store = root.join("store");
    fs::write(root.join("artifact.bin"), b"not executable native code").unwrap();
    fs::write(root.join("addon.toml"), format!("format = 2\nscope = 'agent'\nid = 'fixture.legacy'\nplugin = 'fixture.legacy'\ntarget = '{}'\nartifact = 'artifact.bin'\n", rsi::native_addon_target())).unwrap();
    success(&store, &["install", "addon.toml"]);
    success(&store, &["enable", "fixture.legacy"]);
    let index = store.join("state.json");
    let valid: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    let object = store.join("objects").join(
        valid["installed"]["fixture.legacy"]["artifact_sha256"]
            .as_str()
            .unwrap(),
    );
    for missing_scope in [false, true] {
        let mut legacy = valid.clone();
        legacy["format"] = 1.into();
        if missing_scope {
            for membership in ["installed", "enabled"] {
                legacy[membership]["fixture.legacy"]
                    .as_object_mut()
                    .unwrap()
                    .remove("scope");
            }
        }
        let bytes = serde_json::to_vec(&legacy).unwrap();
        fs::write(&index, &bytes).unwrap();
        for arguments in [&["list"][..], &["disable", "fixture.legacy"]] {
            let output = run(&store, arguments);
            assert!(!output.status.success(), "{arguments:?}");
            assert!(output.stdout.is_empty(), "no success receipt on rejection");
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(
                error.contains("invalid native addon input: store state"),
                "{error}"
            );
            assert_eq!(fs::read(&index).unwrap(), bytes);
            assert_eq!(fs::read_dir(store.join("objects")).unwrap().count(), 1);
            assert_eq!(fs::read(&object).unwrap(), b"not executable native code");
        }
    }
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
        vec!["list", "--enable"],
        vec!["build"],
        vec!["watch"],
        vec!["build", "addon.toml", "--enable", "--enable"],
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
    assert!(String::from_utf8_lossy(&help.stdout).contains("build|watch"));
}

#[test]
fn explicit_build_command_reports_installation_and_opt_in_enable_separately() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let store = root.join("store");
    fs::write(root.join("input"), b"built by CLI").unwrap();
    fs::write(
        root.join("build.sh"),
        "cp input artifact.bin\nprintf compiler-output\n",
    )
    .unwrap();
    fs::write(root.join("addon.toml"), format!("format = 2\nscope = 'agent'\nid = 'fixture.build-cli'\nplugin = 'fixture.build-cli'\ntarget = '{}'\nartifact = 'artifact.bin'\n[build]\ncommand = ['/bin/sh', 'build.sh']\nwatch = ['input', 'build.sh']\n", rsi::native_addon_target())).unwrap();
    let first = success(&store, &["build", "addon.toml"]);
    assert_eq!(first["status"], "succeeded");
    assert_eq!(first["installed"]["revision"], "1");
    assert_eq!(
        first["stdout"]["bytes_hex"],
        hex::encode(b"compiler-output")
    );
    assert!(
        success(&store, &["list"])["enabled"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let next = success(&store, &["build", "addon.toml", "--enable"]);
    assert_eq!(next["enabled"]["revision"], "2");
    assert_eq!(
        success(&store, &["list"])["enabled"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::write(root.join("build.sh"), "exit 17\n").unwrap();
    let failed = run(&store, &["build", "addon.toml", "--enable"]);
    assert!(!failed.status.success());
    let failed: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["exit_code"], 17);
    assert!(failed["installed"].is_null());
    assert_eq!(success(&store, &["list"])["revision"], "2");
    fs::write(
        root.join("build.sh"),
        "cp input artifact.bin\nprintf '\\033[2Jcompiler\\routput'\n",
    )
    .unwrap();
    let text = Command::new(env!("CARGO_BIN_EXE_rsi"))
        .env_clear()
        .env("HOME", root.join("home"))
        .current_dir(&root)
        .args(["addon", "build", "addon.toml", "--root"])
        .arg(&store)
        .output()
        .unwrap();
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(!text.contains(['\u{1b}', '\r']));
    assert!(text.contains("�[2Jcompiler�output"));
}

#[path = "addon_cli/watch.rs"]
mod watch;
