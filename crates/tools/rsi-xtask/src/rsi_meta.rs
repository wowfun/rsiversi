use std::path::Path;

use crate::cargo_step::{self, CargoStep};
use crate::repository_root;

pub fn run(repository: &Path) -> Result<(), String> {
    repository_root::require(repository, "rsi-meta conformance")?;
    let packages = ["rsi-meta", "rsi-meta-plugin", "rsi-meta-loader"];
    let mut steps = Vec::new();
    steps.push(CargoStep::new(
        "format rsi-meta foundation",
        ["fmt".into(), "--all".into(), "--check".into()],
    ));
    for package in packages {
        steps.push(CargoStep::new(
            format!("clippy {package}"),
            [
                "clippy".into(),
                "--locked".into(),
                "-p".into(),
                package.into(),
                "--all-targets".into(),
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
            ],
        ));
    }
    cargo_step::execute(repository, &steps)
}
