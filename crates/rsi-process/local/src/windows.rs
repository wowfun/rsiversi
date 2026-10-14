//! Windows pipe ownership; generic request validation stays with Process.
use super::ProcessLocalConfig;
use rsi_process::{
    ManagedProcess, ProcessControl, ProcessError, ProcessOutcome, ProcessOutput, ProcessRead,
    ProcessSpec, Result,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
};
mod duplex;
#[allow(unsafe_code)]
mod native;

#[derive(Debug)]
pub(super) struct Service {
    accepting: AtomicBool,
    serial: AtomicU64,
    active: Arc<Semaphore>,
    maximum_active: usize,
    capture: Arc<Semaphore>,
    children: Mutex<BTreeMap<u64, Arc<Child>>>,
    changed: Notify,
    timeout: Duration,
}
impl Service {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "The factory transfers its owned configuration into this generation."
    )]
    pub(super) fn new(config: ProcessLocalConfig) -> Arc<Self> {
        Arc::new(Self {
            accepting: AtomicBool::new(true),
            serial: AtomicU64::new(1),
            active: Arc::new(Semaphore::new(config.maximum_active_processes)),
            maximum_active: config.maximum_active_processes,
            capture: Arc::new(Semaphore::new(config.maximum_capture_bytes)),
            children: Mutex::new(BTreeMap::new()),
            changed: Notify::new(),
            timeout: Duration::from_millis(config.shutdown_timeout_ms),
        })
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Admission, suspended spawn, Job assignment and rollback share one ownership boundary."
    )]
    async fn start(
        self: &Arc<Self>,
        spec: ProcessSpec,
        capture_stdout: bool,
    ) -> Result<(tokio::process::Child, Arc<Child>, Vec<u8>, Duration)> {
        spec.validate()?;
        if spec
            .process
            .program
            .extension()
            .and_then(|v| v.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
            })
        {
            return Err(ProcessError::InvalidInput("Windows Process requires an explicit native executable; select cmd.exe to run batch files".into()));
        }
        if !spec.process.program.is_absolute()
            || !spec.process.cwd.is_absolute()
            || spec.process.stamp.backend != rsi_sandbox::SandboxBackend::Unconfined
        {
            return Err(ProcessError::InvalidInput(
                "Windows requires an exact native unconfined pipe plan".into(),
            ));
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Err(ProcessError::ShuttingDown);
        }
        let slot = self
            .active
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProcessError::Capacity)?;
        let slot = Slot {
            _permit: slot,
            parent: Arc::downgrade(self),
        };
        // Retained tail plus its active read buffer, reserved before native spawn.
        let reserved = spec
            .capture_bytes()?
            .checked_mul(2)
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or(ProcessError::Capacity)?;
        let reservation = Arc::new(
            self.capture
                .clone()
                .try_acquire_many_owned(reserved)
                .map_err(|_| ProcessError::Capacity)?,
        );
        let job = Arc::new(native::Job::new().map_err(spawn_error)?);
        let mut command = tokio::process::Command::new(&spec.process.program);
        command
            .args(&spec.process.arguments)
            .current_dir(&spec.process.cwd)
            .env_clear()
            .envs(spec.environment)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .creation_flags(0x0800_0004)
            .kill_on_drop(true);
        let mut process = command.spawn().map_err(spawn_error)?;
        let pid = process
            .id()
            .ok_or_else(|| ProcessError::Spawn("native child has no PID".into()))?;
        let setup = process
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("native process handle unavailable"))
            .and_then(|handle| job.assign(handle))
            .and_then(|()| native::resume_primary(pid));
        if let Err(error) = setup {
            // Cleanup owns the still-suspended/assigned child independently of a
            // cancelled spawn waiter, and keeps its admission until actual reap.
            let cleanup = tokio::spawn(async move {
                let _slot = slot;
                let _reservation = reservation;
                let _ = process.kill().await;
                let _ = process.wait().await;
                let _ = settle_job(&job).await;
            });
            let _ = cleanup.await;
            return Err(spawn_error(error));
        }
        let id = self.serial.fetch_add(1, Ordering::Relaxed);
        let stdout = Arc::new(Tail::new(
            if capture_stdout {
                spec.stdout_max_bytes
            } else {
                0
            },
            reservation.clone(),
        ));
        let stderr = Arc::new(Tail::new(spec.stderr_max_bytes, reservation));
        let child = Arc::new(Child {
            pid,
            job,
            stdout,
            stderr,
            outcome: Mutex::new(None),
            settlement: Mutex::new(None),
            settled: Notify::new(),
            slot: Mutex::new(Some(slot)),
            parent: Arc::downgrade(self),
            id,
            plan: Mutex::new(Some(spec.process)),
            stop: tokio_util::sync::CancellationToken::new(),
        });
        {
            let mut registry = self.children.lock().expect("Windows Process registry");
            registry.insert(id, child.clone());
            if !self.accepting.load(Ordering::Acquire) {
                child.terminate();
            }
        }
        Ok((
            process,
            child,
            spec.stdin,
            Duration::from_millis(spec.termination_grace_ms.max(500)),
        ))
    }
    pub(super) async fn spawn(self: &Arc<Self>, spec: ProcessSpec) -> Result<ManagedProcess> {
        let (mut process, child, input_bytes, grace) = self.start(spec, true).await?;
        let input = process
            .stdin
            .take()
            .expect("successful piped spawn retains stdin");
        let out = process
            .stdout
            .take()
            .expect("successful piped spawn retains stdout");
        let err = process
            .stderr
            .take()
            .expect("successful piped spawn retains stderr");
        let state = child.clone();
        tokio::spawn(async move {
            let mut input = input;
            let input_task = tokio::spawn(async move {
                let _ = input.write_all(&input_bytes).await;
                let _ = input.shutdown().await;
            });
            let out_task = tokio::spawn(drain(out, state.stdout.clone()));
            let err_task = tokio::spawn(drain(err, state.stderr.clone()));
            let status = process.wait().await.map_err(io_error);
            let settlement = settle_job(&state.job).await;
            input_task.abort();
            let _ = input_task.await;
            let pipes = settle_pipes(out_task, err_task, grace).await;
            let resources_settled = status.is_ok() && settlement.is_ok() && pipes.is_ok();
            let outcome = status.and_then(|status| {
                settlement.and(pipes).map(|()| ProcessOutcome {
                    exit_code: status.code(),
                    signal: None,
                })
            });
            state.finish(outcome, resources_settled);
        });
        Ok(ManagedProcess::new(child))
    }
    pub(super) async fn shutdown(&self) -> Result<()> {
        self.accepting.store(false, Ordering::Release);
        self.active.close();
        self.capture.close();
        for child in self
            .children
            .lock()
            .expect("Windows Process registry")
            .values()
        {
            child.terminate();
        }
        tokio::time::timeout(self.timeout, async {
            loop {
                let changed = self.changed.notified();
                    if self
                    .children
                    .lock()
                    .expect("Windows Process registry")
                        .is_empty() && self.active.available_permits() == self.maximum_active
                {
                    return;
                }
                    tokio::select! { () = changed => {}, () = tokio::time::sleep(Duration::from_millis(10)) => {} }
            }
        })
        .await
        .map_err(|_| ProcessError::ShutdownTimeout)
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        for child in self
            .children
            .get_mut()
            .expect("Windows Process registry")
            .values()
        {
            child.terminate();
        }
    }
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the native error."
)]
fn spawn_error(error: std::io::Error) -> ProcessError {
    ProcessError::Spawn(error.to_string())
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the native error."
)]
fn io_error(error: std::io::Error) -> ProcessError {
    ProcessError::Io(error.to_string())
}

