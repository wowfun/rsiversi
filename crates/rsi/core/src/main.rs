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
    MAXIMUM_TURN_TEXT_BYTES, SessionFact, SessionFactBody, SessionId, TurnId, TurnOutcome,
};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::{ContentDelta, LanguageEvent, ModelRef};
use rsi_sandbox::SandboxMode;
use rsi_session::{CreateSession, SessionApplication, SessionHandle, SubmitText};
#[cfg(target_os = "linux")]
use rsi_session_host::UdsSessionApplication;
#[cfg(target_os = "linux")]
use rsi_session_host::{
    HostOwnerMode, HostSignal, SESSION_HOST_DRAIN_TIMEOUT, SessionHostPaths,
    owner_process_is_current, signal_owner,
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

const HELP: &str = "Usage:\n\
  rsi --profile PROFILE [APPLICATION ARGUMENTS]\n\
      headless: TASK|--stdin [--cwd PATH] [--resume SESSION|--session-id SESSION]\n\
                [--turn-id TURN] [--agent-preset ID] [--deployment ID --model ID]\n\
                [--sandbox MODE] [--output text|jsonl]\n\
      session:  [--cwd PATH] [--resume SESSION|--session-id SESSION]\n\
                [--agent-preset ID] [--output text|jsonl]\n\
  rsi profile <application|host> <COMMAND> [--output text|json]\n\
  rsi host <start|serve|restart|stop|status|reload> [--profile HOST]\n\
  rsi agent-preset <COMMAND> [--output text|json]\n\
  rsi agent-store verify [--root ABSOLUTE] [--output text|json]\n\n\
Commands:\n\
  --profile       Run a named Session or headless Application Profile\n\
  profile         Inspect and manage Application and Host Profiles\n\
  host            Control the explicit local Session Host daemon\n\
  agent-preset    Inspect and manage local Agent presets\n\
  agent-store     Verify the durable Agent Store\n";
const PROFILE_HELP: &str = "Usage:\n\
  rsi profile <application|host> list [--output text|json]\n\
  rsi profile <application|host> show ID [--output text|json]\n\
  rsi profile <application|host> path ID [--output text|json]\n\
  rsi profile <application|host> copy FROM TO [--output text|json]\n\
  rsi profile <application|host> delete ID [--output text|json]\n\
  rsi profile host preview ID [--output text|json]\n";
const HOST_HELP: &str = "Usage:\n\
  rsi host start [--profile HOST]\n\
  rsi host serve [--profile HOST]\n\
  rsi host restart [--profile HOST] [--force]\n\
  rsi host stop [--force]\n\
  rsi host status\n\
  rsi host reload\n";
const AGENT_PRESET_HELP: &str = "Usage:\n\
  rsi agent-preset list [--output text|json]\n\
  rsi agent-preset show ID [--output text|json]\n\
  rsi agent-preset path ID [--output text|json]\n\
  rsi agent-preset copy --from SOURCE --id ID [--name NAME] [--output text|json]\n\
  rsi agent-preset delete ID [--output text|json]\n\
  rsi agent-preset default <get|set ID|clear> [--output text|json]\n\n\
Commands:\n\
  list       List the fresh precedence-resolved roster\n\
  show       Show one row and its bounded composition when healthy\n\
  path       Print the winning local preset directory\n\
  copy       Copy a discovered preset into the user root\n\
  delete     Delete a winning user-root preset\n\
  default    Get, set, or clear the user default\n";
const AGENT_PRESET_DEFAULT_HELP: &str = "Usage:\n\
  rsi agent-preset default get [--output text|json]\n\
  rsi agent-preset default set ID [--output text|json]\n\
  rsi agent-preset default clear [--output text|json]\n\n\
Commands:\n\
  get      Print the effective default\n\
  set      Store one syntactically valid preset id\n\
  clear    Re-inherit the deployment default\n";
const AGENT_STORE_HELP: &str = "Usage:\n\
  rsi agent-store verify [--root ABSOLUTE] [--output text|json]\n\n\
Commands:\n\
  verify    Run an offline full integrity audit without creating a Store\n";
const BOOT_FAILURE_EXIT_CODE: u8 = 2;

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

async fn run_agent_store(command: AgentStoreCommand) -> u8 {
    let root = match command.root {
        Some(root) => root,
        None => match standard_paths() {
            Ok(paths) => paths.state().join("agent"),
            Err(error) => return report_error(&error),
        },
    };
    let verify_root = root.clone();
    let verification = tokio::task::spawn_blocking(move || SqliteStore::verify(verify_root)).await;
    match verification {
        Ok(Ok(())) => {
            let output = match command.output {
                ManagementOutput::Text => write_text_line(&format!("verified\t{}", root.display())),
                ManagementOutput::Json => write_json(&AgentStoreVerifyOutput {
                    version: 1,
                    kind: "agent_store_verify",
                    status: "ok",
                    root: &root,
                }),
            };
            output.map_or_else(|error| report_error(&error), |()| 0)
        }
        Ok(Err(error)) => report_error(&RsiError::Boot(format!(
            "Agent Store verification failed: {error}"
        ))),
        Err(error) => report_error(&RsiError::Boot(format!(
            "Agent Store verification worker failed: {error}"
        ))),
    }
}

#[allow(clippy::too_many_lines)] // One closed Profile command matrix owns all output variants.
async fn run_profile(command: &ProfileCommand) -> u8 {
    if matches!(
        (command.kind, command.operation),
        (ProfileKind::Host, ProfileOperationKind::Preview)
    ) {
        return run_host_profile_preview(command).await;
    }
    let result = (|| -> rsi::Result<()> {
        let paths = standard_paths()?;
        let catalog = ProfileCatalog::new(paths.clone());
        match (command.kind, command.operation) {
            (ProfileKind::Application, ProfileOperationKind::List) => {
                let rows = catalog
                    .list_applications()
                    .map_err(profile_management_error)?;
                write_profile_rows(
                    command.output,
                    "application_profile_list",
                    rows.into_iter()
                        .map(|row| (row.id.to_string(), row.source))
                        .collect(),
                )
            }
            (ProfileKind::Host, ProfileOperationKind::List) => {
                let rows = catalog.list_hosts().map_err(profile_management_error)?;
                write_profile_rows(
                    command.output,
                    "host_profile_list",
                    rows.into_iter()
                        .map(|row| (row.id.to_string(), row.source))
                        .collect(),
                )
            }
            (ProfileKind::Application, ProfileOperationKind::Show) => {
                let id = application_profile_id(&command.ids[0])?;
                let document = catalog.application(&id).map_err(profile_management_error)?;
                let contents = toml::to_string_pretty(&document.profile)
                    .map_err(|error| RsiError::Boot(error.to_string()))?;
                write_profile_document(
                    command.output,
                    "application_profile",
                    id.as_str(),
                    document.source,
                    document.path.as_deref(),
                    &contents,
                )
            }
            (ProfileKind::Host, ProfileOperationKind::Show) => {
                let id = host_profile_id(&command.ids[0])?;
                let document = catalog.host(&id).map_err(profile_management_error)?;
                let contents = std::str::from_utf8(&document.contents)
                    .map_err(|_| RsiError::Boot("Host Profile source is not valid UTF-8".into()))?;
                write_profile_document(
                    command.output,
                    "host_profile",
                    id.as_str(),
                    document.source,
                    document.path.as_deref(),
                    contents,
                )
            }
            (ProfileKind::Application, ProfileOperationKind::Path) => {
                let id = application_profile_id(&command.ids[0])?;
                let document = catalog.application(&id).map_err(profile_management_error)?;
                write_profile_path(
                    command.output,
                    "application_profile_path",
                    id.as_str(),
                    document.source,
                    document.path.as_deref(),
                )
            }
            (ProfileKind::Host, ProfileOperationKind::Path) => {
                let id = host_profile_id(&command.ids[0])?;
                let document = catalog.host(&id).map_err(profile_management_error)?;
                write_profile_path(
                    command.output,
                    "host_profile_path",
                    id.as_str(),
                    document.source,
                    document.path.as_deref(),
                )
            }
            (ProfileKind::Application, ProfileOperationKind::Copy) => {
                let source = application_profile_id(&command.ids[0])?;
                let target = application_profile_id(&command.ids[1])?;
                let path = catalog
                    .copy_application(&source, &target)
                    .map_err(profile_management_error)?;
                write_profile_mutation(
                    command.output,
                    "application_profile",
                    "copied",
                    &target,
                    &path,
                )
            }
            (ProfileKind::Host, ProfileOperationKind::Copy) => {
                let source = host_profile_id(&command.ids[0])?;
                let target = host_profile_id(&command.ids[1])?;
                let path = catalog
                    .copy_host(&source, &target)
                    .map_err(profile_management_error)?;
                write_profile_mutation(command.output, "host_profile", "copied", &target, &path)
            }
            (ProfileKind::Application, ProfileOperationKind::Delete) => {
                let id = application_profile_id(&command.ids[0])?;
                let path = catalog.application_path(&id);
                catalog
                    .delete_application(&id)
                    .map_err(profile_management_error)?;
                write_profile_mutation(command.output, "application_profile", "deleted", &id, &path)
            }
            (ProfileKind::Host, ProfileOperationKind::Delete) => {
                let id = host_profile_id(&command.ids[0])?;
                let path = catalog.host_path(&id);
                catalog.delete_host(&id).map_err(profile_management_error)?;
                write_profile_mutation(command.output, "host_profile", "deleted", &id, &path)
            }
            (ProfileKind::Host | ProfileKind::Application, ProfileOperationKind::Preview) => {
                unreachable!()
            }
        }
    })();
    result.map_or_else(|error| report_error(&error), |()| 0)
}

async fn run_host_profile_preview(command: &ProfileCommand) -> u8 {
    let paths = match standard_paths() {
        Ok(paths) => paths,
        Err(error) => return report_error(&error),
    };
    let id = match host_profile_id(&command.ids[0]) {
        Ok(id) => id,
        Err(error) => return report_error(&error),
    };
    let document = match ProfileCatalog::new(paths.clone()).host(&id) {
        Ok(document) => document,
        Err(error) => return report_error(&profile_management_error(error)),
    };
    let coding = match standard_coding_tools() {
        Ok(coding) => coding,
        Err(error) => return report_error(&error),
    };
    let presets =
        match AgentPresetManager::open_standard_preview(paths.clone(), coding.is_some()).await {
            Ok(presets) => presets,
            Err(error) => return report_error(&error),
        };
    let preview = StandardComposition::new(paths, BTreeMap::new(), coding)
        .with_agent_presets(presets.catalog().clone())
        .preview_host(&document);
    let shutdown = presets.shutdown().await;
    let result = preview.and_then(|preview| {
        if !shutdown.is_clean() {
            return Err(RsiError::Boot(
                "Agent-preset preview shutdown was not clean".into(),
            ));
        }
        match command.output {
            ManagementOutput::Json => write_json(&serde_json::json!({
                "version": 1,
                "type": "host_profile_preview",
                "id": id.as_str(),
                "launch_key": preview.launch_key.as_str(),
                "source_digest": preview.profile.source_digest,
                "source_paths": preview.profile.source_paths,
                "leaves": preview.profile.leaves.iter().map(|leaf| serde_json::json!({
                    "instance_id": leaf.instance_id,
                    "plugin_id": leaf.plugin_id,
                })).collect::<Vec<_>>(),
            })),
            ManagementOutput::Text => write_text_line(&format!(
                "id: {}\nlaunch-key: {}\nsource-digest: {}\nleaves: {}",
                id,
                preview.launch_key,
                preview.profile.source_digest,
                preview.profile.leaves.len()
            )),
        }
    });
    result.map_or_else(|error| report_error(&error), |()| 0)
}

async fn run_host(command: HostCommand) -> u8 {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = command;
        return report_error(&RsiError::Boot(
            "Session Host daemon mode requires Linux process-generation fencing; named applications use embedded mode on this platform"
                .into(),
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let result = match command.operation {
            HostOperation::Serve => {
                serve_host_daemon(&command.profile, command.detached_child).await
            }
            HostOperation::Start => start_host_daemon(&command.profile).await,
            HostOperation::Restart => restart_host_daemon(&command.profile, command.force).await,
            HostOperation::Stop => stop_host_daemon(command.force).await,
            HostOperation::Status => status_host_daemon().await,
            HostOperation::Reload => reload_host_daemon(),
        };
        result.map_or_else(|error| report_error(&error), |()| 0)
    }
}

#[cfg(target_os = "linux")]
enum DaemonControlEvent {
    Daemon(std::result::Result<rsi::Result<()>, tokio::task::JoinError>),
    Stop,
    ReloadSignal(Option<()>),
    ReloadFinished(std::result::Result<(), tokio::task::JoinError>),
}

#[cfg(target_os = "linux")]
async fn wait_for_reload_task(
    task: Option<&mut JoinHandle<()>>,
) -> std::result::Result<(), tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}

#[cfg(target_os = "linux")]
async fn next_daemon_control_event<Terminate, Interrupt, Reload>(
    daemon_task: &mut JoinHandle<rsi::Result<()>>,
    terminate: Terminate,
    interrupt: Interrupt,
    reload: Reload,
    reload_signal_enabled: bool,
    reload_task: Option<&mut JoinHandle<()>>,
) -> DaemonControlEvent
where
    Terminate: std::future::Future<Output = Option<()>>,
    Interrupt: std::future::Future<Output = Option<()>>,
    Reload: std::future::Future<Output = Option<()>>,
{
    tokio::select! {
        result = daemon_task => DaemonControlEvent::Daemon(result),
        _ = terminate => DaemonControlEvent::Stop,
        _ = interrupt => DaemonControlEvent::Stop,
        signal = reload, if reload_signal_enabled => DaemonControlEvent::ReloadSignal(signal),
        result = wait_for_reload_task(reload_task) => DaemonControlEvent::ReloadFinished(result),
    }
}

#[cfg(target_os = "linux")]
async fn serve_host_daemon(profile_id: &HostProfileId, detached_child: bool) -> rsi::Result<()> {
    if detached_child {
        rustix::process::setsid()
            .map_err(|error| RsiError::Boot(format!("detach Session Host daemon: {error}")))?;
    }
    let paths = standard_paths()?;
    let profile = ProfileCatalog::new(paths.clone())
        .host(profile_id)
        .map_err(profile_management_error)?;
    let (composition, presets) = prepare_standard_composition(paths).await?;
    let daemon = match StandardSessionDaemon::start(composition, &profile).await {
        Ok(daemon) => daemon,
        Err(error) => {
            let _ = presets.shutdown().await;
            return Err(error);
        }
    };
    let running = daemon.running();
    let cancellation = CancellationToken::new();
    let mut daemon_task = tokio::spawn(daemon.run(cancellation.clone()));
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|error| RsiError::Boot(format!("failed to register SIGTERM: {error}")))?;
    let mut interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|error| RsiError::Boot(format!("failed to register SIGINT: {error}")))?;
    let mut reload = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|error| RsiError::Boot(format!("failed to register SIGHUP: {error}")))?;
    let mut reload_open = true;
    let mut reload_task = None;
    let daemon_result = loop {
        let reload_signal_enabled = reload_open && reload_task.is_none();
        match next_daemon_control_event(
            &mut daemon_task,
            terminate.recv(),
            interrupt.recv(),
            reload.recv(),
            reload_signal_enabled,
            reload_task.as_mut(),
        )
        .await
        {
            DaemonControlEvent::Daemon(result) => {
                break result.map_err(|error| {
                    RsiError::Boot(format!("Session Host task failed: {error}"))
                })?;
            }
            DaemonControlEvent::Stop => {
                cancellation.cancel();
            }
            DaemonControlEvent::ReloadSignal(signal) => {
                if signal.is_none() {
                    eprintln!("Session Host SIGHUP listener closed");
                    reload_open = false;
                } else {
                    let running = Arc::clone(&running);
                    reload_task = Some(tokio::spawn(async move {
                        match running.reload().await {
                            Ok(outcome) => eprintln!("Session Host reload: {outcome:?}"),
                            Err(error) => eprintln!("Session Host reload failed: {error}"),
                        }
                    }));
                }
            }
            DaemonControlEvent::ReloadFinished(result) => {
                if let Err(error) = result {
                    eprintln!("Session Host reload task failed: {error}");
                }
                reload_task = None;
            }
        }
    };
    if let Some(reload_task) = reload_task
        && let Err(error) = reload_task.await
    {
        eprintln!("Session Host reload task failed during shutdown: {error}");
    }
    let shutdown = presets.shutdown().await;
    daemon_result?;
    if !shutdown.is_clean() {
        return Err(RsiError::Boot(format!(
            "Agent-preset daemon shutdown reported {} cleanup failures",
            shutdown.report().total_failures()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn expected_host_launch(
    profile_id: &HostProfileId,
) -> rsi::Result<(rsi_host::HostPaths, String)> {
    let paths = standard_paths()?;
    let profile = ProfileCatalog::new(paths.clone())
        .host(profile_id)
        .map_err(profile_management_error)?;
    let (composition, presets) = prepare_standard_composition(paths.clone()).await?;
    let preview = composition.preview_host(&profile)?;
    let shutdown = presets.shutdown().await;
    if !shutdown.is_clean() {
        return Err(RsiError::Boot(
            "Agent-preset preview shutdown was not clean".into(),
        ));
    }
    Ok((paths, preview.launch_key.as_str().into()))
}

#[cfg(target_os = "linux")]
async fn start_host_daemon(profile_id: &HostProfileId) -> rsi::Result<()> {
    let (paths, expected_key) = expected_host_launch(profile_id).await?;
    let host_paths = SessionHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    if let Some(metadata) = host_paths
        .read_metadata()
        .map_err(|error| RsiError::Boot(error.to_string()))?
        && owner_process_is_current(&metadata).map_err(|error| RsiError::Boot(error.to_string()))?
    {
        return Err(RsiError::Boot(format!(
            "Session Host owner is already active in {:?} mode",
            metadata.mode
        )));
    }
    std::fs::create_dir_all(host_paths.owner_directory())
        .map_err(|error| RsiError::Boot(format!("create Session Host state: {error}")))?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    std::fs::set_permissions(
        host_paths.owner_directory(),
        std::fs::Permissions::from_mode(0o700),
    )
    .map_err(|error| RsiError::Boot(format!("chmod Session Host state: {error}")))?;
    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let log = options
        .open(host_paths.owner_log())
        .map_err(|error| RsiError::Boot(format!("open Session Host log: {error}")))?;
    log.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| RsiError::Boot(format!("chmod Session Host log: {error}")))?;
    let stderr = log
        .try_clone()
        .map_err(|error| RsiError::Boot(format!("clone Session Host log: {error}")))?;
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| RsiError::Boot(format!("resolve current executable: {error}")))?;
    let child = std::process::Command::new(executable)
        .args([
            "host",
            "serve",
            "--profile",
            profile_id.as_str(),
            "--detached-child",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| RsiError::Boot(format!("spawn Session Host daemon: {error}")))?;
    let mut child = DaemonChildGuard::new(child);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Some(status) = child
            .child_mut()
            .try_wait()
            .map_err(|error| RsiError::Boot(format!("wait for Session Host daemon: {error}")))?
        {
            return Err(RsiError::Boot(format!(
                "Session Host daemon exited before readiness with {status}; inspect {}",
                host_paths.owner_log().display()
            )));
        }
        if let Some(metadata) = host_paths
            .read_metadata()
            .map_err(|error| RsiError::Boot(error.to_string()))?
            && metadata.pid == child.id()
            && metadata.mode == HostOwnerMode::Daemon
            && metadata.launch_key == expected_key
            && metadata.socket_path.as_deref() == Some(host_paths.socket())
            && owner_process_is_current(&metadata)
                .map_err(|error| RsiError::Boot(error.to_string()))?
        {
            UdsSessionApplication::connect(host_paths.socket(), &expected_key, metadata.host_epoch)
                .await
                .map_err(|error| RsiError::Boot(format!("daemon readiness handshake: {error}")))?;
            println!("started\t{}", child.id());
            child.disarm();
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RsiError::Boot(format!(
                "Session Host daemon did not become ready within 15 seconds; inspect {}",
                host_paths.owner_log().display()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(target_os = "linux")]
struct DaemonChildGuard(Option<std::process::Child>);

#[cfg(target_os = "linux")]
impl DaemonChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut std::process::Child {
        self.0.as_mut().expect("daemon child guard is armed")
    }

    fn id(&self) -> u32 {
        self.0.as_ref().expect("daemon child guard is armed").id()
    }

    fn disarm(mut self) {
        self.0.take();
    }
}

#[cfg(target_os = "linux")]
impl Drop for DaemonChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(target_os = "linux")]
async fn stop_host_daemon(force: bool) -> rsi::Result<()> {
    let paths = standard_paths()?;
    let host_paths = SessionHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let metadata = host_paths
        .read_metadata()
        .map_err(|error| RsiError::Boot(error.to_string()))?
        .ok_or_else(|| RsiError::Boot("no Session Host owner metadata exists".into()))?;
    if metadata.mode != HostOwnerMode::Daemon {
        return Err(RsiError::Boot(
            "the active Session Host is embedded and cannot be stopped as a daemon".into(),
        ));
    }
    signal_owner(
        &metadata,
        if force {
            HostSignal::ForceStop
        } else {
            HostSignal::Stop
        },
    )
    .map_err(|error| RsiError::Boot(error.to_string()))?;
    let wait = host_stop_timeout(force);
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if !owner_process_is_current(&metadata)
            .map_err(|error| RsiError::Boot(error.to_string()))?
        {
            println!("stopped\t{}", metadata.pid);
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RsiError::Boot(format!(
                "Session Host did not stop within {} seconds; retry with `rsi host stop --force`",
                wait.as_secs()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(target_os = "linux")]
fn host_stop_timeout(force: bool) -> Duration {
    if force {
        FORCE_HOST_STOP_TIMEOUT
    } else {
        SESSION_HOST_DRAIN_TIMEOUT + HOST_SHUTDOWN_MARGIN
    }
}

#[cfg(target_os = "linux")]
async fn restart_host_daemon(profile_id: &HostProfileId, force: bool) -> rsi::Result<()> {
    let paths = standard_paths()?;
    let host_paths = SessionHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    if let Some(metadata) = host_paths
        .read_metadata()
        .map_err(|error| RsiError::Boot(error.to_string()))?
        && owner_process_is_current(&metadata).map_err(|error| RsiError::Boot(error.to_string()))?
    {
        stop_host_daemon(force).await?;
    }
    start_host_daemon(profile_id).await
}

#[cfg(target_os = "linux")]
async fn status_host_daemon() -> rsi::Result<()> {
    let paths = standard_paths()?;
    let host_paths = SessionHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let Some(metadata) = host_paths
        .read_metadata()
        .map_err(|error| RsiError::Boot(error.to_string()))?
    else {
        println!("stopped");
        return Ok(());
    };
    let current =
        owner_process_is_current(&metadata).map_err(|error| RsiError::Boot(error.to_string()))?;
    let compatible = metadata
        .is_compatible_with_current()
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let responsive = if current && compatible && metadata.mode == HostOwnerMode::Daemon {
        UdsSessionApplication::connect(
            metadata
                .socket_path
                .as_deref()
                .ok_or_else(|| RsiError::Boot("daemon metadata has no socket".into()))?,
            &metadata.launch_key,
            metadata.host_epoch.clone(),
        )
        .await
        .is_ok()
    } else {
        false
    };
    println!(
        "{}\tmode={:?}\tpid={}\tepoch={}\tkey={}",
        if current && !compatible {
            "incompatible"
        } else if current && (metadata.mode == HostOwnerMode::Embedded || responsive) {
            "running"
        } else if current {
            "unresponsive"
        } else {
            "stale"
        },
        metadata.mode,
        metadata.pid,
        metadata.host_epoch.as_str(),
        metadata.launch_key
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn reload_host_daemon() -> rsi::Result<()> {
    let paths = standard_paths()?;
    let host_paths = SessionHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let metadata = host_paths
        .read_metadata()
        .map_err(|error| RsiError::Boot(error.to_string()))?
        .ok_or_else(|| RsiError::Boot("no Session Host owner exists".into()))?;
    signal_owner(&metadata, HostSignal::Reload)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    println!("reload-requested\t{}", metadata.pid);
    Ok(())
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

enum ParsedApplication {
    Headless(Command),
    Session(SessionCommand),
}

async fn run_application(invocation: ApplicationInvocation) -> u8 {
    if invocation
        .arguments
        .iter()
        .take_while(|argument| argument.to_str() != Some("--"))
        .any(|argument| matches!(argument.to_str(), Some("-h" | "--help")))
    {
        print!(
            "Usage:\n  rsi --profile headless [TASK | --stdin] [HEADLESS OPTIONS]\n  rsi --profile session [--cwd PATH] [--resume SESSION | --session-id SESSION] [--agent-preset ID] [--output text|jsonl]\n"
        );
        return 0;
    }
    let paths = match standard_paths() {
        Ok(paths) => paths,
        Err(error) => return report_error(&error),
    };
    let catalog = ProfileCatalog::new(paths.clone());
    let application_profile = match catalog.application(&invocation.profile) {
        Ok(profile) => profile,
        Err(error) => return report_error(&profile_management_error(error)),
    };
    let host_profile = match catalog.host(application_profile.profile.host_profile()) {
        Ok(profile) => profile,
        Err(error) => return report_error(&profile_management_error(error)),
    };
    let parsed = match application_profile.profile.application() {
        ApplicationKind::Headless => {
            let mut arguments = Vec::with_capacity(invocation.arguments.len() + 1);
            arguments.push(OsString::from("run"));
            arguments.extend(invocation.arguments);
            match Command::parse(arguments) {
                Ok(Parse::Run(command)) => ParsedApplication::Headless(command),
                Ok(_) => return report_error(&usage("invalid headless application arguments")),
                Err(error) => return report_error(&error),
            }
        }
        ApplicationKind::Session => match SessionCommand::parse(invocation.arguments) {
            Ok(command) => ParsedApplication::Session(command),
            Err(error) => return report_error(&error),
        },
    };
    let (composition, presets) = match prepare_standard_composition(paths).await {
        Ok(prepared) => prepared,
        Err(error) => return report_error(&error),
    };
    let connection = match connect_or_embed_session_host(composition, &host_profile).await {
        Ok(connection) => connection,
        Err(error) => {
            let exit = report_error(&error);
            return shutdown_agent_preset_manager(presets, exit, 2, "application bootstrap").await;
        }
    };
    let (presets, preset_shutdown_exit) =
        if connection.mode() == rsi::SessionHostConnectionMode::Remote {
            (
                None,
                shutdown_agent_preset_manager(presets, 0, 1, "remote application bootstrap").await,
            )
        } else {
            (Some(presets), 0)
        };
    let application = connection.application();
    let exit = match parsed {
        ParsedApplication::Headless(command) => {
            run_headless_application(application, command).await
        }
        ParsedApplication::Session(command) => {
            run_session_application(application, connection.mode(), command).await
        }
    };
    let exit = match connection.shutdown().await {
        Ok(()) => exit,
        Err(error) if exit == 0 => report_error(&error),
        Err(error) => {
            eprintln!("error: {error}");
            exit
        }
    };
    let exit = if exit == 0 {
        preset_shutdown_exit
    } else {
        exit
    };
    match presets {
        Some(presets) => shutdown_agent_preset_manager(presets, exit, 1, "application").await,
        None => exit,
    }
}

#[derive(Clone, Debug)]
struct SessionCommand {
    cwd: Option<PathBuf>,
    resume: Option<SessionId>,
    session_id: Option<SessionId>,
    agent_preset: Option<AgentPresetId>,
    output: OutputMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum OutputMode {
    #[default]
    Text,
    Jsonl,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionSelection {
    Fresh {
        cwd: PathBuf,
        session_id: Option<SessionId>,
        agent_preset_id: Option<AgentPresetId>,
    },
    Resume {
        session_id: SessionId,
        cwd: Option<PathBuf>,
    },
}

#[derive(Clone, Debug)]
struct HeadlessTurnOptions {
    task: String,
    session: SessionSelection,
    turn_id: Option<TurnId>,
    model: Option<ModelRef>,
    sandbox: Option<SandboxMode>,
    output: OutputMode,
}

#[derive(Clone, Debug)]
enum CliEvent {
    Session {
        session_id: SessionId,
        turn_id: TurnId,
        accepted_seq: u64,
    },
    Fact {
        session_id: SessionId,
        fact: Arc<SessionFact>,
        durable_seq: u64,
    },
    Outcome {
        session_id: SessionId,
        turn_id: TurnId,
        outcome: TurnOutcome,
        durable_seq: u64,
    },
}

impl CliEvent {
    fn json_line(&self) -> std::result::Result<String, serde_json::Error> {
        match self {
            Self::Session {
                session_id,
                turn_id,
                accepted_seq,
            } => serde_json::to_string(&SessionEnvelope {
                version: 2,
                kind: "session",
                session_id,
                turn_id,
                accepted_seq: *accepted_seq,
            }),
            Self::Fact {
                session_id,
                fact,
                durable_seq,
            } => serde_json::to_string(&LiveFactEnvelope {
                version: 2,
                kind: "fact",
                session_id,
                fact,
                durable_seq: *durable_seq,
            }),
            Self::Outcome {
                session_id,
                turn_id,
                outcome,
                durable_seq,
            } => serde_json::to_string(&OutcomeEnvelope {
                version: 2,
                kind: "outcome",
                session_id,
                turn_id,
                outcome,
                durable_seq: *durable_seq,
            }),
        }
    }
}

#[derive(Serialize)]
struct SessionEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    turn_id: &'a TurnId,
    accepted_seq: u64,
}

#[derive(Serialize)]
struct LiveFactEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    fact: &'a SessionFact,
    durable_seq: u64,
}

#[derive(Serialize)]
struct OutcomeEnvelope<'a> {
    version: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a SessionId,
    turn_id: &'a TurnId,
    outcome: &'a TurnOutcome,
    durable_seq: u64,
}

impl SessionCommand {
    fn parse(arguments: Vec<OsString>) -> rsi::Result<Self> {
        let mut command = Self {
            cwd: None,
            resume: None,
            session_id: None,
            agent_preset: None,
            output: OutputMode::Text,
        };
        let mut arguments = arguments.into_iter();
        let mut output_set = false;
        while let Some(argument) = arguments.next() {
            let argument = utf8(argument)?;
            match argument.as_str() {
                "--cwd" => set_option(
                    &mut command.cwd,
                    path_value(&mut arguments, "--cwd")?,
                    "--cwd",
                )?,
                "--resume" => set_option(
                    &mut command.resume,
                    session_value(&mut arguments, "--resume")?,
                    "--resume",
                )?,
                "--session-id" => set_option(
                    &mut command.session_id,
                    session_value(&mut arguments, "--session-id")?,
                    "--session-id",
                )?,
                "--agent-preset" => set_option(
                    &mut command.agent_preset,
                    run_preset_value(&mut arguments)?,
                    "--agent-preset",
                )?,
                "--output" => {
                    if output_set {
                        return Err(usage("duplicate --output"));
                    }
                    output_set = true;
                    command.output = output_value(&mut arguments)?;
                }
                option => {
                    return Err(usage(format!(
                        "unknown Session application argument `{option}`"
                    )));
                }
            }
        }
        if command.resume.is_some() && command.session_id.is_some() {
            return Err(usage("--resume and --session-id are mutually exclusive"));
        }
        if command.resume.is_some() && command.agent_preset.is_some() {
            return Err(usage("--resume and --agent-preset are mutually exclusive"));
        }
        Ok(command)
    }
}

async fn resolve_application_handle(
    application: &Arc<dyn SessionApplication>,
    session: SessionSelection,
) -> rsi::Result<Arc<dyn SessionHandle>> {
    match session {
        SessionSelection::Fresh {
            cwd,
            session_id,
            agent_preset_id,
        } => application
            .create(CreateSession {
                cwd,
                session_id,
                agent_preset_id,
            })
            .await
            .map_err(|error| RsiError::Boot(error.to_string())),
        SessionSelection::Resume { session_id, cwd } => {
            let handle = application
                .attach(&session_id)
                .await
                .map_err(|error| RsiError::Boot(error.to_string()))?;
            if let Some(cwd) = cwd {
                let canonical = tokio::fs::canonicalize(cwd)
                    .await
                    .map_err(|error| RsiError::Boot(format!("workspace: {error}")))?;
                let header = handle
                    .header()
                    .await
                    .map_err(|error| RsiError::Run(error.to_string()))?;
                if canonical.to_str() != Some(header.canonical_cwd()) {
                    return Err(RsiError::Boot(
                        "--cwd does not match the durable Session workspace".into(),
                    ));
                }
            }
            Ok(handle)
        }
    }
}

const CLI_RENDER_CHANNEL_CAPACITY: usize = 32;
const TURN_COMPLETION_CHANNEL_CAPACITY: usize = 1;

#[derive(Debug)]
enum CliRenderMessage {
    Event(CliEvent),
    FinishLine,
}

#[derive(Debug)]
struct TurnTaskFinished {
    turn_id: TurnId,
    result: rsi::Result<TurnOutcome>,
    cancellation_requested: bool,
}

async fn drive_application_turn(
    handle: Arc<dyn SessionHandle>,
    request: SubmitText,
    cancellation: CancellationToken,
    rendering_stopped: CancellationToken,
    renderer: tokio::sync::mpsc::Sender<CliRenderMessage>,
    completion: tokio::sync::mpsc::Sender<TurnTaskFinished>,
) {
    let turn_id = request.turn_id.clone();
    let result = async {
        let receipt = handle
            .submit_text(request)
            .await
            .map_err(|error| RsiError::Run(error.to_string()))?;
        send_cli_event(
            &renderer,
            &rendering_stopped,
            &cancellation,
            CliEvent::Session {
                session_id: receipt.session_id.clone(),
                turn_id: receipt.turn_id.clone(),
                accepted_seq: receipt.accepted_seq,
            },
        )
        .await?;
        let mut observation = handle
            .subscribe(receipt.accepted_seq)
            .await
            .map_err(|error| RsiError::Run(error.to_string()))?;
        let mut cancellation_sent = false;
        loop {
            tokio::select! {
                biased;
                () = cancellation.cancelled(), if !cancellation_sent => {
                    cancellation_sent = true;
                    handle.cancel(&turn_id, Some("client interrupt".into())).await
                        .map_err(|error| RsiError::Run(error.to_string()))?;
                }
                update = futures_util::StreamExt::next(&mut observation) => {
                    let update = update
                        .ok_or_else(|| RsiError::Run("Session observation ended before a terminal Fact".into()))?
                        .map_err(|error| RsiError::Run(error.to_string()))?;
                    if let rsi_agent_turn_protocol::TurnUpdate::Fact { fact, durable_seq } = update {
                        let terminal = match fact.body() {
                            SessionFactBody::TurnTerminal { turn_id: observed, outcome }
                                if observed == &turn_id => Some(outcome.clone()),
                            _ => None,
                        };
                        send_cli_event(
                            &renderer,
                            &rendering_stopped,
                            &cancellation,
                            CliEvent::Fact {
                                session_id: receipt.session_id.clone(),
                                fact,
                                durable_seq,
                            },
                        )
                        .await?;
                        if let Some(outcome) = terminal {
                            send_cli_event(
                                &renderer,
                                &rendering_stopped,
                                &cancellation,
                                CliEvent::Outcome {
                                    session_id: receipt.session_id,
                                    turn_id: turn_id.clone(),
                                    outcome: outcome.clone(),
                                    durable_seq,
                                },
                            )
                            .await?;
                            send_finish_line(&renderer, &rendering_stopped, &cancellation).await?;
                            return Ok(outcome);
                        }
                    }
                }
            }
        }
    }
    .await;
    let _ = completion
        .send(TurnTaskFinished {
            turn_id,
            result,
            cancellation_requested: cancellation.is_cancelled(),
        })
        .await;
}

async fn send_cli_event(
    renderer: &tokio::sync::mpsc::Sender<CliRenderMessage>,
    rendering_stopped: &CancellationToken,
    turn_cancellation: &CancellationToken,
    event: CliEvent,
) -> rsi::Result<()> {
    if rendering_stopped.is_cancelled() {
        return Ok(());
    }
    tokio::select! {
        biased;
        () = rendering_stopped.cancelled() => Ok(()),
        result = renderer.send(CliRenderMessage::Event(event)) => result
            .map_err(|_| RsiError::Run("terminal renderer stopped before the turn ended".into())),
        () = turn_cancellation.cancelled() => Ok(()),
    }
}

async fn send_finish_line(
    renderer: &tokio::sync::mpsc::Sender<CliRenderMessage>,
    rendering_stopped: &CancellationToken,
    turn_cancellation: &CancellationToken,
) -> rsi::Result<()> {
    if rendering_stopped.is_cancelled() {
        return Ok(());
    }
    tokio::select! {
        biased;
        () = rendering_stopped.cancelled() => Ok(()),
        result = renderer.send(CliRenderMessage::FinishLine) => result
            .map_err(|_| RsiError::Run("terminal renderer stopped before the turn ended".into())),
        () = turn_cancellation.cancelled() => Ok(()),
    }
}

#[derive(Debug, Default)]
struct CliRenderState {
    wrote_text: bool,
    text_ends_newline: bool,
}

impl CliRenderState {
    fn write(&mut self, output: OutputMode, event: &CliEvent) -> rsi::Result<()> {
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        write_live_event(
            &mut stdout,
            output,
            event,
            &mut self.wrote_text,
            &mut self.text_ends_newline,
        )
    }

    fn finish_line(&mut self, output: OutputMode) -> rsi::Result<()> {
        if output == OutputMode::Text && self.wrote_text && !self.text_ends_newline {
            write_text_line("")?;
            self.text_ends_newline = true;
        }
        Ok(())
    }
}

fn spawn_cli_renderer(
    output: OutputMode,
    mut receiver: tokio::sync::mpsc::Receiver<CliRenderMessage>,
) -> tokio::sync::oneshot::Receiver<rsi::Result<()>> {
    let (outcome, finished) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut renderer = CliRenderState::default();
        let result = (|| {
            while let Some(message) = receiver.blocking_recv() {
                match message {
                    CliRenderMessage::Event(event) => renderer.write(output, &event)?,
                    CliRenderMessage::FinishLine => renderer.finish_line(output)?,
                }
            }
            Ok(())
        })();
        let _ = outcome.send(result);
    });
    finished
}

async fn join_cli_renderer(
    renderer: tokio::sync::oneshot::Receiver<rsi::Result<()>>,
) -> rsi::Result<()> {
    renderer
        .await
        .map_err(|_| RsiError::Run("terminal renderer panicked".into()))?
}

async fn run_headless_application(
    application: Arc<dyn SessionApplication>,
    command: Command,
) -> u8 {
    let task = match command.task().await {
        Ok(task) => task,
        Err(error) => return report_error(&error),
    };
    let options = match command.options(task) {
        Ok(options) => options,
        Err(error) => return report_error(&error),
    };
    let handle = match resolve_application_handle(&application, options.session).await {
        Ok(handle) => handle,
        Err(error) => return report_error(&error),
    };
    let turn_id = match options.turn_id.map_or_else(generated_cli_turn_id, Ok) {
        Ok(id) => id,
        Err(error) => return report_error(&error),
    };
    let cancellation = CancellationToken::new();
    let signal = match arm_signal(cancellation.clone()).await {
        Ok(signal) => signal,
        Err(error) => return report_error(&error),
    };
    let rendering_stopped = CancellationToken::new();
    let (renderer, render_receiver) = tokio::sync::mpsc::channel(CLI_RENDER_CHANNEL_CAPACITY);
    let render_task = spawn_cli_renderer(options.output, render_receiver);
    let (completion, mut finished) = tokio::sync::mpsc::channel(TURN_COMPLETION_CHANNEL_CAPACITY);
    tokio::spawn(drive_application_turn(
        handle,
        SubmitText {
            turn_id,
            text: options.task,
            model: options.model,
            sandbox: options.sandbox,
        },
        cancellation,
        rendering_stopped,
        renderer,
        completion,
    ));
    let completed = finished.recv().await;
    let cancelled = completed
        .as_ref()
        .is_some_and(|finished| finished.cancellation_requested);
    let render_result = if cancelled {
        tokio::time::timeout(Duration::from_secs(1), join_cli_renderer(render_task))
            .await
            .unwrap_or(Ok(()))
    } else {
        join_cli_renderer(render_task).await
    };
    let exit = if let Err(error) = render_result {
        report_error(&error)
    } else if let Some(TurnTaskFinished {
        result,
        cancellation_requested,
        ..
    }) = completed
    {
        match result {
            Ok(outcome) => {
                if !cancellation_requested {
                    report_terminal_diagnostic(&outcome);
                }
                if cancellation_requested {
                    130
                } else {
                    u8::from(outcome != TurnOutcome::Completed)
                }
            }
            Err(error) => report_error(&error),
        }
    } else {
        report_error(&RsiError::Run("turn worker exited".into()))
    };
    signal.abort();
    exit
}

fn generated_cli_turn_id() -> rsi::Result<TurnId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy)
        .map_err(|error| RsiError::Boot(format!("OS entropy failed: {error}")))?;
    TurnId::new(format!("turn-{:032x}", u128::from_le_bytes(entropy)))
        .map_err(|error| RsiError::Boot(error.to_string()))
}

#[derive(Debug)]
enum SessionInput {
    Line(String),
    TooLarge,
    InvalidUtf8,
    Error(String),
    Eof,
}

const SESSION_INPUT_CHANNEL_CAPACITY: usize = 1;
const MAXIMUM_QUEUED_SESSION_TURNS: usize = 16;

fn spawn_session_input() -> tokio::sync::mpsc::Receiver<SessionInput> {
    let (sender, receiver) = tokio::sync::mpsc::channel(SESSION_INPUT_CHANNEL_CAPACITY);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        forward_session_input(&mut stdin, &sender);
    });
    receiver
}

#[cfg(test)]
fn spawn_session_input_reader(
    mut reader: impl std::io::BufRead + Send + 'static,
) -> tokio::sync::mpsc::Receiver<SessionInput> {
    let (sender, receiver) = tokio::sync::mpsc::channel(SESSION_INPUT_CHANNEL_CAPACITY);
    std::thread::spawn(move || forward_session_input(&mut reader, &sender));
    receiver
}

fn forward_session_input(
    reader: &mut impl std::io::BufRead,
    sender: &tokio::sync::mpsc::Sender<SessionInput>,
) {
    loop {
        let input = read_bounded_stdin_line(reader);
        let terminal = matches!(input, SessionInput::Eof | SessionInput::Error(_));
        if sender.blocking_send(input).is_err() || terminal {
            break;
        }
    }
}

fn read_bounded_stdin_line(reader: &mut impl std::io::BufRead) -> SessionInput {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(error) => return SessionInput::Error(error.to_string()),
        };
        if available.is_empty() {
            if bytes.is_empty() && !oversized {
                return SessionInput::Eof;
            }
            break;
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if !oversized {
            if bytes.len().saturating_add(consumed) > MAXIMUM_TURN_TEXT_BYTES + 1 {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&available[..consumed]);
            }
        }
        let ended = available[..consumed].last() == Some(&b'\n');
        reader.consume(consumed);
        if ended {
            break;
        }
    }
    if oversized {
        return SessionInput::TooLarge;
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    String::from_utf8(bytes).map_or(SessionInput::InvalidUtf8, SessionInput::Line)
}

#[allow(clippy::too_many_lines)] // One REPL loop owns FIFO admission, signals, rendering, and detach.
async fn run_session_application(
    application: Arc<dyn SessionApplication>,
    mode: rsi::SessionHostConnectionMode,
    command: SessionCommand,
) -> u8 {
    let selection = match command.resume {
        Some(session_id) => SessionSelection::Resume {
            session_id,
            cwd: command.cwd,
        },
        None => SessionSelection::Fresh {
            cwd: match command.cwd {
                Some(cwd) => cwd,
                None => match std::env::current_dir() {
                    Ok(cwd) => cwd,
                    Err(error) => return report_error(&RsiError::Boot(error.to_string())),
                },
            },
            session_id: command.session_id,
            agent_preset_id: command.agent_preset,
        },
    };
    let handle = match resolve_application_handle(&application, selection).await {
        Ok(handle) => handle,
        Err(error) => return report_error(&error),
    };
    let header = match handle.header().await {
        Ok(header) => header,
        Err(error) => return report_error(&RsiError::Run(error.to_string())),
    };
    eprintln!("session: {}", header.session_id());
    let mut input = spawn_session_input();
    let rendering_stopped = CancellationToken::new();
    let (renderer, render_receiver) = tokio::sync::mpsc::channel(CLI_RENDER_CHANNEL_CAPACITY);
    let render_task = spawn_cli_renderer(command.output, render_receiver);
    let (completion, mut completed_turns) =
        tokio::sync::mpsc::channel(TURN_COMPLETION_CHANNEL_CAPACITY);
    let mut queue = VecDeque::new();
    let mut active: Option<(TurnId, CancellationToken)> = None;
    let mut detaching = false;
    let mut turn_failed = false;
    loop {
        if active.is_none() {
            if let Some(text) = queue.pop_front() {
                let turn_id = match generated_cli_turn_id() {
                    Ok(id) => id,
                    Err(error) => return report_error(&error),
                };
                let cancellation = CancellationToken::new();
                tokio::spawn(drive_application_turn(
                    Arc::clone(&handle),
                    SubmitText {
                        turn_id: turn_id.clone(),
                        text,
                        model: None,
                        sandbox: None,
                    },
                    cancellation.clone(),
                    rendering_stopped.clone(),
                    renderer.clone(),
                    completion.clone(),
                ));
                active = Some((turn_id, cancellation));
            } else if detaching {
                break;
            }
        }

        tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c(), if active.is_some() => {
                if signal.is_ok()
                    && let Some((_, cancellation)) = &active {
                    cancellation.cancel();
                }
            }
            message = completed_turns.recv(), if active.is_some() => {
                match message {
                    Some(TurnTaskFinished { turn_id, result, cancellation_requested: _ }) => {
                        if active.as_ref().is_some_and(|(active, _)| active == &turn_id) {
                            active = None;
                            match result {
                                Ok(outcome) => {
                                    turn_failed |= !matches!(
                                        &outcome,
                                        TurnOutcome::Completed | TurnOutcome::Cancelled
                                    );
                                    if !detaching {
                                        report_terminal_diagnostic(&outcome);
                                    }
                                }
                                Err(error) => {
                                    turn_failed = true;
                                    if !detaching {
                                        eprintln!("error: {error}");
                                    }
                                }
                            }
                        }
                    }
                    None => return report_error(&RsiError::Run("Session turn worker exited".into())),
                }
            }
            incoming = input.recv(), if !detaching && queue.len() < MAXIMUM_QUEUED_SESSION_TURNS => {
                match incoming.unwrap_or(SessionInput::Eof) {
                    SessionInput::Line(line) => {
                        if let Some(text) = line.strip_prefix("::") {
                            queue.push_back(format!(":{text}"));
                        } else if let Some(command_line) = line.strip_prefix(':') {
                            match handle_session_command(command_line, &handle, active.as_ref(), queue.len()).await {
                                SessionCommandAction::Continue => {}
                                SessionCommandAction::Exit => {
                                    queue.clear();
                                    if active.is_none() || mode == rsi::SessionHostConnectionMode::Remote {
                                        rendering_stopped.cancel();
                                        break;
                                    }
                                    rendering_stopped.cancel();
                                    detaching = true;
                                }
                            }
                        } else if !line.is_empty() {
                            queue.push_back(line);
                        }
                    }
                    SessionInput::TooLarge => eprintln!("error: input line exceeds {MAXIMUM_TURN_TEXT_BYTES} bytes"),
                    SessionInput::InvalidUtf8 => eprintln!("error: input line is not UTF-8"),
                    SessionInput::Error(error) => {
                        eprintln!("error: stdin read failed: {error}");
                        queue.clear();
                        rendering_stopped.cancel();
                        detaching = true;
                    }
                    SessionInput::Eof => {
                        queue.clear();
                        if active.is_none() || mode == rsi::SessionHostConnectionMode::Remote {
                            rendering_stopped.cancel();
                            break;
                        }
                        rendering_stopped.cancel();
                        detaching = true;
                    }
                }
            }
        }
    }
    drop(renderer);
    drop(completion);
    if active.is_none()
        && let Err(error) = join_cli_renderer(render_task).await
    {
        return report_error(&error);
    }
    u8::from(turn_failed)
}

enum SessionCommandAction {
    Continue,
    Exit,
}

async fn handle_session_command(
    command: &str,
    handle: &Arc<dyn SessionHandle>,
    active: Option<&(TurnId, CancellationToken)>,
    queued: usize,
) -> SessionCommandAction {
    let mut parts = command.split_whitespace();
    match parts.next().unwrap_or("") {
        "queue" => eprintln!(
            "active: {}\tqueued: {queued}",
            active.map_or("none", |(turn, _)| turn.as_str())
        ),
        "cancel" => {
            if let Some((_, cancellation)) = active {
                cancellation.cancel();
            } else {
                eprintln!("no active turn");
            }
        }
        "approvals" => match handle.pending_approvals().await {
            Ok(requests) if requests.is_empty() => eprintln!("no pending approvals"),
            Ok(requests) => {
                for request in requests {
                    eprintln!("{}\t{}\t{}", request.id, request.action, request.reason);
                }
            }
            Err(error) => eprintln!("error: {error}"),
        },
        decision @ ("allow" | "deny") => {
            let Some(id) = parts.next() else {
                eprintln!("usage: :{decision} APPROVAL_ID");
                return SessionCommandAction::Continue;
            };
            if parts.next().is_some() {
                eprintln!("usage: :{decision} APPROVAL_ID");
                return SessionCommandAction::Continue;
            }
            let choice = if decision == "allow" {
                rsi_approval_protocol::ApprovalDecision::AllowOnce
            } else {
                rsi_approval_protocol::ApprovalDecision::Deny
            };
            match handle.answer_approval(id, choice).await {
                Ok(true) => eprintln!("answered {id}"),
                Ok(false) => eprintln!("approval is no longer pending: {id}"),
                Err(error) => eprintln!("error: {error}"),
            }
        }
        "exit" => return SessionCommandAction::Exit,
        "help" | "" => {
            eprintln!(":queue  :cancel  :approvals  :allow ID  :deny ID  :exit  :help  ::TEXT");
        }
        other => eprintln!("unknown Session command: :{other}"),
    }
    SessionCommandAction::Continue
}

fn application_profile_id(value: &str) -> rsi::Result<ApplicationProfileId> {
    ApplicationProfileId::new(value).map_err(profile_management_error)
}

fn host_profile_id(value: &str) -> rsi::Result<HostProfileId> {
    HostProfileId::new(value).map_err(profile_management_error)
}

#[allow(clippy::needless_pass_by_value)] // Kept as a direct `map_err` adapter.
fn profile_management_error(error: rsi::ProfileCatalogError) -> RsiError {
    RsiError::Boot(error.to_string())
}

fn profile_source_name(source: ProfileSource) -> &'static str {
    match source {
        ProfileSource::Builtin => "builtin",
        ProfileSource::User => "user",
    }
}

