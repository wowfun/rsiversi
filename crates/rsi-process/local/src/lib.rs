//! Tokio-backed local managed-process provider.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_process::{
    MAXIMUM_ACTIVE_PROCESSES, MAXIMUM_PROCESS_CAPTURE_BYTES, ManagedProcess, Process,
    ProcessContract, ProcessError, ProcessSpec, Result,
};
#[cfg(unix)]
use rsi_process::{ProcessControl, ProcessOutcome, ProcessOutput, ProcessRead};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt as _;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Mutex, Weak};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
#[cfg(unix)]
use tokio::sync::Notify;

const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 10_000;
#[cfg(unix)]
const POST_KILL_GROUP_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration for one local Process provider generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessLocalConfig {
    /// Maximum simultaneously unsettled direct children.
    #[serde(default = "default_maximum_active_processes")]
    pub maximum_active_processes: usize,
    /// Aggregate retained stdout/stderr reservation.
    #[serde(default = "default_maximum_capture_bytes")]
    pub maximum_capture_bytes: usize,
    /// Provider retirement wait bound.
    #[serde(default = "default_shutdown_timeout_ms")]
    pub shutdown_timeout_ms: u64,
}

const fn default_maximum_active_processes() -> usize {
    MAXIMUM_ACTIVE_PROCESSES
}

const fn default_maximum_capture_bytes() -> usize {
    MAXIMUM_PROCESS_CAPTURE_BYTES
}

const fn default_shutdown_timeout_ms() -> u64 {
    DEFAULT_SHUTDOWN_TIMEOUT_MS
}

impl Default for ProcessLocalConfig {
    fn default() -> Self {
        Self {
            maximum_active_processes: default_maximum_active_processes(),
            maximum_capture_bytes: default_maximum_capture_bytes(),
            shutdown_timeout_ms: default_shutdown_timeout_ms(),
        }
    }
}

