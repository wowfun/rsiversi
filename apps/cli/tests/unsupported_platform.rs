#![cfg(windows)]

use std::{fs, process::Command};

#[test]
fn built_binary_store_verification_refuses_missing_or_invalid_roots_without_repair() {
    let temporary = tempfile::tempdir().unwrap();
    for exists in [false, true] {
        let root = temporary
            .path()
            .join(if exists { "existing" } else { "missing" });
        if exists {
            fs::create_dir(&root).unwrap();
            fs::write(root.join("sessions.sqlite3"), b"retained original").unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_rsi"));
        command.env_clear().current_dir(temporary.path());
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        let output = command
            .args(["agent-store", "verify", "--root"])
            .arg(&root)
            .args(["--output", "json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("unsupported"),
            "{output:?}"
        );
        if exists {
            assert_eq!(
                fs::read(root.join("sessions.sqlite3")).unwrap(),
                b"retained original"
            );
            assert_eq!(fs::read_dir(root).unwrap().count(), 1);
        } else {
            assert!(!root.exists());
        }
    }
}