fn write_profile_rows(
    output: ManagementOutput,
    kind: &'static str,
    rows: Vec<(String, ProfileSource)>,
) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&serde_json::json!({
            "version": 1,
            "type": kind,
            "profiles": rows.iter().map(|(id, source)| serde_json::json!({
                "id": id,
                "source": profile_source_name(*source),
            })).collect::<Vec<_>>(),
        })),
        ManagementOutput::Text => {
            let mut text = String::from("ID\tSOURCE");
            for (id, source) in rows {
                text.push('\n');
                text.push_str(&id);
                text.push('\t');
                text.push_str(profile_source_name(source));
            }
            write_text_line(&text)
        }
    }
}

fn write_profile_document(
    output: ManagementOutput,
    kind: &'static str,
    id: &str,
    source: ProfileSource,
    path: Option<&std::path::Path>,
    contents: &str,
) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&serde_json::json!({
            "version": 1,
            "type": kind,
            "id": id,
            "source": profile_source_name(source),
            "path": path,
            "contents": contents,
        })),
        ManagementOutput::Text => write_text_line(&format!(
            "id: {id}\nsource: {}\npath: {}\ncontents:\n{contents}",
            profile_source_name(source),
            path.map_or_else(|| "<builtin>".into(), |path| path.display().to_string())
        )),
    }
}

