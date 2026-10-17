use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Duration,
};

#[test]
fn source_tool_lock_matches_the_web_package_manager() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |path| {
        serde_json::from_slice::<serde_json::Value>(&fs::read(root.join(path)).unwrap()).unwrap()
    };
    let web = read("../web/package.json");
    let tools = read("tools/package.json");
    let lock = read("tools/package-lock.json");
    let pinned = web["packageManager"]
        .as_str()
        .unwrap()
        .strip_prefix("pnpm@")
        .unwrap();
    assert_eq!(tools["dependencies"]["pnpm"], pinned);
    assert_eq!(lock["lockfileVersion"], 3);
    assert_eq!(lock["packages"][""]["dependencies"], tools["dependencies"]);
    assert_eq!(lock["packages"]["node_modules/pnpm"]["version"], pinned);
    assert!(
        lock["packages"]["node_modules/pnpm"]["integrity"]
            .as_str()
            .unwrap()
            .starts_with("sha512-")
    );
}

#[test]
fn collection_waits_for_another_process_build_owner_and_reports_removed_generations() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("apps/devtools")).unwrap();
    fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(root.path().join("apps/devtools/Cargo.toml"), "").unwrap();
    let cache = root.path().join("target/rsi-app");
    fs::create_dir_all(&cache).unwrap();
    let lock = fs::File::create(cache.join("build.lock")).unwrap();
    for id in 1..=3 {
        let bundle = cache
            .join("generations")
            .join(format!("{id:020}"))
            .join("bundle");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("receipt.json"), b"{}").unwrap();
    }
    fs::write(cache.join("current"), "00000000000000000003\n").unwrap();
    lock.lock().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
        .args(["dist", "gc"])
        .current_dir(root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stderr).read_line(&mut line).unwrap();
        send.send(line).unwrap();
    });
    let ready = receive.recv_timeout(Duration::from_secs(10));
    if ready.is_err() {
        let _ = child.kill();
    }
    assert!(
        ready
            .unwrap()
            .contains("Waiting for the paired build owner")
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        child.try_wait().unwrap().is_none(),
        "GC bypassed the cross-process build lock"
    );
    drop(lock);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("GC did not resume after releasing the build lock");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    assert!(output.status.success());
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["event"], "application-gc");
    assert_eq!(
        event["removed"],
        serde_json::json!(["00000000000000000001"])
    );
    assert!(!cache.join("generations/00000000000000000001").exists());
    assert!(cache.join("generations/00000000000000000002").is_dir());
    assert!(cache.join("generations/00000000000000000003").is_dir());
}

