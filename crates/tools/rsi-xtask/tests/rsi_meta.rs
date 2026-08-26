use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("xtask is nested three levels below the repository")
}

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

const DIRECT_RSI_META_AUTHORITIES: [&str; 3] = [
    "cargo clippy --locked -p rsi-meta",
    "cargo test --locked -p rsi-meta",
    "fixtures/rsi-meta/foundation-probe/Cargo.toml",
];

fn direct_rsi_meta_authorities(workflow: &str) -> Vec<&'static str> {
    DIRECT_RSI_META_AUTHORITIES
        .into_iter()
        .filter(|command| workflow.contains(command))
        .collect()
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
    let output = run(repository(), &["rsi-meta", "conformance", "--unexpected"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("usage: rsi-xtask"));
}

#[test]
fn ci_delegates_rsi_meta_enumeration_only_to_conformance() {
    let workflow = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    assert_eq!(
        workflow.matches("cargo xtask rsi-meta conformance").count(),
        2,
        "the Unix matrix and Windows job must invoke the same authority"
    );
    assert_eq!(
        direct_rsi_meta_authorities(&workflow),
        Vec::<&str>::new(),
        "CI retained a second rsi-meta authority"
    );
    for package in [
        "rsi-meta",
        "rsi-meta-scope",
        "rsi-meta-plugin",
        "rsi-meta-loader",
    ] {
        assert_eq!(
            workflow
                .lines()
                .filter(|line| line.trim() == format!("--exclude {package}"))
                .count(),
            2,
            "workspace lint and test must both defer `{package}` to conformance"
        );
    }
}

#[test]
fn direct_foundation_probe_invocation_is_a_second_ci_authority() {
    let workflow =
        "cargo run --locked --manifest-path fixtures/rsi-meta/foundation-probe/Cargo.toml";
    assert_eq!(
        direct_rsi_meta_authorities(workflow),
        vec!["fixtures/rsi-meta/foundation-probe/Cargo.toml"]
    );
}
