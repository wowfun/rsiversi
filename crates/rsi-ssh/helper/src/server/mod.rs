use rsi_execution_local::NativeCapabilities;
use rsi_process::ProcessError;
use rsi_sandbox::{ConfinedProcess, SandboxError};
use rsi_ssh_protocol::{
    execution::{Preparation, Prepared, Program},
    rpc::{Failure, Reply, Request},
};
use rsi_ssh_transport::{
    Connection, Control, Incoming, IncomingRequest, RequestKind, Role, SendStream, StreamId,
};
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::watch,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
mod files;
mod output;
mod process;
use process::Native;
const PROCESSES: usize = 20;
type Result<T> = std::result::Result<T, Failure>;
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
struct Plan {
    process: ConfinedProcess,
    environment: Vec<(OsString, OsString)>,
    output: SendStream,
    error: Option<SendStream>,
}
struct Entry {
    prepared: Prepared,
    plan: Mutex<Option<Plan>>,
    native: Mutex<Option<Native>>,
    outcome: watch::Sender<Option<Result<rsi_ssh_protocol::rpc::Outcome>>>,
    settlement: watch::Sender<Option<Result<()>>>,
    cancel: CancellationToken,
    terminated: std::sync::atomic::AtomicBool,
    task: Mutex<Option<JoinHandle<()>>>,
}
impl Entry {
    fn terminate(&self) {
        self.terminated
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(native) = &*lock(&self.native) {
            native.terminate();
        }
    }
    fn is_terminated(&self) -> bool {
        self.terminated.load(std::sync::atomic::Ordering::Acquire)
    }
}
#[derive(Default)]
struct Registry {
    generation: u64,
    entries: BTreeMap<u64, Arc<Entry>>,
    occupied: [bool; PROCESSES],
}
/// One target connection's bounded native execution owner.
/// Construction does not grant product access or replace lifecycle prerequisite checks.
pub struct ExecutionServer {
    capabilities: NativeCapabilities,
    programs: BTreeMap<String, Program>,
    configured: tokio::sync::Mutex<Vec<(rsi_ssh_protocol::initialization::ProgramPolicy, Program)>>,
}
impl std::fmt::Debug for ExecutionServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionServer")
            .field("program_count", &self.programs.len())
            .finish_non_exhaustive()
    }
}
impl ExecutionServer {
    /// Checks a finite target catalog before accepting any connection request.
    pub async fn new(
        capabilities: NativeCapabilities,
        mut programs: BTreeMap<String, Program>,
    ) -> rsi_process::Result<Self> {
        if programs.len() > 128 {
            return Err(ProcessError::Capacity);
        }
        let mut environment_bytes = 0usize;
        for (selector, program) in &mut programs {
            rsi_ssh_protocol::execution::validate_selector(selector)
                .map_err(|_| ProcessError::InvalidInput("invalid target selector".into()))?;
            program
                .validate()
                .map_err(|_| ProcessError::InvalidInput("invalid target program policy".into()))?;
            environment_bytes = environment_bytes.saturating_add(
                program
                    .environment
                    .iter()
                    .map(|(key, value)| key.len() + value.len() + 2)
                    .sum::<usize>(),
            );
            if environment_bytes > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_BYTES {
                return Err(ProcessError::Capacity);
            }
            let path = tokio::fs::canonicalize(&program.program)
                .await
                .map_err(|_| ProcessError::Unsupported)?;
            let metadata = tokio::fs::metadata(&path)
                .await
                .map_err(|_| ProcessError::Unsupported)?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(ProcessError::Unsupported);
            }
            program.program = path
                .into_os_string()
                .into_string()
                .map_err(|_| ProcessError::Unsupported)?;
            program.validate().map_err(|_| ProcessError::Unsupported)?;
        }
        Ok(Self {
            capabilities,
            programs,
            configured: tokio::sync::Mutex::default(),
        })
    }
    /// Serves one helper-role transport and retains accepted effects through cleanup.
    /// Closing the connection joins handlers and native process owners before returning.
    pub async fn serve(
        self,
        connection: Connection,
        mut incoming: Incoming,
    ) -> rsi_process::Result<()> {
        if connection.role() != Role::Helper {
            return Err(ProcessError::InvalidInput("helper role required".into()));
        }
        let shared = Arc::new(Server {
            owner: self,
            connection,
            registry: Mutex::new(Registry::default()),
            files: files::registry(),
            stop: CancellationToken::new(),
        });
        let mut handlers = JoinSet::new();
        loop {
            tokio::select! { biased;
                () = shared.connection.closed() => break,
                Some(result) = handlers.join_next(), if !handlers.is_empty() => {
                    if result.is_err() { shared.connection.close(); }
                }
                request = incoming.next() => {
                    let Some(request) = request else { break; };
                    // Request slots remain charged by IncomingRequest through reply publication.
                    let server = shared.clone();
                    handlers.spawn(async move { server.handle(request).await; });
                }
            }
        }
        shared.connection.close();
        shared.stop.cancel();
        shared.cancel_all();
        while handlers.join_next().await.is_some() {}
        shared.close_files();
        // A confine/spawn admitted before close may have published while handlers drained.
        shared.cancel_all();
        let entries = std::mem::take(&mut lock(&shared.registry).entries);
        for entry in entries.into_values() {
            let task = lock(&entry.task).take();
            if let Some(task) = task {
                let _ = task.await;
            }
        }
        shared.connection.settled().await;
        Ok(())
    }
}
struct Server {
    owner: ExecutionServer,
    connection: Connection,
    registry: Mutex<Registry>,
    files: Mutex<files::FileRegistry>,
    stop: CancellationToken,
}
impl Server {
    fn cancel_all(&self) {
        for entry in lock(&self.registry).entries.values() {
            entry.cancel.cancel();
            entry.terminate();
        }
    }
    async fn handle(self: Arc<Self>, request: IncomingRequest) {
        let result = match request.kind() {
            RequestKind::Control(control) => self.control(control).await,
            RequestKind::Ordinary => match serde_json::from_slice::<Request>(request.payload()) {
                Ok(value) => self.ordinary(value).await,
                Err(_) => Err(Failure::Invalid),
            },
        };
        let reply = result.unwrap_or_else(|failure| Reply::Failed { failure });
        match serde_json::to_vec(&reply) {
            Ok(bytes) => {
                let _ = request.reply(bytes);
            }
            Err(_) => self.connection.close(),
        }
    }
    async fn ordinary(self: &Arc<Self>, request: Request) -> Result<Reply> {
        match request {
            Request::Initialize { .. } => Err(Failure::Invalid),
            Request::WorkspaceRead {
                mode,
                cwd,
                workspace,
            } => {
                rsi_ssh_protocol::execution::validate_path(&cwd).map_err(|_| Failure::Invalid)?;
                rsi_ssh_protocol::execution::validate_path(&workspace)
                    .map_err(|_| Failure::Invalid)?;
                self.owner
                    .capabilities
                    .sandbox
                    .workspace_read(rsi_sandbox::WorkspaceReadRequest {
                        mode,
                        cwd: cwd.into(),
                        workspace: workspace.into(),
                    })
                    .await
                    .map_err(|error| sandbox_failure(&error))?;
                Ok(Reply::Done)
            }
            Request::FilesOpen {
                workspace,
                path,
                kind,
            } => Ok(self.files_open(workspace, path, kind).await),
            Request::FilesRead {
                handle,
                offset,
                maximum,
            } => Ok(self.files_read(handle, offset, maximum).await),
            Request::FilesList {
                handle,
                offset,
                maximum,
            } => Ok(self.files_list(handle, offset, maximum).await),
            Request::Canonicalize { path } => Self::canonicalize(&path).await,
            Request::Resolve { selector } => {
                rsi_ssh_protocol::execution::validate_selector(&selector)
                    .map_err(|_| Failure::Invalid)?;
                Ok(Reply::Program {
                    program: self
                        .owner
                        .programs
                        .get(&selector)
                        .cloned()
                        .ok_or(Failure::Unsupported)?,
                })
            }
            Request::ResolveConfigured { policy } => self.resolve_configured(policy).await,
            Request::Prepare { preparation } => self.prepare(preparation).await,
            Request::Start { handle, options } => self.start(handle, options).await,
            Request::Write { handle, bytes } => {
                if bytes.is_empty() || bytes.len() > rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES {
                    return Err(Failure::Invalid);
                }
                let entry = self.entry(handle)?;
                if entry.is_terminated() {
                    return Err(Failure::Closed);
                }
                let native = lock(&entry.native).clone().ok_or(Failure::Invalid)?;
                let bytes = native.write(&bytes).await?;
                Ok(Reply::Written { bytes })
            }
            Request::CloseInput { handle } => {
                let native = lock(&self.entry(handle)?.native)
                    .clone()
                    .ok_or(Failure::Invalid)?;
                native.close().await?;
                Ok(Reply::Done)
            }
            Request::Status { handle } => {
                let entry = self.entry(handle)?;
                Ok(wait_status(entry.outcome.subscribe(), entry.settlement.subscribe()).await)
            }
            Request::Release { handle } => {
                let entry = self.entry(handle)?;
                if entry.outcome.borrow().is_none() || *entry.settlement.borrow() != Some(Ok(())) {
                    return Err(Failure::Invalid);
                }
                self.release(handle, &entry).await;
                Ok(Reply::Done)
            }
        }
    }
    async fn canonicalize(path: &str) -> Result<Reply> {
        rsi_ssh_protocol::execution::validate_path(path).map_err(|_| Failure::Invalid)?;
        let path = tokio::fs::canonicalize(path)
            .await
            .map_err(|_| Failure::Io)?;
        if !tokio::fs::metadata(&path)
            .await
            .map_err(|_| Failure::Io)?
            .is_dir()
        {
            return Err(Failure::Invalid);
        }
        let path = path
            .into_os_string()
            .into_string()
            .map_err(|_| Failure::Invalid)?;
        rsi_ssh_protocol::execution::validate_path(&path).map_err(|_| Failure::Invalid)?;
        Ok(Reply::Path { path })
    }
    fn entry(&self, handle: u64) -> Result<Arc<Entry>> {
        lock(&self.registry)
            .entries
            .get(&handle)
            .cloned()
            .ok_or(Failure::Invalid)
    }
    async fn control(&self, control: Control) -> Result<Reply> {
        match control {
            Control::Terminate { process } => {
                let entry = self.entry(process)?;
                entry.terminate();
                // Prepared but unconsumed plans have no native work to settle.
                if lock(&entry.plan).take().is_some() {
                    self.release(process, &entry).await;
                } else if lock(&entry.native).is_none() {
                    entry.cancel.cancel();
                }
                Ok(Reply::Done)
            }
            Control::Resize {
                process,
                columns,
                rows,
            } => {
                let entry = self.entry(process)?;
                if entry.is_terminated() {
                    return Err(Failure::Closed);
                }
                let native = lock(&entry.native).clone().ok_or(Failure::Invalid)?;
                native
                    .resize(rsi_process::PtySize { columns, rows })
                    .await?;
                Ok(Reply::Done)
            }
            Control::FilesRelease { handle } => Ok(self.files_release(handle)),
        }
    }
    async fn resolve_configured(
        &self,
        policy: rsi_ssh_protocol::initialization::ProgramPolicy,
    ) -> Result<Reply> {
        policy.validate().map_err(|_| Failure::Invalid)?;
        {
            let catalog = self.owner.configured.lock().await;
            if let Some((_, program)) = catalog.iter().find(|(selected, _)| selected == &policy) {
                return Ok(Reply::Program {
                    program: program.clone(),
                });
            }
            if catalog.len() >= 128 {
                return Err(Failure::Capacity);
            }
        }
        let program = crate::native::configured_program(&policy)
            .await
            .map_err(|_| Failure::Unsupported)?
            .ok_or(Failure::Unsupported)?;
        let mut catalog = self.owner.configured.lock().await;
        if let Some((_, program)) = catalog.iter().find(|(selected, _)| selected == &policy) {
            return Ok(Reply::Program {
                program: program.clone(),
            });
        }
        let environment_bytes = |rows: &[(String, String)]| {
            rows.iter()
                .map(|(key, value)| key.len() + value.len() + 2)
                .sum::<usize>()
        };
        let retained: usize = catalog
            .iter()
            .map(|(policy, program)| {
                policy.command.len()
                    + program.program.len()
                    + environment_bytes(&policy.environment)
                    + environment_bytes(&program.environment)
            })
            .sum();
        let selected = policy.command.len() + environment_bytes(&policy.environment);
        if catalog.len() >= 128
            || retained.saturating_add(selected) > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_BYTES
        {
            return Err(Failure::Capacity);
        }
        let total = retained
            .saturating_add(selected)
            .saturating_add(program.program.len())
            .saturating_add(environment_bytes(&program.environment));
        if total > rsi_process::MAXIMUM_PROCESS_ENVIRONMENT_BYTES {
            return Err(Failure::Capacity);
        }
        catalog.push((policy, program.clone()));
        Ok(Reply::Program { program })
    }
    async fn prepare(&self, preparation: Preparation) -> Result<Reply> {
        preparation.validate().map_err(|_| Failure::Invalid)?;
        let matches = |program: &Program| {
            program.program == preparation.program && program.environment == preparation.environment
        };
        if !self.owner.programs.values().any(matches)
            && !self
                .owner
                .configured
                .lock()
                .await
                .iter()
                .any(|(_, program)| matches(program))
        {
            return Err(Failure::Invalid);
        }
        let source_reader = preparation.source_reader;
        let pty = preparation.pty;
        let (request, environment) = preparation.into_native().map_err(|_| Failure::Invalid)?;
        let sandbox = &self.owner.capabilities.sandbox;
        let process = if source_reader {
            sandbox.confine_source_reader(request).await
        } else {
            sandbox.confine(request).await
        }
        .map_err(|error| sandbox_failure(&error))?;
        if pty {
            rsi_process::PtyProcessSpec {
                process: process.clone(),
                environment: environment.clone(),
                size: rsi_process::PtySize {
                    columns: 80,
                    rows: 24,
                },
                termination_grace_ms: 2000,
            }
            .validate()
            .map_err(Failure::from)?;
        }
        let mut registry = lock(&self.registry);
        let slot = registry
            .occupied
            .iter()
            .position(|used| !used)
            .ok_or(Failure::Capacity)?;
        let generation = registry
            .generation
            .checked_add(1)
            .filter(|v| *v < (u64::MAX >> 6))
            .ok_or(Failure::Capacity)?;
        registry.generation = generation;
        let first = u8::try_from(slot * 3).map_err(|_| Failure::Capacity)?;
        let stdin = StreamId::new(first, generation).map_err(transport_failure)?;
        let stdout = StreamId::new(first + 1, generation).map_err(transport_failure)?;
        let stderr = (!pty)
            .then(|| StreamId::new(first + 2, generation))
            .transpose()
            .map_err(transport_failure)?;
        let output = self
            .connection
            .open_sender(stdout)
            .map_err(transport_failure)?;
        let error = match stderr.map(|id| self.connection.open_sender(id)).transpose() {
            Ok(stream) => stream,
            Err(error) => {
                let _ = self.connection.retire_stream(stdout);
                return Err(transport_failure(error));
            }
        };
        let prepared = Prepared {
            handle: stdin.raw(),
            enforcement: process.stamp.clone(),
            stdin: stdin.raw(),
            stdout: stdout.raw(),
            stderr: stderr.map(StreamId::raw),
        };
        let (outcome, _) = watch::channel(None);
        let (settlement, _) = watch::channel(None);
        registry.occupied[slot] = true;
        registry.entries.insert(
            prepared.handle,
            Arc::new(Entry {
                prepared: prepared.clone(),
                plan: Mutex::new(Some(Plan {
                    process,
                    environment,
                    output,
                    error,
                })),
                native: Mutex::new(None),
                outcome,
                settlement,
                cancel: CancellationToken::new(),
                terminated: std::sync::atomic::AtomicBool::new(false),
                task: Mutex::new(None),
            }),
        );
        Ok(Reply::Prepared { prepared })
    }
    async fn release(&self, handle: u64, entry: &Arc<Entry>) {
        entry.cancel.cancel();
        let task = lock(&entry.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let mut registry = lock(&self.registry);
        if registry
            .entries
            .get(&handle)
            .is_some_and(|actual| Arc::ptr_eq(actual, entry))
        {
            for id in [
                Some(entry.prepared.stdin),
                Some(entry.prepared.stdout),
                entry.prepared.stderr,
            ]
            .into_iter()
            .flatten()
            {
                if let Ok(id) = StreamId::from_raw(id) {
                    let _ = self.connection.retire_stream(id);
                }
            }
            registry.entries.remove(&handle);
            registry.occupied[((entry.prepared.stdin & 63) / 3) as usize] = false;
        }
    }
}
fn sandbox_failure(error: &SandboxError) -> Failure {
    match error {
        SandboxError::Unsupported(_) => Failure::Unsupported,
        SandboxError::InvalidInput(_) => Failure::Invalid,
        SandboxError::Probe(_) => Failure::Io,
    }
}
fn transport_failure(error: rsi_ssh_transport::Error) -> Failure {
    match error {
        rsi_ssh_transport::Error::Closed => Failure::Closed,
        rsi_ssh_transport::Error::OutcomeUnknown => Failure::OutcomeUnknown,
        rsi_ssh_transport::Error::Capacity => Failure::Capacity,
        rsi_ssh_transport::Error::Invalid => Failure::Invalid,
    }
}

async fn wait_status(
    mut outcome: tokio::sync::watch::Receiver<
        Option<std::result::Result<rsi_ssh_protocol::rpc::Outcome, Failure>>,
    >,
    mut settlement: tokio::sync::watch::Receiver<Option<std::result::Result<(), Failure>>>,
) -> Reply {
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
    tokio::pin!(deadline);
    loop {
        let result = outcome.borrow_and_update().clone();
        let settled = *settlement.borrow_and_update();
        if result.is_some() && settled.is_some() {
            return Reply::Status {
                outcome: result,
                settlement: settled,
            };
        }
        tokio::select! {
            () = &mut deadline => break,
            result = outcome.changed() => if result.is_err() { break; },
            result = settlement.changed() => if result.is_err() { break; },
        }
    }
    Reply::Status {
        outcome: outcome.borrow().clone(),
        settlement: *settlement.borrow(),
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn pending_status_waits_until_the_bound_then_returns_a_snapshot() {
        let (_outcome, outcome) = watch::channel(None);
        let (_settlement, settlement) = watch::channel(None);
        let start = tokio::time::Instant::now();
        assert!(matches!(
            wait_status(outcome, settlement).await,
            Reply::Status {
                outcome: None,
                settlement: None
            }
        ));
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(5));
    }
    #[tokio::test(start_paused = true)]
    async fn status_waits_for_both_native_facts_and_wakes_immediately_on_settlement() {
        let (outcome, reader) = watch::channel(None);
        let (settlement, settled) = watch::channel(None);
        let task = tokio::spawn(wait_status(reader, settled));
        tokio::task::yield_now().await;
        outcome.send_replace(Some(Err(Failure::Io)));
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        let start = tokio::time::Instant::now();
        settlement.send_replace(Some(Ok(())));
        assert!(matches!(
            task.await.unwrap(),
            Reply::Status {
                outcome: Some(Err(Failure::Io)),
                settlement: Some(Ok(()))
            }
        ));
        assert_eq!(start.elapsed(), std::time::Duration::ZERO);
    }
}
