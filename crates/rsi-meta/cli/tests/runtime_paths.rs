use std::process::Command;

use std::fs;

#[test]
fn missing_stable_state_home_requires_an_explicit_state_directory() {
    let output = Command::new(env!("CARGO_BIN_EXE_rsi-meta"))
        .env_remove("RSI_META_STATE_DIR")
        .env_remove("RSI_META_HOME")
        .env_remove("RSI_META_SOCKET")
        .env_remove("RSI_META_TOKEN_FILE")
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME")
        .arg("graph")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("no stable default state directory"),
        "unexpected diagnostic: {stderr}"
    );
}

#[test]
fn offline_lock_does_not_require_daemon_runtime_paths() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("rsi-meta.toml");
    let lock = directory.path().join("candidate.lock");
    fs::write(
        &manifest,
        "format_version = 0\nscopes = []\ninstances = []\n\n[composition]\nid = \"offline\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_rsi-meta"))
        .env_remove("RSI_META_STATE_DIR")
        .env_remove("RSI_META_HOME")
        .env_remove("RSI_META_SOCKET")
        .env_remove("RSI_META_TOKEN_FILE")
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME")
        .arg("lock")
        .arg(&manifest)
        .arg("--lock")
        .arg(&lock)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "offline lock unexpectedly required daemon paths: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(lock.is_file());
}

#[test]
fn relative_or_empty_environment_state_homes_are_rejected() {
    for state_home in ["", "relative"] {
        let output = Command::new(env!("CARGO_BIN_EXE_rsi-meta"))
            .env_remove("RSI_META_STATE_DIR")
            .env_remove("RSI_META_HOME")
            .env_remove("RSI_META_SOCKET")
            .env_remove("RSI_META_TOKEN_FILE")
            .env("XDG_STATE_HOME", state_home)
            .env("HOME", "/tmp/rsi-meta-test-home")
            .arg("graph")
            .output()
            .unwrap();

        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("XDG_STATE_HOME must be an absolute, non-empty path"),
            "unexpected diagnostic for {state_home:?}: {stderr}"
        );
    }
}