impl ProcessLocalConfig {
    fn validate(&self) -> Result<()> {
        if self.maximum_active_processes == 0
            || self.maximum_active_processes > MAXIMUM_ACTIVE_PROCESSES
        {
            return Err(ProcessError::InvalidInput(format!(
                "maximum_active_processes must be within 1..={MAXIMUM_ACTIVE_PROCESSES}"
            )));
        }
        if self.maximum_capture_bytes == 0
            || self.maximum_capture_bytes > MAXIMUM_PROCESS_CAPTURE_BYTES
        {
            return Err(ProcessError::InvalidInput(format!(
                "maximum_capture_bytes must be within 1..={MAXIMUM_PROCESS_CAPTURE_BYTES}"
            )));
        }
        if self.shutdown_timeout_ms == 0 || self.shutdown_timeout_ms > 300_000 {
            return Err(ProcessError::InvalidInput(
                "shutdown_timeout_ms must be within 1..=300000".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Service {
    #[cfg(unix)]
    config: ProcessLocalConfig,
    #[cfg(unix)]
    state: Arc<ServiceState>,
    #[cfg(unix)]
    groups: Arc<dyn ProcessGroups>,
}

#[cfg(unix)]
#[derive(Debug)]
struct ServiceState {
    inner: Mutex<Registry>,
    changed: Notify,
}

#[cfg(unix)]
#[derive(Debug, Default)]
struct Registry {
    accepting: bool,
    active: usize,
    inflight: usize,
    capture_reserved: usize,
    managed: HashMap<u32, Arc<ChildState>>,
}

#[cfg(unix)]
impl Registry {
    fn accepting() -> Self {
        Self {
            accepting: true,
            ..Self::default()
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct Tail {
    maximum: usize,
    inner: Mutex<TailInner>,
    _reservation: Arc<CaptureReservation>,
}

#[cfg(unix)]
#[derive(Debug)]
struct CaptureReservation {
    service: Weak<ServiceState>,
    bytes: usize,
}

#[cfg(unix)]
impl Drop for CaptureReservation {
    fn drop(&mut self) {
        if let Some(service) = self.service.upgrade() {
            let mut registry = lock_registry(&service);
            registry.capture_reserved = registry.capture_reserved.saturating_sub(self.bytes);
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Default)]
struct TailInner {
    bytes: VecDeque<u8>,
    total: u64,
}

#[cfg(unix)]
impl Tail {
    fn new(maximum: usize, reservation: Arc<CaptureReservation>) -> Self {
        Self {
            maximum,
            inner: Mutex::new(TailInner::default()),
            _reservation: reservation,
        }
    }

    fn push(&self, chunk: &[u8]) -> Result<()> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.total = inner
            .total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| ProcessError::Io("process output offset overflow".into()))?;
        inner.bytes.extend(chunk);
        let excess = inner.bytes.len().saturating_sub(self.maximum);
        inner.bytes.drain(..excess);
        Ok(())
    }
}

#[cfg(unix)]
impl ProcessOutput for Tail {
    fn read_from(&self, offset: u64) -> Result<ProcessRead> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if offset > inner.total {
            return Err(ProcessError::InvalidInput(
                "process output offset exceeds the stream tail".into(),
            ));
        }
        let retained = inner.bytes.len() as u64;
        let oldest_offset = inner.total.saturating_sub(retained);
        let lossy = offset < oldest_offset;
        let start = usize::try_from(offset.max(oldest_offset).saturating_sub(oldest_offset))
            .map_err(|_| ProcessError::InvalidInput("process output offset is too large".into()))?;
        Ok(ProcessRead {
            bytes: inner.bytes.iter().skip(start).copied().collect(),
            oldest_offset,
            next_offset: inner.total,
            lossy,
        })
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ChildState {
    pid: u32,
    grace: Duration,
    runtime: tokio::runtime::Handle,
    service: Weak<ServiceState>,
    groups: Arc<dyn ProcessGroups>,
    outcome: Mutex<Option<Result<ProcessOutcome>>>,
    settled: Notify,
    active_released: AtomicBool,
    termination_started: AtomicBool,
}

#[cfg(unix)]
#[derive(Debug)]
struct ManagedControl {
    child: Arc<ChildState>,
    stdout: Arc<Tail>,
    stderr: Arc<Tail>,
}

#[cfg(unix)]
impl ChildState {
    fn finish(&self, outcome: Result<ProcessOutcome>) {
        let mut current = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.is_some() {
            return;
        }
        *current = Some(outcome);
        drop(current);
        self.release_active();
        self.settled.notify_waiters();
    }

    fn release_active(&self) {
        if self.active_released.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(service) = self.service.upgrade() {
            let mut registry = lock_registry(&service);
            registry.active = registry.active.saturating_sub(1);
            if registry
                .managed
                .get(&self.pid)
                .is_some_and(|current| std::ptr::eq(Arc::as_ptr(current), std::ptr::from_ref(self)))
            {
                registry.managed.remove(&self.pid);
            }
            drop(registry);
            service.changed.notify_waiters();
        }
    }

    fn group_is_alive(&self) -> bool {
        self.service.upgrade().is_some_and(|service| {
            let registry = lock_registry(&service);
            registry
                .managed
                .get(&self.pid)
                .is_some_and(|current| Arc::as_ptr(current) == std::ptr::from_ref(self))
                && self.groups.is_alive(self.pid)
        })
    }

    fn signal_if_current(&self, tier: SignalTier) {
        let Some(service) = self.service.upgrade() else {
            return;
        };
        let registry = lock_registry(&service);
        if registry
            .managed
            .get(&self.pid)
            .is_some_and(|current| Arc::as_ptr(current) == std::ptr::from_ref(self))
        {
            self.groups.signal(self.pid, tier);
        }
    }
}

#[cfg(unix)]
impl Drop for ChildState {
    fn drop(&mut self) {
        self.release_active();
    }
}

#[cfg(unix)]
impl ChildState {
    fn terminate(self: &Arc<Self>) {
        if self.termination_started.swap(true, Ordering::AcqRel) {
            return;
        }
        self.signal_if_current(SignalTier::Terminate);
        let grace = self.grace;
        let state = Arc::downgrade(self);
        self.runtime.spawn(async move {
            tokio::time::sleep(grace).await;
            if let Some(state) = state.upgrade()
                && state.group_is_alive()
            {
                state.signal_if_current(SignalTier::Kill);
            }
        });
    }

    async fn wait_outcome(&self) -> Result<ProcessOutcome> {
        loop {
            let notified = self.settled.notified();
            if let Some(outcome) = self
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return outcome;
            }
            notified.await;
        }
    }
}

#[cfg(unix)]
#[async_trait]
impl ProcessControl for ManagedControl {
    fn pid(&self) -> u32 {
        self.child.pid
    }

    fn stdout(&self) -> Arc<dyn ProcessOutput> {
        self.stdout.clone()
    }

    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        self.stderr.clone()
    }

    fn terminate(&self) {
        self.child.terminate();
    }

    async fn wait(&self) -> Result<ProcessOutcome> {
        self.child.wait_outcome().await
    }
}

impl Process for Service {
    fn spawn(&self, spec: ProcessSpec) -> Result<ManagedProcess> {
        spec.validate()?;
        #[cfg(unix)]
        {
            let runtime = tokio::runtime::Handle::try_current()
                .map_err(|_| ProcessError::Spawn("Tokio runtime is unavailable".into()))?;
            self.spawn_unix(spec, &runtime)
        }
        #[cfg(not(unix))]
        {
            let _ = spec;
            Err(ProcessError::Unsupported)
        }
    }
}

impl Service {
    #[cfg(unix)]
    fn new(config: ProcessLocalConfig) -> Self {
        Self::with_groups(config, Arc::new(SystemProcessGroups))
    }

    #[cfg(unix)]
    fn with_groups(config: ProcessLocalConfig, groups: Arc<dyn ProcessGroups>) -> Self {
        Self {
            config,
            state: Arc::new(ServiceState {
                inner: Mutex::new(Registry::accepting()),
                changed: Notify::new(),
            }),
            groups,
        }
    }

    #[cfg(unix)]
    fn spawn_unix(
        &self,
        spec: ProcessSpec,
        runtime: &tokio::runtime::Handle,
    ) -> Result<ManagedProcess> {
        let (child, pid, capture_bytes) = self.admit_and_spawn(&spec)?;
        let reservation = Arc::new(CaptureReservation {
            service: Arc::downgrade(&self.state),
            bytes: capture_bytes,
        });
        let stdout = Arc::new(Tail::new(spec.stdout_max_bytes, Arc::clone(&reservation)));
        let stderr = Arc::new(Tail::new(spec.stderr_max_bytes, reservation));
        let state = Arc::new(ChildState {
            pid,
            grace: Duration::from_millis(spec.termination_grace_ms),
            runtime: runtime.clone(),
            service: Arc::downgrade(&self.state),
            groups: Arc::clone(&self.groups),
            outcome: Mutex::new(None),
            settled: Notify::new(),
            active_released: AtomicBool::new(false),
            termination_started: AtomicBool::new(false),
        });
        let published = {
            let mut registry = lock_registry(&self.state);
            registry.managed.insert(pid, Arc::clone(&state));
            registry.inflight = registry
                .inflight
                .checked_sub(1)
                .expect("every spawn publication has an in-flight admission");
            registry.accepting
        };
        self.state.changed.notify_waiters();
        supervise_child(
            runtime,
            child,
            &state,
            Arc::clone(&stdout),
            Arc::clone(&stderr),
            spec.stdin,
        );

        if !published {
            state.terminate();
            return Err(ProcessError::ShuttingDown);
        }

        let control: Arc<dyn ProcessControl> = Arc::new(ManagedControl {
            child: state,
            stdout,
            stderr,
        });
        Ok(ManagedProcess::new(control))
    }

    #[cfg(unix)]
    fn admit_and_spawn(&self, spec: &ProcessSpec) -> Result<(tokio::process::Child, u32, usize)> {
        let capture_bytes = spec.capture_bytes()?;
        let mut registry = lock_registry(&self.state);
        let capture_after = registry
            .capture_reserved
            .checked_add(capture_bytes)
            .ok_or(ProcessError::Capacity)?;
        if !registry.accepting {
            return Err(ProcessError::ShuttingDown);
        }
        if registry.active >= self.config.maximum_active_processes
            || capture_after > self.config.maximum_capture_bytes
        {
            return Err(ProcessError::Capacity);
        }
        registry.active += 1;
        registry.inflight += 1;
        registry.capture_reserved = capture_after;
        drop(registry);

        let mut command = tokio::process::Command::new(&spec.process.program);
        command
            .args(&spec.process.arguments)
            .current_dir(&spec.process.cwd)
            .env_clear()
            .envs(spec.environment.iter().cloned())
            .stdin(if spec.stdin.is_empty() {
                std::process::Stdio::null()
            } else {
                std::process::Stdio::piped()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false)
            .process_group(0);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                rollback_admission(&self.state, capture_bytes);
                return Err(ProcessError::Spawn(error.to_string()));
            }
        };
        let pid = child
            .id()
            .expect("a successfully spawned Tokio child has a process id");
        Ok((child, pid, capture_bytes))
    }

    #[cfg(unix)]
    async fn shutdown(&self) -> Result<()> {
        let processes = loop {
            let notified = self.state.changed.notified();
            let processes = {
                let mut registry = lock_registry(&self.state);
                registry.accepting = false;
                (registry.inflight == 0)
                    .then(|| registry.managed.values().cloned().collect::<Vec<_>>())
            };
            if let Some(processes) = processes {
                break processes;
            }
            notified.await;
        };
        for process in &processes {
            process.terminate();
        }
        let state = Arc::clone(&self.state);
        let mut cleanup = tokio::spawn(async move {
            let _state = state;
            for process in processes {
                let _ = process.wait_outcome().await;
            }
        });
        match tokio::time::timeout(
            Duration::from_millis(self.config.shutdown_timeout_ms),
            &mut cleanup,
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(ProcessError::Io(format!(
                "process cleanup task failed: {error}"
            ))),
            Err(_) => Err(ProcessError::ShutdownTimeout),
        }
    }
}

#[cfg(unix)]
fn supervise_child(
    runtime: &tokio::runtime::Handle,
    mut child: tokio::process::Child,
    state: &Arc<ChildState>,
    stdout: Arc<Tail>,
    stderr: Arc<Tail>,
    input: Vec<u8>,
) {
    let mut stdin_task = child.stdin.take().map(|mut stdin| {
        runtime.spawn(async move {
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        })
    });
    let stdout_pipe = child
        .stdout
        .take()
        .expect("piped stdout is present after successful spawn");
    let stderr_pipe = child
        .stderr
        .take()
        .expect("piped stderr is present after successful spawn");
    let mut stdout_task = runtime.spawn(drain(stdout_pipe, stdout));
    let mut stderr_task = runtime.spawn(drain(stderr_pipe, stderr));
    let wait_state = Arc::clone(state);
    let drain_grace = state.grace;
    runtime.spawn(async move {
        let status = child
            .wait()
            .await
            .map_err(|error| ProcessError::Io(error.to_string()));
        let group_settlement_timed_out = if wait_state.group_is_alive() {
            wait_state.terminate();
            !wait_for_group_disappearance(
                &wait_state,
                wait_state
                    .grace
                    .saturating_add(POST_KILL_GROUP_SETTLEMENT_TIMEOUT),
            )
            .await
        } else {
            false
        };
        if let Some(task) = stdin_task.as_mut() {
            if !task.is_finished() {
                task.abort();
            }
            let _ = task.await;
        }
        let drains = tokio::time::timeout(drain_grace, async {
            let stdout = (&mut stdout_task).await;
            let stderr = (&mut stderr_task).await;
            (stdout, stderr)
        })
        .await;
        let drain_error = match drains {
            Ok((Ok(Ok(())), Ok(Ok(())))) => None,
            Ok((stdout, stderr)) => {
                Some(format!("stdout drain={stdout:?}, stderr drain={stderr:?}"))
            }
            Err(_) => {
                stdout_task.abort();
                stderr_task.abort();
                let _ = (&mut stdout_task).await;
                let _ = (&mut stderr_task).await;
                Some("captured pipe drain timed out before EOF".into())
            }
        };
        let outcome = status.and_then(|status| {
            if group_settlement_timed_out {
                return Err(ProcessError::SettlementTimeout);
            }
            if let Some(error) = drain_error {
                return Err(ProcessError::Io(error));
            }
            Ok(ProcessOutcome {
                exit_code: status.code(),
                signal: status.signal(),
            })
        });
        wait_state.finish(outcome);
    });
}

#[cfg(unix)]
async fn wait_for_group_disappearance(state: &Arc<ChildState>, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, async {
        let mut delay = Duration::from_millis(5);
        while state.group_is_alive() {
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(Duration::from_millis(250));
        }
    })
    .await
    .is_ok()
}

#[cfg(unix)]
async fn drain(mut reader: impl AsyncRead + Unpin, tail: Arc<Tail>) -> Result<()> {
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| ProcessError::Io(error.to_string()))?;
        if read == 0 {
            return Ok(());
        }
        tail.push(&buffer[..read])?;
    }
}

#[cfg(unix)]
fn lock_registry(state: &ServiceState) -> std::sync::MutexGuard<'_, Registry> {
    state
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(unix)]
fn rollback_admission(state: &ServiceState, capture_bytes: usize) {
    let mut registry = lock_registry(state);
    registry.active = registry.active.saturating_sub(1);
    registry.inflight = registry.inflight.saturating_sub(1);
    registry.capture_reserved = registry.capture_reserved.saturating_sub(capture_bytes);
    drop(registry);
    state.changed.notify_waiters();
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum SignalTier {
    Terminate,
    Kill,
}

#[cfg(unix)]
trait ProcessGroups: std::fmt::Debug + Send + Sync + 'static {
    fn signal(&self, pid: u32, tier: SignalTier);
    fn is_alive(&self, pid: u32) -> bool;
}

#[cfg(unix)]
#[derive(Debug)]
struct SystemProcessGroups;

#[cfg(unix)]
impl ProcessGroups for SystemProcessGroups {
    fn signal(&self, pid: u32, tier: SignalTier) {
        let Some(pid) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            return;
        };
        let signal = match tier {
            SignalTier::Terminate => rustix::process::Signal::TERM,
            SignalTier::Kill => rustix::process::Signal::KILL,
        };
        let _ = rustix::process::kill_process_group(pid, signal);
    }

