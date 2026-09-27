#[test]
fn frozen_distribution_inputs_reject_changes_and_escaping_symlinks() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/dist_desktop.py"
        ))
        .output()
        .expect("Python 3 is required for paired distribution verification");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn distribution_uses_the_selected_checkout_after_relocation() {
    let checkout = tempfile::tempdir().unwrap();
    let tooling = checkout.path().join("apps/devtools");
    std::fs::create_dir_all(&tooling).unwrap();
    std::fs::write(checkout.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(tooling.join("Cargo.toml"), "").unwrap();
    std::fs::write(
        tooling.join("distribution.py"),
        "print('selected checkout tooling')\n",
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"))
        .args(["dist", "--help"])
        .current_dir(checkout.path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "selected checkout tooling"
    );
}
