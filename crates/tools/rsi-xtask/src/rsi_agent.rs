use std::ffi::OsString;
use std::path::Path;

use crate::cargo_step::{self, CargoStep};
use crate::repository_root;

const NATIVE_PACKAGES: [&str; 3] = [
    "rsi-agent-fixture-capability-anchor",
    "rsi-agent-fixture-echo-tools",
    "rsi-agent-fixture-scripted-model",
];
const FIXTURE_PACKAGES: [&str; 4] = [
    "rsi-agent-fixture-capability-anchor",
    "rsi-agent-fixture-conformance",
    "rsi-agent-fixture-echo-tools",
    "rsi-agent-fixture-scripted-model",
];

pub fn run(repository: &Path) -> Result<(), String> {
    repository_root::require(repository, "rsi-agent conformance")?;
    let steps = conformance_plan(repository)?;
    cargo_step::execute(repository, &steps)
}

fn conformance_plan(repository: &Path) -> Result<Vec<CargoStep>, String> {
    let manifest = repository.join("fixtures/rsi-agent/Cargo.toml");
    if !manifest.is_file() {
        return Err(format!(
            "required rsi-agent fixture workspace manifest `{}` is missing",
            cargo_step::display_relative(repository, &manifest)
        ));
    }

    let target_dir = repository.join("fixtures/rsi-agent/target");
    let build_target = cargo_step::detect_native_target(repository)?.triple();
    let manifest_argument = manifest.as_os_str().to_owned();

    let mut native_build = vec![
        "build".into(),
        "--locked".into(),
        "--offline".into(),
        "--manifest-path".into(),
        manifest_argument.clone(),
        "--release".into(),
        "--target".into(),
        build_target.into(),
        "--target-dir".into(),
        target_dir.as_os_str().to_owned(),
    ];
    for package in NATIVE_PACKAGES {
        native_build.extend([OsString::from("-p"), package.into()]);
    }

    let mut format = vec![
        "fmt".into(),
        "--manifest-path".into(),
        manifest_argument.clone(),
    ];
    for package in FIXTURE_PACKAGES {
        format.extend([OsString::from("-p"), package.into()]);
    }
    format.push("--check".into());

    Ok(vec![
        CargoStep::new(
            "fetch rsi-agent fixture workspace",
            [
                "fetch".into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest_argument.clone(),
            ],
        ),
        CargoStep::new("format rsi-agent fixture workspace", format),
        CargoStep::new(
            "clippy rsi-agent fixture workspace",
            [
                "clippy".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                manifest_argument.clone(),
                "--target-dir".into(),
                target_dir.as_os_str().to_owned(),
                "--workspace".into(),
                "--all-targets".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
        ),
        CargoStep::new(
            "test rsi-agent fixture workspace",
            [
                "test".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                manifest_argument.clone(),
                "--target-dir".into(),
                target_dir.as_os_str().to_owned(),
                "--workspace".into(),
            ],
        ),
        CargoStep::new("build rsi-agent native fixtures", native_build),
        CargoStep::new(
            "run keyless rsi-agent conformance",
            [
                "run".into(),
                "--quiet".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                manifest_argument,
                "--target-dir".into(),
                target_dir.into_os_string(),
                "-p".into(),
                "rsi-agent-fixture-conformance".into(),
            ],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn conformance_plan_checks_one_fixture_workspace_then_runs_the_assembled_scenario() {
        let repository = tempfile::tempdir().unwrap();
        let manifest = repository.path().join("fixtures/rsi-agent/Cargo.toml");
        let target_dir = repository.path().join("fixtures/rsi-agent/target");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(&manifest, "[workspace]\n").unwrap();

        let plan = conformance_plan(repository.path()).unwrap();
        assert_eq!(
            plan.iter()
                .map(|step| step.label.as_str())
                .collect::<Vec<_>>(),
            [
                "fetch rsi-agent fixture workspace",
                "format rsi-agent fixture workspace",
                "clippy rsi-agent fixture workspace",
                "test rsi-agent fixture workspace",
                "build rsi-agent native fixtures",
                "run keyless rsi-agent conformance",
            ]
        );
        for step in plan.iter().filter(|step| step.label.starts_with("fetch")) {
            assert!(!step.arguments.contains(&OsString::from("--offline")));
        }
        for step in plan.iter().filter(|step| {
            step.label.starts_with("clippy")
                || step.label.starts_with("test")
                || step.label.starts_with("build")
                || step.label.starts_with("run")
        }) {
            assert!(step.arguments.contains(&OsString::from("--offline")));
        }
        assert!(plan.iter().all(|step| {
            step.arguments
                .windows(2)
                .any(|pair| pair[0] == "--manifest-path" && pair[1] == manifest.as_os_str())
        }));
        assert_eq!(
            plan[4]
                .arguments
                .iter()
                .filter(|argument| argument.as_os_str() == "-p")
                .count(),
            3
        );
        assert_eq!(
            plan[1]
                .arguments
                .iter()
                .filter(|argument| argument.as_os_str() == "-p")
                .count(),
            4
        );
        assert!(!plan[1].arguments.contains(&OsString::from("--all")));
        for step in &plan[2..] {
            assert!(
                step.arguments
                    .windows(2)
                    .any(|pair| { pair[0] == "--target-dir" && pair[1] == target_dir.as_os_str() })
            );
        }
    }

    #[test]
    fn conformance_requires_the_fixture_workspace() {
        let repository = tempfile::tempdir().unwrap();
        let error = conformance_plan(repository.path()).unwrap_err();
        assert!(error.contains("fixtures/rsi-agent/Cargo.toml"));
    }
}
