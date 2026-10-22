use super::{
    CancellationToken, DAEMON_READINESS_TIMEOUT, Duration, FORCE_HOST_STOP_TIMEOUT,
    HOST_DIAGNOSTICS_INTERVAL, HOST_SHUTDOWN_MARGIN, HostCommand, HostOperation, HostOwnerMode,
    HostProfileId, JoinHandle, ProfileCatalog, RsiError, SERVICE_HOST_DRAIN_TIMEOUT,
    ServiceHostDiagnostics, ServiceHostDiagnosticsSnapshot, ServiceHostPaths,
    StandardServiceDaemon, Stdio, Write, daemon_readiness_timeout_error, launch,
    owner_process_is_current, prepare_standard_composition, probe_service_host,
    profile_management_error, report_error, standard_paths,
};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct ProductHostLauncher;
#[async_trait::async_trait]
impl rsi::LocalHostLauncher for ProductHostLauncher {
    async fn ensure_started(&self, paths: &rsi_host::HostPaths) -> rsi::Result<()> {
        let selected = standard_paths()?;
        if selected.config() != paths.config() || selected.state() != paths.state() {
            return Err(RsiError::Boot(
                "application launcher received different home authorities".into(),
            ));
        }
        start_host_daemon_inner(&HostProfileId::new("standard").expect("constant"), true).await
    }
}

pub(super) async fn run_host(command: HostCommand) -> u8 {
    if matches!(command.operation, HostOperation::Serve)
        && let Err(error) = launch::maintain()
    {
        return report_error(&error);
    }
    let result = match command.operation {
        HostOperation::Serve => serve_host_daemon(&command.profile, command.detached_child).await,
        HostOperation::Start => start_host_daemon(&command.profile).await,
        HostOperation::Restart => restart_host_daemon(&command.profile, command.force).await,
        HostOperation::Stop => stop_host_daemon(command.force).await,
        HostOperation::Status => status_host_daemon().await,
        HostOperation::Reload => reload_host_daemon().await,
    };
    result.map_or_else(|error| report_error(&error), |()| 0)
}

#[cfg(target_os = "linux")]
pub(super) enum DaemonControlEvent {
    Daemon(std::result::Result<rsi::Result<()>, tokio::task::JoinError>),
    Stop,
    ReloadSignal(Option<()>),
    ReloadFinished(std::result::Result<(), tokio::task::JoinError>),
}

#[cfg(target_os = "linux")]
pub(super) async fn wait_for_reload_task(
    task: Option<&mut JoinHandle<()>>,
) -> std::result::Result<(), tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn stop_reload_task(task: &mut Option<JoinHandle<()>>) {
    let Some(task) = task.take() else {
        return;
    };
    if !task.is_finished() {
        task.abort();
    }
    if let Err(error) = task.await
        && !error.is_cancelled()
    {
        let _ = writeln!(
            std::io::stderr(),
            "Service Host reload task failed during shutdown: {error}"
        );
    }
}

