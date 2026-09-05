#[cfg(target_os = "linux")]
use rsi::StandardSessionDaemon;
use rsi::{
    AgentPresetManager, AgentPresetSource, AgentPresetTrust, ApplicationKind, ApplicationProfileId,
    HostProfileId, ProfileCatalog, ProfileSource, RsiError, StandardCodingTools,
    StandardComposition, capture_standard_environment, connect_or_embed_session_host,
    maybe_run_apply_patch_helper, scrub_child_environment, standard_agent_preset_root,
    standard_paths,
};
use rsi_agent_presets::{AgentPresetHealth, AgentPresetId, AgentPresetRow, PresetError};
use rsi_agent_session_protocol::{
    AgentControlRecordBody, MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS, MAXIMUM_TURN_TEXT_BYTES,
    MessageId, SessionFact, SessionFactBody, SessionId, TurnId, TurnOutcome, WorkspaceTrust,
};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_agent_turn_protocol::{CancelTarget, MessageState, ObservationCursor, SessionObservation};
use rsi_ai_protocol::{ContentDelta, LanguageEvent, ModelRef};
use rsi_sandbox::SandboxMode;
use rsi_session::{
    CreateSession, MAXIMUM_SESSION_INPUT_IMAGE_BYTES, SessionApplication, SessionApplicationError,
    SessionHandle, SessionInput as MessageInput, SubmitInput,
};
#[cfg(target_os = "linux")]
use rsi_session_host::UdsSessionApplication;
#[cfg(target_os = "linux")]
use rsi_session_host::{
    HostOwnerMode, HostSignal, SESSION_HOST_DRAIN_TIMEOUT, SessionHostDiagnostics,
    SessionHostDiagnosticsSnapshot, SessionHostPaths, owner_process_is_current, signal_owner,
};
use rsi_tools_protocol::ToolContent;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::future::Future as _;
use std::io::Read as _;
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::task::JoinHandle;
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

mod application;
mod cli;
mod host_cli;
mod management;

use application::{
    HeadlessTurnOptions, OutputMode, SessionSelection, report_error, run_application,
    standard_coding_tools,
};
use cli::{
    AgentPresetCommand, AgentPresetOperation, AgentStoreCommand, ApplicationInvocation,
    BOOT_FAILURE_EXIT_CODE, Command, HostCommand, HostOperation, ManagementOutput, Parse,
    ProfileCommand, ProfileKind, ProfileOperationKind, output_value, path_value, run_preset_value,
    session_value, set_flag, set_option, usage, utf8,
};
use host_cli::run_host;
use management::{
    profile_management_error, run_agent_preset, run_agent_store, run_profile,
    shutdown_agent_preset_manager, write_text_line,
};

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
    match Command::parse_cli(std::env::args_os().skip(1)) {
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
        Ok(Parse::Host(command)) => run_host(command).await,
        Ok(Parse::AgentPreset(command)) => run_agent_preset(command).await,
        Ok(Parse::AgentStore(command)) => run_agent_store(command).await,
        Ok(Parse::Run(_)) => report_error(&usage("internal headless parser escaped its scope")),
        Err(error) => report_error(&error),
    }
}

async fn prepare_standard_composition(
    paths: rsi_host::HostPaths,
) -> rsi::Result<(StandardComposition, AgentPresetManager)> {
    let environment = capture_standard_environment()?;
    let coding_tools = standard_coding_tools()?;
    let system_root =
        standard_agent_preset_root(&paths).map_err(|error| RsiError::Boot(error.to_string()))?;
    let presets =
        AgentPresetManager::open_standard(paths.clone(), system_root, coding_tools.is_some())
            .await?;
    let composition = StandardComposition::new(paths, environment, coding_tools)
        .with_agent_presets(presets.catalog().clone());
    Ok((composition, presets))
}

#[cfg(test)]
mod tests;
