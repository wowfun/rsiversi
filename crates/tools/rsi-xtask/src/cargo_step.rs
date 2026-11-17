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

pub(crate) fn execute(repository: &Path, steps: &[CargoStep]) -> Vec<Result<(), String>> {
    execute_with(steps, |step| step.run(repository))
}

fn execute_with(
    steps: &[CargoStep],
    mut run: impl FnMut(&CargoStep) -> Result<(), String>,
) -> Vec<Result<(), String>> {
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            eprintln!("[{}/{}] {}", index + 1, steps.len(), step.label);
            let outcome = run(step);
            if let Err(error) = &outcome {
                eprintln!("{error}");
            }
            outcome
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_steps_run_after_a_failure_and_retain_each_outcome() {
        let steps = ["lint", "test", "release"].map(|name| CargoStep::new(name, []));
        let mut visited = Vec::new();
        let outcomes = execute_with(&steps, |step| {
            visited.push(step.label.clone());
            if step.label == "lint" {
                Err("lint refused".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(visited, ["lint", "test", "release"]);
        assert_eq!(outcomes, [Err("lint refused".into()), Ok(()), Ok(())]);
    }
}