fn write_profile_path(
    output: ManagementOutput,
    kind: &'static str,
    id: &str,
    source: ProfileSource,
    path: Option<&std::path::Path>,
) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&serde_json::json!({
            "version": 1,
            "type": kind,
            "id": id,
            "source": profile_source_name(source),
            "path": path,
        })),
        ManagementOutput::Text => write_text_line(
            &path.map_or_else(|| "<builtin>".into(), |path| path.display().to_string()),
        ),
    }
}

fn write_profile_mutation<I: std::fmt::Display>(
    output: ManagementOutput,
    kind: &'static str,
    action: &'static str,
    id: &I,
    path: &std::path::Path,
) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&serde_json::json!({
            "version": 1,
            "type": "profile_mutation",
            "profile_type": kind,
            "action": action,
            "id": id.to_string(),
            "path": path,
        })),
        ManagementOutput::Text => write_text_line(&format!("{action} {id}\t{}", path.display())),
    }
}

async fn run_agent_preset(command: AgentPresetCommand) -> u8 {
    let paths = match standard_paths() {
        Ok(paths) => paths,
        Err(error) => return report_error(&error),
    };
    let system_root = match standard_agent_preset_root(&paths) {
        Ok(root) => root,
        Err(error) => return report_error(&RsiError::Boot(error.to_string())),
    };
    let manager = match AgentPresetManager::open_standard(
        paths,
        system_root,
        cfg!(target_os = "linux"),
    )
    .await
    {
        Ok(manager) => manager,
        Err(error) => return report_error(&error),
    };
    let result = execute_agent_preset(&manager, command).await;
    let exit = match result {
        Ok(()) => 0,
        Err(error) => report_error(&error),
    };
    shutdown_agent_preset_manager(manager, exit, 2, "management").await
}

