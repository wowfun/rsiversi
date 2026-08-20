use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CargoStep {
    pub(crate) label: String,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: Vec<(OsString, OsString)>,
}

impl CargoStep {
    pub(crate) fn new(
        label: impl Into<String>,
        arguments: impl IntoIterator<Item = OsString>,
    ) -> Self {
        Self::with_env(label, arguments, [])
    }

    pub(crate) fn with_env(
        label: impl Into<String>,
        arguments: impl IntoIterator<Item = OsString>,
        environment: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Self {
        Self {
            label: label.into(),
            arguments: arguments.into_iter().collect(),
            environment: environment.into_iter().collect(),
        }
    }

    pub(crate) fn run(&self, repository: &Path) -> Result<(), String> {
        let status = Command::new("cargo")
            .args(&self.arguments)
            .envs(self.environment.iter().cloned())
            .current_dir(repository)
            .status()
            .map_err(|error| format!("{}: could not run cargo: {error}", self.label))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{} failed with {status}", self.label))
        }
    }
}

pub(crate) fn execute(repository: &Path, steps: &[CargoStep]) -> Result<(), String> {
    for (index, step) in steps.iter().enumerate() {
        eprintln!("[{}/{}] {}", index + 1, steps.len(), step.label);
        step.run(repository)?;
    }
    Ok(())
}

pub(crate) fn display_relative(repository: &Path, path: &Path) -> String {
    path.strip_prefix(repository)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeTarget {
    LinuxX86_64,
    MacOsAarch64,
}

impl NativeTarget {
    pub(crate) const fn triple(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Self::MacOsAarch64 => "aarch64-apple-darwin",
        }
    }

    pub(crate) const fn dynamic_library_extension(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "so",
            Self::MacOsAarch64 => "dylib",
        }
    }
}

pub(crate) fn parse_native_target(version: &str) -> Result<NativeTarget, String> {
    let target = version
        .lines()
        .find_map(|line| line.strip_prefix("host:").map(str::trim))
        .ok_or_else(|| "rustc -vV did not report a host target".to_owned())?;
    match target {
        "x86_64-unknown-linux-gnu" => Ok(NativeTarget::LinuxX86_64),
        "aarch64-apple-darwin" => Ok(NativeTarget::MacOsAarch64),
        _ => Err(format!(
            "native conformance is not verified for target {target}"
        )),
    }
}

pub(crate) fn detect_native_target(repository: &Path) -> Result<NativeTarget, String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .current_dir(repository)
        .output()
        .map_err(|error| format!("could not run `rustc -vV`: {error}"))?;
    if !output.status.success() {
        return Err(format!("`rustc -vV` failed with {}", output.status));
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|error| format!("`rustc -vV` returned non-UTF-8 output: {error}"))?;
    parse_native_target(&version)
}
