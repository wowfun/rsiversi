//! `ConPTY` shares admission and Job settlement with native pipe processes.
use super::{Service, Slot, io_error, native, settle_job, spawn_error};
use rsi_process::{
    MAXIMUM_PTY_IO_BYTES, ManagedPtyProcess, ProcessError, ProcessOutcome, PtyControl,
    PtyProcessSpec, PtyRead, PtySize, Result,
};
use std::{
    fs::OpenOptions,
    sync::{Arc, Mutex, Weak, atomic::Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::{Notify, OwnedSemaphorePermit, mpsc},
};
use tokio_util::sync::CancellationToken;
const CHUNK: usize = 8192;
const QUEUE: usize = 8;
const DEADLINE: Duration = Duration::from_millis(500);
struct CreationDrain(CancellationToken, bool);
impl Drop for CreationDrain {
    fn drop(&mut self) {
        if !self.1 {
            self.0.cancel();
        }
    }
}

#[derive(Debug)]
pub(super) struct Control {
    pid: u32,
    job: Arc<native::Job>,
    console: Arc<native::Console>,
    input: tokio::sync::Mutex<NamedPipeServer>,
    output: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
    stop: CancellationToken,
    drain: CancellationToken,
    outcome: Mutex<Option<Result<ProcessOutcome>>>,
    changed: Notify,
    parent: Weak<Service>,
    id: u64,
    slot: Mutex<Option<Slot>>,
    plan: Mutex<Option<rsi_sandbox::ConfinedProcess>>,
    _capture: OwnedSemaphorePermit,
}
impl Control {
    pub(super) fn terminate(&self) {
        self.stop.cancel();
        self.drain.cancel();
        let _ = self.job.terminate();
    }
    fn finish(&self, outcome: Result<ProcessOutcome>, settled: bool) {
        if settled {
            self.plan.lock().expect("PTY plan").take();
            self.slot.lock().expect("PTY slot").take();
        }
        if let Some(parent) = self.parent.upgrade() {
            if settled {
                parent.ptys.lock().expect("PTY registry").remove(&self.id);
            } else {
                parent.accepting.store(false, Ordering::Release);
            }
            parent.changed.notify_waiters();
        }
        *self.outcome.lock().expect("PTY outcome") = Some(outcome);
        self.changed.notify_waiters();
    }
}
#[async_trait::async_trait]
impl PtyControl for Control {
    fn pid(&self) -> u32 {
        self.pid
    }
    fn terminate(&self) {
        self.terminate();
    }
    async fn read(&self) -> Result<PtyRead> {
        let mut output = self.output.try_lock().map_err(|_| ProcessError::Capacity)?;
        if let Some(bytes) = output.recv().await {
            Ok(PtyRead { bytes, eof: false })
        } else {
            self.wait().await?;
            Ok(PtyRead {
                bytes: vec![],
                eof: true,
            })
        }
    }
    async fn write(&self, bytes: &[u8]) -> Result<usize> {
        if bytes.len() > MAXIMUM_PTY_IO_BYTES {
            return Err(ProcessError::InvalidInput("PTY input exceeds limit".into()));
        }
        if self.stop.is_cancelled() {
            return Err(ProcessError::OutcomeUnknown);
        }
        let mut input = self.input.try_lock().map_err(|_| ProcessError::Capacity)?;
        tokio::select! { biased;
            () = self.stop.cancelled() => Err(ProcessError::OutcomeUnknown),
            result = tokio::time::timeout(DEADLINE, input.write(bytes)) => result.map_err(|_| ProcessError::OutcomeUnknown)?.map_err(io_error),
        }
    }
    async fn resize(&self, size: PtySize) -> Result<()> {
        size.validate()?;
        if self.stop.is_cancelled() {
            return Err(ProcessError::OutcomeUnknown);
        }
        let console = self.console.clone();
        tokio::task::spawn_blocking(move || console.resize(size.rows, size.columns))
            .await
            .map_err(|_| ProcessError::OutcomeUnknown)?
            .map_err(io_error)
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        loop {
            let changed = self.changed.notified();
            if let Some(outcome) = self.outcome.lock().expect("PTY outcome").clone() {
                return outcome;
            }
            changed.await;
        }
    }
}
impl Service {
    pub(crate) async fn spawn_pty(
        self: &Arc<Self>,
        spec: PtyProcessSpec,
    ) -> Result<ManagedPtyProcess> {
        spec.validate()?;
        if spec.process.stamp.backend != rsi_sandbox::SandboxBackend::Unconfined {
            return Err(ProcessError::Unsupported);
        }
        let service = self.clone();
        // Once admitted, waiter cancellation cannot interrupt native publication/cleanup.
        tokio::spawn(async move { service.start_pty(spec).await })
            .await
            .map_err(|_| ProcessError::OutcomeUnknown)?
    }
    async fn start_pty(self: &Arc<Self>, spec: PtyProcessSpec) -> Result<ManagedPtyProcess> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(ProcessError::ShuttingDown);
        }
        let slot = Slot {
            _permit: self
                .active
                .clone()
                .try_acquire_owned()
                .map_err(|_| ProcessError::Capacity)?,
            parent: Arc::downgrade(self),
        };
        let reservation = self
            .capture
            .clone()
            .try_acquire_many_owned(
                u32::try_from(CHUNK * (QUEUE + 2) + MAXIMUM_PTY_IO_BYTES)
                    .map_err(|_| ProcessError::Capacity)?,
            )
            .map_err(|_| ProcessError::Capacity)?;
        let job = Arc::new(native::Job::new().map_err(spawn_error)?);
        let (input, client_input) = pipe(false).await.map_err(spawn_error)?;
        let (output, client_output) = pipe(true).await.map_err(spawn_error)?;
        let (send, receive) = mpsc::channel(QUEUE);
        let drain = CancellationToken::new();
        let reader = tokio::spawn(drain_output(output, send, drain.clone()));
        let creation_job = job.clone();
        let creation_drain = drain.clone();
        let created = tokio::task::spawn_blocking(move || {
            let result = (|| {
                let console = Arc::new(native::Console::new(
                    &client_input,
                    &client_output,
                    spec.size.rows,
                    spec.size.columns,
                )?);
                let mut rollback = CreationDrain(creation_drain.clone(), false);
                let child = match console.spawn(
                    &spec.process.program,
                    &spec.process.arguments,
                    &spec.environment,
                    &spec.process.cwd,
                    &creation_job,
                ) {
                    Ok(child) => child,
                    Err(error) => {
                        creation_drain.cancel();
                        console.close();
                        return Err(error);
                    }
                };
                rollback.1 = true;
                Ok((console, child, spec.process))
            })();
            drop(client_input);
            drop(client_output);
            result
        })
        .await;
        let Ok(created) = created else {
            drain.cancel();
            let _ = reader.await;
            return Err(ProcessError::OutcomeUnknown);
        };
        let (console, child, plan) = match created {
            Ok(value) => value,
            Err(error) => {
                drain.cancel();
                let _ = reader.await;
                return Err(spawn_error(error));
            }
        };
        let id = self.serial.fetch_add(1, Ordering::Relaxed);
        let control = Arc::new(Control {
            pid: child.pid(),
            job,
            console,
            input: tokio::sync::Mutex::new(input),
            output: tokio::sync::Mutex::new(receive),
            stop: CancellationToken::new(),
            drain,
            outcome: Mutex::new(None),
            changed: Notify::new(),
            parent: Arc::downgrade(self),
            id,
            slot: Mutex::new(Some(slot)),
            plan: Mutex::new(Some(plan)),
            _capture: reservation,
        });
        self.ptys
            .lock()
            .expect("PTY registry")
            .insert(id, control.clone());
        if !self.accepting.load(Ordering::Acquire) {
            control.terminate();
        }
        tokio::spawn(supervise(control.clone(), child, reader));
        Ok(ManagedPtyProcess::new(control))
    }
}