async fn shutdown_agent_preset_manager(
    manager: AgentPresetManager,
    mut exit: u8,
    clean_failure_exit: u8,
    operation: &str,
) -> u8 {
    let shutdown = manager.shutdown().await;
    if !shutdown.is_clean() {
        eprintln!(
            "Agent-preset {operation} shutdown reported {} cleanup failures",
            shutdown.report().total_failures()
        );
        if exit == 0 {
            exit = clean_failure_exit;
        }
    }
    exit
}

async fn execute_agent_preset(
    manager: &AgentPresetManager,
    command: AgentPresetCommand,
) -> rsi::Result<()> {
    match command.operation {
        AgentPresetOperation::List => list_agent_presets(manager, command.output).await,
        AgentPresetOperation::Show(id) => show_agent_preset(manager, command.output, id).await,
        AgentPresetOperation::Path(id) => path_agent_preset(manager, command.output, &id),
        AgentPresetOperation::Copy {
            source,
            target,
            name,
        } => {
            manager
                .catalog()
                .copy(&source, target.clone(), name)
                .await
                .map_err(preset_management_error)?;
            write_action(command.output, "copied", &target)
        }
        AgentPresetOperation::Delete(id) => {
            manager
                .catalog()
                .delete(&id)
                .await
                .map_err(preset_management_error)?;
            write_action(command.output, "deleted", &id)
        }
        AgentPresetOperation::DefaultGet => {
            let id = manager
                .catalog()
                .default_id()
                .await
                .map_err(preset_management_error)?;
            write_default(command.output, "get", &id)
        }
        AgentPresetOperation::DefaultSet(id) => {
            manager
                .catalog()
                .set_default(&id)
                .await
                .map_err(preset_management_error)?;
            write_default(command.output, "set", &id)
        }
        AgentPresetOperation::DefaultClear => {
            manager
                .catalog()
                .clear_default()
                .await
                .map_err(preset_management_error)?;
            let id = manager
                .catalog()
                .default_id()
                .await
                .map_err(preset_management_error)?;
            write_default(command.output, "clear", &id)
        }
    }
}

