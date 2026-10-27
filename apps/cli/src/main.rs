#[cfg(any(target_os = "linux", windows))]
use rsi::StandardServiceDaemon;
#[cfg(any(target_os = "linux", windows))]
use rsi::probe_service_host;
use rsi::{
    AgentPresetManager, AgentPresetSource, AgentPresetTrust, ApplicationProfileId, HostProfileId,
    ProfileCatalog, ProfileSource, RsiError, StandardComposition, capture_standard_environment,
    maybe_run_apply_patch_helper, standard_agent_preset_root,
};
#[cfg(any(target_os = "linux", windows))]
use rsi::{StandardCodingTools, scrub_child_environment};
use rsi_agent_presets::{AgentPresetHealth, AgentPresetId, AgentPresetRow, PresetError};
use rsi_agent_store_sqlite::SqliteStore;
#[cfg(any(target_os = "linux", windows))]
use rsi_service_host::{
    HostOwnerMode, ServiceHostDiagnostics, ServiceHostDiagnosticsSnapshot, ServiceHostPaths,
    owner_process_is_current,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(any(target_os = "linux", windows))]
use std::process::Stdio;
#[cfg(any(target_os = "linux", windows))]
use std::time::Duration;
#[cfg(any(target_os = "linux", windows))]
use tokio::task::JoinHandle;
#[cfg(any(target_os = "linux", windows))]
use tokio_util::sync::CancellationToken;

#[cfg(any(target_os = "linux", windows))]
const FORCE_HOST_STOP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(any(target_os = "linux", windows))]
const HOST_DIAGNOSTICS_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(any(target_os = "linux", windows))]
const DAEMON_READINESS_TIMEOUT: Duration = rsi::APPLICATION_TRANSITION_TIMEOUT;

#[cfg(any(target_os = "linux", windows))]
fn daemon_readiness_timeout_error() -> RsiError {
    RsiError::Boot(format!(
        "daemon readiness probe exceeded {} seconds",
        DAEMON_READINESS_TIMEOUT.as_secs()
    ))
}

#[cfg(any(target_os = "linux", windows))]
fn standard_coding_tools() -> rsi::Result<Option<StandardCodingTools>> {
    StandardCodingTools::new(
        rsi::capture_standard_bash()?,
        std::env::current_exe().map_err(|e| RsiError::Boot(e.to_string()))?,
        scrub_child_environment(std::env::vars_os()),
    )
    .map(Some)
}

mod addon_cli;
mod application;
mod cli;
#[cfg(any(target_os = "linux", windows))]
mod core_cli;
#[cfg(any(target_os = "linux", windows))]
mod host_cli;
#[cfg(any(target_os = "linux", windows))]
mod host_runtime;
mod import_cli;
mod launch;
mod management;
mod plugin_cli;
#[cfg(any(target_os = "linux", windows))]
mod source_build;
use launch::paths as standard_paths;

use application::{report_error, run_application};
use cli::{
    AgentPresetCommand, AgentPresetOperation, AgentStoreCommand, ApplicationInvocation,
    BOOT_FAILURE_EXIT_CODE, ManagementOutput, Parse, ProfileCommand, ProfileKind,
    ProfileOperationKind,
};
#[cfg(any(target_os = "linux", windows))]
use cli::{HostCommand, HostOperation};
#[cfg(any(target_os = "linux", windows))]
use host_cli::run_host;
#[cfg(any(target_os = "linux", windows))]
use management::profile_management_error;
use management::{run_agent_preset, run_agent_store, run_profile};

