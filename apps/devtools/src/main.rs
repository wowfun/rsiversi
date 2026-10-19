//! Application-owned development and paired publication commands.
use std::{path::Path, process::ExitCode};
// Diagnostics must not turn a closed consumer pipe into a build or cleanup panic.
macro_rules! diagnostic {
    ($($message:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr().lock(), $($message)*);
    }};
}
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
const USAGE: &str = "usage: rsi-app-tools dev tui|web [OPTIONS] | dist tui|web|desktop [ABSOLUTE_OUTPUT] [--debug] [--with-ssh-helper] [--verbose] | dist addon-workbench ABSOLUTE_NEW_REPORT | fixture-build profile-leaf-probe ABSOLUTE_NEW_REPORT | gc [tui|web|desktop|all] | --help | -h";
fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        #[cfg(unix)]
        [command, rest @ ..] if command == "dev" => dev::run(rest),
        #[cfg(not(unix))]
        [command, ..] if command == "dev" => {
            Err("the isolated product development launcher currently requires Linux or WSL".into())
        }
        [command, rest @ ..] if command == "dist" => dist::run(rest),
        [command, rest @ ..] if command == "fixture-build" => dist::fixture_build(rest),
        [command, rest @ ..] if command == "gc" => dist::gc(rest),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            diagnostic!("{error}");
            ExitCode::FAILURE
        }
    }
}
