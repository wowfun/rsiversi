//! Independent native maintenance; no installed main executable is invoked.
#![deny(unsafe_code)]
use rsi_installation::{Result, error};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::PathBuf,
};
const HELP: &str = "Usage: rsi-uninstall uninstall --interface-version 1 [OPTIONS]\nBootstrap: rsi-uninstall install --interface-version 1 --prefix DIR --binary FILE --uninstaller FILE --launcher FILE [--no-path]\n  --prefix DIR --config-home DIR --state-home DIR --cache-home DIR\n  --purge --dry-run --no-path --force --yes\nOrdinary uninstall preserves homes. Purge preserves user-authored configuration.\nForce permits confirmed fixed-file removal without a record; it never force-kills.\nUse the installed uninstall.ps1 launcher on Windows to remove the running tool.\n";
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rsi-uninstall: {e}");
            std::process::ExitCode::from(1)
        }
    }
}
async fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let operation = args.next().unwrap_or_else(|| "--help".into());
    if operation == "--installation-protocol" && args.len() == 0 {
        println!("{}", rsi_installation::PROTOCOL);
        return Ok(());
    }
    if operation == "--help" || operation == "-h" {
        print!("{HELP}");
        return Ok(());
    }
    if !cfg!(any(windows, target_os = "linux")) {
        return Err(error(
            "installation maintenance supports Windows and Linux/WSL",
        ));
    }
    if operation != "install" && operation != "uninstall" {
        return Err(error("expected install or uninstall"));
    }
    let mut values: BTreeMap<String, OsString> = BTreeMap::new();
    let mut flags = BTreeSet::new();
    while let Some(option) = args.next() {
        let option = option.to_str().ok_or_else(|| error("non-UTF8 option"))?;
        if option == "--help" || option == "-h" {
            print!("{HELP}");
            return Ok(());
        }
        if ["--purge", "--dry-run", "--no-path", "--force", "--yes"].contains(&option) {
            if !flags.insert(option.to_owned()) {
                return Err(error("duplicate flag"));
            }
        } else if [
            "--prefix",
            "--config-home",
            "--state-home",
            "--cache-home",
            "--binary",
            "--uninstaller",
            "--launcher",
            "--interface-version",
        ]
        .contains(&option)
        {
            let value = args
                .next()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| error(format!("missing {option} value")))?;
            if values.insert(option.into(), value).is_some() {
                return Err(error("duplicate option"));
            }
        } else {
            return Err(error(format!("unknown option: {option}")));
        }
    }
    if values.get("--interface-version").and_then(|v| v.to_str()) != Some("1") {
        return Err(error("expected --interface-version 1"));
    }
    let prefix = values
        .get("--prefix")
        .map(PathBuf::from)
        .map_or_else(rsi_installation::default_prefix, Ok)?;
    if operation == "install" {
        if flags.iter().any(|f| f != "--no-path") {
            return Err(error("unsupported install flag"));
        }
        if values.keys().any(|name| {
            matches!(
                name.as_str(),
                "--config-home" | "--state-home" | "--cache-home"
            )
        }) {
            return Err(error("install does not accept home overrides"));
        }
        let required = |key| {
            values
                .get(key)
                .map(PathBuf::from)
                .ok_or_else(|| error(format!("missing {key}")))
        };
        return rsi_installation::install(rsi_installation::InstallOptions {
            prefix,
            binary: required("--binary")?,
            uninstaller: required("--uninstaller")?,
            launcher: required("--launcher")?,
            no_path: flags.contains("--no-path"),
        })
        .await;
    }
    if ["--binary", "--launcher", "--uninstaller"]
        .iter()
        .any(|key| values.contains_key(*key))
    {
        return Err(error("uninstall does not accept install sources"));
    }
    rsi_installation::uninstall::uninstall(uninstall_options(prefix, &values, &flags)?).await
}

fn uninstall_options(
    prefix: PathBuf,
    values: &BTreeMap<String, OsString>,
    flags: &BTreeSet<String>,
) -> Result<rsi_installation::uninstall::Options> {
    let defaults = rsi_installation::standard_paths()?;
    let path = |name, default: &std::path::Path| -> Result<PathBuf> {
        rsi_installation::filesystem::absolute(
            &values
                .get(name)
                .map_or_else(|| default.to_owned(), PathBuf::from),
        )
    };
    let homes = rsi_host::HostPaths::new(
        path("--config-home", defaults.config())?,
        path("--state-home", defaults.state())?,
        path("--cache-home", defaults.cache())?,
    )
    .map_err(error)?;
    Ok(rsi_installation::uninstall::Options {
        prefix,
        homes,
        purge: flags.contains("--purge"),
        dry_run: flags.contains("--dry-run"),
        no_path: flags.contains("--no-path"),
        force: flags.contains("--force"),
        yes: flags.contains("--yes"),
    })
}
