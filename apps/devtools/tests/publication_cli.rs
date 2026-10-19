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
    let cache = root.path().join("target/rsi-app/web");
    fs::create_dir_all(&cache).unwrap();
    let lock = fs::File::create(cache.parent().unwrap().join("build.lock")).unwrap();
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
        .args(["gc", "web"])
        .current_dir(root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let mut stderr = BufReader::new(stderr);
        stderr.read_line(&mut line).unwrap();
        send.send(line).unwrap();
        std::io::copy(&mut stderr, &mut std::io::sink()).unwrap();
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
        event["removed"]["web"],
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

#[test]
fn collection_is_target_local_and_rejects_the_old_alias() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("apps/devtools")).unwrap();
    fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(root.path().join("apps/devtools/Cargo.toml"), "").unwrap();
    for target in ["tui", "web", "desktop"] {
        let cache = root.path().join("target/rsi-app").join(target);
        for id in 1..=3 {
            let bundle = cache
                .join("generations")
                .join(format!("{id:020}"))
                .join("bundle");
            fs::create_dir_all(&bundle).unwrap();
            fs::write(bundle.join("receipt.json"), b"{}").unwrap();
        }
        fs::write(cache.join("current"), b"00000000000000000003\n").unwrap();
    }
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
            .args(args)
            .current_dir(root.path())
            .output()
            .unwrap()
    };
    assert!(invoke(&["gc", "web"]).status.success());
    for (target, retained) in [("tui", true), ("web", false), ("desktop", true)] {
        assert_eq!(
            root.path()
                .join("target/rsi-app")
                .join(target)
                .join("generations/00000000000000000001")
                .exists(),
            retained
        );
    }
    assert!(!invoke(&["dist", "gc"]).status.success());
    assert!(invoke(&["gc"]).status.success());
    for target in ["tui", "desktop"] {
        assert!(
            !root
                .path()
                .join("target/rsi-app")
                .join(target)
                .join("generations/00000000000000000001")
                .exists()
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn minimal_tui_setup_doctor_and_build_never_require_frontend_or_musl_tools() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let tools = root.path().join("bin");
    fs::create_dir_all(root.path().join("scripts")).unwrap();
    fs::create_dir(&tools).unwrap();
    fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/rsiversi.sh"),
        root.path().join("scripts/rsiversi.sh"),
    )
    .unwrap();
    fs::write(
        root.path().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.90.0\"\n",
    )
    .unwrap();
    for tool in ["uname", "dirname", "sed"] {
        std::os::unix::fs::symlink(format!("/usr/bin/{tool}"), tools.join(tool)).unwrap();
    }
    for tool in ["git", "cc", "rustup", "cargo"] {
        fs::write(
            tools.join(tool),
            format!("#!/bin/sh\nprintf '%s\\n' '{tool}' \"$@\" >> \"$RSI_TEST_CALLS\"\n"),
        )
        .unwrap();
        fs::set_permissions(tools.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let calls = root.path().join("calls");
    for args in [
        vec!["doctor"],
        vec!["setup"],
        vec!["build", "--debug"],
        vec!["build", "tui", "--verbose"],
    ] {
        let output = Command::new("/bin/bash")
            .arg(root.path().join("scripts/rsiversi.sh"))
            .args(args)
            .env("PATH", &tools)
            .env("RSI_TEST_CALLS", &calls)
            .env_remove("BASH_ENV")
            .current_dir("/")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let recorded = fs::read_to_string(calls).unwrap();
    assert!(recorded.contains("toolchain\ninstall\n1.90.0\n--profile\nminimal"));
    assert!(recorded.contains("dist\ntui\n--debug") && recorded.contains("dist\ntui\n--verbose"));
    assert!(!recorded.contains("target\nadd") && !root.path().join("target").exists());
}

#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::too_many_lines)] // One fixture follows all four profile/helper recipes and failed-target isolation.
fn tui_producer_skips_document_tools_and_keeps_json_separate_from_verbose_logs() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let tools = root.path().join("tools");
    fs::create_dir_all(repo.join("apps/devtools")).unwrap();
    fs::create_dir(&tools).unwrap();
    fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(repo.join("apps/devtools/Cargo.toml"), "").unwrap();
    fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    for tool in ["git", "mkdir", "uname", "dirname"] {
        std::os::unix::fs::symlink(format!("/usr/bin/{tool}"), tools.join(tool)).unwrap();
    }
    for (name, script) in [
        (
            "rustc",
            "printf 'rustc 1.90.0\\nhost: %s-unknown-linux-gnu\\n' \"$(uname -m)\"",
        ),
        (
            "cargo",
            r#"if [ "$1" = -V ]; then echo 'cargo fixture'; exit; fi
profile=debug
target=$(uname -m)-unknown-linux-gnu
binary=rsi
while [ $# -gt 0 ]; do
  case "$1" in
    --release) profile=release ;;
    --target) shift; target=$1 ;;
    -p) shift; if [ "$1" = rsi-ssh-helper-app ]; then binary=rsi-ssh-helper; fi ;;
  esac
  shift
done
destination="$CARGO_TARGET_DIR/$target/$profile/$binary"
mkdir -p "$(dirname "$destination")"
printf binary > "$destination"
printf 'fixture tool stdout\n'
printf 'fixture tool stderr\n' >&2"#,
        ),
    ] {
        fs::write(tools.join(name), format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
        fs::set_permissions(tools.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
            .args(args)
            .env("PATH", &tools)
            .current_dir(&repo)
            .output()
            .unwrap()
    };
    for (helper, verbose) in [(false, false), (false, true), (true, false), (true, true)] {
        if helper && !tools.join("musl-gcc").exists() {
            fs::write(tools.join("musl-gcc"), "#!/bin/sh\necho 'musl fixture'\n").unwrap();
            fs::set_permissions(tools.join("musl-gcc"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut args = if verbose {
            vec!["dist", "tui", "--debug", "--verbose"]
        } else {
            vec!["dist", "tui"]
        };
        if helper {
            args.push("--with-ssh-helper");
        }
        let output = invoke(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let bundle = std::path::Path::new(result["bundle"].as_str().unwrap());
        let read = |name| {
            serde_json::from_slice::<serde_json::Value>(&fs::read(bundle.join(name)).unwrap())
                .unwrap()
        };
        let receipt = read("receipt.json");
        let manifest = read("build-family.json");
        assert_eq!(receipt["kind"], "tui");
        assert_eq!(
            receipt["profile"],
            if verbose { "debug" } else { "release" }
        );
        if helper {
            let target = format!("{}-unknown-linux-musl", std::env::consts::ARCH);
            assert_eq!(receipt["helper_target"], target);
            assert_eq!(manifest["helper"]["profile"], "release");
            let binary = bundle.join("rsi-ssh-helper");
            assert_eq!(fs::read(&binary).unwrap(), b"binary");
            assert_ne!(
                fs::metadata(&binary).unwrap().permissions().mode() & 0o111,
                0
            );
            assert_eq!(
                receipt["artifacts"]["rsi-ssh-helper"],
                format!("{:x}", Sha256::digest(b"binary"))
            );
            let target_dir = repo.join("target/rsi-app/build/tui/target").join(target);
            assert!(target_dir.join("release/rsi-ssh-helper").is_file());
            assert!(!target_dir.join("debug/rsi-ssh-helper").exists());
        } else {
            assert!(receipt["helper_target"].is_null());
            assert!(!bundle.join("rsi-ssh-helper").exists());
        }
        assert!(manifest["worker_target"].is_null());
        assert_eq!(
            manifest["packages"],
            if helper {
                serde_json::json!(["rsi-cli", "rsi-ssh-helper-app"])
            } else {
                serde_json::json!(["rsi-cli"])
            }
        );
        assert_eq!(manifest["document_toolchain"], serde_json::json!({}));
        assert!(!bundle.join("assets").exists());
        let log = fs::read_to_string(bundle.parent().unwrap().join("build.log")).unwrap();
        for marker in ["fixture tool stdout", "fixture tool stderr"] {
            assert!(log.contains(marker));
            assert_eq!(
                String::from_utf8_lossy(&output.stderr).contains(marker),
                verbose
            );
        }
    }
    let current = fs::read(repo.join("target/rsi-app/tui/current")).unwrap();
    let capture = fs::read(repo.join("target/rsi-app/build/tui/build-family.json")).unwrap();
    assert!(!invoke(&["dist", "web"]).status.success());
    assert_eq!(
        fs::read(repo.join("target/rsi-app/tui/current")).unwrap(),
        current
    );
    assert_eq!(
        fs::read(repo.join("target/rsi-app/build/tui/build-family.json")).unwrap(),
        capture
    );
    assert!(!repo.join("target/rsi-app/web/current").exists());
}

#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::too_many_lines)] // Keep probe, browser outcome, lease retention and report reuse checks together.
fn addon_report_follows_the_browser_child_result() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("target/rsi-app/web");
    let tools = root.path().join("bin");
    for path in [
        "apps/devtools",
        "bin",
        "target/rsi-app/build/web/source",
        "target/rsi-app/web/leases",
        "target/rsi-app/web/generations/00000000000000000001/bundle",
    ] {
        fs::create_dir_all(root.path().join(path)).unwrap();
    }
    fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(root.path().join("apps/devtools/Cargo.toml"), "").unwrap();
    fs::write(cache.join("current"), "00000000000000000001\n").unwrap();
    fs::write(cache.join("leases/00000000000000000001"), "").unwrap();
    let target = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
    let manifest = serde_json::json!({"files":{},"target":target,"worker_target":"wasm32-unknown-unknown","profile":"debug","default_features":false,"features":["paired-web-tests"], "flags":{"RUSTFLAGS":"","CARGO_ENCODED_RUSTFLAGS":""}}).to_string();
    let family = format!("{:x}", Sha256::digest(manifest.as_bytes()));
    fs::write(
        cache.join("generations/00000000000000000001/bundle/build-family.json"),
        manifest,
    )
    .unwrap();
    fs::write(
        cache.join("generations/00000000000000000001/bundle/receipt.json"),
        serde_json::json!({"format":2,"family_sha256":family,"kind":"web","mode":"published"})
            .to_string(),
    )
    .unwrap();
    let cargo = format!(
        "#!/bin/sh\nset -eu\nmkdir -p \"$CARGO_TARGET_DIR/{target}/debug/examples\"\nprintf artifact > \"$CARGO_TARGET_DIR/{target}/debug/examples/workbench-addon\"\nprintf probe > \"$CARGO_TARGET_DIR/{target}/debug/examples/profile-leaf-probe\"\n"
    );
    fs::write(tools.join("cargo"), cargo).unwrap();
    fs::set_permissions(tools.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rsi-app-tools"), tools.join("rsi-gc")).unwrap();
    for id in ["00000000000000000002", "00000000000000000003"] {
        let bundle = cache.join("generations").join(id).join("bundle");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("receipt.json"), b"{}").unwrap();
    }
    let unrelated = root.path().join("target/rsi-app/build/desktop");
    fs::create_dir_all(unrelated.join("source")).unwrap();
    fs::write(unrelated.join("build-family.json"), "different target").unwrap();
    let probe_report = root.path().join("probe-report");
    let probe = Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
        .args(["fixture-build", "profile-leaf-probe"])
        .arg(&probe_report)
        .current_dir(root.path())
        .env(
            "PATH",
            std::env::join_paths([tools.as_path(), std::path::Path::new("/usr/bin")]).unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(receipt["family"], family);
    assert_eq!(
        receipt["binary_sha256"],
        format!("{:x}", Sha256::digest(b"probe"))
    );
    assert_eq!(
        fs::read(receipt["binary"].as_str().unwrap()).unwrap(),
        b"probe"
    );
    assert!(probe_report.join("fixture-receipt.json").is_file());
    let log = fs::read_to_string(probe_report.join("build.log")).unwrap();
    assert!(
        log.contains("profile-leaf-probe")
            && log.contains("--no-default-features")
            && log.contains("paired-web-tests")
    );
    for (name, exit) in [("failed", 7), ("passed", 0)] {
        fs::write(cache.join("current"), "00000000000000000001\n").unwrap();
        // A deterministic verifier process proves orchestration, not browser rendering.
        fs::write(tools.join("node"), format!("#!/bin/sh\nset -eu\nreport=$(dirname \"$RSI_WORKBENCH_REPORT\")\ntest ! -e \"$report/fixture-receipt.json\" || exit 99\nprintf reached > \"$report/verifier-entered\"\nprintf '00000000000000000003\\n' > target/rsi-app/web/current\ntimeout 5 rsi-gc gc web > \"$report/gc.json\"\ntest -d target/rsi-app/web/generations/00000000000000000001\nprintf pinned > \"$report/gc-pinned\"\nexit {exit}\n")).unwrap();
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
        let log = fs::read_to_string(report.join("build.log")).unwrap();
        assert!(
            log.contains("--no-default-features") && log.contains("paired-web-tests"),
            "{log}"
        );
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
    fs::create_dir(root.path().join("scripts")).unwrap();
    fs::copy(
        source.join("scripts/rsiversi.sh"),
        root.path().join("scripts/rsiversi.sh"),
    )
    .unwrap();
    for tool in ["uname", "dirname", "sed", "grep"] {
        std::os::unix::fs::symlink(format!("/usr/bin/{tool}"), tools.join(tool)).unwrap();
    }
    for (tool, body) in [
        ("git", "exit 0"),
        ("cc", "exit 0"),
        ("musl-gcc", "exit 0"),
        ("npm", "exit 0"),
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
            .arg(root.path().join("scripts/rsiversi.sh"))
            .args(["doctor", "web"])
            .env("PATH", &tools)
            .env("RSI_TEST_TARGETS", targets)
            .env_remove("RSI_MUSL_CC")
            .output()
            .unwrap()
    };
    let invalid_cc = Command::new("/bin/bash")
        .arg(root.path().join("scripts/rsiversi.sh"))
        .args(["doctor", "web"])
        .env("PATH", &tools)
        .env("RSI_MUSL_CC", "musl-gcc")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&invalid_cc.stderr).contains("RSI_MUSL_CC must be absolute"));
    let missing = run("wasm32-unknown-unknown");
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("musl target missing"));
    let targets = format!(
        "wasm32-unknown-unknown\n{}-unknown-linux-musl",
        std::env::consts::ARCH
    );
    assert!(String::from_utf8_lossy(&missing.stderr).contains("setup web"));
    let ready = run(&targets);
    assert!(
        ready.status.success(),
        "{}",
        String::from_utf8_lossy(&ready.stderr)
    );
    fs::write(&version, "[toolchain]\n  channel  =  \"1.90.0\"\n").unwrap();
    assert!(run(&targets).status.success());
    fs::remove_file(tools.join("npm")).unwrap();
    assert!(String::from_utf8_lossy(&run(&targets).stderr).contains("Missing npm;"));
    fs::write(tools.join("npm"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(tools.join("npm"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(tools.join("rustup"), "#!/bin/sh\nexit 1\n").unwrap();
    let absent = run(&targets);
    assert!(!absent.status.success());
    assert!(
        String::from_utf8_lossy(&absent.stderr)
            .contains("Cannot inspect pinned Rust 1.90.0; run bash scripts/rsiversi.sh setup web")
    );
    fs::write(version, "[toolchain]\nchannel = \"stable\"\n").unwrap();
    let invalid = run(&targets);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("Invalid repository Rust toolchain"));
}
#[test]
fn management_help_needs_neither_checkout_nor_build_tools() {
    let root = tempfile::tempdir().unwrap();
    for flag in ["--help", "-h"] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
            .arg(flag)
            .current_dir(root.path())
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(result.status.success());
        let text = String::from_utf8(result.stdout).unwrap();
        for entry in [
            "fixture-build profile-leaf-probe",
            "dist addon-workbench",
            "--help",
            "-h",
        ] {
            assert!(text.contains(entry));
        }
        assert!(result.stderr.is_empty());
    }
    let unknown = std::process::Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
        .arg("not-a-command")
        .current_dir(root.path())
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("--help"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
