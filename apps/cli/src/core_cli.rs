//! Explicit core replacement; all executable bytes pass ordinary package admission.
use super::{HostProfileId, OsString, RsiError};
use rsi::CoreUpgrade;
use rsi_service_host::{HostOwnerMode, HostStartupLease, ServiceHostPaths};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use super::cli::CORE_HELP as HELP;

pub(super) fn ensure_no_pending() -> rsi::Result<()> {
    let executable = std::env::current_exe().map_err(error)?;
    if CoreUpgrade::has_pending(
        executable
            .parent()
            .ok_or_else(|| error("missing executable parent"))?,
    )? {
        return Err(error(
            "a core upgrade needs recovery; run rsi core recover with its original config/state homes",
        ));
    }
    Ok(())
}
pub(super) async fn run(arguments: &[OsString]) -> u8 {
    let command = match arguments {
        [operation] if operation == "upgrade" => Command::Upgrade(None),
        [operation, option, path] if operation == "upgrade" && option == "--local" => {
            Command::Upgrade(Some(PathBuf::from(path)))
        }
        [operation] if operation == "recover" => Command::Recover,
        _ => return super::report_error(&error(HELP)),
    };
    execute(command)
        .await
        .map_or_else(|failure| super::report_error(&failure), |()| 0)
}
enum Command {
    Upgrade(Option<PathBuf>),
    Recover,
}
async fn execute(command: Command) -> rsi::Result<()> {
    super::launch::maintain()?;
    let paths = super::launch::paths()?;
    let executable = std::env::current_exe().map_err(error)?;
    let package = if let Command::Upgrade(local) = &command {
        ensure_no_pending()?;
        Some(super::plugin_cli::install_core(local.as_deref()).await?)
    } else {
        None
    };
    let host = ServiceHostPaths::from_host_paths(&paths).map_err(error)?;
    let startup = HostStartupLease::try_acquire(&host).map_err(error)?;
    if matches!(command, Command::Recover) {
        let transaction = CoreUpgrade::recover(
            executable
                .parent()
                .ok_or_else(|| error("missing installation directory"))?,
            &paths,
        )?;
        restore(transaction, &paths, &host, &startup).await?;
        return Ok(());
    }
    let package = package.expect("upgrade acquired a package");
    let candidate = package
        .export("core", rsi_packages::ExportKind::Executable)
        .map_err(error)?;
    let export = &package.manifest().exports["core"];
    let digest = &package
        .manifest()
        .files
        .iter()
        .find(|file| file.path == export.path)
        .ok_or_else(|| error("core export is not a declared file"))?
        .digest;
    let was_running = active(&host)?;
    for home in [paths.config(), paths.state()] {
        rsi_files_native_fs::create_absolute_directory_no_follow(home).map_err(error)?;
    }
    let transaction = CoreUpgrade::prepare(&executable, &candidate, digest, &paths, was_running)?;
    if let Err(failure) = probe(&transaction.candidate()).await {
        transaction.finish(false)?;
        return Err(failure);
    }
    if was_running && let Err(failure) = super::host_cli::stop_host_daemon(false).await {
        transaction.finish(false)?;
        return Err(failure);
    }
    let result = async {
        transaction.promote()?;
        start(&transaction.executable(), &paths, &startup).await
    }
    .await;
    if let Err(failure) = result {
        return match restore(transaction, &paths, &host, &startup).await {
            Ok(()) => Err(error(format!(
                "new core activation failed; previous installation restored: {failure}"
            ))),
            Err(recovery) => Err(error(format!(
                "new core activation failed: {failure}; recovery remains pending: {recovery}"
            ))),
        };
    }
    let digest = transaction.candidate_digest().to_owned();
    let receipt = transaction.finish(true)?;
    println!(
        "{}",
        serde_json::json!({"event":"core_upgraded","sha256":digest,"receipt":receipt})
    );
    Ok(())
}
fn active(host: &ServiceHostPaths) -> rsi::Result<bool> {
    match host.read_lifecycle_identity().map_err(error)? {
        Some(owner) if owner.is_current().map_err(error)? => {
            if owner.mode != HostOwnerMode::Daemon {
                return Err(error(
                    "close the embedded Host owner before core maintenance",
                ));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}
async fn start(
    executable: &Path,
    paths: &rsi_host::HostPaths,
    startup: &HostStartupLease,
) -> rsi::Result<()> {
    super::host_cli::start_host_coordinated(
        &HostProfileId::new("standard").expect("constant"),
        false,
        paths,
        startup,
        Some(executable),
        tokio::time::Instant::now() + rsi::APPLICATION_TRANSITION_TIMEOUT,
    )
    .await
}
async fn restore(
    transaction: CoreUpgrade,
    paths: &rsi_host::HostPaths,
    host: &ServiceHostPaths,
    startup: &HostStartupLease,
) -> rsi::Result<()> {
    if active(host)? {
        super::host_cli::stop_host_daemon(false).await?;
    }
    transaction.rollback()?;
    if transaction.was_running() {
        start(&transaction.executable(), paths, startup).await?;
    }
    let receipt = transaction.finish(false)?;
    println!(
        "{}",
        serde_json::json!({"event":"core_recovered","receipt":receipt})
    );
    Ok(())
}
async fn probe(executable: &Path) -> rsi::Result<()> {
    let mut command = tokio::process::Command::new(executable);
    command
        .arg("--version")
        .env_clear()
        .envs(rsi::scrub_child_environment(std::env::vars_os()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW; no console is needed for a load probe.
    let mut child = command
        .spawn()
        .map_err(|_| error("candidate executable could not start"))?;
    match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        _ => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(error(
                "candidate executable failed its bounded version probe",
            ))
        }
    }
}
fn error(value: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("core: {value}"))
}