    fn is_alive(&self, pid: u32) -> bool {
        i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|pid| group_probe_is_alive(rustix::process::test_kill_process_group(pid)))
    }
}

#[cfg(unix)]
fn group_probe_is_alive(result: rustix::io::Result<()>) -> bool {
    matches!(result, Ok(()) | Err(rustix::io::Errno::PERM))
}

/// Ordinary factory for one local Process provider generation.
#[derive(Clone, Debug, Default)]
pub struct ProcessLocalFactory;

#[async_trait]
impl PluginFactory for ProcessLocalFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config = if desired.is_null() || desired == &serde_json::json!({}) {
            ProcessLocalConfig::default()
        } else {
            serde_json::from_value(desired.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        Ok(PreparedActivation::with_state(
            serde_json::to_value(&config)
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?,
            config,
            std::mem::size_of::<ProcessLocalConfig>(),
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        #[cfg(unix)]
        let service = Arc::new(Service::new(plan.take_state::<ProcessLocalConfig>()?));
        #[cfg(not(unix))]
        let service = {
            let _: ProcessLocalConfig = plan.take_state()?;
            Arc::new(Service {})
        };
        let process: Arc<dyn Process> = service.clone();
        let supply = plan.context().provide_local::<ProcessContract>(process)?;
        plan.defer(
            "shutdown local Process provider",
            Box::new(move || {
                Box::pin(async move {
                    #[cfg(unix)]
                    let result = service.shutdown().await.map_err(|error| error.to_string());
                    #[cfg(not(unix))]
                    let result = Ok(());
                    drop(service);
                    drop(supply);
                    result
                })
            }),
        )
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use rsi_sandbox::{
        ConfinedProcess, EnforcementStamp, SandboxBackend, SandboxFileSystem, SandboxMode,
        SandboxNetwork, SandboxScratch,
    };
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[derive(Debug, Default)]
    struct StubbornProcessGroups {
        alive: AtomicBool,
        terminated: AtomicBool,
        killed: AtomicBool,
        changed: Notify,
    }

    impl StubbornProcessGroups {
        async fn wait_until_terminated(&self) {
            loop {
                let changed = self.changed.notified();
                if self.terminated.load(Ordering::Acquire) {
                    return;
                }
                changed.await;
            }
        }
    }

    impl ProcessGroups for StubbornProcessGroups {
        fn signal(&self, _pid: u32, tier: SignalTier) {
            match tier {
                SignalTier::Terminate => self.terminated.store(true, Ordering::Release),
                SignalTier::Kill => self.killed.store(true, Ordering::Release),
            }
            self.changed.notify_waiters();
        }

        fn is_alive(&self, _pid: u32) -> bool {
            self.alive.load(Ordering::Acquire)
        }
    }

    fn immediate_process() -> ProcessSpec {
        let workspace = std::env::current_dir().unwrap().canonicalize().unwrap();
        ProcessSpec {
            process: ConfinedProcess {
                program: PathBuf::from("/bin/sh").canonicalize().unwrap(),
                arguments: vec![OsString::from("-c"), OsString::from("exit 0")],
                cwd: workspace.clone(),
                stamp: EnforcementStamp {
                    requested: SandboxMode::DangerFullAccess,
                    backend: SandboxBackend::Unconfined,
                    workspace,
                    filesystem: SandboxFileSystem::Unconfined,
                    scratch: SandboxScratch::Host,
                    network: SandboxNetwork::Host,
                },
            },
            stdin: Vec::new(),
            environment: Vec::new(),
            stdout_max_bytes: 1,
            stderr_max_bytes: 1,
            termination_grace_ms: 50,
        }
    }

    fn child_state(
        pid: u32,
        service: &Arc<ServiceState>,
        groups: Arc<dyn ProcessGroups>,
    ) -> Arc<ChildState> {
        Arc::new(ChildState {
            pid,
            grace: Duration::from_millis(1),
            runtime: tokio::runtime::Handle::current(),
            service: Arc::downgrade(service),
            groups,
            outcome: Mutex::new(None),
            settled: Notify::new(),
            active_released: AtomicBool::new(false),
            termination_started: AtomicBool::new(false),
        })
    }

    #[tokio::test]
    async fn a_stale_pid_owner_neither_removes_nor_signals_its_replacement() {
        let service = Arc::new(ServiceState {
            inner: Mutex::new(Registry {
                accepting: true,
                active: 2,
                ..Registry::default()
            }),
            changed: Notify::new(),
        });
        let groups = Arc::new(StubbornProcessGroups::default());
        let old_groups: Arc<dyn ProcessGroups> = groups.clone();
        let replacement_groups: Arc<dyn ProcessGroups> = groups.clone();
        let old = child_state(42, &service, old_groups);
        let replacement = child_state(42, &service, replacement_groups);
        lock_registry(&service)
            .managed
            .insert(42, Arc::clone(&replacement));

        old.release_active();

        old.terminate();
        tokio::task::yield_now().await;

        let registry = lock_registry(&service);
        assert_eq!(registry.active, 1);
        assert!(
            registry
                .managed
                .get(&42)
                .is_some_and(|current| Arc::ptr_eq(current, &replacement)),
            "retiring the old owner must preserve the replacement for a reused PID"
        );
        assert!(!groups.terminated.load(Ordering::Acquire));
        assert!(!groups.killed.load(Ordering::Acquire));
    }

    #[test]
    fn permission_denied_group_probe_still_reports_a_live_group() {
        assert!(group_probe_is_alive(Err(rustix::io::Errno::PERM)));
        assert!(!group_probe_is_alive(Err(rustix::io::Errno::SRCH)));
    }

    #[tokio::test(start_paused = true)]
    async fn unkillable_descendant_fails_within_a_bound_and_releases_active_capacity() {
        let groups = Arc::new(StubbornProcessGroups {
            alive: AtomicBool::new(true),
            ..StubbornProcessGroups::default()
        });
        let service = Service::with_groups(
            ProcessLocalConfig {
                maximum_active_processes: 1,
                maximum_capture_bytes: 4,
                shutdown_timeout_ms: 1_000,
            },
            groups.clone(),
        );
        let managed = service.spawn(immediate_process()).unwrap();

        let waiting = tokio::spawn({
            let managed = managed.clone();
            async move { managed.wait().await }
        });
        groups.wait_until_terminated().await;
        let result = tokio::time::timeout(Duration::from_secs(75), waiting)
            .await
            .map(|joined| joined.unwrap());
        groups.alive.store(false, Ordering::Release);
        assert!(
            matches!(result, Ok(Err(ProcessError::SettlementTimeout))),
            "unexpected wait result: {result:?}"
        );
        assert!(groups.terminated.load(Ordering::Acquire));
        assert!(groups.killed.load(Ordering::Acquire));

        let replacement = service.spawn(immediate_process()).unwrap();
        assert_eq!(replacement.wait().await.unwrap().exit_code, Some(0));
        drop((replacement, managed));
        assert!(service.shutdown().await.is_ok());
    }
}
