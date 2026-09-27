//! Application-owned development and paired publication commands.
use std::{path::Path, process::ExitCode};
#[cfg(unix)]
mod dev;
mod dist;
fn require_repository_root(root: &Path) -> Result<(), String> {
    if root.join("apps/devtools/Cargo.toml").is_file() && root.join("Cargo.toml").is_file() {
        Ok(())
    } else {
        Err("rsi-app-tools must run from the repository root".into())
    }
}
fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        #[cfg(unix)]
        [command, rest @ ..] if command == "dev" => dev::run(rest),
        #[cfg(not(unix))]
        [command, ..] if command == "dev" => Err("the isolated product development launcher currently requires Linux or WSL".into()),
        [command, rest @ ..] if command == "dist" => dist::run(rest),
        [command] if command == "gc" => dist::run(&["gc".into()]),
        _ => Err("usage: rsi-app-tools dev tui|web [OPTIONS] | dist web|desktop [ABSOLUTE_OUTPUT] [--debug] | gc".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