#[derive(Debug)]
struct Child {
    pid: u32,
    job: Arc<native::Job>,
    stdout: Arc<Tail>,
    stderr: Arc<Tail>,
    outcome: Mutex<Option<Result<ProcessOutcome>>>,
    settlement: Mutex<Option<Result<()>>>,
    settled: Notify,
    slot: Mutex<Option<Slot>>,
    parent: Weak<Service>,
    id: u64,
    plan: Mutex<Option<rsi_sandbox::ConfinedProcess>>,
    stop: tokio_util::sync::CancellationToken,
}
impl Child {
    fn terminate(&self) {
        self.stop.cancel();
        let _ = self.job.terminate();
    }
    fn finish(&self, outcome: Result<ProcessOutcome>, resources_settled: bool) {
        *self.outcome.lock().expect("Windows child outcome") = Some(outcome);
        *self.settlement.lock().expect("Windows child settlement") = Some(if resources_settled {
            Ok(())
        } else {
            Err(ProcessError::OutcomeUnknown)
        });
        if resources_settled {
            self.plan.lock().expect("Windows child plan").take();
            self.slot.lock().expect("Windows child admission").take();
        }
        if let Some(parent) = self.parent.upgrade() {
            if resources_settled {
                parent
                    .children
                    .lock()
                    .expect("Windows Process registry")
                    .remove(&self.id);
            } else {
                parent.accepting.store(false, Ordering::Release);
            }
            parent.changed.notify_waiters();
        }
        self.settled.notify_waiters();
    }
}