fn main() -> ExitCode {
    if std::env::args_os()
        .skip(1)
        .eq([OsString::from("--installation-protocol")])
    {
        println!("{}", rsi_installation::PROTOCOL);
        return ExitCode::SUCCESS;
    }
    let _generation = match rsi_build_info::GenerationLease::current() {
        Ok(lease) => lease,
        Err(error) => {
            eprintln!("cannot pin application generation: {error}");
            return ExitCode::from(2);
        }
    };
    #[cfg(windows)]
    if let Some(exit) = source_build::run_batch_helper() {
        return ExitCode::from(exit);
    }
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
    let (options, arguments) = match launch::parse(std::env::args_os().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => return report_error(&error),
    };
    let default_application = arguments.is_empty();
    let parsed = match cli::parse_cli(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return report_error(&error),
    };
    if let Err(error) = launch::validate_command(&options, &parsed) {
        return report_error(&error);
    }
    if let Parse::Application(invocation) = &parsed
        && application::print_help(invocation)
    {
        return 0;
    }
    #[cfg(any(target_os = "linux", windows))]
    if !matches!(
        &parsed,
        Parse::Core(_)
            | Parse::Help(_)
            | Parse::Version
            | Parse::Host(HostCommand {
                operation: HostOperation::Stop | HostOperation::Status,
                ..
            })
    ) && let Err(error) = core_cli::ensure_no_pending()
    {
        return report_error(&error);
    }
    if matches!(parsed, Parse::Help(_) | Parse::Version) {
        // Help and version remain independent of path/configuration availability.
    } else if let Err(error) = launch::initialize(options) {
        return report_error(&error);
    }
    if let Err(error) = rsi_service_host::set_installation_origin_resolver(|| {
        match rsi_installation::origin::current() {
            Ok(origin) => origin,
            Err(error) => {
                eprintln!("Installation provenance unavailable: {error}");
                None
            }
        }
    }) {
        return report_error(&RsiError::Boot(error.to_string()));
    }
    if matches!(
        &parsed,
        Parse::Profile(_) | Parse::AgentPreset(_) | Parse::AgentStore(_)
    ) && let Err(error) = launch::maintain()
    {
        return report_error(&error);
    }
    #[cfg(unix)]
    if matches!(&parsed, Parse::Addon(_))
        && let Err(error) = launch::maintain()
    {
        return report_error(&error);
    }
    run_command(parsed, default_application).await
}

async fn run_command(parsed: Parse, default_application: bool) -> u8 {
    match parsed {
        Parse::Help(help) => {
            print!("{help}");
            0
        }
        Parse::Version => {
            println!("rsi {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Parse::Application(mut command) => {
            if let Err(error) = launch::maintain() {
                return report_error(&error);
            }
            let configuration = match launch::current().and_then(launch::Startup::configuration) {
                Ok(configuration) => configuration,
                Err(error) => return report_error(&error),
            };
            if default_application {
                command.profile = match ApplicationProfileId::new(configuration.default_application) {
                    Ok(profile) => profile,
                    Err(error) => return report_error(&RsiError::Boot(error.to_string())),
                };
            }
            run_application(command).await
        }
        Parse::Config(arguments) => launch::run_config(&arguments),
        Parse::Import(arguments) => import_cli::run(&arguments),
        #[cfg(any(target_os = "linux", windows))]
        Parse::Core(arguments) => core_cli::run(&arguments).await,
        #[cfg(not(any(target_os = "linux", windows)))]
        Parse::Core(_) => report_error(&RsiError::Boot("core maintenance requires Windows or Linux".into())),
        Parse::Plugin(arguments) => plugin_cli::run(&arguments).await,
        Parse::WebControl(stop) => application::web_control(stop).await,
        Parse::Profile(command) => run_profile(&command).await,
        #[cfg(any(target_os = "linux", windows))]
        Parse::Host(command) => run_host(command).await,
        #[cfg(not(any(target_os = "linux", windows)))]
        Parse::HostUnsupported => report_error(&RsiError::Boot(
            "Service Host daemon mode requires Linux process-generation fencing; named applications use embedded mode on this platform".into(),
        )),
        Parse::AgentPreset(command) => run_agent_preset(command).await,
        Parse::AgentStore(command) => run_agent_store(command).await,
        #[cfg(unix)]
        Parse::Addon(command) => addon_cli::run(command).await,
        #[cfg(not(unix))]
        Parse::AddonUnsupported => report_error(&RsiError::Boot("native addon source management requires Unix".into())),
    }
}

#[cfg(any(target_os = "linux", windows))]
async fn prepare_standard_composition(
    parent: Option<&rsi_meta::Context>,
    paths: rsi_host::HostPaths,
) -> rsi::Result<(StandardComposition, AgentPresetManager)> {
    let environment = capture_standard_environment()?;
    #[cfg(any(target_os = "linux", windows))]
    let coding_tools = standard_coding_tools()?;
    #[cfg(not(any(target_os = "linux", windows)))]
    let coding_tools = None;
    let system_root =
        standard_agent_preset_root(&paths).map_err(|error| RsiError::Boot(error.to_string()))?;
    let listener_addons = rsi_serve::service_addons(paths.clone())?;
    let composition = StandardComposition::new(
        paths,
        environment,
        coding_tools,
        rsi_app_catalog::metadata(),
    )
    .with_user_home(rsi::capture_standard_home()?)?
    .with_watch_policy(rsi_meta_profile::SourceWatchPolicy::leased())
    .with_addons(listener_addons);
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
