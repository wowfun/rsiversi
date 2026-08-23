use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CargoStep {
    pub(crate) label: String,
    pub(crate) arguments: Vec<OsString>,
}

impl CargoStep {
    pub(crate) fn new(
        label: impl Into<String>,
        arguments: impl IntoIterator<Item = OsString>,
    ) -> Self {
        Self {
            label: label.into(),
            arguments: arguments.into_iter().collect(),
        }
    }

    pub(crate) fn run(&self, repository: &Path) -> Result<(), String> {
        let status = Command::new("cargo")
            .args(&self.arguments)
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