#[cfg(target_os = "linux")]
pub(super) async fn next_daemon_control_event<Terminate, Interrupt, Reload>(
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

#[cfg(any(target_os = "linux", windows))]
fn acquire_daemon_owner(
    paths: &rsi_host::HostPaths,
    detached_child: bool,
) -> rsi::Result<rsi_service_host::HostOwnerLease> {
    #[cfg(target_os = "linux")]
    if detached_child {
        rustix::process::setsid()
            .map_err(|error| RsiError::Boot(format!("detach Service Host daemon: {error}")))?;
    }
    #[cfg(windows)]
    let _ = detached_child;
    let host_paths = ServiceHostPaths::from_host_paths(paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let lease = rsi_service_host::HostOwnerLease::try_acquire(host_paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    rsi::reject_pending_import(paths)?;
    Ok(lease)
}

#[cfg(target_os = "linux")]
pub(super) async fn serve_host_daemon(
    profile_id: &HostProfileId,
    detached_child: bool,
) -> rsi::Result<()> {
    let paths = standard_paths()?;
    let owner_lease = acquire_daemon_owner(&paths, detached_child)?;
    let [mut terminate, mut interrupt, mut reload] = daemon_signals()?;
    let profile = selected_host_profile(&paths, profile_id)?;
    let (composition, presets) = prepare_standard_composition(None, paths).await?;
    let daemon = match StandardServiceDaemon::start(composition, &profile, owner_lease).await {
        Ok(daemon) => daemon,
        Err(error) => {
            let _ = presets.shutdown().await;
            return Err(error);
        }
    };
    let running = daemon.running();
    let diagnostics = daemon.diagnostics();
    let cancellation = CancellationToken::new();
    let diagnostics_stop = CancellationToken::new();
    let diagnostics_task = tokio::spawn(log_service_host_diagnostics(
        diagnostics,
        diagnostics_stop.clone(),
    ));
    let mut daemon_task = tokio::spawn(daemon.run(cancellation.clone()));
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
                break result;
            }
            DaemonControlEvent::Stop => {
                reload_open = false;
                cancellation.cancel();
                stop_reload_task(&mut reload_task).await;
            }
            DaemonControlEvent::ReloadSignal(signal) => {
                if signal.is_none() {
                    let _ = writeln!(std::io::stderr(), "Service Host SIGHUP listener closed");
                    reload_open = false;
                } else {
                    reload_task = Some(tokio::spawn(reload_host_profile(Arc::clone(&running))));
                }
            }
            DaemonControlEvent::ReloadFinished(result) => {
                if let Err(error) = result {
                    let _ = writeln!(
                        std::io::stderr(),
                        "Service Host reload task failed: {error}"
                    );
                }
                reload_task = None;
            }
        }
    };
    finish_daemon_shutdown(
        daemon_result,
        &mut reload_task,
        diagnostics_stop,
        diagnostics_task,
        async {
            let shutdown = presets.shutdown().await;
            if !shutdown.is_clean() {
                return Err(RsiError::Boot(format!(
                    "Agent-preset daemon shutdown reported {} cleanup failures",
                    shutdown.report().total_failures()
                )));
            }
            Ok(())
        },
    )
    .await
}

#[cfg(target_os = "linux")]
fn daemon_signals() -> rsi::Result<[tokio::signal::unix::Signal; 3]> {
    use tokio::signal::unix::{SignalKind, signal};
    let register = |kind, name| {
        signal(kind).map_err(|error| RsiError::Boot(format!("failed to register {name}: {error}")))
    };
    Ok([
        register(SignalKind::terminate(), "SIGTERM")?,
        register(SignalKind::interrupt(), "SIGINT")?,
        register(SignalKind::hangup(), "SIGHUP")?,
    ])
}

fn selected_host_profile(
    paths: &rsi_host::HostPaths,
    profile_id: &HostProfileId,
) -> rsi::Result<rsi::HostProfileDocument> {
    let catalog = ProfileCatalog::new(paths.clone(), rsi_app_catalog::metadata());
    if profile_id.as_str() == "standard" {
        launch::current()?
            .configuration()?
            .service_profile(&catalog)
    } else {
        catalog.host(profile_id).map_err(profile_management_error)
    }
}