#[derive(Debug)]
struct Slot {
    _permit: OwnedSemaphorePermit,
    parent: Weak<Service>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.upgrade() {
            parent.changed.notify_waiters();
        }
    }
}
#[async_trait::async_trait]
impl ProcessControl for Child {
    fn pid(&self) -> u32 {
        self.pid
    }
    fn stdout(&self) -> Arc<dyn ProcessOutput> {
        self.stdout.clone()
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        self.stderr.clone()
    }
    fn terminate(&self) {
        self.terminate();
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        loop {
            let notified = self.settled.notified();
            if let Some(outcome) = self.outcome.lock().expect("Windows child outcome").clone() {
                return outcome;
            }
            notified.await;
        }
    }
}
#[derive(Debug)]
struct Tail {
    maximum: usize,
    data: Mutex<(VecDeque<u8>, u64)>,
    reservation: Arc<OwnedSemaphorePermit>,
}
impl Tail {
    fn new(maximum: usize, reservation: Arc<OwnedSemaphorePermit>) -> Self {
        Self {
            maximum,
            data: Mutex::new((VecDeque::with_capacity(maximum), 0)),
            reservation,
        }
    }
    fn push(&self, bytes: &[u8]) {
        let mut data = self.data.lock().expect("Windows process tail");
        data.1 = data.1.saturating_add(bytes.len() as u64);
        let remove = data
            .0
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(self.maximum)
            .min(data.0.len());
        data.0.drain(..remove);
        data.0.extend(
            bytes
                .iter()
                .skip(bytes.len().saturating_sub(self.maximum))
                .copied(),
        );
    }
}
impl ProcessOutput for Tail {
    fn read_from(&self, offset: u64) -> Result<ProcessRead> {
        let data = self.data.lock().expect("Windows process tail");
        let oldest = data.1 - data.0.len() as u64;
        if offset > data.1 {
            return Err(ProcessError::InvalidInput(
                "output offset exceeds stream".into(),
            ));
        }
        Ok(ProcessRead {
            bytes: data
                .0
                .iter()
                .skip(
                    usize::try_from(offset.saturating_sub(oldest))
                        .expect("offset lies within the bounded retained tail"),
                )
                .copied()
                .collect(),
            oldest_offset: oldest,
            next_offset: data.1,
            lossy: offset < oldest,
            full_output: None,
        })
    }
    fn peek_tail(&self, maximum: usize) -> Result<ProcessRead> {
        if !(1..=32768).contains(&maximum) {
            return Err(ProcessError::InvalidInput("tail peek exceeds bound".into()));
        }
        let data = self.data.lock().expect("Windows process tail");
        let start = data.0.len().saturating_sub(maximum);
        Ok(ProcessRead {
            bytes: data.0.iter().skip(start).copied().collect(),
            oldest_offset: data.1 - (data.0.len() - start) as u64,
            next_offset: data.1,
            lossy: data.1 > (data.0.len() - start) as u64,
            full_output: None,
        })
    }
}
async fn drain(mut stream: impl AsyncRead + Unpin, tail: Arc<Tail>) -> Result<()> {
    let mut buffer = vec![0; tail.maximum.min(65536)];
    loop {
        let read = stream.read(&mut buffer).await.map_err(io_error)?;
        if read == 0 {
            return Ok(());
        }
        tail.push(&buffer[..read]);
    }
}
async fn settle_job(job: &native::Job) -> Result<()> {
    if job.active().map_err(io_error)? != 0 {
        job.terminate().map_err(io_error)?;
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if job.active().map_err(io_error)? == 0 {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| ProcessError::SettlementTimeout)?
}
async fn settle_pipes(
    mut out: tokio::task::JoinHandle<Result<()>>,
    mut err: tokio::task::JoinHandle<Result<()>>,
    grace: Duration,
) -> Result<()> {
    let result = tokio::time::timeout(grace, async {
        let (out, err) = tokio::join!(&mut out, &mut err);
        out.map_err(|error| ProcessError::Io(error.to_string()))??;
        err.map_err(|error| ProcessError::Io(error.to_string()))??;
        Ok(())
    })
    .await;
    if let Ok(result) = result {
        return result;
    }
    if !out.is_finished() {
        out.abort();
        let _ = out.await;
    }
    if !err.is_finished() {
        err.abort();
        let _ = err.await;
    }
    Err(ProcessError::Io("Windows pipe drain timed out".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_sandbox::{
        ConfinedProcess, EnforcementStamp, ProcessStdio, SandboxBackend, SandboxFileSystem,
        SandboxMode, SandboxNetwork, SandboxScratch,
    };
    fn specification(
        workspace: &std::path::Path,
        program: &str,
        arguments: &[&str],
    ) -> ProcessSpec {
        ProcessSpec {
            process: ConfinedProcess {
                owner: None,
                stdio: ProcessStdio::Pipes,
                program: program.into(),
                arguments: arguments.iter().map(std::ffi::OsString::from).collect(),
                cwd: workspace.to_owned(),
                stamp: EnforcementStamp {
                    requested: SandboxMode::DangerFullAccess,
                    backend: SandboxBackend::Unconfined,
                    workspace: workspace.to_owned(),
                    filesystem: SandboxFileSystem::Unconfined,
                    scratch: SandboxScratch::Host,
                    network: SandboxNetwork::Host,
                },
            },
            stdin: vec![],
            environment: vec![],
            stdout_max_bytes: 8,
            stderr_max_bytes: 8,
            termination_grace_ms: 1000,
        }
    }
    #[tokio::test]
    async fn implicit_batch_shell_refuses_before_spawn_or_admission() {
        let temporary = tempfile::tempdir().unwrap();
        let service = Service::new(ProcessLocalConfig::default());
        let script = temporary.path().join("implicit.CMD");
        std::fs::write(&script, "echo invoked>marker").unwrap();
        assert!(matches!(
            service
                .spawn(specification(
                    temporary.path(),
                    script.to_str().unwrap(),
                    &[]
                ))
                .await,
            Err(ProcessError::InvalidInput(_))
        ));
        assert!(!temporary.path().join("marker").exists());
        assert_eq!(service.active.available_permits(), service.maximum_active);
        service.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn native_windows_job_captures_exact_tails_and_releases_active_admission_after_reap() {
        let temporary = tempfile::tempdir().unwrap();
        let service = Service::new(ProcessLocalConfig {
            maximum_active_processes: 1,
            ..ProcessLocalConfig::default()
        });
        let process = service
            .spawn(specification(
                temporary.path(),
                r"C:\Windows\System32\cmd.exe",
                &["/d", "/c", "echo 1234567890 & echo abcdefghij 1>&2"],
            ))
            .await
            .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(10), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.exit_code, Some(0));
        let output = process.stdout().read_from(0).unwrap();
        assert!(output.lossy);
        assert_eq!(output.bytes.len(), 8);
        assert_eq!(output.next_offset - output.oldest_offset, 8);
        assert!(process.stderr().read_from(0).unwrap().lossy);
        let second = service
            .spawn(specification(
                temporary.path(),
                r"C:\Windows\System32\cmd.exe",
                &["/d", "/c", "exit 7"],
            ))
            .await
            .unwrap();
        assert_eq!(second.wait().await.unwrap().exit_code, Some(7));
        service.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn termination_closes_native_descendants_and_a_cancelled_waiter_does_not_own_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let service = Service::new(ProcessLocalConfig::default());
        let script = r"$child=Start-Process -PassThru -WindowStyle Hidden -FilePath 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe' -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30'; Write-Output $child.Id; Start-Sleep -Seconds 30";
        let process = service
            .spawn(specification(
                temporary.path(),
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                &["-NoProfile", "-NonInteractive", "-Command", script],
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let active = service
                    .children
                    .lock()
                    .unwrap()
                    .values()
                    .next()
                    .unwrap()
                    .job
                    .active()
                    .unwrap();
                if active >= 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), process.wait())
                .await
                .is_err()
        );
        service.shutdown().await.unwrap();
        assert!(process.wait().await.unwrap().exit_code.is_some());
        assert!(service.children.lock().unwrap().is_empty());
    }
}
