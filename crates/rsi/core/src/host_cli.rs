use super::*;

pub(super) async fn run_host(command: HostCommand) -> u8 {
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

#[cfg(target_os = "linux")]
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
            "Session Host reload task failed during shutdown: {error}"
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

#[cfg(target_os = "linux")]
fn acquire_daemon_owner(
    paths: &rsi_host::HostPaths,
    detached_child: bool,
) -> rsi::Result<rsi_session_host::HostOwnerLease> {
    if detached_child {
        rustix::process::setsid()
            .map_err(|error| RsiError::Boot(format!("detach Session Host daemon: {error}")))?;
    }
    let host_paths = SessionHostPaths::from_host_paths(paths)
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    if detached_child {
        use std::os::fd::AsFd as _;
        let stdin = std::io::stdin();
        rustix::io::fcntl_setfd(stdin.as_fd(), rustix::io::FdFlags::CLOEXEC)
            .map_err(|error| RsiError::Boot(format!("fence inherited owner lease: {error}")))?;
        let file = stdin
            .as_fd()
            .try_clone_to_owned()
            .map_err(|error| RsiError::Boot(format!("inherit owner lease: {error}")))?;
        rsi_session_host::HostOwnerLease::adopt_startup_file(host_paths, file.into())
    } else {
        rsi_session_host::HostOwnerLease::try_acquire(host_paths)
    }
    .map_err(|error| RsiError::Boot(error.to_string()))
}

#[cfg(target_os = "linux")]
pub(super) async fn serve_host_daemon(
    profile_id: &HostProfileId,
    detached_child: bool,
) -> rsi::Result<()> {
    let paths = standard_paths()?;
    let owner_lease = acquire_daemon_owner(&paths, detached_child)?;
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|error| RsiError::Boot(format!("failed to register SIGTERM: {error}")))?;
    let mut interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|error| RsiError::Boot(format!("failed to register SIGINT: {error}")))?;
    let mut reload = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|error| RsiError::Boot(format!("failed to register SIGHUP: {error}")))?;
    let profile = ProfileCatalog::new(paths.clone())
        .host(profile_id)
        .map_err(profile_management_error)?;
    let (composition, presets) = prepare_standard_composition(paths).await?;
    let daemon = match StandardSessionDaemon::start(composition, &profile, owner_lease).await {
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
    let diagnostics_task = tokio::spawn(log_session_host_diagnostics(
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
                break result.map_err(|error| {
                    RsiError::Boot(format!("Session Host task failed: {error}"))
                })?;
            }
            DaemonControlEvent::Stop => {
                reload_open = false;
                cancellation.cancel();
                stop_reload_task(&mut reload_task).await;
            }
            DaemonControlEvent::ReloadSignal(signal) => {
                if signal.is_none() {
                    let _ = writeln!(std::io::stderr(), "Session Host SIGHUP listener closed");
                    reload_open = false;
                } else {
                    reload_task = Some(tokio::spawn(reload_host_profile(Arc::clone(&running))));
                }
            }
            DaemonControlEvent::ReloadFinished(result) => {
                if let Err(error) = result {
                    let _ = writeln!(
                        std::io::stderr(),
                        "Session Host reload task failed: {error}"
                    );
                }
                reload_task = None;
            }
        }
    };
    stop_reload_task(&mut reload_task).await;
    diagnostics_stop.cancel();
    if let Err(error) = diagnostics_task.await {
        let _ = writeln!(
            std::io::stderr(),
            "Session Host diagnostics task failed during shutdown: {error}"
        );
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
async fn reload_host_profile(running: Arc<rsi::RunningRsi>) {
    match running.reload().await {
        Ok(outcome) => {
            let _ = writeln!(std::io::stderr(), "Session Host reload: {outcome:?}");
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "Session Host reload failed: {error}");
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) async fn log_session_host_diagnostics(
    diagnostics: SessionHostDiagnostics,
    stop: CancellationToken,
) {
    let mut previous = SessionHostDiagnosticsSnapshot::default();
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + HOST_DIAGNOSTICS_INTERVAL,
        HOST_DIAGNOSTICS_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => {
                let delta = diagnostics.snapshot().saturating_delta_since(previous);
                let _ = writeln!(std::io::stderr(), "{}", format_session_host_diagnostics(delta, true));
                return;
            }
            _ = interval.tick() => {
                let current = diagnostics.snapshot();
                let delta = current.saturating_delta_since(previous);
                previous = current;
                if delta.has_anomaly() {
                    let _ = writeln!(std::io::stderr(), "{}", format_session_host_diagnostics(delta, false));
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn format_session_host_diagnostics(
    delta: SessionHostDiagnosticsSnapshot,
    final_delta: bool,
) -> String {
    format!(
        "Session Host diagnostics final={final_delta} accepted_connections={} accept_errors={} peer_credential_errors={} foreign_uid_rejections={} capacity_rejections={} handshake_rejections={} handshake_failures={} request_failures={} response_failures={} connection_task_panics={} drain_aborted_connections={}",
        delta.accepted_connections,
        delta.accept_errors,
        delta.peer_credential_errors,
        delta.foreign_uid_rejections,
        delta.capacity_rejections,
        delta.handshake_rejections,
        delta.handshake_failures,
        delta.request_failures,
        delta.response_failures,
        delta.connection_task_panics,
        delta.drain_aborted_connections,
    )
}

#[cfg(target_os = "linux")]
pub(super) async fn expected_host_launch(
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
fn open_daemon_log(host_paths: &SessionHostPaths) -> rsi::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let log = options
        .open(host_paths.owner_log())
        .map_err(|error| RsiError::Boot(format!("open Session Host log: {error}")))?;
    if !log
        .metadata()
        .map_err(|error| RsiError::Boot(error.to_string()))?
        .is_file()
    {
        return Err(RsiError::Boot(
            "Session Host log is not a regular file".into(),
        ));
    }
    log.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| RsiError::Boot(format!("chmod Session Host log: {error}")))?;
    Ok(log)
}

#[cfg(target_os = "linux")]
pub(super) async fn start_host_daemon(profile_id: &HostProfileId) -> rsi::Result<()> {
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
    let owner_lease = rsi_session_host::HostOwnerLease::try_acquire(host_paths.clone())
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let log = open_daemon_log(&host_paths)?;
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
        .stdin(Stdio::from(
            owner_lease
                .into_startup_file()
                .map_err(|error| RsiError::Boot(error.to_string()))?,
        ))
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| RsiError::Boot(format!("spawn Session Host daemon: {error}")))?;
    let mut child = DaemonChildGuard::new(child);
    let deadline = tokio::time::Instant::now() + DAEMON_READINESS_TIMEOUT;
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
            tokio::time::timeout_at(
                deadline,
                UdsSessionApplication::connect(
                    host_paths.socket(),
                    &expected_key,
                    metadata.host_epoch,
                ),
            )
            .await
            .map_err(|_| daemon_readiness_timeout_error())?
            .map_err(|error| RsiError::Boot(format!("daemon readiness probe: {error}")))?;
            println!("started\t{}", child.id());
            child.disarm();
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RsiError::Boot(format!(
                "Session Host daemon did not become ready within {} seconds; inspect {}",
                DAEMON_READINESS_TIMEOUT.as_secs(),
                host_paths.owner_log().display()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(target_os = "linux")]
pub(super) struct DaemonChildGuard(Option<std::process::Child>);

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
pub(super) async fn stop_host_daemon(force: bool) -> rsi::Result<()> {
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
pub(super) fn host_stop_timeout(force: bool) -> Duration {
    if force {
        FORCE_HOST_STOP_TIMEOUT
    } else {
        SESSION_HOST_DRAIN_TIMEOUT + HOST_SHUTDOWN_MARGIN
    }
}

#[cfg(target_os = "linux")]
pub(super) async fn restart_host_daemon(
    profile_id: &HostProfileId,
    force: bool,
) -> rsi::Result<()> {
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
pub(super) async fn status_host_daemon() -> rsi::Result<()> {
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
pub(super) fn reload_host_daemon() -> rsi::Result<()> {
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