#[cfg(windows)]
pub(super) async fn serve_host_daemon(
    profile_id: &HostProfileId,
    detached_child: bool,
) -> rsi::Result<()> {
    let paths = standard_paths()?;
    let lease = acquire_daemon_owner(&paths, detached_child)?;
    let profile = selected_host_profile(&paths, profile_id)?;
    let (composition, presets) = prepare_standard_composition(None, paths).await?;
    let daemon = match StandardServiceDaemon::start(composition, &profile, lease).await {
        Ok(daemon) => daemon,
        Err(error) => {
            presets.shutdown().await;
            return Err(error);
        }
    };
    let diagnostics_stop = CancellationToken::new();
    let diagnostics_task = tokio::spawn(log_service_host_diagnostics(
        daemon.diagnostics(),
        diagnostics_stop.clone(),
    ));
    let stop = CancellationToken::new();
    let mut task = tokio::spawn(daemon.run(stop.clone()));
    let result = tokio::select! {
        result = &mut task => result,
        signal = tokio::signal::ctrl_c() => {
            stop.cancel();
            let result = task.await;
            match signal {
                Ok(()) => result,
                Err(error) => Ok(Err(RsiError::Boot(format!("console control: {error}")))),
            }
        }
    };
    finish_daemon_shutdown(
        result,
        &mut None,
        diagnostics_stop,
        diagnostics_task,
        async {
            let cleanup = presets.shutdown().await;
            if !cleanup.is_clean() {
                return Err(RsiError::Boot("Agent-preset daemon cleanup failed".into()));
            }
            Ok(())
        },
    )
    .await
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn finish_daemon_shutdown(
    daemon_result: std::result::Result<rsi::Result<()>, tokio::task::JoinError>,
    reload_task: &mut Option<JoinHandle<()>>,
    diagnostics_stop: CancellationToken,
    diagnostics_task: JoinHandle<()>,
    presets: impl std::future::Future<Output = rsi::Result<()>>,
) -> rsi::Result<()> {
    stop_reload_task(reload_task).await;
    diagnostics_stop.cancel();
    if let Err(error) = diagnostics_task.await {
        let _ = writeln!(
            std::io::stderr(),
            "Service Host diagnostics task failed during shutdown: {error}"
        );
    }
    let shutdown = presets.await;
    daemon_result
        .map_err(|error| RsiError::Boot(format!("Service Host task failed: {error}")))??;
    shutdown
}

#[cfg(target_os = "linux")]
async fn reload_host_profile(running: Arc<rsi::RunningRsi>) {
    match running.reload().await {
        Ok(outcome) => {
            let _ = writeln!(std::io::stderr(), "Service Host reload: {outcome:?}");
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "Service Host reload failed: {error}");
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn log_service_host_diagnostics(
    mut generations: tokio::sync::watch::Receiver<ServiceHostDiagnostics>,
    stop: CancellationToken,
) {
    let mut diagnostics = generations.borrow_and_update().clone();
    let mut generations_open = true;
    let mut previous = ServiceHostDiagnosticsSnapshot::default();
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + HOST_DIAGNOSTICS_INTERVAL,
        HOST_DIAGNOSTICS_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            changed = generations.changed(), if generations_open => {
                if changed.is_err() {
                    generations_open = false;
                    continue;
                }
                let delta = diagnostics.snapshot().saturating_delta_since(previous);
                let _ = writeln!(std::io::stderr(), "{}", format_service_host_diagnostics(delta, true));
                diagnostics = generations.borrow_and_update().clone();
                previous = ServiceHostDiagnosticsSnapshot::default();
            }
            () = stop.cancelled() => {
                let delta = diagnostics.snapshot().saturating_delta_since(previous);
                let _ = writeln!(std::io::stderr(), "{}", format_service_host_diagnostics(delta, true));
                return;
            }
            _ = interval.tick() => {
                let current = diagnostics.snapshot();
                let delta = current.saturating_delta_since(previous);
                previous = current;
                if delta.has_anomaly() {
                    let _ = writeln!(std::io::stderr(), "{}", format_service_host_diagnostics(delta, false));
                }
            }
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
pub(super) fn format_service_host_diagnostics(
    delta: ServiceHostDiagnosticsSnapshot,
    final_delta: bool,
) -> String {
    format!(
        "Service Host diagnostics final={final_delta} accepted_connections={} accept_errors={} peer_credential_errors={} foreign_uid_rejections={} capacity_rejections={} service_failures={} api_rejected_requests={} api_failed_requests={} api_connection_failures={} api_tls_failures={} connection_task_panics={} drain_aborted_connections={}",
        delta.accepted_connections,
        delta.accept_errors,
        delta.peer_credential_errors,
        delta.foreign_uid_rejections,
        delta.capacity_rejections,
        delta.service_failures,
        delta.api.rejected_requests,
        delta.api.failed_requests,
        delta.api.connection_failures,
        delta.api.tls_failures,
        delta.connection_task_panics,
        delta.drain_aborted_connections,
    )
}

#[cfg(any(target_os = "linux", windows))]
fn open_daemon_log(host_paths: &ServiceHostPaths) -> rsi::Result<std::fs::File> {
    #[cfg(windows)]
    use std::io::Seek as _;
    #[cfg(windows)]
    {
        let directory =
            rsi_files_native_fs::open_absolute_directory_no_follow(host_paths.owner_directory())
                .map_err(|error| RsiError::Boot(error.to_string()))?;
        let name = std::path::Path::new("owner.log");
        let file =
            match rsi_files_native_fs::create_relative_private_file_no_follow(&directory, name) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    rsi_files_native_fs::open_relative_file_read_write_no_follow(&directory, name)
                        .map_err(|error| RsiError::Boot(error.to_string()))?
                }
                Err(error) => return Err(RsiError::Boot(error.to_string())),
            };
        rsi_files_native_fs::verify_private_file(&file)
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        let mut file = file;
        file.seek(std::io::SeekFrom::End(0))
            .map_err(|error| RsiError::Boot(error.to_string()))?;
        Ok(file)
    }
    #[cfg(unix)]
    {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let log = options
            .open(host_paths.owner_log())
            .map_err(|error| RsiError::Boot(format!("open Service Host log: {error}")))?;
        if !log
            .metadata()
            .map_err(|error| RsiError::Boot(error.to_string()))?
            .is_file()
        {
            return Err(RsiError::Boot(
                "Service Host log is not a regular file".into(),
            ));
        }
        log.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| RsiError::Boot(format!("chmod Service Host log: {error}")))?;
        Ok(log)
    }
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn start_host_daemon(profile_id: &HostProfileId) -> rsi::Result<()> {
    launch::maintain()?;
    start_host_daemon_inner(profile_id, false).await
}

async fn start_host_daemon_inner(profile_id: &HostProfileId, attach: bool) -> rsi::Result<()> {
    let paths = standard_paths()?;
    let host_paths = ServiceHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let coordination_deadline = tokio::time::Instant::now() + DAEMON_READINESS_TIMEOUT;
    let startup = loop {
        match rsi_service_host::HostStartupLease::try_acquire(&host_paths) {
            Ok(lease) => break lease,
            Err(rsi_service_host::ServiceHostError::OwnerActive)
                if attach && tokio::time::Instant::now() < coordination_deadline =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(RsiError::Boot(error.to_string())),
        }
    };
    // A caller may have waited while an upgrade was interrupted.
    rsi::reject_pending_import(&paths)?;
    start_host_coordinated(
        profile_id,
        attach,
        &paths,
        &startup,
        None,
        coordination_deadline,
    )
    .await
}

/// The caller retains the startup coordinator through native readiness.
/// An explicitly supplied executable is an already verified upgrade artifact;
/// its readiness uses frozen Lifecycle v1 so product schema changes are allowed.
#[allow(clippy::too_many_lines)] // The startup coordinator spans identity checks, native spawn, readiness and failure reaping.
pub(super) async fn start_host_coordinated(
    profile_id: &HostProfileId,
    attach: bool,
    paths: &rsi_host::HostPaths,
    _startup: &rsi_service_host::HostStartupLease,
    source: Option<&std::path::Path>,
    deadline: tokio::time::Instant,
) -> rsi::Result<()> {
    let host_paths = ServiceHostPaths::from_host_paths(paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    if source.is_some() {
        if let Some(owner) = host_paths
            .read_lifecycle_identity()
            .map_err(|error| RsiError::Boot(error.to_string()))?
            && owner
                .is_current()
                .map_err(|error| RsiError::Boot(error.to_string()))?
        {
            return Err(RsiError::Boot(
                "a Host became active during core replacement".into(),
            ));
        }
    } else if let Some(metadata) = host_paths
        .read_metadata()
        .map_err(|error| RsiError::Boot(error.to_string()))?
        && owner_process_is_current(&metadata).map_err(|error| RsiError::Boot(error.to_string()))?
    {
        if attach && metadata.mode == HostOwnerMode::Daemon {
            let environment_matches = tokio::time::timeout_at(deadline, async {
                probe_service_host(&metadata).await?;
                rsi::service_environment_matches(paths).await
            })
            .await
            .map_err(|_| daemon_readiness_timeout_error())??;
            if !environment_matches {
                eprintln!(
                    "warning: this Service Host retains different tool environment or credential fallbacks; restart the Host explicitly to adopt this caller's environment"
                );
            }
            return Ok(());
        }
        return Err(RsiError::Boot(format!(
            "Service Host owner is already active in {:?} mode",
            metadata.mode
        )));
    }
    // Only the child opens providers. Validate source references after checking
    // for an existing owner; later clients use its published Service selection.
    selected_host_profile(paths, profile_id)?;
    // The coordinator remains live through readiness. The child owns both leases;
    // no provider is activated in this short acquisition preflight.
    drop(
        rsi_service_host::HostOwnerLease::try_acquire(host_paths.clone())
            .map_err(|error| RsiError::Boot(error.to_string()))?,
    );
    let log = open_daemon_log(&host_paths)?;
    let stderr = log
        .try_clone()
        .map_err(|error| RsiError::Boot(format!("clone Service Host log: {error}")))?;
    let executable = super::host_runtime::publish_before(paths, source, deadline.into_std())?;
    let mut command = std::process::Command::new(&executable);
    launch::current()?.append_child_options(&mut command);
    command.args([
        "host",
        "serve",
        "--profile",
        profile_id.as_str(),
        "--detached-child",
    ]);
    #[cfg(windows)]
    super::host_runtime::detach(&mut command)?;
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| RsiError::Boot(format!(
            "spawn detached Service Host: {error}; Windows launchers must permit Job breakaway; run the built rsi executable from an unrestricted terminal"
        )))?;
    let mut child = DaemonChildGuard::new(child);
    loop {
        if let Some(status) = child
            .child_mut()
            .try_wait()
            .map_err(|error| RsiError::Boot(format!("wait for Service Host daemon: {error}")))?
        {
            return Err(RsiError::Boot(format!(
                "Service Host daemon exited before readiness with {status}; inspect {}",
                host_paths.owner_log().display()
            )));
        }
        if source.is_some() {
            if let Some(owner) = host_paths
                .read_lifecycle_identity()
                .map_err(|error| RsiError::Boot(error.to_string()))?
                && owner.pid == child.id()
                && owner.mode == HostOwnerMode::Daemon
                && owner.socket_path.as_deref() == Some(host_paths.socket())
                && owner
                    .is_current()
                    .map_err(|error| RsiError::Boot(error.to_string()))?
            {
                let status = tokio::time::timeout_at(
                    deadline,
                    rsi_service_host::request_lifecycle_identity(
                        &owner,
                        rsi_service_host::LifecycleOperation::Status,
                    ),
                )
                .await
                .map_err(|_| daemon_readiness_timeout_error())?
                .map_err(|error| RsiError::Boot(error.to_string()))?;
                if status.stopping {
                    return Err(RsiError::Boot("new core stopped during readiness".into()));
                }
                child.disarm();
                if let Err(error) = super::host_runtime::collect(paths, &executable) {
                    eprintln!("warning: {error}");
                }
                return Ok(());
            }
        } else if let Some(metadata) = host_paths
            .read_metadata()
            .map_err(|error| RsiError::Boot(error.to_string()))?
            && metadata.pid == child.id()
            && metadata.mode == HostOwnerMode::Daemon
            && metadata.socket_path.as_deref() == Some(host_paths.socket())
            && owner_process_is_current(&metadata)
                .map_err(|error| RsiError::Boot(error.to_string()))?
        {
            tokio::time::timeout_at(deadline, probe_service_host(&metadata))
                .await
                .map_err(|_| daemon_readiness_timeout_error())?
                .map_err(|error| RsiError::Boot(format!("daemon readiness probe: {error}")))?;
            if !attach {
                println!("started\t{}", child.id());
            }
            child.disarm();
            if let Err(error) = super::host_runtime::collect(paths, &executable) {
                eprintln!("warning: {error}");
            }
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RsiError::Boot(format!(
                "Service Host daemon did not become ready within {} seconds; inspect {}",
                DAEMON_READINESS_TIMEOUT.as_secs(),
                host_paths.owner_log().display()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(any(target_os = "linux", windows))]
pub(super) struct DaemonChildGuard(Option<std::process::Child>);

#[cfg(any(target_os = "linux", windows))]
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

#[cfg(any(target_os = "linux", windows))]
impl Drop for DaemonChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn stop_host_daemon(force: bool) -> rsi::Result<()> {
    let paths = standard_paths()?;
    let host_paths = ServiceHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let metadata = host_paths
        .read_lifecycle_identity()
        .map_err(|error| RsiError::Boot(error.to_string()))?
        .ok_or_else(|| RsiError::Boot("no Service Host owner metadata exists".into()))?;
    if metadata.mode != HostOwnerMode::Daemon {
        return Err(RsiError::Boot(
            "the active Service Host is embedded and cannot be stopped as a daemon".into(),
        ));
    }
    if force {
        metadata
            .force_stop()
            .map_err(|error| RsiError::Boot(error.to_string()))?;
    } else {
        rsi_service_host::request_lifecycle_identity(
            &metadata,
            rsi_service_host::LifecycleOperation::Stop,
        )
        .await
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    }
    let wait = host_stop_timeout(force);
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if !metadata
            .is_current()
            .map_err(|error| RsiError::Boot(error.to_string()))?
        {
            println!("stopped\t{}", metadata.pid);
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RsiError::Boot(format!(
                "Service Host did not stop within {} seconds; retry with `rsi host stop --force`",
                wait.as_secs()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(any(target_os = "linux", windows))]
pub(super) fn host_stop_timeout(force: bool) -> Duration {
    if force {
        FORCE_HOST_STOP_TIMEOUT
    } else {
        SERVICE_HOST_DRAIN_TIMEOUT + HOST_SHUTDOWN_MARGIN
    }
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn restart_host_daemon(
    profile_id: &HostProfileId,
    force: bool,
) -> rsi::Result<()> {
    let paths = standard_paths()?;
    let host_paths = ServiceHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    if let Some(identity) = host_paths
        .read_lifecycle_identity()
        .map_err(|error| RsiError::Boot(error.to_string()))?
        && identity
            .is_current()
            .map_err(|error| RsiError::Boot(error.to_string()))?
    {
        let impact = match host_paths.read_metadata().ok().flatten() {
            Some(metadata) => rsi::workflow_restart_impact(&metadata).await,
            None => Err(RsiError::Boot("product metadata differs".into())),
        };
        match impact {
            Ok(impact) => eprintln!(
                "Host restart interrupts {}{} unfinished workflows; scripts are not replayed.",
                if impact.truncated { "at least " } else { "" },
                impact.unfinished
            ),
            Err(_) => eprintln!(
                "Host restart interrupts unfinished workflows; current count is unavailable. Scripts are not replayed."
            ),
        }
        stop_host_daemon(force).await?;
    }
    start_host_daemon(profile_id).await
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn status_host_daemon() -> rsi::Result<()> {
    let paths = standard_paths()?;
    let host_paths = ServiceHostPaths::from_host_paths(&paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let Some(metadata) = host_paths
        .read_lifecycle_identity()
        .map_err(|error| RsiError::Boot(error.to_string()))?
    else {
        println!("stopped");
        return Ok(());
    };
    let current = metadata
        .is_current()
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let compatible = host_paths
        .read_metadata()
        .ok()
        .flatten()
        .is_some_and(|product| product.is_compatible_with_current().unwrap_or(false));
    let responsive = if current && metadata.mode == HostOwnerMode::Daemon {
        rsi_service_host::request_lifecycle_identity(
            &metadata,
            rsi_service_host::LifecycleOperation::Status,
        )
        .await
        .is_ok()
    } else {
        false
    };
    println!(
        "{}\tmode={:?}\tpid={}\tepoch={}\tproduct-compatible={}",
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
        compatible
    );
    Ok(())
}

#[cfg(any(target_os = "linux", windows))]
pub(super) async fn reload_host_daemon() -> rsi::Result<()> {
    let reply = rsi::reload_service_host(&standard_paths()?).await?;
    println!(
        "{}",
        serde_json::to_string(&reply).map_err(|error| RsiError::Boot(error.to_string()))?
    );
    Ok(())
}
