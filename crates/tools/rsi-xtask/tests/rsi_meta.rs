use std::path::Path;
use std::process::{Command, Output};

fn run(cwd: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rsi-xtask"))
        .args(arguments)
        .current_dir(cwd)
        .output()
        .expect("rsi-xtask should run")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn rsi_meta_commands_are_recognized_and_require_the_repository_root() {
    let directory = tempfile::tempdir().unwrap();

    let output = run(directory.path(), &["rsi-meta", "conformance"]);
    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(
        error.contains("must run from the repository root"),
        "unexpected conformance error: {error}"
    );
    assert!(!error.contains("usage: rsi-xtask"));
}

#[test]
fn rsi_meta_commands_reject_extra_arguments() {
    let output = run(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("xtask is nested three levels below the repository"),
        &["rsi-meta", "conformance", "--unexpected"],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("usage: rsi-xtask"));
}
