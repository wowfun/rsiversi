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
