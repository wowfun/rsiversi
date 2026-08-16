use std::ffi::OsString;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Conformance,
    ReleaseDemo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostTarget {
    LinuxX86_64,
    MacOsAarch64,
}

#[derive(Debug, Eq, PartialEq)]
struct CargoStep {
    label: String,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
}

#[derive(Debug, Eq, PartialEq)]
enum Step {
    Cargo(CargoStep),
    VerifyMarkers {
        label: String,
        default_binary: PathBuf,
        failpoint_binary: PathBuf,
    },
}

pub fn run(repository: &Path, command: Command) -> Result<(), String> {
    if !repository.join("Cargo.toml").is_file()
        || !repository
            .join("crates/tools/rsi-xtask/Cargo.toml")
            .is_file()
    {
        return Err(format!(
            "rsi-meta {} must run from the repository root",
            command.name()
        ));
    }
    let target = detect_host_target(repository)?;
    let steps = match command {
        Command::Conformance => {
            let workspaces = discover_standalone_workspaces(repository)?;
            let plugins = discover_plugin_packages(repository)?;
            conformance_plan(repository, target, &workspaces, &plugins)
        }
        Command::ReleaseDemo => release_demo_plan(repository, target),
    };
    execute(repository, &steps)
}

impl Command {
    const fn name(self) -> &'static str {
        match self {
            Self::Conformance => "conformance",
            Self::ReleaseDemo => "release-demo",
        }
    }
}

fn discover_standalone_workspaces(repository: &Path) -> Result<Vec<PathBuf>, String> {
    let mut manifests = Vec::new();
    for namespace in ["fixtures/rsi-meta", "plugins/rsi-meta"] {
        let directory = repository.join(namespace);
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("could not read `{}`: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("could not read entry in `{}`: {error}", directory.display())
            })?;
            let kind = entry.file_type().map_err(|error| {
                format!("could not inspect `{}`: {error}", entry.path().display())
            })?;
            if kind.is_dir() {
                let manifest = entry.path().join("Cargo.toml");
                if manifest.is_file() {
                    manifests.push(manifest);
                }
            }
        }
    }
    manifests.sort();
    Ok(manifests)
}

fn discover_plugin_packages(repository: &Path) -> Result<Vec<PathBuf>, String> {
    Ok(discover_standalone_workspaces(repository)?
        .into_iter()
        .filter_map(|manifest| manifest.parent().map(Path::to_path_buf))
        .filter(|directory| directory.join("plugin.toml").is_file())
        .collect())
}

fn parse_host_target(version: &str) -> Result<HostTarget, String> {
    let target = version
        .lines()
        .find_map(|line| line.strip_prefix("host:").map(str::trim))
        .ok_or_else(|| "rustc -vV did not report a host target".to_owned())?;
    match target {
        "x86_64-unknown-linux-gnu" => Ok(HostTarget::LinuxX86_64),
        "aarch64-apple-darwin" => Ok(HostTarget::MacOsAarch64),
        _ => Err(format!(
            "rsi-meta conformance is not verified for target {target}"
        )),
    }
}

fn detect_host_target(repository: &Path) -> Result<HostTarget, String> {
    let output = ProcessCommand::new("rustc")
        .arg("-vV")
        .current_dir(repository)
        .output()
        .map_err(|error| format!("could not run `rustc -vV`: {error}"))?;
    if !output.status.success() {
        return Err(format!("`rustc -vV` failed with {}", output.status));
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|error| format!("`rustc -vV` returned non-UTF-8 output: {error}"))?;
    parse_host_target(&version)
}

impl HostTarget {
    const fn triple(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Self::MacOsAarch64 => "aarch64-apple-darwin",
        }
    }

    const fn dynamic_library_extension(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "so",
            Self::MacOsAarch64 => "dylib",
        }
    }
}

fn lifecycle_probe_artifact(repository: &Path, target: HostTarget) -> PathBuf {
    repository
        .join("fixtures/rsi-meta/lifecycle-probe/target")
        .join(target.triple())
        .join("release")
        .join(format!(
            "librsi_meta_fixture_lifecycle_probe.{}",
            target.dynamic_library_extension()
        ))
}

