#[cfg(target_os = "linux")]
use rsi::probe_service_host;
use rsi::{
    AgentPresetManager, AgentPresetSource, AgentPresetTrust, ApplicationProfileId, HostProfileId,
    ProfileCatalog, ProfileSource, RsiError, StandardComposition, capture_standard_environment,
    maybe_run_apply_patch_helper, standard_agent_preset_root, standard_paths,
};
#[cfg(target_os = "linux")]
use rsi::{StandardCodingTools, StandardServiceDaemon, scrub_child_environment};
use rsi_agent_presets::{AgentPresetHealth, AgentPresetId, AgentPresetRow, PresetError};
use rsi_agent_store_sqlite::SqliteStore;
#[cfg(target_os = "linux")]
use rsi_service_host::{
    HostOwnerMode, HostSignal, SERVICE_HOST_DRAIN_TIMEOUT, ServiceHostDiagnostics,
    ServiceHostDiagnosticsSnapshot, ServiceHostPaths, owner_process_is_current, signal_owner,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use tokio::task::JoinHandle;
#[cfg(target_os = "linux")]
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
const HOST_SHUTDOWN_MARGIN: Duration = Duration::from_secs(15);
#[cfg(target_os = "linux")]
const FORCE_HOST_STOP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const HOST_DIAGNOSTICS_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const DAEMON_READINESS_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(target_os = "linux")]
fn daemon_readiness_timeout_error() -> RsiError {
    RsiError::Boot(format!(
        "daemon readiness probe exceeded {} seconds",
        DAEMON_READINESS_TIMEOUT.as_secs()
    ))
}

#[cfg(target_os = "linux")]
fn standard_coding_tools() -> rsi::Result<Option<StandardCodingTools>> {
    let helper = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| {
            RsiError::Boot(format!("failed to resolve current executable: {error}"))
        })?;
    let bash = std::fs::canonicalize("/bin/bash")
        .map_err(|error| RsiError::Boot(format!("/bin/bash is unavailable: {error}")))?;
    let environment = scrub_child_environment(std::env::vars_os());
    StandardCodingTools::new(bash, helper, environment).map(Some)
}

mod addon_cli;
mod application;
mod cli;
#[cfg(target_os = "linux")]
mod host_cli;
mod management;

use application::{report_error, run_application};
use cli::{
    AgentPresetCommand, AgentPresetOperation, AgentStoreCommand, ApplicationInvocation,
    BOOT_FAILURE_EXIT_CODE, ManagementOutput, Parse, ProfileCommand, ProfileKind,
    ProfileOperationKind,
};
#[cfg(target_os = "linux")]
use cli::{HostCommand, HostOperation};
#[cfg(target_os = "linux")]
use host_cli::run_host;
use management::{profile_management_error, run_agent_preset, run_agent_store, run_profile};

fn main() -> ExitCode {
    if let Some(exit) = maybe_run_apply_patch_helper(std::env::args_os().skip(1)) {
        return ExitCode::from(exit);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to construct Tokio runtime: {error}");
            return ExitCode::from(BOOT_FAILURE_EXIT_CODE);
        }
    };
    ExitCode::from(runtime.block_on(run_main()))
}

async fn run_main() -> u8 {
    match cli::parse_cli(std::env::args_os().skip(1)) {
        Ok(Parse::Help(help)) => {
            print!("{help}");
            0
        }
        Ok(Parse::Version) => {
            println!("rsi {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Ok(Parse::Application(command)) => run_application(command).await,
        Ok(Parse::Profile(command)) => run_profile(&command).await,
        #[cfg(target_os = "linux")]
        Ok(Parse::Host(command)) => run_host(command).await,
        #[cfg(not(target_os = "linux"))]
        Ok(Parse::HostUnsupported) => report_error(&RsiError::Boot(
            "Service Host daemon mode requires Linux process-generation fencing; named applications use embedded mode on this platform".into(),
        )),
        Ok(Parse::AgentPreset(command)) => run_agent_preset(command).await,
        Ok(Parse::AgentStore(command)) => run_agent_store(command).await,
        #[cfg(unix)]
        Ok(Parse::Addon(command)) => addon_cli::run(command).await,
        #[cfg(not(unix))]
        Ok(Parse::AddonUnsupported) => report_error(&RsiError::Boot("native addon source management requires Unix".into())),
        Err(error) => report_error(&error),
    }
}

#[cfg(target_os = "linux")]
async fn prepare_standard_composition(
    parent: Option<&rsi_meta::Context>,
    paths: rsi_host::HostPaths,
) -> rsi::Result<(StandardComposition, AgentPresetManager)> {
    let environment = capture_standard_environment()?;
    #[cfg(target_os = "linux")]
    let coding_tools = standard_coding_tools()?;
    #[cfg(not(target_os = "linux"))]
    let coding_tools = None;
    let system_root =
        standard_agent_preset_root(&paths).map_err(|error| RsiError::Boot(error.to_string()))?;
    let composition = StandardComposition::new(paths, environment, coding_tools);
    let presets = if let Some(parent) = parent {
        AgentPresetManager::open_standard_in(parent, &composition, system_root).await?
    } else {
        AgentPresetManager::open_standard(&composition, system_root).await?
    };
    let composition = composition.with_agent_presets(&presets)?;
    Ok((composition, presets))
}

#[cfg(test)]
mod tests;