async fn list_agent_presets(
    manager: &AgentPresetManager,
    output: ManagementOutput,
) -> rsi::Result<()> {
    let roster = manager
        .catalog()
        .roster()
        .await
        .map_err(preset_management_error)?;
    let presets = roster
        .presets
        .into_iter()
        .map(PresetOutput::from)
        .collect::<Vec<_>>();
    match output {
        ManagementOutput::Json => write_json(&ListOutput {
            version: 1,
            kind: "agent_preset_list",
            authorable: roster.authorable,
            presets,
        }),
        ManagementOutput::Text => write_roster_text(&presets),
    }
}

async fn show_agent_preset(
    manager: &AgentPresetManager,
    output: ManagementOutput,
    id: AgentPresetId,
) -> rsi::Result<()> {
    let roster = manager
        .catalog()
        .roster()
        .await
        .map_err(preset_management_error)?;
    let available = roster
        .presets
        .iter()
        .map(|row| row.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let row = roster
        .presets
        .into_iter()
        .find(|row| row.id == id)
        .ok_or_else(|| {
            preset_management_error(PresetError::PresetNotFound {
                id: id.as_str().to_owned(),
                available,
            })
        })?;
    let composition = if row.health == AgentPresetHealth::Healthy {
        Some(
            manager
                .catalog()
                .document(&id)
                .map_err(preset_management_error)?
                .content,
        )
    } else {
        None
    };
    let preset = PresetOutput::from(row);
    match output {
        ManagementOutput::Json => write_json(&ShowOutput {
            version: 1,
            kind: "agent_preset",
            preset,
            composition,
        }),
        ManagementOutput::Text => write_show_text(&preset, composition.as_deref()),
    }
}

fn path_agent_preset(
    manager: &AgentPresetManager,
    output: ManagementOutput,
    id: &AgentPresetId,
) -> rsi::Result<()> {
    let path = manager
        .catalog()
        .location(id)
        .map_err(preset_management_error)?;
    let path = path
        .to_str()
        .ok_or_else(|| RsiError::Boot("Agent preset path cannot be represented as UTF-8".into()))?;
    match output {
        ManagementOutput::Json => write_json(&PathOutput {
            version: 1,
            kind: "agent_preset_path",
            id: id.as_str(),
            path,
        }),
        ManagementOutput::Text => write_text_line(path),
    }
}

#[derive(Debug, Serialize)]
struct PresetOutput {
    id: String,
    metadata: MetadataOutput,
    source: &'static str,
    trust: &'static str,
    status: &'static str,
    reason: Option<String>,
    default: bool,
}

#[derive(Debug, Serialize)]
struct MetadataOutput {
    name: Option<String>,
    description: Option<String>,
}

impl From<AgentPresetRow> for PresetOutput {
    fn from(row: AgentPresetRow) -> Self {
        let (status, reason) = match row.health {
            AgentPresetHealth::Healthy => ("healthy", None),
            AgentPresetHealth::Broken { reason } => ("broken", Some(reason)),
        };
        Self {
            id: row.id.as_str().to_owned(),
            metadata: MetadataOutput {
                name: row.name,
                description: row.description,
            },
            source: source_name(row.source),
            trust: trust_name(row.trust),
            status,
            reason,
            default: row.is_default,
        }
    }
}

#[derive(Debug, Serialize)]
struct ListOutput {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    authorable: bool,
    presets: Vec<PresetOutput>,
}

#[derive(Debug, Serialize)]
struct ShowOutput {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    preset: PresetOutput,
    composition: Option<String>,
}

#[derive(Debug, Serialize)]
struct PathOutput<'a> {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
    path: &'a str,
}

#[derive(Debug, Serialize)]
struct AgentStoreVerifyOutput<'a> {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    status: &'static str,
    root: &'a std::path::Path,
}

#[derive(Debug, Serialize)]
struct ActionOutput<'a> {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    action: &'a str,
    id: &'a str,
}

fn source_name(source: AgentPresetSource) -> &'static str {
    match source {
        AgentPresetSource::System => "system",
        AgentPresetSource::Configured => "configured",
        AgentPresetSource::User => "user",
    }
}

fn trust_name(trust: AgentPresetTrust) -> &'static str {
    match trust {
        AgentPresetTrust::System => "system",
        AgentPresetTrust::User => "user",
    }
}

fn write_roster_text(presets: &[PresetOutput]) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "DEFAULT\tID\tSOURCE\tTRUST\tHEALTH\tNAME").map_err(output_error)?;
    for preset in presets {
        let health = preset.reason.as_ref().map_or_else(
            || preset.status.to_owned(),
            |reason| format!("broken: {reason}"),
        );
        writeln!(
            stdout,
            "{}\t{}\t{}\t{}\t{}\t{}",
            if preset.default { "*" } else { "" },
            preset.id,
            preset.source,
            preset.trust,
            health,
            preset.metadata.name.as_deref().unwrap_or("")
        )
        .map_err(output_error)?;
    }
    stdout.flush().map_err(output_error)
}

