//! Public developer command admission never opens ordinary state on failure.
use std::{fs, process::Command};

#[cfg(target_os = "linux")]
#[test]
fn bash_bootstrap_uses_the_shared_private_layout_and_preserves_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("source space 中文");
    let caller = directory.path().join("caller space 中文");
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("apps/devtools")).unwrap();
    fs::create_dir(&caller).unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["scripts/rsiversi.sh", "apps/devtools/build-environment.txt"] {
        fs::copy(source.join(name), root.join(name)).unwrap();
    }
    let helper = directory.path().join("helper.rs");
    fs::write(&helper, r#"
use std::{env,fs,path::PathBuf};
fn main() {
    let exe=env::current_exe().unwrap();
    if exe.file_stem().unwrap()=="cargo" {
        assert!(env::var_os("DEEPSEEK_API_KEY").is_none());
        assert!(env::var_os("CARGO_PKG_NAME").is_none());
        let args:Vec<_>=env::args_os().collect();
        let target=PathBuf::from(&args[args.iter().position(|arg|arg=="--target-dir").unwrap()+1]);
        let policy=fs::read_to_string(target.parent().unwrap().join("apps/devtools/build-environment.txt")).unwrap();
        for (key,suffix) in policy.lines().filter_map(|line|line.split_once('=')) {
            assert_eq!(PathBuf::from(env::var_os(key).unwrap()),target.join("rsi-app").join(suffix),"{key}");
        }
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::copy(exe,target.join("debug/rsi-app-tools")).unwrap();
    } else {
        assert_eq!(env::var("DEEPSEEK_API_KEY").unwrap(),"synthetic-not-a-provider-key");
        assert!(!exe.parent().unwrap().ends_with("debug"));
        fs::write("runtime-caller",env::var_os("RSI_DEV_CALLER_CWD").unwrap().to_string_lossy().as_bytes()).unwrap();
        fs::write("private-runner",exe.to_string_lossy().as_bytes()).unwrap();
    }
}
"#).unwrap();
    let fake_cargo = directory.path().join("cargo");
    let compiled = Command::new("rustc")
        .arg(helper)
        .arg("-o")
        .arg(&fake_cargo)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let search = std::env::join_paths(
        std::iter::once(directory.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = Command::new("bash")
        .arg(root.join("scripts/rsiversi.sh"))
        .args(["dev", "web", "--no-open"])
        .current_dir(&caller)
        .env("PATH", search)
        .env("DEEPSEEK_API_KEY", "synthetic-not-a-provider-key")
        .env_remove("CARGO_MANIFEST_DIR")
        .env_remove("CARGO_PKG_NAME")
        .env_remove("RSI_DEV_CALLER_CWD")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("runtime-caller")).unwrap(),
        caller.to_str().unwrap()
    );
    assert!(
        !std::path::Path::new(&fs::read_to_string(root.join("private-runner")).unwrap()).exists()
    );
}

#[cfg(windows)]
#[test]
fn shared_windows_tool_requires_a_private_source_wrapper_copy() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
        .args(["dev", "tui"])
        .current_dir(root)
        .env_remove("CARGO_MANIFEST_DIR")
        .env_remove("CARGO_PKG_NAME")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("shared development tool can be relinked")
    );
}

#[test]
fn invalid_options_and_cargo_runtime_are_rejected_before_product_or_state_setup() {
    let root = tempfile::tempdir().unwrap();
    for arguments in [
        vec!["dev", "tui", "--", "--reset-state"],
        vec!["dev", "web", "--assets", "unpaired"],
        vec!["dev", "web", "--port", "42", "--port", "43"],
        vec!["dev", "tui"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
            .args(&arguments)
            .current_dir(root.path())
            .env("CARGO_MANIFEST_DIR", "synthetic-cargo-context")
            .env("CARGO_PKG_NAME", "synthetic-cargo-context")
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("DEEPSEEK_API_KEY", "synthetic-not-a-provider-key")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{arguments:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-not-a-provider-key"));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
