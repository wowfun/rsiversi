use super::{OsString, RsiError};
use rsi_packages::{InstallSource, Manifest, PackageStore, Provenance};
use std::{io::Read as _, path::Path};
mod download;

use super::cli::PLUGIN_HELP as HELP;

pub(super) async fn run(arguments: &[OsString]) -> u8 {
    let result: rsi::Result<()> = async {
        let command = match arguments.first().and_then(|arg| arg.to_str()) {
            Some("list" | "gc") if arguments.len() == 1 => arguments[0].to_str().unwrap(),
            Some("install")
                if arguments.get(1).is_some_and(|arg| arg == "--local")
                    && (3..=130).contains(&arguments.len()) =>
            {
                "local"
            }
            Some("install" | "uninstall")
                if (2..=129).contains(&arguments.len())
                    && arguments[1..]
                        .iter()
                        .all(|arg| arg.to_str().is_some_and(|name| !name.starts_with('-'))) =>
            {
                arguments[0].to_str().unwrap()
            }
            _ => return Err(error(HELP)),
        };
        super::launch::maintain()?;
        let paths = super::launch::paths()?;
        let store = PackageStore::open(&paths.state().join("packages")).map_err(error)?;
        let resolution = if command == "local" {
            let cwd = std::env::current_dir().map_err(error)?;
            let sources = arguments[2..]
                .iter()
                .map(|arg| local_source(Path::new(arg), &cwd))
                .collect::<rsi::Result<Vec<_>>>()?;
            store
                .install(
                    &sources,
                    &[],
                    &rsi_serve::supported_protocols(),
                    &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                )
                .map_err(error)?
        } else if command == "install" {
            let names = arguments[1..]
                .iter()
                .map(|arg| arg.to_str().unwrap().to_owned())
                .collect();
            let repository =
                download::configured(super::launch::current()?.configuration()?.repository)?;
            download::install(
                &store,
                repository,
                names,
                &rsi_serve::supported_protocols(),
                &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            )
            .await?
        } else if command == "uninstall" {
            store
                .uninstall(
                    &arguments[1..]
                        .iter()
                        .map(|arg| arg.to_str().unwrap().to_owned())
                        .collect::<Vec<_>>(),
                )
                .map_err(error)?
        } else if command == "gc" {
            println!(
                "{}",
                serde_json::to_string_pretty(&store.collect().map_err(error)?).map_err(error)?
            );
            return Ok(());
        } else {
            store.resolution().map_err(error)?
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&resolution).map_err(error)?
        );
        Ok(())
    }
    .await;
    result.map_or_else(|error| super::report_error(&error), |()| 0)
}

fn local_source(path: &Path, cwd: &Path) -> rsi::Result<InstallSource> {
    let directory = rsi::normalize_launch_path(path, cwd)?;
    let input =
        rsi_files_native_fs::open_absolute_directory_no_follow(&directory).map_err(error)?;
    let mut manifest = Vec::new();
    rsi_files_native_fs::open_relative_file_no_follow(&input, Path::new("package.json"))
        .map_err(error)?
        .take(rsi_packages::MAX_DOCUMENT as u64 + 1)
        .read_to_end(&mut manifest)
        .map_err(error)?;
    Manifest::parse(&manifest).map_err(error)?;
    Ok(InstallSource {
        directory,
        manifest,
        provenance: Provenance::Local,
    })
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn install_core(local: Option<&Path>) -> rsi::Result<rsi_packages::PackageLease> {
    let paths = super::launch::paths()?;
    let store = PackageStore::open(&paths.state().join("packages")).map_err(error)?;
    let target = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    if let Some(local) = local {
        let source = local_source(local, &std::env::current_dir().map_err(error)?)?;
        if Manifest::parse(&source.manifest).map_err(error)?.name != "core" {
            return Err(error("core upgrade requires a core package"));
        }
        store
            .install(&[source], &[], &rsi_serve::supported_protocols(), &target)
            .map_err(error)?;
    } else {
        let repository =
            download::configured_core(super::launch::current()?.configuration()?.repository)?;
        download::install(
            &store,
            repository,
            vec!["core".into()],
            &rsi_serve::supported_protocols(),
            &target,
        )
        .await?;
    }
    store.acquire("core").map_err(error)
}

fn error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("plugin: {error}"))
}