fn conformance_plan(
    repository: &Path,
    target: HostTarget,
    workspaces: &[PathBuf],
    plugin_packages: &[PathBuf],
) -> Vec<Step> {
    let mut steps = Vec::new();
    // Integration tests load these exact native outputs. Build them before
    // test execution so a test process never recursively invokes Cargo.
    for package in plugin_packages {
        let relative = display_relative(repository, package);
        steps.push(cargo_step(
            format!("build release plugin {relative}"),
            [
                "build".into(),
                "--locked".into(),
                "--release".into(),
                "--target".into(),
                target.triple().into(),
                "--target-dir".into(),
                package.join("target").into_os_string(),
                "--manifest-path".into(),
                package.join("Cargo.toml").into_os_string(),
            ],
        ));
    }
    for manifest in workspaces {
        let package = manifest
            .parent()
            .expect("workspace manifests have a package directory");
        let relative = display_relative(repository, package);
        steps.push(cargo_step(
            format!("format {relative}"),
            [
                "fmt".into(),
                "--manifest-path".into(),
                manifest.as_os_str().into(),
                "--check".into(),
            ],
        ));
        steps.push(cargo_step(
            format!("clippy {relative}"),
            [
                "clippy".into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest.as_os_str().into(),
                "--all-targets".into(),
                "--".into(),
                "-D".into(),
                "warnings".into(),
            ],
        ));
        steps.push(cargo_step(
            format!("test {relative}"),
            [
                "test".into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest.as_os_str().into(),
            ],
        ));
    }

    steps.push(cargo_step(
        "run real-library conformance",
        [
            "run".into(),
            "--quiet".into(),
            "--locked".into(),
            "--manifest-path".into(),
            repository
                .join("fixtures/rsi-meta/conformance/Cargo.toml")
                .into_os_string(),
        ],
    ));
    steps.extend(release_demo_plan(repository, target));
    steps
}

fn release_demo_plan(repository: &Path, target: HostTarget) -> Vec<Step> {
    let root_manifest = repository.join("Cargo.toml");
    let root_target = repository.join("target");
    let demo = repository.join("fixtures/rsi-meta/release-demo");
    let demo_manifest = demo.join("Cargo.toml");
    let demo_target = demo.join("target");
    let failpoint_target = demo_target.join("failpoint-cli");
    let lifecycle = repository.join("fixtures/rsi-meta/lifecycle-probe");
    let default_binary = root_target.join("debug/rsi-meta");
    let failpoint_binary = failpoint_target.join("debug/rsi-meta");
    let artifact = lifecycle_probe_artifact(repository, target);

    vec![
        cargo_step(
            "build default rsi-meta-cli",
            [
                "build".into(),
                "--quiet".into(),
                "--locked".into(),
                "--manifest-path".into(),
                root_manifest.clone().into_os_string(),
                "--target-dir".into(),
                root_target.into_os_string(),
                "-p".into(),
                "rsi-meta-cli".into(),
            ],
        ),
        cargo_step(
            "build failpoint rsi-meta-cli",
            [
                "build".into(),
                "--quiet".into(),
                "--locked".into(),
                "--manifest-path".into(),
                root_manifest.into_os_string(),
                "--target-dir".into(),
                failpoint_target.into_os_string(),
                "-p".into(),
                "rsi-meta-cli".into(),
                "--features".into(),
                "test-failpoints".into(),
            ],
        ),
        cargo_step(
            "build release-demo",
            [
                "build".into(),
                "--quiet".into(),
                "--locked".into(),
                "--manifest-path".into(),
                demo_manifest.clone().into_os_string(),
                "--target-dir".into(),
                demo_target.clone().into_os_string(),
            ],
        ),
        cargo_step(
            "build lifecycle-probe",
            [
                "build".into(),
                "--quiet".into(),
                "--locked".into(),
                "--release".into(),
                "--target".into(),
                target.triple().into(),
                "--target-dir".into(),
                lifecycle.join("target").into_os_string(),
                "--manifest-path".into(),
                lifecycle.join("Cargo.toml").into_os_string(),
            ],
        ),
        Step::VerifyMarkers {
            label: "verify failpoint marker isolation".to_owned(),
            default_binary: default_binary.clone(),
            failpoint_binary: failpoint_binary.clone(),
        },
        cargo_step_with_env(
            "run release demonstration",
            [
                "run".into(),
                "--quiet".into(),
                "--locked".into(),
                "--offline".into(),
                "--manifest-path".into(),
                demo_manifest.into_os_string(),
                "--target-dir".into(),
                demo_target.into_os_string(),
            ],
            [
                ("RSI_META_BIN".into(), default_binary.into_os_string()),
                (
                    "RSI_META_FAILPOINT_BIN".into(),
                    failpoint_binary.into_os_string(),
                ),
                (
                    "RSI_META_LIFECYCLE_PROBE_ARTIFACT".into(),
                    artifact.into_os_string(),
                ),
            ],
        ),
    ]
}