fn write_show_text(preset: &PresetOutput, composition: Option<&str>) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "id: {}", preset.id).map_err(output_error)?;
    writeln!(stdout, "source: {}", preset.source).map_err(output_error)?;
    writeln!(stdout, "trust: {}", preset.trust).map_err(output_error)?;
    writeln!(
        stdout,
        "default: {}",
        if preset.default { "yes" } else { "no" }
    )
    .map_err(output_error)?;
    writeln!(stdout, "health: {}", preset.status).map_err(output_error)?;
    if let Some(reason) = &preset.reason {
        writeln!(stdout, "reason: {reason}").map_err(output_error)?;
    }
    if let Some(name) = &preset.metadata.name {
        writeln!(stdout, "name: {name}").map_err(output_error)?;
    }
    if let Some(description) = &preset.metadata.description {
        writeln!(stdout, "description: {description}").map_err(output_error)?;
    }
    if let Some(composition) = composition {
        writeln!(stdout, "composition:").map_err(output_error)?;
        stdout
            .write_all(composition.as_bytes())
            .map_err(output_error)?;
        if !composition.ends_with('\n') {
            stdout.write_all(b"\n").map_err(output_error)?;
        }
    }
    stdout.flush().map_err(output_error)
}

fn write_action(output: ManagementOutput, action: &str, id: &AgentPresetId) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&ActionOutput {
            version: 1,
            kind: "agent_preset_mutation",
            action,
            id: id.as_str(),
        }),
        ManagementOutput::Text => write_text_line(&format!("{action} {}", id.as_str())),
    }
}

fn write_default(output: ManagementOutput, action: &str, id: &AgentPresetId) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&ActionOutput {
            version: 1,
            kind: "agent_preset_default",
            action,
            id: id.as_str(),
        }),
        ManagementOutput::Text if action == "get" => write_text_line(id.as_str()),
        ManagementOutput::Text => write_text_line(&format!("default: {}", id.as_str())),
    }
}

fn write_json(value: &impl Serialize) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, value)
        .map_err(|error| RsiError::Boot(format!("stdout JSON write failed: {error}")))?;
    stdout
        .write_all(b"\n")
        .and_then(|()| stdout.flush())
        .map_err(output_error)
}

fn write_text_line(value: &str) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{value}")
        .and_then(|()| stdout.flush())
        .map_err(output_error)
}

#[allow(clippy::needless_pass_by_value)] // Exact `map_err` adapter for owned I/O failures.
fn output_error(error: std::io::Error) -> RsiError {
    RsiError::Boot(format!("stdout write failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)] // Exact `map_err` adapter for owned catalog failures.
fn preset_management_error(error: PresetError) -> RsiError {
    RsiError::Boot(error.to_string())
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

#[cfg(not(target_os = "linux"))]
fn standard_coding_tools() -> rsi::Result<Option<StandardCodingTools>> {
    Ok(None)
}

async fn arm_signal(cancellation: CancellationToken) -> rsi::Result<JoinHandle<()>> {
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut signal = Box::pin(tokio::signal::ctrl_c());
        let initial = std::future::poll_fn(|context| {
            Poll::Ready(match signal.as_mut().poll(context) {
                Poll::Ready(result) => Some(result),
                Poll::Pending => None,
            })
        })
        .await;
        match initial {
            Some(Ok(())) => {
                cancellation.cancel();
                let _ignored = armed_tx.send(Ok(()));
            }
            Some(Err(error)) => {
                let _ignored = armed_tx.send(Err(error));
            }
            None => {
                let _ignored = armed_tx.send(Ok(()));
                if signal.await.is_ok() {
                    cancellation.cancel();
                }
            }
        }
    });
    armed_rx
        .await
        .map_err(|_| RsiError::Boot("SIGINT listener exited before registration".into()))?
        .map_err(|error| RsiError::Boot(format!("failed to register SIGINT listener: {error}")))?;
    Ok(task)
}

fn report_terminal_diagnostic(outcome: &TurnOutcome) {
    match outcome {
        TurnOutcome::Failed { code, message }
        | TurnOutcome::PartialFailed { code, message, .. } => eprintln!("{code}: {message}"),
        TurnOutcome::Interrupted { reason, .. } => eprintln!("interrupted: {reason}"),
        TurnOutcome::BudgetExceeded {
            dimension,
            consumed,
            limit,
        } => {
            eprintln!("turn budget exceeded for {dimension:?}: consumed {consumed}, limit {limit}");
        }
        TurnOutcome::Completed | TurnOutcome::Cancelled => {}
    }
}

fn write_live_event(
    stdout: &mut impl Write,
    mode: OutputMode,
    event: &CliEvent,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> rsi::Result<()> {
    match mode {
        OutputMode::Jsonl => {
            let line = event
                .json_line()
                .map_err(|error| RsiError::Run(error.to_string()))?;
            stdout
                .write_all(line.as_bytes())
                .and_then(|()| stdout.write_all(b"\n"))
                .and_then(|()| stdout.flush())
                .map_err(|error| RsiError::Run(format!("stdout write failed: {error}")))
        }
        OutputMode::Text => {
            if let CliEvent::Fact { fact, .. } = event {
                match fact.body() {
                    SessionFactBody::ModelEvent {
                        event:
                            LanguageEvent::ContentDelta {
                                delta: ContentDelta::Text(text),
                                ..
                            },
                        ..
                    } => {
                        stdout
                            .write_all(text.as_bytes())
                            .and_then(|()| stdout.flush())
                            .map_err(|error| {
                                RsiError::Run(format!("stdout write failed: {error}"))
                            })?;
                        *wrote_text = true;
                        *text_ends_newline = text.ends_with('\n');
                    }
                    SessionFactBody::ToolResult { result, .. } => {
                        for content in &result.content {
                            if let ToolContent::Image { media } = content {
                                if *wrote_text && !*text_ends_newline {
                                    stdout.write_all(b"\n").map_err(|error| {
                                        RsiError::Run(format!("stdout write failed: {error}"))
                                    })?;
                                }
                                writeln!(stdout, "media:{}", media.id).map_err(|error| {
                                    RsiError::Run(format!("stdout write failed: {error}"))
                                })?;
                                stdout.flush().map_err(|error| {
                                    RsiError::Run(format!("stdout write failed: {error}"))
                                })?;
                                *wrote_text = true;
                                *text_ends_newline = true;
                            }
                        }
                    }
                    SessionFactBody::ImageOutput { media, .. } => {
                        if *wrote_text && !*text_ends_newline {
                            stdout.write_all(b"\n").map_err(|error| {
                                RsiError::Run(format!("stdout write failed: {error}"))
                            })?;
                        }
                        writeln!(stdout, "media:{}", media.id)
                            .and_then(|()| stdout.flush())
                            .map_err(|error| {
                                RsiError::Run(format!("stdout write failed: {error}"))
                            })?;
                        *wrote_text = true;
                        *text_ends_newline = true;
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }
}

fn report_error(error: &RsiError) -> u8 {
    eprintln!("error: {error}");
    error.exit_code()
}

#[derive(Clone, Debug)]
struct Command {
    positional: Option<String>,
    stdin: bool,
    cwd: Option<PathBuf>,
    resume: Option<SessionId>,
    session_id: Option<SessionId>,
    turn_id: Option<TurnId>,
    agent_preset: Option<AgentPresetId>,
    deployment: Option<String>,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    output: OutputMode,
}

enum Parse {
    Help(&'static str),
    Version,
    Application(ApplicationInvocation),
    Profile(ProfileCommand),
    Host(HostCommand),
    Run(Command),
    AgentPreset(AgentPresetCommand),
    AgentStore(AgentStoreCommand),
}

#[derive(Clone, Debug)]
struct ApplicationInvocation {
    profile: ApplicationProfileId,
    arguments: Vec<OsString>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProfileKind {
    Application,
    Host,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProfileOperationKind {
    List,
    Show,
    Path,
    Copy,
    Delete,
    Preview,
}

#[derive(Clone, Debug)]
struct ProfileCommand {
    kind: ProfileKind,
    operation: ProfileOperationKind,
    ids: Vec<String>,
    output: ManagementOutput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostOperation {
    Start,
    Serve,
    Restart,
    Stop,
    Status,
    Reload,
}

#[derive(Clone, Debug)]
struct HostCommand {
    operation: HostOperation,
    profile: HostProfileId,
    force: bool,
    detached_child: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagementOutput {
    Text,
    Json,
}

#[derive(Clone, Debug)]
struct AgentPresetCommand {
    operation: AgentPresetOperation,
    output: ManagementOutput,
}

#[derive(Clone, Debug)]
struct AgentStoreCommand {
    root: Option<PathBuf>,
    output: ManagementOutput,
}

#[derive(Clone, Debug)]
enum AgentPresetOperation {
    List,
    Show(AgentPresetId),
    Path(AgentPresetId),
    Copy {
        source: AgentPresetId,
        target: AgentPresetId,
        name: Option<String>,
    },
    Delete(AgentPresetId),
    DefaultGet,
    DefaultSet(AgentPresetId),
    DefaultClear,
}

fn parse_agent_store(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
    let arguments = arguments.map(utf8).collect::<rsi::Result<Vec<_>>>()?;
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(agent_store_usage("missing agent-store command"));
    };
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Parse::Help(AGENT_STORE_HELP));
    }
    if command != "verify" {
        return Err(agent_store_usage(format!(
            "unknown agent-store command `{command}`"
        )));
    }
    let mut root = None;
    let mut output = ManagementOutput::Text;
    let mut output_set = false;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--root" => {
                if root.is_some() {
                    return Err(agent_store_usage("duplicate --root"));
                }
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| agent_store_usage("--root requires a value"))?;
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err(agent_store_usage("--root must be absolute"));
                }
                root = Some(path);
            }
            "--output" => {
                if output_set {
                    return Err(agent_store_usage("duplicate --output"));
                }
                output_set = true;
                index += 1;
                output = match arguments.get(index).map(String::as_str) {
                    Some("text") => ManagementOutput::Text,
                    Some("json") => ManagementOutput::Json,
                    Some(_) => return Err(agent_store_usage("invalid --output mode")),
                    None => return Err(agent_store_usage("--output requires a value")),
                };
            }
            option if option.starts_with('-') => {
                return Err(agent_store_usage(format!("unknown option `{option}`")));
            }
            positional => {
                return Err(agent_store_usage(format!(
                    "unexpected positional argument `{positional}`"
                )));
            }
        }
        index += 1;
    }
    Ok(Parse::AgentStore(AgentStoreCommand { root, output }))
}

fn parse_agent_preset(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
    let arguments = arguments.map(utf8).collect::<rsi::Result<Vec<_>>>()?;
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(agent_preset_usage("missing agent-preset command"));
    };
    if matches!(command, "-h" | "--help") {
        return Ok(Parse::Help(AGENT_PRESET_HELP));
    }
    let remaining = &arguments[1..];
    if remaining
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Parse::Help(if command == "default" {
            AGENT_PRESET_DEFAULT_HELP
        } else {
            AGENT_PRESET_HELP
        }));
    }
    let parsed = match command {
        "list" => {
            let parsed = management_arguments(remaining, false)?;
            require_positionals(command, &parsed.positionals, 0)?;
            AgentPresetCommand {
                operation: AgentPresetOperation::List,
                output: parsed.output,
            }
        }
        "show" | "path" | "delete" => {
            let parsed = management_arguments(remaining, false)?;
            require_positionals(command, &parsed.positionals, 1)?;
            let id = preset_id(&parsed.positionals[0])?;
            let operation = match command {
                "show" => AgentPresetOperation::Show(id),
                "path" => AgentPresetOperation::Path(id),
                "delete" => AgentPresetOperation::Delete(id),
                _ => unreachable!(),
            };
            AgentPresetCommand {
                operation,
                output: parsed.output,
            }
        }
        "copy" => {
            let parsed = management_arguments(remaining, true)?;
            require_positionals(command, &parsed.positionals, 0)?;
            let source = parsed
                .source
                .as_deref()
                .ok_or_else(|| agent_preset_usage("agent-preset copy requires --from"))?;
            let target = parsed
                .target
                .as_deref()
                .ok_or_else(|| agent_preset_usage("agent-preset copy requires --id"))?;
            AgentPresetCommand {
                operation: AgentPresetOperation::Copy {
                    source: preset_id(source)?,
                    target: preset_id(target)?,
                    name: parsed.name,
                },
                output: parsed.output,
            }
        }
        "default" => parse_default_command(remaining)?,
        _ => {
            return Err(agent_preset_usage(format!(
                "unknown agent-preset command `{command}`"
            )));
        }
    };
    Ok(Parse::AgentPreset(parsed))
}

fn parse_default_command(arguments: &[String]) -> rsi::Result<AgentPresetCommand> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(default_usage("missing agent-preset default command"));
    };
    let parsed = management_arguments(&arguments[1..], false)?;
    let operation = match command {
        "get" => {
            require_default_positionals(command, &parsed.positionals, 0)?;
            AgentPresetOperation::DefaultGet
        }
        "set" => {
            require_default_positionals(command, &parsed.positionals, 1)?;
            AgentPresetOperation::DefaultSet(preset_id(&parsed.positionals[0])?)
        }
        "clear" => {
            require_default_positionals(command, &parsed.positionals, 0)?;
            AgentPresetOperation::DefaultClear
        }
        _ => {
            return Err(default_usage(format!(
                "unknown agent-preset default command `{command}`"
            )));
        }
    };
    Ok(AgentPresetCommand {
        operation,
        output: parsed.output,
    })
}

