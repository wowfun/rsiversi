use std::fs;
use std::path::{Path, PathBuf};
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn code_check_repository(config: &str, source: &str) -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    write(&repository.path().join("Cargo.toml"), "[workspace]\n");
    write(
        &repository.path().join("crates/tools/rsi-xtask/Cargo.toml"),
        "[package]\nname = \"rsi-xtask\"\nversion = \"0.0.0\"\n",
    );
    write(
        &repository
            .path()
            .join("crates/tools/rsi-xtask/code-check.toml"),
        config,
    );
    write(&repository.path().join("src/lib.rs"), source);
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    repository
}

#[test]
fn code_check_warns_without_failing() {
    let repository = code_check_repository(
        "version = 1\n[line_count]\nwarning_threshold = 1\n",
        "pub fn one() {}\npub fn two() {}\npub fn three() {}\n",
    );
    write(
        &repository.path().join("src/a_tie.rs"),
        "pub fn one() {}\npub fn two() {}\n",
    );
    write(
        &repository.path().join("src/z_tie.rs"),
        "pub fn one() {}\npub fn two() {}\n",
    );
    write(
        &repository.path().join("src/at_threshold.rs"),
        "pub fn one() {}\n",
    );

    let output = run(repository.path(), &["code-check"]);
    assert!(
        output.status.success(),
        "unexpected error: {}",
        stderr(&output)
    );
    assert_eq!(
        stderr(&output),
        concat!(
            "warning: code-check line-count: src/lib.rs: 3 effective Rust lines exceed soft warning threshold 1\n",
            "  top-level item: src/lib.rs:1: fn one: 1 effective Rust lines\n",
            "  top-level item: src/lib.rs:2: fn two: 1 effective Rust lines\n",
            "  top-level item: src/lib.rs:3: fn three: 1 effective Rust lines\n",
            "  largest callable: src/lib.rs:1: fn one: 1 effective Rust lines\n",
            "  deepest control flow: src/lib.rs:1: fn one: depth 0\n",
            "warning: code-check line-count: src/a_tie.rs: 2 effective Rust lines exceed soft warning threshold 1\n",
            "  top-level item: src/a_tie.rs:1: fn one: 1 effective Rust lines\n",
            "  top-level item: src/a_tie.rs:2: fn two: 1 effective Rust lines\n",
            "  largest callable: src/a_tie.rs:1: fn one: 1 effective Rust lines\n",
            "  deepest control flow: src/a_tie.rs:1: fn one: depth 0\n",
            "warning: code-check line-count: src/z_tie.rs: 2 effective Rust lines exceed soft warning threshold 1\n",
            "  top-level item: src/z_tie.rs:1: fn one: 1 effective Rust lines\n",
            "  top-level item: src/z_tie.rs:2: fn two: 1 effective Rust lines\n",
            "  largest callable: src/z_tie.rs:1: fn one: 1 effective Rust lines\n",
            "  deepest control flow: src/z_tie.rs:1: fn one: depth 0\n",
        )
    );
    assert_eq!(
        stdout(&output),
        "code-check line-count: scanned 4 Rust files; 3 exceeded warning threshold 1\n"
    );
}

#[test]
fn code_check_configuration_and_source_errors_fail() {
    let bad_config = code_check_repository(
        "version = 1\nhard_limit = 1200\n[regions]\ncore = 1\n",
        "pub fn valid() {}\n",
    );
    let output = run(bad_config.path(), &["code-check"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("parse"));

    let bad_source = code_check_repository(
        "version = 1\n[line_count]\nwarning_threshold = 1\n",
        "pub fn one() {}\npub fn two() {}\n",
    );
    write(
        &bad_source.path().join("src/a_bad.rs"),
        "pub fn invalid( {\n",
    );
    write(
        &bad_source.path().join("src/z_bad.rs"),
        "pub fn also_invalid( {\n",
    );
    let output = run(bad_source.path(), &["code-check"]);
    assert!(!output.status.success());
    let errors = stderr(&output);
    assert!(errors.contains("analyze Rust sources:"));
    assert!(errors.find("src/a_bad.rs:").unwrap() < errors.find("src/z_bad.rs:").unwrap());
    assert!(!errors.contains("warning: code-check"));
    assert_eq!(stdout(&output), "");
}

#[test]
fn code_check_is_root_only_and_has_no_legacy_or_write_interface() {
    let directory = tempfile::tempdir().unwrap();
    let output = run(directory.path(), &["code-check"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("code-check must run from the repository root"));
    assert!(!stderr(&output).contains("usage: rsi-xtask"));

    for arguments in [
        vec!["code-check", "--write"],
        vec!["rsi-meta", "code-health"],
        vec!["rsi-meta", "code-health", "--write"],
    ] {
        let output = run(repository(), &arguments);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("usage: rsi-xtask"));
    }
}

#[test]
fn automation_does_not_invoke_optional_code_check() {
    let workflows = repository().join(".github/workflows");
    let mut paths = fs::read_dir(&workflows)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .collect::<Vec<PathBuf>>();
    paths.sort();

    for path in paths {
        let workflow = fs::read_to_string(&path).unwrap();
        assert!(
            !workflow.contains("cargo xtask code-check")
                && !workflow.contains("cargo xtask rsi-meta code-health"),
            "{} invokes optional code-check",
            path.display()
        );
    }
}
