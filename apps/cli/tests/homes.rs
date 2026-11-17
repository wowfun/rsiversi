use std::process::Command;

#[test]
fn import_dry_run_refuses_pending_reset_without_recovering_it() {
    use std::io::Write as _;
    use std::path::Path;
    let root = tempfile::tempdir().unwrap();
    let paths = rsi_host::HostPaths::new(
        root.path().join("config"),
        root.path().join("data"),
        root.path().join("state"),
        root.path().join("cache"),
        root.path().join("tmp"),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rsi"));
    for (name, path) in [
        ("config", paths.config()),
        ("data", paths.data()),
        ("state", paths.state()),
        ("cache", paths.cache()),
        ("tmp", paths.tmp()),
    ] {
        command.arg(format!("--{name}-home")).arg(path);
    }
    for name in ["CONFIG", "DATA", "STATE", "CACHE"] {
        command.env(
            format!("XDG_{name}_HOME"),
            root.path().join(format!("default-{name}")),
        );
    }
    let configuration =
        rsi_files_native_fs::create_absolute_directory_no_follow(paths.config()).unwrap();
    rsi_files_native_fs::create_relative_private_file_no_follow(
        &configuration,
        Path::new("settings.json"),
    )
    .unwrap()
    .write_all(b"{}")
    .unwrap();
    let authority = rsi_files_native_fs::create_absolute_directory_no_follow(
        &paths.config().join("session-host"),
    )
    .unwrap();
    let intent = serde_json::to_vec(&serde_json::json!({
        "format": 2, "id": "a".repeat(32),
        "config": paths.config(), "data": paths.data(), "state": paths.state(),
        "selection": {"config": true, "state": false},
        "entries": [{"home":"config", "name":"settings.json", "directory":false}]
    }))
    .unwrap();
    rsi_files_native_fs::create_relative_private_file_no_follow(
        &authority,
        Path::new("reset.pending.json"),
    )
    .unwrap()
    .write_all(&intent)
    .unwrap();
    command.arg("import");
    for name in ["config", "data", "state"] {
        let source = root.path().join("source").join(name);
        rsi_files_native_fs::create_absolute_directory_no_follow(&source).unwrap();
        command.arg(format!("--{name}-home")).arg(source);
    }
    let output = command.arg("--dry-run").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pending reset"));
    assert_eq!(
        std::fs::read(paths.config().join("settings.json")).unwrap(),
        b"{}"
    );
    assert_eq!(
        std::fs::read(paths.config().join("session-host/reset.pending.json")).unwrap(),
        intent
    );
    assert!(!paths.data().exists());
    assert!(!paths.state().exists());
}

#[test]
fn paths_is_read_only_and_resolves_all_explicit_relative_roots() {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rsi"));
    command.current_dir(root.path());
    for name in ["CONFIG", "DATA", "STATE", "CACHE"] {
        command.env(
            format!("XDG_{name}_HOME"),
            root.path().join(format!("default-{name}")),
        );
    }
    for name in ["config", "data", "state", "cache", "tmp"] {
        command
            .arg(format!("--{name}-home"))
            .arg(format!("./chosen/{name}"));
    }
    let output = command.args(["config", "paths"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for name in ["config", "data", "state", "cache", "tmp"] {
        let expected = root.path().join("chosen").join(name);
        assert_eq!(
            std::path::Path::new(paths[name].as_str().unwrap()),
            expected
        );
        assert!(!expected.exists());
    }
    assert!(!root.path().join("chosen").exists());
}