#[test]
fn invalid_distribution_arguments_report_usage_without_creating_output() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("apps/devtools")).unwrap();
    fs::write(root.path().join("Cargo.toml"), "").unwrap();
    fs::write(root.path().join("apps/devtools/Cargo.toml"), "").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
        .arg("dist")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: rsi-app-tools dist"));
    assert!(!root.path().join("target").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn addon_report_follows_the_browser_child_result() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("target/rsi-app");
    let tools = root.path().join("bin");
    for path in [
        "apps/devtools",
        "bin",
        "target/rsi-app/build/source",
        "target/rsi-app/leases",
        "target/rsi-app/generations/00000000000000000001/bundle",
    ] {
        fs::create_dir_all(root.path().join(path)).unwrap();
    }
    fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(root.path().join("apps/devtools/Cargo.toml"), "").unwrap();
    fs::write(cache.join("current"), "00000000000000000001\n").unwrap();
    fs::write(cache.join("leases/00000000000000000001"), "").unwrap();
    let target = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
    let manifest = serde_json::json!({"files":{},"target":target,"profile":"debug", "flags":{"RUSTFLAGS":"","CARGO_ENCODED_RUSTFLAGS":""}}).to_string();
    let family = format!("{:x}", Sha256::digest(manifest.as_bytes()));
    fs::write(cache.join("build/build-family.json"), manifest).unwrap();
    fs::write(
        cache.join("generations/00000000000000000001/bundle/receipt.json"),
        serde_json::json!({"format":2,"family_sha256":family}).to_string(),
    )
    .unwrap();
    let cargo = format!(
        "#!/bin/sh\nset -eu\nmkdir -p \"$CARGO_TARGET_DIR/{target}/debug/examples\"\nprintf artifact > \"$CARGO_TARGET_DIR/{target}/debug/examples/workbench-addon\"\n"
    );
    fs::write(tools.join("cargo"), cargo).unwrap();
    fs::set_permissions(tools.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rsi-app-tools"), tools.join("rsi-gc")).unwrap();
    for id in ["00000000000000000002", "00000000000000000003"] {
        let bundle = cache.join("generations").join(id).join("bundle");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("receipt.json"), b"{}").unwrap();
    }
    for (name, exit) in [("failed", 7), ("passed", 0)] {
        fs::write(cache.join("current"), "00000000000000000001\n").unwrap();
        // A deterministic verifier process proves orchestration, not browser rendering.
        fs::write(tools.join("node"), format!("#!/bin/sh\nset -eu\nreport=$(dirname \"$RSI_WORKBENCH_REPORT\")\ntest ! -e \"$report/fixture-receipt.json\" || exit 99\nprintf reached > \"$report/verifier-entered\"\nprintf '00000000000000000003\\n' > target/rsi-app/current\ntimeout 5 rsi-gc dist gc > \"$report/gc.json\"\ntest -d target/rsi-app/generations/00000000000000000001\nprintf pinned > \"$report/gc-pinned\"\nexit {exit}\n")).unwrap();
        fs::set_permissions(tools.join("node"), fs::Permissions::from_mode(0o755)).unwrap();
        let report = root.path().join(name);
        let run = || {
            Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
                .args(["dist", "addon-workbench"])
                .arg(&report)
                .current_dir(root.path())
                .env(
                    "PATH",
                    std::env::join_paths([
                        tools.as_path(),
                        std::path::Path::new("/usr/bin"),
                        std::path::Path::new("/bin"),
                    ])
                    .unwrap(),
                )
                .output()
                .unwrap()
        };
        let result = run();
        assert_eq!(
            result.status.success(),
            exit == 0,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            fs::read(report.join("verifier-entered")).unwrap(),
            b"reached"
        );
        assert_eq!(fs::read(report.join("gc-pinned")).unwrap(), b"pinned");
        assert_eq!(report.join("fixture-receipt.json").exists(), exit == 0);
        assert_eq!(report.join("failure.json").exists(), exit != 0);
        if exit != 0 {
            let failure: serde_json::Value =
                serde_json::from_slice(&fs::read(report.join("failure.json")).unwrap()).unwrap();
            assert_eq!(failure["stage"], "browser-verification");
        }
        let again = run();
        assert!(!again.status.success());
        assert!(String::from_utf8_lossy(&again.stderr).contains("choose a new absolute path"));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_doctor_validates_musl_target_and_version_without_desktop_tools() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let tools = root.path().join("bin");
    fs::create_dir(&tools).unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::copy(source.join("rsi.sh"), root.path().join("rsi.sh")).unwrap();
    for tool in ["uname", "dirname", "sed", "grep"] {
        std::os::unix::fs::symlink(format!("/usr/bin/{tool}"), tools.join(tool)).unwrap();
    }
    for (tool, body) in [
        ("git", "exit 0"),
        ("cc", "exit 0"),
        ("musl-gcc", "exit 0"),
        ("rustup", "printf '%s\\n' \"$RSI_TEST_TARGETS\""),
        ("node", "if [ \"$1\" = -p ]; then echo linux; fi"),
    ] {
        fs::write(tools.join(tool), format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(tools.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
    }
    for tool in [
        "pnpm/node_modules/pnpm/bin/pnpm.mjs",
        "wasm-bindgen/bin/wasm-bindgen",
    ] {
        let path = root.path().join("target/rsi-tools").join(tool);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let version = root.path().join("rust-toolchain.toml");
    fs::write(&version, "[toolchain]\nchannel = \"1.90.0\"\n").unwrap();
    let run = |targets| {
        Command::new("/bin/bash")
            .arg(root.path().join("rsi.sh"))
            .arg("doctor")
            .env("PATH", &tools)
            .env("RSI_TEST_TARGETS", targets)
            .env_remove("RSI_MUSL_CC")
            .output()
            .unwrap()
    };
    let missing = run("wasm32-unknown-unknown");
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("musl target missing"));
    let targets = format!(
        "wasm32-unknown-unknown\n{}-unknown-linux-musl",
        std::env::consts::ARCH
    );
    let ready = run(&targets);
    assert!(
        ready.status.success(),
        "{}",
        String::from_utf8_lossy(&ready.stderr)
    );
    fs::write(&version, "[toolchain]\n  channel  =  \"1.90.0\"\n").unwrap();
    assert!(run(&targets).status.success());
    fs::write(tools.join("rustup"), "#!/bin/sh\nexit 1\n").unwrap();
    let absent = run(&targets);
    assert!(!absent.status.success());
    assert!(
        String::from_utf8_lossy(&absent.stderr)
            .contains("Cannot inspect pinned Rust 1.90.0; run setup")
    );
    fs::write(version, "[toolchain]\nchannel = \"stable\"\n").unwrap();
    let invalid = run(&targets);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("Invalid repository Rust toolchain"));
}