#[derive(Debug)]
struct ParsedManagementArguments {
    positionals: Vec<String>,
    name: Option<String>,
    source: Option<String>,
    target: Option<String>,
    output: ManagementOutput,
}

fn management_arguments(
    arguments: &[String],
    allow_copy: bool,
) -> rsi::Result<ParsedManagementArguments> {
    let mut positionals = Vec::new();
    let mut name = None;
    let mut source = None;
    let mut target = None;
    let mut output = ManagementOutput::Text;
    let mut output_set = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--output" => {
                if output_set {
                    return Err(agent_preset_usage("duplicate --output"));
                }
                output_set = true;
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| agent_preset_usage("--output requires a value"))?;
                output = match value.as_str() {
                    "text" => ManagementOutput::Text,
                    "json" => ManagementOutput::Json,
                    _ => return Err(agent_preset_usage("invalid --output mode")),
                };
            }
            "--name" if allow_copy => {
                if name.is_some() {
                    return Err(agent_preset_usage("duplicate --name"));
                }
                index += 1;
                name = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| agent_preset_usage("--name requires a value"))?
                        .clone(),
                );
            }
            "--from" if allow_copy => {
                if source.is_some() {
                    return Err(agent_preset_usage("duplicate --from"));
                }
                index += 1;
                source = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| agent_preset_usage("--from requires a value"))?
                        .clone(),
                );
            }
            "--id" if allow_copy => {
                if target.is_some() {
                    return Err(agent_preset_usage("duplicate --id"));
                }
                index += 1;
                target = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| agent_preset_usage("--id requires a value"))?
                        .clone(),
                );
            }
            option if option.starts_with('-') => {
                return Err(agent_preset_usage(format!("unknown option `{option}`")));
            }
            positional => positionals.push(positional.to_owned()),
        }
        index += 1;
    }
    Ok(ParsedManagementArguments {
        positionals,
        name,
        source,
        target,
        output,
    })
}

fn require_positionals(command: &str, values: &[String], expected: usize) -> rsi::Result<()> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(agent_preset_usage(format!(
            "agent-preset {command} expects {expected} positional argument(s)"
        )))
    }
}

fn require_default_positionals(
    command: &str,
    values: &[String],
    expected: usize,
) -> rsi::Result<()> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(default_usage(format!(
            "agent-preset default {command} expects {expected} positional argument(s)"
        )))
    }
}

fn preset_id(value: &str) -> rsi::Result<AgentPresetId> {
    AgentPresetId::new(value).map_err(|error| agent_preset_usage(error.to_string()))
}

fn parse_profile_command(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
    let arguments = arguments.map(utf8).collect::<rsi::Result<Vec<_>>>()?;
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Parse::Help(PROFILE_HELP));
    }
    let kind = match arguments.first().map(String::as_str) {
        Some("application") => ProfileKind::Application,
        Some("host") => ProfileKind::Host,
        Some(value) => return Err(profile_usage(format!("unknown Profile kind `{value}`"))),
        None => return Err(profile_usage("missing Profile kind")),
    };
    let operation = match arguments.get(1).map(String::as_str) {
        Some("list") => ProfileOperationKind::List,
        Some("show") => ProfileOperationKind::Show,
        Some("path") => ProfileOperationKind::Path,
        Some("copy") => ProfileOperationKind::Copy,
        Some("delete") => ProfileOperationKind::Delete,
        Some("preview") if kind == ProfileKind::Host => ProfileOperationKind::Preview,
        Some(value) => return Err(profile_usage(format!("unknown Profile command `{value}`"))),
        None => return Err(profile_usage("missing Profile command")),
    };
    let mut ids = Vec::new();
    let mut output = ManagementOutput::Text;
    let mut output_set = false;
    let mut index = 2;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--output" => {
                if output_set {
                    return Err(profile_usage("duplicate --output"));
                }
                output_set = true;
                index += 1;
                output = match arguments.get(index).map(String::as_str) {
                    Some("text") => ManagementOutput::Text,
                    Some("json") => ManagementOutput::Json,
                    Some(_) => return Err(profile_usage("invalid --output mode")),
                    None => return Err(profile_usage("--output requires a value")),
                };
            }
            option if option.starts_with('-') => {
                return Err(profile_usage(format!("unknown option `{option}`")));
            }
            id => ids.push(id.into()),
        }
        index += 1;
    }
    let expected = match operation {
        ProfileOperationKind::List => 0,
        ProfileOperationKind::Show
        | ProfileOperationKind::Path
        | ProfileOperationKind::Delete
        | ProfileOperationKind::Preview => 1,
        ProfileOperationKind::Copy => 2,
    };
    if ids.len() != expected {
        return Err(profile_usage(format!(
            "Profile command expects {expected} identifier(s)"
        )));
    }
    Ok(Parse::Profile(ProfileCommand {
        kind,
        operation,
        ids,
        output,
    }))
}

fn parse_host_command(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
    let arguments = arguments.map(utf8).collect::<rsi::Result<Vec<_>>>()?;
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Parse::Help(HOST_HELP));
    }
    let operation = match arguments.first().map(String::as_str) {
        Some("start") => HostOperation::Start,
        Some("serve") => HostOperation::Serve,
        Some("restart") => HostOperation::Restart,
        Some("stop") => HostOperation::Stop,
        Some("status") => HostOperation::Status,
        Some("reload") => HostOperation::Reload,
        Some(value) => return Err(host_usage(format!("unknown Host command `{value}`"))),
        None => return Err(host_usage("missing Host command")),
    };
    let mut profile =
        HostProfileId::new("standard").map_err(|error| host_usage(error.to_string()))?;
    let mut profile_set = false;
    let mut force = false;
    let mut detached_child = false;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--profile" => {
                if profile_set {
                    return Err(host_usage("duplicate --profile"));
                }
                profile_set = true;
                index += 1;
                profile = HostProfileId::new(
                    arguments
                        .get(index)
                        .ok_or_else(|| host_usage("--profile requires a value"))?
                        .clone(),
                )
                .map_err(|error| host_usage(error.to_string()))?;
            }
            "--force" => {
                if force {
                    return Err(host_usage("duplicate --force"));
                }
                force = true;
            }
            "--detached-child" => {
                if detached_child {
                    return Err(host_usage("duplicate --detached-child"));
                }
                detached_child = true;
            }
            option => return Err(host_usage(format!("unknown Host option `{option}`"))),
        }
        index += 1;
    }
    if profile_set
        && matches!(
            operation,
            HostOperation::Stop | HostOperation::Status | HostOperation::Reload
        )
    {
        return Err(host_usage("this Host command does not select a Profile"));
    }
    if force && !matches!(operation, HostOperation::Stop | HostOperation::Restart) {
        return Err(host_usage("--force is valid only for stop or restart"));
    }
    if detached_child && operation != HostOperation::Serve {
        return Err(host_usage(
            "--detached-child is valid only for the internal serve child",
        ));
    }
    Ok(Parse::Host(HostCommand {
        operation,
        profile,
        force,
        detached_child,
    }))
}

impl Command {
    fn parse_cli(arguments: impl IntoIterator<Item = OsString>) -> rsi::Result<Parse> {
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        if arguments.first().and_then(|argument| argument.to_str()) == Some("run") {
            return Err(usage(
                "the direct `run` command was removed; select a named Application Profile with `rsi --profile headless ...`",
            ));
        }
        Self::parse(arguments)
    }

    fn empty() -> Self {
        Self {
            positional: None,
            stdin: false,
            cwd: None,
            resume: None,
            session_id: None,
            turn_id: None,
            agent_preset: None,
            deployment: None,
            model: None,
            sandbox: None,
            output: OutputMode::Text,
        }
    }

    #[allow(clippy::too_many_lines)] // One ordered CLI grammar owns option conflicts and exact diagnostics.
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> rsi::Result<Parse> {
        let mut arguments = arguments.into_iter();
        let Some(first) = arguments.next() else {
            return Err(usage("missing `run` command"));
        };
        let first = utf8(first)?;
        if matches!(first.as_str(), "-h" | "--help") {
            return Ok(Parse::Help(HELP));
        }
        if matches!(first.as_str(), "-V" | "--version") {
            return Ok(Parse::Version);
        }
        if first == "--profile" {
            let profile = arguments
                .next()
                .ok_or_else(|| usage("--profile requires an Application Profile name"))?;
            let profile = ApplicationProfileId::new(utf8(profile)?)
                .map_err(|error| usage(error.to_string()))?;
            return Ok(Parse::Application(ApplicationInvocation {
                profile,
                arguments: arguments.collect(),
            }));
        }
        if first == "profile" {
            return parse_profile_command(arguments);
        }
        if first == "host" {
            return parse_host_command(arguments);
        }
        if first == "agent-preset" {
            return parse_agent_preset(arguments);
        }
        if first == "agent-store" {
            return parse_agent_store(arguments);
        }
        if first != "run" {
            return Err(usage(format!("unknown command `{first}`")));
        }

        let mut command = Self::empty();
        let mut literal = false;
        let mut sandbox_set = false;
        let mut output_set = false;
        while let Some(argument) = arguments.next() {
            let argument = utf8(argument)?;
            if !literal && argument == "--" {
                literal = true;
                continue;
            }
            if !literal && argument.starts_with('-') {
                match argument.as_str() {
                    "--stdin" => set_flag(&mut command.stdin, "--stdin")?,
                    "--cwd" => set_option(
                        &mut command.cwd,
                        path_value(&mut arguments, "--cwd")?,
                        "--cwd",
                    )?,
                    "--resume" => set_option(
                        &mut command.resume,
                        session_value(&mut arguments, "--resume")?,
                        "--resume",
                    )?,
                    "--session-id" => {
                        set_option(
                            &mut command.session_id,
                            session_value(&mut arguments, "--session-id")?,
                            "--session-id",
                        )?;
                    }
                    "--turn-id" => set_option(
                        &mut command.turn_id,
                        turn_value(&mut arguments, "--turn-id")?,
                        "--turn-id",
                    )?,
                    "--agent-preset" => set_option(
                        &mut command.agent_preset,
                        run_preset_value(&mut arguments)?,
                        "--agent-preset",
                    )?,
                    "--deployment" => {
                        set_option(
                            &mut command.deployment,
                            string_value(&mut arguments, "--deployment")?,
                            "--deployment",
                        )?;
                    }
                    "--model" => set_option(
                        &mut command.model,
                        string_value(&mut arguments, "--model")?,
                        "--model",
                    )?,
                    "--sandbox" => {
                        if sandbox_set {
                            return Err(usage("duplicate --sandbox"));
                        }
                        sandbox_set = true;
                        command.sandbox = Some(sandbox_value(&mut arguments)?);
                    }
                    "--output" => {
                        if output_set {
                            return Err(usage("duplicate --output"));
                        }
                        output_set = true;
                        command.output = output_value(&mut arguments)?;
                    }
                    "-h" | "--help" => return Ok(Parse::Help(HELP)),
                    _ => return Err(usage(format!("unknown option `{argument}`"))),
                }
            } else if command.positional.replace(argument).is_some() {
                return Err(usage("exactly one task positional is allowed"));
            }
        }
        command.validate()?;
        Ok(Parse::Run(command))
    }

