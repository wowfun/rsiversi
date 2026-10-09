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

mod duplex;
mod pty;

const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 10_000;
#[cfg(unix)]
mod output_cache;
#[cfg(unix)]
pub use output_cache::OutputCacheConfig;
#[cfg(unix)]
const POST_KILL_GROUP_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration for one local Process provider generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessLocalConfig {
    /// Optional best-effort completed-output cache.
    #[cfg(unix)]
    #[serde(default)]
    pub output_cache: Option<OutputCacheConfig>,
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
            #[cfg(unix)]
            output_cache: None,
            maximum_active_processes: default_maximum_active_processes(),
            maximum_capture_bytes: default_maximum_capture_bytes(),
            shutdown_timeout_ms: default_shutdown_timeout_ms(),
        }
    }
}

impl ProcessLocalConfig {
    fn validate(&self) -> Result<()> {
        #[cfg(unix)]
        if let Some(config) = &self.output_cache {
            config.validate()?;
        }
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
    cache: Option<Arc<output_cache::Cache>>,
    #[cfg(unix)]
    cache_failure: Option<ProcessError>,
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
    capture: Option<output_cache::Capture>,
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
#[derive(Debug)]
struct TailInner {
    bytes: Box<[u8]>,
    head: usize,
    length: usize,
    total: u64,
}

#[cfg(unix)]
impl TailInner {
    fn copy_from(&self, offset: usize) -> Vec<u8> {
        let length = self.length - offset;
        let start = (self.head + offset) % self.bytes.len();
        let first = length.min(self.bytes.len() - start);
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(&self.bytes[start..start + first]);
        bytes.extend_from_slice(&self.bytes[..length - first]);
        bytes
    }
}

#[cfg(unix)]
impl Tail {
    fn new(maximum: usize, reservation: Arc<CaptureReservation>) -> Self {
        assert!(maximum > 0, "capture tail requires a nonzero bound");
        Self {
            capture: None,
            maximum,
            inner: Mutex::new(TailInner {
                bytes: vec![0; maximum].into_boxed_slice(),
                head: 0,
                length: 0,
                total: 0,
            }),
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
        let suffix = &chunk[chunk.len().saturating_sub(self.maximum)..];
        let excess = inner
            .length
            .saturating_add(suffix.len())
            .saturating_sub(self.maximum);
        inner.head = (inner.head + excess) % self.maximum;
        inner.length -= excess;
        let start = (inner.head + inner.length) % self.maximum;
        let first = suffix.len().min(self.maximum - start);
        inner.bytes[start..start + first].copy_from_slice(&suffix[..first]);
        inner.bytes[..suffix.len() - first].copy_from_slice(&suffix[first..]);
        inner.length += suffix.len();
        if let Some(capture) = &self.capture {
            capture.push(chunk);
        }
        Ok(())
    }
}

#[cfg(unix)]
impl ProcessOutput for Tail {
    fn peek_tail(&self, maximum: usize) -> Result<ProcessRead> {
        if !(1..=32 * 1024).contains(&maximum) {
            return Err(ProcessError::InvalidInput(
                "invalid process peek bound".into(),
            ));
        }
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let length = inner.length.min(maximum);
        Ok(ProcessRead {
            bytes: inner.copy_from(inner.length - length),
            oldest_offset: inner.total - length as u64,
            next_offset: inner.total,
            lossy: inner.total > length as u64,
            full_output: self
                .capture
                .as_ref()
                .and_then(output_cache::Capture::reference),
        })
    }
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
        let retained = inner.length as u64;
        let oldest_offset = inner.total.saturating_sub(retained);
        let lossy = offset < oldest_offset;
        let start = usize::try_from(offset.max(oldest_offset).saturating_sub(oldest_offset))
            .map_err(|_| ProcessError::InvalidInput("process output offset is too large".into()))?;
        Ok(ProcessRead {
            bytes: inner.copy_from(start),
            oldest_offset,
            next_offset: inner.total,
            lossy,
            full_output: self
                .capture
                .as_ref()
                .and_then(output_cache::Capture::reference),
        })
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ChildState {
    plan_owner: Mutex<Option<rsi_sandbox::ProcessPlanOwner>>,
    pid: u32,
    grace: Duration,
    runtime: tokio::runtime::Handle,
    service: Weak<ServiceState>,
    groups: Arc<dyn ProcessGroups>,
    outcome: Mutex<Option<Completion>>,
    settled: Notify,
    active_released: AtomicBool,
    termination_started: AtomicBool,
    duplex_stop: Option<tokio_util::sync::CancellationToken>,
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct Completion {
    outcome: Result<ProcessOutcome>,
    settlement: Result<()>,
    provisional: bool,
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
    fn recovery_interrupted(&self) {
        if let Some(service) = self.service.upgrade() {
            lock_registry(&service).accepting = false;
        }
        self.signal_if_current(SignalTier::Kill);
        let mut current = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.is_none() {
            let error =
                ProcessError::Io("duplex supervisor recovery ended before settlement".into());
            *current = Some(Completion {
                outcome: Err(error.clone()),
                settlement: Err(error),
                provisional: true,
            });
        }
        drop(current);
        // This is an error receipt, not proof of reaping. Retain plan/registry ownership.
        self.settled.notify_waiters();
    }
    fn finish(&self, outcome: Result<ProcessOutcome>, settlement: Result<()>) {
        let mut current = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.as_ref().is_some_and(|receipt| !receipt.provisional) {
            return;
        }
        // Reaping and pipe settlement precede finish. Captured-output handles may
        // outlive this point but must not retain an execution admission permit.
        let owner = self
            .plan_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(owner);
        *current = Some(Completion {
            outcome,
            settlement,
            provisional: false,
        });
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
        if let Some(stop) = &self.duplex_stop {
            stop.cancel();
        }
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
        self.wait_completion().await.outcome
    }
    async fn wait_settlement(&self) -> Result<()> {
        self.wait_completion().await.settlement
    }
    async fn wait_completion(&self) -> Completion {
        loop {
            let notified = self.settled.notified();
            if let Some(completion) = self
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return completion;
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

#[cfg(unix)]
#[async_trait]
impl rsi_process::ProcessOutputCache for Service {
    async fn read(&self, id: &str, offset: u64, limit: usize) -> Result<rsi_process::OutputPage> {
        rsi_process::validate_output_read(id, limit)?;
        let cache = self.cache.as_ref().ok_or_else(|| {
            self.cache_failure
                .clone()
                .unwrap_or_else(|| ProcessError::Io("completed output cache is unavailable".into()))
        })?;
        cache.read(id, offset, limit).await
    }
}

#[async_trait::async_trait]

impl Process for Service {
    async fn spawn(&self, spec: ProcessSpec) -> Result<ManagedProcess> {
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
            cache: None,
            cache_failure: None,
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
        let (child, pid, capture_bytes) = self.admit_and_spawn(&spec, false)?;
        let reservation = Arc::new(CaptureReservation {
            service: Arc::downgrade(&self.state),
            bytes: capture_bytes,
        });
        let mut stdout = Tail::new(spec.stdout_max_bytes, Arc::clone(&reservation));
        let mut stderr = Tail::new(spec.stderr_max_bytes, reservation);
        if let Some(cache) = &self.cache {
            stdout.capture = cache.capture();
            stderr.capture = cache.capture();
        }
        let stdout = Arc::new(stdout);
        let stderr = Arc::new(stderr);
        let (state, published) = self.publish_child(
            pid,
            spec.termination_grace_ms,
            runtime,
            None,
            spec.process.owner.clone(),
        );
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
    fn publish_child(
        &self,
        pid: u32,
        grace: u64,
        runtime: &tokio::runtime::Handle,
        duplex_stop: Option<tokio_util::sync::CancellationToken>,
        plan_owner: Option<rsi_sandbox::ProcessPlanOwner>,
    ) -> (Arc<ChildState>, bool) {
        let state = Arc::new(ChildState {
            plan_owner: Mutex::new(plan_owner),
            pid,
            grace: Duration::from_millis(grace),
            runtime: runtime.clone(),
            service: Arc::downgrade(&self.state),
            groups: Arc::clone(&self.groups),
            outcome: Mutex::new(None),
            settled: Notify::new(),
            active_released: AtomicBool::new(false),
            termination_started: AtomicBool::new(false),
            duplex_stop,
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
        (state, published)
    }

    #[cfg(unix)]
    fn reserve_capture(&self, capture_bytes: usize) -> Result<()> {
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

        Ok(())
    }

    #[cfg(unix)]
    fn admit_and_spawn(
        &self,
        spec: &ProcessSpec,
        persistent_stdin: bool,
    ) -> Result<(tokio::process::Child, u32, usize)> {
        let capture_bytes = spec
            .capture_bytes()?
            .checked_add(if persistent_stdin {
                rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES
            } else {
                0
            })
            .ok_or(ProcessError::Capacity)?;
        self.reserve_capture(capture_bytes)?;

        let mut command = tokio::process::Command::new(&spec.process.program);
        command
            .args(&spec.process.arguments)
            .current_dir(&spec.process.cwd)
            .env_clear()
            .envs(spec.environment.iter().cloned())
            .stdin(if spec.stdin.is_empty() && !persistent_stdin {
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
        let mut processes = loop {
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
        processes.sort_by_key(|process| process.pid);
        for process in &processes {
            process.terminate();
        }
        let state = Arc::clone(&self.state);
        let cache = self.cache.clone();
        let mut cleanup = tokio::spawn(async move {
            let _state = state;
            let mut failure = None;
            for process in processes {
                if let Err(error) = process.wait_settlement().await {
                    failure.get_or_insert(error);
                }
            }
            if let Some(cache) = cache
                && let Err(error) = cache.shutdown().await
            {
                failure.get_or_insert(error);
            }
            failure.map_or(Ok(()), Err)
        });
        match tokio::time::timeout(
            Duration::from_millis(self.config.shutdown_timeout_ms),
            &mut cleanup,
        )
        .await
        {
            Ok(Ok(result)) => result,
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
    let stdout_task = runtime.spawn(drain(stdout_pipe, stdout.clone()));
    let stderr_task = runtime.spawn(drain(stderr_pipe, stderr.clone()));
    let wait_state = Arc::clone(state);
    let drain_grace = state.grace;
    runtime.spawn(async move {
        let status = reap_group(&mut child, &wait_state).await;
        if let Some(task) = stdin_task.as_mut() {
            if !task.is_finished() {
                task.abort();
            }
            let _ = task.await;
        }
        let drain_error = settle_drains(stdout_task, stderr_task, drain_grace).await;
        if drain_error.is_none() {
            // Cache publication has its own bound and cannot consume pipe-drain grace.
            tokio::join!(
                async {
                    if let Some(capture) = &stdout.capture {
                        capture.finish().await;
                    }
                },
                async {
                    if let Some(capture) = &stderr.capture {
                        capture.finish().await;
                    }
                },
            );
        } else {
            for capture in [&stdout.capture, &stderr.capture].into_iter().flatten() {
                capture.abandon();
            }
        }
        let settlement = status.as_ref().map(|_| ()).map_err(Clone::clone);
        let outcome = status.and_then(|status| {
            if let Some(error) = drain_error {
                return Err(ProcessError::Io(error));
            }
            Ok(status)
        });
        drop((stdout, stderr));
        wait_state.finish(outcome, settlement);
    });
}

#[cfg(unix)]
async fn reap_group(
    child: &mut tokio::process::Child,
    wait_state: &Arc<ChildState>,
) -> Result<ProcessOutcome> {
    let status = child
        .wait()
        .await
        .map_err(|error| ProcessError::Io(error.to_string()));
    let group_settlement_timed_out = if wait_state.group_is_alive() {
        wait_state.terminate();
        !wait_for_group_disappearance(
            wait_state,
            wait_state
                .grace
                .saturating_add(POST_KILL_GROUP_SETTLEMENT_TIMEOUT),
        )
        .await
    } else {
        false
    };
    if group_settlement_timed_out {
        return Err(ProcessError::SettlementTimeout);
    }
    status.map(|status| ProcessOutcome {
        exit_code: status.code(),
        signal: status.signal(),
    })
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
        let service = {
            let config = plan.take_state::<ProcessLocalConfig>()?;
            let cache_config = config.output_cache.clone();
            let cache = tokio::task::spawn_blocking(move || {
                cache_config.map(output_cache::Cache::open).transpose()
            })
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
            let mut service = Service::new(config);
            match cache {
                Ok(cache) => service.cache = cache,
                Err(error) => service.cache_failure = Some(error),
            }
            Arc::new(service)
        };
        #[cfg(not(unix))]
        let service = {
            let _: ProcessLocalConfig = plan.take_state()?;
            Arc::new(Service {})
        };
        let process: Arc<dyn Process> = service.clone();
        let supply = plan.context().provide_local::<ProcessContract>(process)?;
        let duplex: Arc<dyn rsi_process::DuplexProcess> = service.clone();
        let duplex_supply = plan
            .context()
            .provide_local::<rsi_process::DuplexProcessContract>(duplex)?;
        let pty: Arc<dyn rsi_process::PtyProcess> = service.clone();
        let pty_supply = plan
            .context()
            .provide_local::<rsi_process::PtyProcessContract>(pty)?;
        #[cfg(unix)]
        let cache_supply = if service.config.output_cache.is_some() {
            let cache: Arc<dyn rsi_process::ProcessOutputCache> = service.clone();
            Some(
                plan.context()
                    .provide_local::<rsi_process::ProcessOutputCacheContract>(cache)?,
            )
        } else {
            None
        };
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
                    drop(duplex_supply);
                    drop(pty_supply);
                    #[cfg(unix)]
                    drop(cache_supply);
                    result
                })
            }),
        )
    }
}

#[cfg(unix)]
async fn settle_drains(
    stdout: tokio::task::JoinHandle<Result<()>>,
    stderr: tokio::task::JoinHandle<Result<()>>,
    grace: Duration,
) -> Option<String> {
    DrainTasks::new(stdout, stderr).settle(grace).await
}

#[cfg(unix)]
type DrainResult = std::result::Result<Result<()>, tokio::task::JoinError>;

#[cfg(unix)]
struct DrainTasks {
    stdout: Option<tokio::task::JoinHandle<Result<()>>>,
    stderr: Option<tokio::task::JoinHandle<Result<()>>>,
    out: Option<DrainResult>,
    err: Option<DrainResult>,
}

#[cfg(unix)]
impl DrainTasks {
    fn new(
        stdout: tokio::task::JoinHandle<Result<()>>,
        stderr: tokio::task::JoinHandle<Result<()>>,
    ) -> Self {
        Self {
            stdout: Some(stdout),
            stderr: Some(stderr),
            out: None,
            err: None,
        }
    }
    // Borrowed joins stay with the supervisor owner if this wait is interrupted.
    async fn settle(&mut self, grace: Duration) -> Option<String> {
        let deadline = tokio::time::sleep(grace);
        tokio::pin!(deadline);
        let mut timed_out = false;
        while self.stdout.is_some() || self.stderr.is_some() {
            tokio::select! { biased;
                result = async { self.stdout.as_mut().expect("pending stdout").await }, if self.stdout.is_some() => {
                    self.out = Some(result); self.stdout = None;
                },
                result = async { self.stderr.as_mut().expect("pending stderr").await }, if self.stderr.is_some() => {
                    self.err = Some(result); self.stderr = None;
                },
                () = &mut deadline => {
                    timed_out = true;
                    if let Some(task) = &self.stdout { task.abort(); }
                    if let Some(task) = &self.stderr { task.abort(); }
                    // A task can finish between the deadline and abort. Its actual
                    // join result wins; an already joined task must never be polled twice.
                    if let Some(task) = &mut self.stdout { self.out = Some(task.await); self.stdout = None; }
                    if let Some(task) = &mut self.stderr { self.err = Some(task.await); self.stderr = None; }
                    break;
                }
            }
        }
        match (
            self.out.as_ref().expect("stdout joined"),
            self.err.as_ref().expect("stderr joined"),
        ) {
            (Ok(Ok(())), Ok(Ok(()))) => None,
            (out, err) => {
                let reason = if timed_out {
                    "pipe drain timed out before EOF"
                } else {
                    "pipe closed before clean EOF"
                };
                Some(format!(
                    "{reason}: stdout drain={out:?}, stderr drain={err:?}"
                ))
            }
        }
    }
}

#[cfg(unix)]
impl Drop for DrainTasks {
    fn drop(&mut self) {
        if let Some(task) = &self.stdout {
            task.abort();
        }
        if let Some(task) = &self.stderr {
            task.abort();
        }
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

    #[tokio::test(start_paused = true)]
    async fn drain_join_preserves_clean_completion_and_never_repolls_a_joined_task() {
        let out = tokio::spawn(async { Ok(()) });
        let err = tokio::spawn(async { Ok(()) });
        while !out.is_finished() || !err.is_finished() {
            tokio::task::yield_now().await;
        }
        assert!(settle_drains(out, err, Duration::ZERO).await.is_none());
        let out = tokio::spawn(async { Ok(()) });
        let err = tokio::spawn(std::future::pending::<Result<()>>());
        while !out.is_finished() {
            tokio::task::yield_now().await;
        }
        let error = settle_drains(out, err, Duration::from_millis(100))
            .await
            .unwrap();
        assert!(error.contains("stdout drain=Ok(Ok(()))"), "{error}");
        assert!(error.contains("Cancelled"), "{error}");
    }
    #[tokio::test(start_paused = true)]
    async fn drain_join_retains_io_failure_when_the_other_drain_times_out() {
        let out = tokio::spawn(async { Err(ProcessError::Io("fixture failure".into())) });
        let err = tokio::spawn(std::future::pending::<Result<()>>());
        let error = settle_drains(out, err, Duration::from_millis(100))
            .await
            .unwrap();
        assert!(error.contains("fixture failure"), "{error}");
        assert!(error.contains("Cancelled"), "{error}");
    }
    #[tokio::test(start_paused = true)]
    async fn interrupted_drain_wait_retains_completed_and_pending_join_ownership() {
        use std::{
            future::Future as _,
            task::{Context, Poll, Waker},
        };
        let out = tokio::spawn(async { Ok(()) });
        while !out.is_finished() {
            tokio::task::yield_now().await;
        }
        let err = tokio::spawn(std::future::pending::<Result<()>>());
        let abort = err.abort_handle();
        let mut drains = DrainTasks::new(out, err);
        {
            let mut wait = std::pin::pin!(drains.settle(Duration::from_secs(1)));
            assert!(matches!(
                wait.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
        }
        assert!(matches!(drains.out, Some(Ok(Ok(())))));
        assert!(drains.stdout.is_none());
        assert!(drains.stderr.is_some());
        abort.abort();
        let error = drains.settle(Duration::from_secs(1)).await.unwrap();
        assert!(error.contains("stdout drain=Ok(Ok(()))"), "{error}");
        assert!(error.contains("Cancelled"), "{error}");
        assert!(drains.stderr.is_none());
    }

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

    pub(super) fn immediate_process() -> ProcessSpec {
        let workspace = std::env::current_dir().unwrap().canonicalize().unwrap();
        ProcessSpec {
            process: ConfinedProcess {
                owner: None,
                stdio: rsi_sandbox::ProcessStdio::Pipes,
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
            plan_owner: Mutex::new(None),
            pid,
            grace: Duration::from_millis(1),
            runtime: tokio::runtime::Handle::current(),
            service: Arc::downgrade(service),
            groups,
            outcome: Mutex::new(None),
            settled: Notify::new(),
            active_released: AtomicBool::new(false),
            termination_started: AtomicBool::new(false),
            duplex_stop: None,
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
                ..ProcessLocalConfig::default()
            },
            groups.clone(),
        );
        let managed = service.spawn(immediate_process()).await.unwrap();

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

        let replacement = service.spawn(immediate_process()).await.unwrap();
        assert_eq!(replacement.wait().await.unwrap().exit_code, Some(0));
        drop((replacement, managed));
        assert!(service.shutdown().await.is_ok());
    }
    #[tokio::test(start_paused = true)]
    async fn duplex_resource_settlement_preserves_group_failure() {
        use rsi_process::DuplexProcessSpec;
        let groups = Arc::new(StubbornProcessGroups {
            alive: AtomicBool::new(true),
            ..StubbornProcessGroups::default()
        });
        let service = Service::with_groups(ProcessLocalConfig::default(), groups.clone());
        let managed = rsi_process::DuplexProcess::spawn(
            &service,
            DuplexProcessSpec {
                process: immediate_process().process,
                environment: vec![],
                stdout_buffer_bytes: 64,
                stderr_max_bytes: 64,
                termination_grace_ms: 1,
            },
        )
        .await
        .unwrap();
        groups.wait_until_terminated().await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(75), managed.wait_settlement())
                .await
                .unwrap(),
            Err(ProcessError::SettlementTimeout)
        );
        assert_eq!(managed.wait().await, Err(ProcessError::SettlementTimeout));
        groups.alive.store(false, Ordering::Release);
        service.shutdown().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn retirement_reports_the_failure_of_a_child_it_joins() {
        let groups = Arc::new(StubbornProcessGroups {
            alive: AtomicBool::new(true),
            ..StubbornProcessGroups::default()
        });
        let service = Service::with_groups(
            ProcessLocalConfig {
                shutdown_timeout_ms: 30_000,
                ..ProcessLocalConfig::default()
            },
            groups.clone(),
        );
        let managed = service.spawn(immediate_process()).await.unwrap();
        groups.wait_until_terminated().await;
        assert_eq!(
            service.shutdown().await,
            Err(ProcessError::SettlementTimeout)
        );
        assert_eq!(managed.wait().await, Err(ProcessError::SettlementTimeout));
        assert!(lock_registry(&service.state).managed.is_empty());
        groups.alive.store(false, Ordering::Release);
    }

    #[test]
    #[should_panic(expected = "capture tail requires a nonzero bound")]
    fn zero_capture_tail_is_rejected_at_construction() {
        let _tail = Tail::new(
            0,
            Arc::new(CaptureReservation {
                service: Weak::new(),
                bytes: 0,
            }),
        );
    }

    #[test]
    fn wrapped_tail_reads_requested_suffixes_and_preserves_offsets() {
        let tail = Tail::new(
            7,
            Arc::new(CaptureReservation {
                service: Weak::new(),
                bytes: 7,
            }),
        );
        assert!(tail.read_from(0).unwrap().bytes.is_empty());
        tail.push(b"0123456").unwrap();
        tail.push(b"78").unwrap();
        let expired = tail.read_from(0).unwrap();
        assert_eq!(expired.bytes, b"2345678");
        assert_eq!(expired.oldest_offset, 2);
        assert_eq!(expired.next_offset, 9);
        assert!(expired.lossy);
        for (offset, bytes) in [(4, b"45678".as_slice()), (7, b"78"), (9, b"")] {
            let read = tail.read_from(offset).unwrap();
            assert_eq!(read.bytes, bytes);
            assert!(!read.lossy);
        }
        let peek = tail.peek_tail(4).unwrap();
        assert_eq!(peek.bytes, b"5678");
        assert_eq!(peek.oldest_offset, 5);
        assert_eq!(peek.next_offset, 9);
        assert!(peek.lossy);
    }

    #[test]
    fn lossy_tail_reads_translate_absolute_offsets_before_copying_retained_bytes() {
        let stream: Vec<u8> = (0..127).collect();
        for maximum in [1, 7, 32] {
            let tail = Tail::new(
                maximum,
                Arc::new(CaptureReservation {
                    service: Weak::new(),
                    bytes: maximum,
                }),
            );
            for chunk in [
                &stream[..5],
                &stream[5..7],
                &[],
                &stream[7..52],
                &stream[52..],
            ] {
                tail.push(chunk).unwrap();
            }
            let oldest = stream.len() - maximum;
            for offset in 0..=stream.len() {
                let read = tail.read_from(offset as u64).unwrap();
                assert_eq!(read.bytes, stream[offset.max(oldest)..]);
                assert_eq!(read.oldest_offset, oldest as u64);
                assert_eq!(read.next_offset, stream.len() as u64);
                assert_eq!(read.lossy, offset < oldest);
            }
            assert!(matches!(
                tail.read_from(stream.len() as u64 + 1),
                Err(ProcessError::InvalidInput(_))
            ));
        }
    }
    #[test]
    fn tail_overflow_keeps_exact_storage_and_raw_offsets() {
        for maximum in [1, 7, 8192, 1_048_576] {
            let tail = Tail::new(
                maximum,
                Arc::new(CaptureReservation {
                    service: Weak::new(),
                    bytes: maximum,
                }),
            );
            let allocation = tail.inner.lock().unwrap().bytes.as_ptr();
            let mut all = Vec::new();
            for length in [maximum - 1, maximum, maximum + 1, 8192, 3] {
                let chunk: Vec<_> = (0..length)
                    .map(|i| u8::try_from(i % 256).unwrap())
                    .collect();
                all.extend_from_slice(&chunk);
                tail.push(&chunk).unwrap();
                let inner = tail.inner.lock().unwrap();
                assert_eq!(inner.bytes.len(), maximum);
                assert_eq!(inner.bytes.as_ptr(), allocation);
                drop(inner);
                let read = tail.read_from(0).unwrap();
                assert_eq!(read.bytes, all[all.len().saturating_sub(maximum)..]);
                assert_eq!(read.next_offset, all.len() as u64);
                assert_eq!(read.oldest_offset, all.len().saturating_sub(maximum) as u64);
            }
        }
    }
}
