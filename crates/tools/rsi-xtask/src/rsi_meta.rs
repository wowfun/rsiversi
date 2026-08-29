use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cargo_step::{self, CargoStep};
use crate::repository_root;

const CONFORMANCE_PACKAGES: [&str; 6] = [
    "rsi-meta-contract",
    "rsi-meta",
    "rsi-meta-scope",
    "rsi-meta-profile",
    "rsi-meta-native",
    "rsi-meta-native-loader",
];
const ECHO_MANIFEST: &str = "fixtures/rsi-meta/echo-bidi/Cargo.toml";
const ECHO_TARGET: &str = "target/rsi-meta-conformance/echo-bidi";
const FOUNDATION_MANIFEST: &str = "fixtures/rsi-meta/foundation-probe/Cargo.toml";
const FOUNDATION_TARGET: &str = "target/rsi-meta-conformance/foundation-probe";
const ENTRY_PREFIX: &str = "rsi_meta_plugin_entry_";
const ENTRY_V3: &str = "rsi_meta_plugin_entry_v3";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Host {
    Linux,
    Other,
}

impl Host {
    const fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

fn conformance_packages() -> [&'static str; 6] {
    CONFORMANCE_PACKAGES
}

pub fn run(repository: &Path) -> Result<(), String> {
    repository_root::require(repository, "rsi-meta conformance")?;
    let host = Host::current();
    let target = rustc_host_target()?;
    cargo_step::execute(repository, &conformance_steps(host, &target))?;
    if host == Host::Linux {
        eprintln!("[release exports] verify standalone echo-bidi ELF symbols");
        verify_linux_entry_exports(repository, &target)?;
    }
    Ok(())
}

fn conformance_steps(host: Host, target: &str) -> Vec<CargoStep> {
    let mut steps = Vec::new();
    for package in conformance_packages() {
        steps.push(CargoStep::new(
            format!("clippy {package}"),
            [
                "clippy".into(),
                "--locked".into(),
                "-p".into(),
                package.into(),
                "--all-targets".into(),
                "--target".into(),
                target.into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
        ));
        steps.push(CargoStep::new(
            format!("test {package}"),
            [
                "test".into(),
                "--locked".into(),
                "-p".into(),
                package.into(),
                "--all-targets".into(),
                "--target".into(),
                target.into(),
            ],
        ));
    }
    steps.extend(standalone_steps(host, target));
    steps
}

fn standalone_steps(host: Host, target: &str) -> Vec<CargoStep> {
    let target = OsString::from(target);
    let mut steps = vec![
        CargoStep::new(
            "metadata standalone echo-bidi",
            [
                "metadata".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                ECHO_MANIFEST.into(),
                "--format-version".into(),
                "1".into(),
                "--no-deps".into(),
            ],
        ),
        CargoStep::new(
            "format standalone echo-bidi",
            [
                "fmt".into(),
                "--manifest-path".into(),
                ECHO_MANIFEST.into(),
                "--check".into(),
            ],
        ),
        CargoStep::new(
            "clippy standalone echo-bidi",
            standalone_build_arguments(
                "clippy",
                ECHO_MANIFEST,
                ECHO_TARGET,
                &target,
                ["--all-targets", "--", "-D", "warnings"],
            ),
        ),
        CargoStep::new(
            "test standalone echo-bidi table header",
            standalone_build_arguments(
                "test",
                ECHO_MANIFEST,
                ECHO_TARGET,
                &target,
                ["--all-targets"],
            ),
        ),
        CargoStep::new(
            "release-build standalone echo-bidi",
            standalone_build_arguments("build", ECHO_MANIFEST, ECHO_TARGET, &target, ["--release"]),
        ),
        CargoStep::new(
            "metadata standalone foundation-probe",
            [
                "metadata".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                FOUNDATION_MANIFEST.into(),
                "--format-version".into(),
                "1".into(),
                "--no-deps".into(),
            ],
        ),
        CargoStep::new(
            "format standalone foundation-probe",
            [
                "fmt".into(),
                "--manifest-path".into(),
                FOUNDATION_MANIFEST.into(),
                "--check".into(),
            ],
        ),
        CargoStep::new(
            "clippy standalone foundation-probe",
            standalone_build_arguments(
                "clippy",
                FOUNDATION_MANIFEST,
                FOUNDATION_TARGET,
                &target,
                ["--all-targets", "--", "-D", "warnings"],
            ),
        ),
    ];
    let (command, label) = match host {
        Host::Linux => ("run", "release-run standalone foundation-probe"),
        Host::Other => ("build", "release-build standalone foundation-probe"),
    };
    steps.push(CargoStep::new(
        label,
        standalone_build_arguments(
            command,
            FOUNDATION_MANIFEST,
            FOUNDATION_TARGET,
            &target,
            ["--release"],
        ),
    ));
    steps
}

fn standalone_build_arguments<const N: usize>(
    command: &str,
    manifest: &str,
    target_directory: &str,
    target: &OsString,
    trailing: [&str; N],
) -> Vec<OsString> {
    let mut arguments = vec![
        command.into(),
        "--locked".into(),
        "--offline".into(),
        "--manifest-path".into(),
        manifest.into(),
        "--target-dir".into(),
        target_directory.into(),
        "--target".into(),
        target.clone(),
    ];
    arguments.extend(trailing.into_iter().map(OsString::from));
    arguments
}

fn rustc_host_target() -> Result<String, String> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(&rustc)
        .arg("-vV")
        .output()
        .map_err(|error| format!("could not query `{}`: {error}", rustc.to_string_lossy()))?;
    if !output.status.success() {
        return Err(command_failure(
            &format!("{} -vV", rustc.to_string_lossy()),
            output.status,
            &output.stdout,
            &output.stderr,
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        format!(
            "`{} -vV` returned non-UTF-8 output",
            rustc.to_string_lossy()
        )
    })?;
    parse_rustc_host_target(&stdout)
}

fn parse_rustc_host_target(output: &str) -> Result<String, String> {
    let mut targets = output
        .lines()
        .filter_map(|line| line.strip_prefix("host: "));
    let target = targets
        .next()
        .filter(|target| !target.is_empty() && !target.chars().any(char::is_whitespace));
    if let (Some(target), None) = (target, targets.next()) {
        Ok(target.to_owned())
    } else {
        Err("`rustc -vV` did not report exactly one valid host target".to_owned())
    }
}

fn verify_linux_entry_exports(repository: &Path, target: &str) -> Result<(), String> {
    let artifact = linux_echo_artifact(repository, target);
    if !artifact.is_file() {
        return Err(format!(
            "standalone echo-bidi release artifact is missing: `{}`",
            artifact.display()
        ));
    }
    let nm = env::var_os("NM").unwrap_or_else(|| "nm".into());
    let output = Command::new(&nm)
        .args(["-D", "--defined-only"])
        .arg(&artifact)
        .output()
        .map_err(|error| {
            format!(
                "could not inspect `{}` with `{}`: {error}",
                artifact.display(),
                nm.to_string_lossy()
            )
        })?;
    if !output.status.success() {
        return Err(command_failure(
            &format!(
                "{} -D --defined-only {}",
                nm.to_string_lossy(),
                artifact.display()
            ),
            output.status,
            &output.stdout,
            &output.stderr,
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        format!(
            "`nm` returned non-UTF-8 output for `{}`",
            artifact.display()
        )
    })?;
    validate_entry_exports(&stdout).map_err(|error| {
        format!(
            "standalone echo-bidi artifact `{}` failed export validation: {error}",
            artifact.display()
        )
    })
}

fn linux_echo_artifact(repository: &Path, target: &str) -> PathBuf {
    repository
        .join(ECHO_TARGET)
        .join(target)
        .join("release/librsi_meta_fixture_echo_bidi.so")
}

fn validate_entry_exports(symbol_table: &str) -> Result<(), String> {
    let exports = symbol_table
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .filter(|symbol| symbol.starts_with(ENTRY_PREFIX))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([ENTRY_V3.to_owned()]);
    if exports == expected {
        Ok(())
    } else {
        Err(format!(
            "standalone echo-bidi entry exports were {exports:?}, expected {expected:?}"
        ))
    }
}

fn command_failure(
    invocation: &str,
    status: impl std::fmt::Display,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => format!("`{invocation}` failed with {status} and no captured output"),
        (false, true) => format!("`{invocation}` failed with {status}\nstdout:\n{stdout}"),
        (true, false) => format!("`{invocation}` failed with {status}\nstderr:\n{stderr}"),
        (false, false) => {
            format!("`{invocation}` failed with {status}\nstdout:\n{stdout}\nstderr:\n{stderr}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(step: &CargoStep) -> Vec<&str> {
        step.arguments
            .iter()
            .map(|argument| argument.to_str().expect("test argument is UTF-8"))
            .collect()
    }

    fn labels(steps: &[CargoStep]) -> Vec<&str> {
        steps.iter().map(|step| step.label.as_str()).collect()
    }

    #[test]
    fn conformance_covers_the_exact_rsi_meta_package_set() {
        assert_eq!(
            conformance_packages(),
            [
                "rsi-meta-contract",
                "rsi-meta",
                "rsi-meta-scope",
                "rsi-meta-profile",
                "rsi-meta-native",
                "rsi-meta-native-loader",
            ]
        );
    }

    #[test]
    fn linux_conformance_plan_has_one_exact_twenty_one_step_authority() {
        let steps = conformance_steps(Host::Linux, "test-host");
        assert_eq!(
            labels(&steps),
            [
                "clippy rsi-meta-contract",
                "test rsi-meta-contract",
                "clippy rsi-meta",
                "test rsi-meta",
                "clippy rsi-meta-scope",
                "test rsi-meta-scope",
                "clippy rsi-meta-profile",
                "test rsi-meta-profile",
                "clippy rsi-meta-native",
                "test rsi-meta-native",
                "clippy rsi-meta-native-loader",
                "test rsi-meta-native-loader",
                "metadata standalone echo-bidi",
                "format standalone echo-bidi",
                "clippy standalone echo-bidi",
                "test standalone echo-bidi table header",
                "release-build standalone echo-bidi",
                "metadata standalone foundation-probe",
                "format standalone foundation-probe",
                "clippy standalone foundation-probe",
                "release-run standalone foundation-probe",
            ]
        );
        for (offset, package) in conformance_packages().into_iter().enumerate() {
            let clippy = &steps[offset * 2];
            let test = &steps[1 + offset * 2];
            assert_eq!(
                arguments(clippy),
                [
                    "clippy",
                    "--locked",
                    "-p",
                    package,
                    "--all-targets",
                    "--target",
                    "test-host",
                    "--",
                    "-D",
                    "warnings",
                ]
            );
            assert_eq!(
                arguments(test),
                [
                    "test",
                    "--locked",
                    "-p",
                    package,
                    "--all-targets",
                    "--target",
                    "test-host",
                ]
            );
        }
    }

    #[test]
    fn standalone_plan_is_locked_offline_and_exercises_each_owned_seam() {
        let steps = standalone_steps(Host::Linux, "test-host");
        assert_eq!(
            labels(&steps),
            [
                "metadata standalone echo-bidi",
                "format standalone echo-bidi",
                "clippy standalone echo-bidi",
                "test standalone echo-bidi table header",
                "release-build standalone echo-bidi",
                "metadata standalone foundation-probe",
                "format standalone foundation-probe",
                "clippy standalone foundation-probe",
                "release-run standalone foundation-probe",
            ]
        );
        assert_eq!(
            arguments(&steps[0]),
            [
                "metadata",
                "--locked",
                "--offline",
                "--manifest-path",
                ECHO_MANIFEST,
                "--format-version",
                "1",
                "--no-deps",
            ]
        );
        assert_eq!(arguments(&steps[2])[0], "clippy");
        assert_eq!(arguments(&steps[3])[0], "test");
        assert_eq!(arguments(&steps[4])[0], "build");
        assert_eq!(arguments(&steps[8])[0], "run");
        for index in [2, 3, 4, 7, 8] {
            let arguments = arguments(&steps[index]);
            assert!(arguments.contains(&"--locked"));
            assert!(arguments.contains(&"--offline"));
            assert!(
                arguments
                    .windows(2)
                    .any(|pair| pair == ["--target", "test-host"])
            );
        }
        for index in [0, 5] {
            let arguments = arguments(&steps[index]);
            assert!(arguments.contains(&"--locked"));
            assert!(arguments.contains(&"--offline"));
        }
        assert_eq!(
            arguments(&steps[1]),
            ["fmt", "--manifest-path", ECHO_MANIFEST, "--check"]
        );
        assert_eq!(
            arguments(&steps[6]),
            ["fmt", "--manifest-path", FOUNDATION_MANIFEST, "--check"]
        );
    }

    #[test]
    fn non_linux_plan_compiles_but_does_not_run_the_foundation_probe() {
        let steps = standalone_steps(Host::Other, "test-host");
        let last = steps.last().expect("foundation release step");
        assert_eq!(last.label, "release-build standalone foundation-probe");
        assert_eq!(arguments(last)[0], "build");
    }

    #[test]
    fn rustc_host_parser_fails_closed() {
        assert_eq!(
            parse_rustc_host_target("rustc 1.97.0\nhost: x86_64-unknown-linux-gnu\n").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert!(parse_rustc_host_target("rustc 1.97.0\n").is_err());
        assert!(parse_rustc_host_target("host: two targets\n").is_err());
        assert!(parse_rustc_host_target("host: first\nhost: second\n").is_err());
    }

    #[test]
    fn entry_export_validation_requires_exactly_v3() {
        validate_entry_exports("000000000001 T rsi_meta_plugin_entry_v3\n").unwrap();
        assert!(validate_entry_exports("000000000001 T unrelated\n").is_err());
        assert!(
            validate_entry_exports(
                "000000000001 T rsi_meta_plugin_entry_v2\n000000000002 T rsi_meta_plugin_entry_v3\n"
            )
            .is_err()
        );
        assert!(validate_entry_exports("000000000001 T rsi_meta_plugin_entry_v3@@V3\n").is_err());
    }

    #[test]
    fn subprocess_failure_reports_both_captured_streams() {
        let error = command_failure(
            "fixture-command",
            "exit status: 7",
            b"useful stdout\n",
            b"useful stderr\n",
        );
        assert!(error.contains("`fixture-command` failed with exit status: 7"));
        assert!(error.contains("stdout:\nuseful stdout"));
        assert!(error.contains("stderr:\nuseful stderr"));
    }
}