#[cfg(test)]
mod tests;

async fn pipe(read: bool) -> std::io::Result<(NamedPipeServer, std::fs::File)> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
    let name = format!(
        r"\\.\pipe\rsi-conpty-{}-{}",
        std::process::id(),
        hex::encode(random)
    );
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .access_inbound(read)
        .access_outbound(!read)
        .in_buffer_size(u32::try_from(CHUNK).expect("fixed pipe buffer size"))
        .out_buffer_size(u32::try_from(CHUNK).expect("fixed pipe buffer size"))
        .create(&name)?;
    let client = OpenOptions::new().read(!read).write(read).open(&name)?;
    server.connect().await?;
    Ok((server, client))
}

async fn drain_output(
    mut output: NamedPipeServer,
    send: mpsc::Sender<Vec<u8>>,
    draining: CancellationToken,
) -> Result<bool> {
    let mut lost = false;
    loop {
        let mut bytes = vec![0; CHUNK];
        let count = match output.read(&mut bytes).await {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => 0,
            Err(error) => return Err(io_error(error)),
        };
        if count == 0 {
            return Ok(lost);
        }
        bytes.truncate(count);
        tokio::select! { biased;
            () = draining.cancelled() => lost = true,
            result = send.send(bytes) => if result.is_err() { draining.cancel(); lost = true; },
        }
    }
}
async fn supervise(
    state: Arc<Control>,
    child: native::ConsoleChild,
    reader: tokio::task::JoinHandle<Result<bool>>,
) {
    let status = tokio::task::spawn_blocking(move || child.wait())
        .await
        .map_err(|_| ProcessError::OutcomeUnknown)
        .and_then(|v| v.map_err(io_error));
    let terminated = state.drain.is_cancelled();
    state.stop.cancel();
    let settlement = settle_job(&state.job).await;
    let console = state.console.clone();
    let mut closing = tokio::task::spawn_blocking(move || console.close());
    // A caller that stops reading cannot deadlock console close. Keep
    // draining after this bounded grace, reporting lost normal output.
    let closed =
        if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), &mut closing).await {
            result
        } else {
            state.drain.cancel();
            closing.await
        }
        .map_err(|_| ProcessError::OutcomeUnknown);
    let reader = reader.await;
    let settled = status.is_ok() && settlement.is_ok() && closed.is_ok() && reader.is_ok();
    let output = reader
        .map_err(|_| ProcessError::OutcomeUnknown)
        .and_then(|v| v);
    let outcome = status.and_then(|code| {
        settlement.and(closed).and(output).and_then(|lost| {
            if lost && !terminated {
                Err(ProcessError::Io(
                    "PTY output did not drain before console closure".into(),
                ))
            } else {
                Ok(ProcessOutcome {
                    exit_code: Some(code),
                    signal: None,
                })
            }
        })
    });
    state.finish(outcome, settled);
}
