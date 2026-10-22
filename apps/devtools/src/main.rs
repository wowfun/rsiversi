//! Isolated application conformance fixture tooling.
use std::{path::Path, process::ExitCode};
// Diagnostics must not turn a closed consumer pipe into a build or cleanup panic.
macro_rules! diagnostic {
    ($($message:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr().lock(), $($message)*);
    }};
}
mod dist;
#[cfg(unix)]
mod fixture_dev;
mod supervision;
fn fixture_lease(bundle: &str) -> Result<(), String> {
    use std::io::Write as _;
    let path = Path::new(bundle);
    if !path.is_absolute() {
        return Err("fixture-lease requires an absolute bundle path".into());
    }
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    if !path.is_dir() {
        return Err("fixture-lease requires a bundle directory".into());
    }
    let _lease = rsi_build_info::GenerationLease::open(&path).map_err(|error| error.to_string())?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(b"ready\n")
        .and_then(|()| stdout.flush())
        .map_err(|error| error.to_string())?;
    std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink())
        .map_err(|error| error.to_string())?;
    Ok(())
}
fn require_repository_root(root: &Path) -> Result<(), String> {
    if root.join("apps/devtools/Cargo.toml").is_file() && root.join("Cargo.toml").is_file() {
        Ok(())
    } else {
        Err("rsi-app-tools must run from the repository root".into())
    }
}
const USAGE: &str = "usage: rsi-app-tools fixture-dev tui|web [OPTIONS] | fixture-dist tui|web|desktop [ABSOLUTE_OUTPUT] [--debug] [--with-ssh-helper] [--verbose] | fixture-dist addon-workbench ABSOLUTE_NEW_REPORT | fixture-path tui|web|desktop | fixture-build profile-leaf-probe ABSOLUTE_NEW_REPORT | gc [tui|web|desktop|all] | fixture-lease ABSOLUTE_BUNDLE | --help | -h";
fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        [command, bundle] if command == "fixture-lease" => fixture_lease(bundle),
        #[cfg(unix)]
        [command, rest @ ..] if command == "fixture-dev" => fixture_dev::run(rest),
        #[cfg(not(unix))]
        [command, ..] if command == "fixture-dev" => {
            Err("the isolated product development launcher currently requires Linux or WSL".into())
        }
        [command, rest @ ..] if command == "fixture-dist" => dist::run(rest),
        [command, target] if command == "fixture-path" => dist::fixture_path(target),
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