fn cargo_step(label: impl Into<String>, arguments: impl IntoIterator<Item = OsString>) -> Step {
    cargo_step_with_env(label, arguments, [])
}

fn cargo_step_with_env(
    label: impl Into<String>,
    arguments: impl IntoIterator<Item = OsString>,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Step {
    Step::Cargo(CargoStep {
        label: label.into(),
        arguments: arguments.into_iter().collect(),
        environment: environment.into_iter().collect(),
    })
}

fn display_relative(repository: &Path, path: &Path) -> String {
    path.strip_prefix(repository)
        .unwrap_or(path)
        .display()
        .to_string()
}

impl Step {
    fn label(&self) -> &str {
        match self {
            Self::Cargo(step) => &step.label,
            Self::VerifyMarkers { label, .. } => label,
        }
    }
}

fn execute(repository: &Path, steps: &[Step]) -> Result<(), String> {
    for (index, step) in steps.iter().enumerate() {
        eprintln!("[{}/{}] {}", index + 1, steps.len(), step.label());
        match step {
            Step::Cargo(step) => {
                let status = ProcessCommand::new("cargo")
                    .args(&step.arguments)
                    .envs(step.environment.iter().cloned())
                    .current_dir(repository)
                    .status()
                    .map_err(|error| format!("{}: could not run cargo: {error}", step.label))?;
                if !status.success() {
                    return Err(format!("{} failed with {status}", step.label));
                }
            }
            Step::VerifyMarkers {
                default_binary,
                failpoint_binary,
                ..
            } => verify_marker_isolation(default_binary, failpoint_binary)?,
        }
    }
    Ok(())
}

fn verify_marker_isolation(default_binary: &Path, failpoint_binary: &Path) -> Result<(), String> {
    const FAILPOINT_MARKER: &[u8] = b"RSI_META_TEST_ACK_GATE";
    if file_contains_bytes(default_binary, FAILPOINT_MARKER)? {
        return Err(format!(
            "default daemon binary `{}` contains the test acknowledgement gate",
            default_binary.display()
        ));
    }
    if !file_contains_bytes(failpoint_binary, FAILPOINT_MARKER)? {
        return Err(format!(
            "test-failpoints daemon binary `{}` does not contain the acknowledgement gate",
            failpoint_binary.display()
        ));
    }
    Ok(())
}

fn file_contains_bytes(path: &Path, needle: &[u8]) -> Result<bool, String> {
    if needle.is_empty() {
        return Ok(true);
    }
    let file = fs::File::open(path)
        .map_err(|error| format!("could not open `{}`: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut buffer = [0_u8; 4_096];
    let mut overlap = Vec::new();
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("could not read `{}`: {error}", path.display()))?;
        if read == 0 {
            return Ok(false);
        }
        let mut window = overlap;
        window.extend_from_slice(&buffer[..read]);
        if window
            .windows(needle.len())
            .any(|candidate| candidate == needle)
        {
            return Ok(true);
        }
        let retained = needle.len().saturating_sub(1).min(window.len());
        overlap = window.split_off(window.len() - retained);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn standalone_workspaces_are_discovered_in_stable_repository_order() {
        let repository = tempfile::tempdir().unwrap();
        for relative in [
            "plugins/rsi-meta/zeta/Cargo.toml",
            "fixtures/rsi-meta/beta/Cargo.toml",
            "plugins/rsi-meta/alpha/Cargo.toml",
            "fixtures/rsi-meta/not-a-package/README.md",
        ] {
            let path = repository.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "fixture").unwrap();
        }

        let discovered = discover_standalone_workspaces(repository.path()).unwrap();
        let relative = discovered
            .iter()
            .map(|path| path.strip_prefix(repository.path()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            relative,
            [
                Path::new("fixtures/rsi-meta/beta/Cargo.toml"),
                Path::new("plugins/rsi-meta/alpha/Cargo.toml"),
                Path::new("plugins/rsi-meta/zeta/Cargo.toml"),
            ]
        );
    }

    #[test]
    fn plugin_packages_are_discovered_from_their_owned_manifests() {
        let repository = tempfile::tempdir().unwrap();
        for relative in [
            "fixtures/rsi-meta/echo/Cargo.toml",
            "fixtures/rsi-meta/echo/plugin.toml",
            "fixtures/rsi-meta/runner/Cargo.toml",
            "plugins/rsi-meta/watch/Cargo.toml",
            "plugins/rsi-meta/watch/plugin.toml",
        ] {
            let path = repository.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "fixture").unwrap();
        }

        let discovered = discover_plugin_packages(repository.path()).unwrap();
        let relative = discovered
            .iter()
            .map(|path| path.strip_prefix(repository.path()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            relative,
            [
                Path::new("fixtures/rsi-meta/echo"),
                Path::new("plugins/rsi-meta/watch"),
            ]
        );
    }

    #[test]
    fn rustc_host_selects_the_supported_artifact_contract() {
        let linux = parse_host_target("rustc 1.97.0\nhost: x86_64-unknown-linux-gnu\n").unwrap();
        assert_eq!(linux.triple(), "x86_64-unknown-linux-gnu");
        assert_eq!(linux.dynamic_library_extension(), "so");

        let mac = parse_host_target("host: aarch64-apple-darwin\n").unwrap();
        assert_eq!(mac.triple(), "aarch64-apple-darwin");
        assert_eq!(mac.dynamic_library_extension(), "dylib");

        let unsupported = parse_host_target("host: x86_64-pc-windows-msvc\n").unwrap_err();
        assert!(unsupported.contains("not verified"));
    }

    #[test]
    fn lifecycle_probe_artifact_is_target_qualified() {
        let repository = Path::new("/workspace");
        assert_eq!(
            lifecycle_probe_artifact(repository, HostTarget::LinuxX86_64),
            Path::new(
                "/workspace/fixtures/rsi-meta/lifecycle-probe/target/x86_64-unknown-linux-gnu/release/librsi_meta_fixture_lifecycle_probe.so"
            )
        );
        assert_eq!(
            lifecycle_probe_artifact(repository, HostTarget::MacOsAarch64),
            Path::new(
                "/workspace/fixtures/rsi-meta/lifecycle-probe/target/aarch64-apple-darwin/release/librsi_meta_fixture_lifecycle_probe.dylib"
            )
        );
    }

    #[test]
    fn marker_scan_detects_a_value_across_read_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("binary");
        let marker = b"RSI_META_TEST_ACK_GATE";
        let mut contents = vec![b'x'; 4_095];
        contents.extend_from_slice(marker);
        contents.extend_from_slice(b"tail");
        fs::write(&binary, contents).unwrap();

        assert!(file_contains_bytes(&binary, marker).unwrap());
        assert!(!file_contains_bytes(&binary, b"missing-marker").unwrap());
    }

    #[test]
    fn complete_conformance_plan_ends_with_the_release_demonstration() {
        let repository = Path::new("/workspace");
        let workspaces = vec![
            repository.join("fixtures/rsi-meta/echo/Cargo.toml"),
            repository.join("plugins/rsi-meta/watch/Cargo.toml"),
        ];
        let plugins = vec![repository.join("plugins/rsi-meta/watch")];

        let plan = conformance_plan(repository, HostTarget::LinuxX86_64, &workspaces, &plugins);
        assert_eq!(
            plan.iter().map(Step::label).collect::<Vec<_>>(),
            [
                "build release plugin plugins/rsi-meta/watch",
                "format fixtures/rsi-meta/echo",
                "clippy fixtures/rsi-meta/echo",
                "test fixtures/rsi-meta/echo",
                "format plugins/rsi-meta/watch",
                "clippy plugins/rsi-meta/watch",
                "test plugins/rsi-meta/watch",
                "run real-library conformance",
                "build default rsi-meta-cli",
                "build failpoint rsi-meta-cli",
                "build release-demo",
                "build lifecycle-probe",
                "verify failpoint marker isolation",
                "run release demonstration",
            ]
        );
    }
}