    fn validate(&self) -> rsi::Result<()> {
        if self.stdin == self.positional.is_some() {
            return Err(usage("provide exactly one task positional or --stdin"));
        }
        if self.resume.is_some() && self.session_id.is_some() {
            return Err(usage("--resume and --session-id are mutually exclusive"));
        }
        if self.resume.is_some() && self.agent_preset.is_some() {
            return Err(usage("--resume and --agent-preset are mutually exclusive"));
        }
        if self.deployment.is_some() != self.model.is_some() {
            return Err(usage("--deployment and --model must be supplied together"));
        }
        if let (Some(deployment), Some(model)) = (&self.deployment, &self.model) {
            ModelRef::new(deployment, model).map_err(|error| usage(error.to_string()))?;
        }
        Ok(())
    }

    async fn task(&self) -> rsi::Result<String> {
        if let Some(task) = &self.positional {
            return Ok(task.clone());
        }
        let input = tokio::task::spawn_blocking(|| {
            let mut input = Vec::new();
            std::io::stdin()
                .take(u64::try_from(MAXIMUM_TURN_TEXT_BYTES).unwrap_or(u64::MAX) + 1)
                .read_to_end(&mut input)
                .map(|_| input)
        })
        .await
        .map_err(|error| RsiError::Boot(format!("stdin worker failed: {error}")))?
        .map_err(|error| RsiError::Boot(format!("stdin read failed: {error}")))?;
        if input.len() > MAXIMUM_TURN_TEXT_BYTES {
            return Err(usage("stdin task exceeds the Agent text bound"));
        }
        String::from_utf8(input).map_err(|_| usage("stdin task is not UTF-8"))
    }

    fn options(&self, task: String) -> rsi::Result<HeadlessTurnOptions> {
        let session = match &self.resume {
            Some(session_id) => SessionSelection::Resume {
                session_id: session_id.clone(),
                cwd: self.cwd.clone(),
            },
            None => SessionSelection::Fresh {
                cwd: match &self.cwd {
                    Some(cwd) => cwd.clone(),
                    None => std::env::current_dir().map_err(|error| {
                        RsiError::Boot(format!("current directory is unavailable: {error}"))
                    })?,
                },
                session_id: self.session_id.clone(),
                agent_preset_id: self.agent_preset.clone(),
            },
        };
        let model = self
            .deployment
            .as_ref()
            .zip(self.model.as_ref())
            .map(|(deployment, model)| {
                ModelRef::new(deployment, model).map_err(|error| usage(error.to_string()))
            })
            .transpose()?;
        Ok(HeadlessTurnOptions {
            task,
            session,
            turn_id: self.turn_id.clone(),
            model,
            sandbox: self.sandbox,
            output: self.output,
        })
    }
}

fn sandbox_value(arguments: &mut impl Iterator<Item = OsString>) -> rsi::Result<SandboxMode> {
    match string_value(arguments, "--sandbox")?.as_str() {
        "read-only" => Ok(SandboxMode::ReadOnly),
        "workspace-write" => Ok(SandboxMode::WorkspaceWrite),
        "danger-full-access" => Ok(SandboxMode::DangerFullAccess),
        _ => Err(usage("invalid --sandbox mode")),
    }
}

fn output_value(arguments: &mut impl Iterator<Item = OsString>) -> rsi::Result<OutputMode> {
    match string_value(arguments, "--output")?.as_str() {
        "text" => Ok(OutputMode::Text),
        "jsonl" => Ok(OutputMode::Jsonl),
        _ => Err(usage("invalid --output mode")),
    }
}

fn run_preset_value(arguments: &mut impl Iterator<Item = OsString>) -> rsi::Result<AgentPresetId> {
    let value = string_value(arguments, "--agent-preset")?;
    AgentPresetId::new(value).map_err(|error| usage(error.to_string()))
}

fn set_flag(value: &mut bool, name: &str) -> rsi::Result<()> {
    if *value {
        return Err(usage(format!("duplicate {name}")));
    }
    *value = true;
    Ok(())
}

fn set_option<T>(slot: &mut Option<T>, value: T, name: &str) -> rsi::Result<()> {
    if slot.is_some() {
        return Err(usage(format!("duplicate {name}")));
    }
    *slot = Some(value);
    Ok(())
}

fn path_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<PathBuf> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage(format!("{option} requires a value")))
}

fn session_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<SessionId> {
    let value = string_value(arguments, option)?;
    SessionId::new(value).map_err(|error| usage(error.to_string()))
}

fn turn_value(arguments: &mut impl Iterator<Item = OsString>, option: &str) -> rsi::Result<TurnId> {
    let value = string_value(arguments, option)?;
    TurnId::new(value).map_err(|error| usage(error.to_string()))
}

fn string_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<String> {
    let value = arguments
        .next()
        .ok_or_else(|| usage(format!("{option} requires a value")))?;
    utf8(value)
}

fn utf8(value: OsString) -> rsi::Result<String> {
    value
        .into_string()
        .map_err(|_| usage("CLI arguments must be UTF-8"))
}

fn usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{HELP}", message.into()))
}

fn agent_preset_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{AGENT_PRESET_HELP}", message.into()))
}

fn default_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{AGENT_PRESET_DEFAULT_HELP}", message.into()))
}

fn agent_store_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{AGENT_STORE_HELP}", message.into()))
}

fn profile_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{PROFILE_HELP}", message.into()))
}

fn host_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{HOST_HELP}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> rsi::Result<Parse> {
        Command::parse_cli(arguments.iter().map(OsString::from))
    }

    fn parse_headless(arguments: &[&str]) -> rsi::Result<Command> {
        let mut values = vec![OsString::from("run")];
        values.extend(arguments.iter().map(OsString::from));
        match Command::parse(values)? {
            Parse::Run(command) => Ok(command),
            _ => Err(usage("headless parser returned a non-headless command")),
        }
    }

    #[test]
    fn enforces_input_model_and_session_exclusivity() {
        assert!(parse_headless(&[]).is_err());
        assert!(parse_headless(&["task", "--stdin"]).is_err());
        assert!(parse_headless(&["task", "--deployment", "one"]).is_err());
        assert!(
            parse_headless(&[
                "task",
                "--resume",
                "session-one",
                "--session-id",
                "session-two"
            ])
            .is_err()
        );
        assert!(
            parse_headless(&["task", "--deployment", "contains space", "--model", "model"])
                .is_err()
        );
        assert!(parse_headless(&["task", "--output", "text", "--output", "jsonl"]).is_err());
        assert!(parse(&["run", "task"]).is_err());
    }

    #[test]
    fn parses_one_valid_agent_preset_only_for_a_fresh_session() {
        let command = parse_headless(&["task", "--agent-preset", "coding-agent"]).unwrap();
        assert_eq!(
            command.agent_preset.as_ref().map(AgentPresetId::as_str),
            Some("coding-agent")
        );
        let options = command.options("task".into()).unwrap();
        assert!(matches!(
            options.session,
            SessionSelection::Fresh {
                agent_preset_id: Some(ref id),
                ..
            } if id.as_str() == "coding-agent"
        ));
        assert!(
            parse_headless(&[
                "task",
                "--agent-preset",
                "coding-agent",
                "--agent-preset",
                "review-agent"
            ])
            .is_err()
        );
        assert!(parse_headless(&["task", "--agent-preset", "Upper"]).is_err());
        assert!(
            parse_headless(&[
                "task",
                "--resume",
                "session-one",
                "--agent-preset",
                "coding-agent"
            ])
            .is_err()
        );
    }

    #[test]
    fn leading_slash_is_plain_task_and_dash_task_uses_separator() {
        let command = parse_headless(&["/status"]).unwrap();
        assert_eq!(command.positional.as_deref(), Some("/status"));
        let command = parse_headless(&["--", "--literal"]).unwrap();
        assert_eq!(command.positional.as_deref(), Some("--literal"));
    }

    #[test]
    fn agent_store_verify_has_a_strict_absolute_root_contract() {
        let Parse::AgentStore(command) = parse(&[
            "agent-store",
            "verify",
            "--root",
            "/tmp/rsi-agent-store",
            "--output",
            "json",
        ])
        .unwrap() else {
            panic!("agent-store")
        };
        assert_eq!(command.root, Some(PathBuf::from("/tmp/rsi-agent-store")));
        assert_eq!(command.output, ManagementOutput::Json);
        assert!(parse(&["agent-store", "verify", "--root", "relative"]).is_err());
        assert!(parse(&["agent-store", "verify", "--root", "/a", "--root", "/b"]).is_err());
        assert!(parse(&["agent-store", "unknown"]).is_err());
    }

    #[test]
    fn parses_named_applications_and_strict_profile_management() {
        let Parse::Application(application) = parse(&[
            "--profile",
            "headless",
            "task",
            "--session-id",
            "session-one",
        ])
        .unwrap() else {
            panic!("application")
        };
        assert_eq!(application.profile.as_str(), "headless");
        assert_eq!(application.arguments.len(), 3);

        let Parse::Profile(profile) = parse(&[
            "profile", "host", "copy", "standard", "custom", "--output", "json",
        ])
        .unwrap() else {
            panic!("profile")
        };
        assert_eq!(profile.kind, ProfileKind::Host);
        assert_eq!(profile.operation, ProfileOperationKind::Copy);
        assert_eq!(profile.ids, ["standard", "custom"]);
        assert_eq!(profile.output, ManagementOutput::Json);
        assert!(parse(&["profile", "application", "preview", "session"]).is_err());
        assert!(parse(&["profile", "host", "delete"]).is_err());
    }

    #[test]
    fn parses_explicit_host_lifecycle_without_ambiguous_targets() {
        let Parse::Host(command) =
            parse(&["host", "restart", "--profile", "custom", "--force"]).unwrap()
        else {
            panic!("host")
        };
        assert_eq!(command.operation, HostOperation::Restart);
        assert_eq!(command.profile.as_str(), "custom");
        assert!(command.force);
        assert!(parse(&["host", "status", "--profile", "custom"]).is_err());
        assert!(parse(&["host", "reload", "--force"]).is_err());

        let Parse::Host(detached) = parse(&["host", "serve", "--detached-child"]).unwrap() else {
            panic!("detached serve")
        };
        assert!(detached.detached_child);
        assert!(parse(&["host", "start", "--detached-child"]).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn graceful_host_stop_wait_includes_drain_and_shutdown_margin() {
        assert_eq!(
            host_stop_timeout(false),
            SESSION_HOST_DRAIN_TIMEOUT + HOST_SHUTDOWN_MARGIN
        );
        assert!(host_stop_timeout(false) > SESSION_HOST_DRAIN_TIMEOUT);
        assert_eq!(host_stop_timeout(true), FORCE_HOST_STOP_TIMEOUT);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn daemon_stop_selection_remains_live_during_reload() {
        let mut daemon_task = tokio::spawn(std::future::pending::<rsi::Result<()>>());
        let mut reload_task = tokio::spawn(std::future::pending::<()>());

        let event = next_daemon_control_event(
            &mut daemon_task,
            std::future::ready(Some(())),
            std::future::pending::<Option<()>>(),
            std::future::pending::<Option<()>>(),
            false,
            Some(&mut reload_task),
        )
        .await;

        assert!(matches!(event, DaemonControlEvent::Stop));
        assert!(!reload_task.is_finished());
        reload_task.abort();
        daemon_task.abort();
    }

    #[test]
    fn session_input_is_bounded_before_a_complete_line_is_allocated() {
        for capacity in [7, 8 * 1024] {
            let mut reader = std::io::BufReader::with_capacity(
                capacity,
                std::io::Cursor::new(format!("{}\nok\n", "x".repeat(MAXIMUM_TURN_TEXT_BYTES + 1))),
            );
            assert!(matches!(
                read_bounded_stdin_line(&mut reader),
                SessionInput::TooLarge
            ));
            assert!(matches!(
                read_bounded_stdin_line(&mut reader),
                SessionInput::Line(line) if line == "ok"
            ));
        }
    }

    #[tokio::test]
    async fn session_input_reader_backpressures_after_one_complete_line() {
        let mut input = spawn_session_input_reader(std::io::Cursor::new("one\ntwo\nthree\n"));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while input.len() != SESSION_INPUT_CHANNEL_CAPACITY {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reader did not fill its bounded handoff");
        assert_eq!(input.len(), 1);
        assert!(matches!(input.recv().await, Some(SessionInput::Line(line)) if line == "one"));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while input.len() != SESSION_INPUT_CHANNEL_CAPACITY {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reader did not resume after consumer progress");
        assert_eq!(input.len(), 1);
    }
}
