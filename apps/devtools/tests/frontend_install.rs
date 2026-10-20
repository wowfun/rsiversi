//! Opt-in package-manager behavior with an offline synthetic dependency.
use std::{fs, path::Path, process::Command};

#[test]
#[ignore = "requires pinned Node/pnpm tools from setup web"]
fn repeated_installs_keep_dependency_lifecycle_scripts_denied() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pnpm = repository.join("target/rsi-tools/pnpm/node_modules/pnpm/bin/pnpm.mjs");
    assert!(
        pnpm.is_file(),
        "run the source entrypoint's setup web first"
    );
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("project");
    let dependency = temporary.path().join("dependency");
    let home = temporary.path().join("home");
    for directory in [&project, &dependency, &home] {
        fs::create_dir(directory).unwrap();
    }
    let marker = temporary.path().join("unexpected-install-script");
    fs::write(
        dependency.join("package.json"),
        serde_json::json!({"name":"esbuild","version":"0.25.12","scripts":{"postinstall":"node -e \"require('fs').writeFileSync(process.env.RSI_TEST_INSTALL_MARKER, 'executed')\""}}).to_string(),
    ).unwrap();
    fs::write(
        project.join("package.json"),
        r#"{"private":true,"dependencies":{"esbuild":"file:../dependency"}}"#,
    )
    .unwrap();
    fs::copy(
        repository.join("apps/web/pnpm-workspace.yaml"),
        project.join("pnpm-workspace.yaml"),
    )
    .unwrap();
    for arguments in [
        vec!["--lockfile-only", "--ignore-scripts"],
        vec!["--frozen-lockfile", "--ignore-scripts"],
        vec!["--frozen-lockfile", "--ignore-scripts"],
        vec!["--frozen-lockfile"],
    ] {
        let mut command = Command::new("node");
        command
            .arg(&pnpm)
            .arg("install")
            .args(&arguments)
            .args(["--offline", "--store-dir"])
            .arg(temporary.path().join("store"))
            .current_dir(&project)
            .env_clear();
        for key in include_str!("../build-environment.txt")
            .lines()
            .filter(|line| !line.contains('='))
        {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        for key in [
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "XDG_CACHE_HOME",
            "COREPACK_HOME",
        ] {
            command.env(key, &home);
        }
        let output = command
            .env("CI", "true")
            .env("RSI_TEST_INSTALL_MARKER", &marker)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!marker.exists(), "dependency lifecycle script ran");
    }
}
