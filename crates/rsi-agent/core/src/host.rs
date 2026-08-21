use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::num::NonZeroU8;
use std::sync::Arc;
use std::time::Duration;

use rsi_meta::{CompositionHost, InstanceId};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::{Id as TaskId, JoinSet};

use crate::adapter::{CompositionPortFactory, PortFactory};
use crate::ai_operations;
use crate::error::StoreErrorClass;
use crate::persistence::{ColdReader, HealthLatch, ProbeSession, WriterHandle};
use crate::{
    AgentError, AgentImageOutput, AgentRealtimeSession, AgentSpeechOutput,
    AgentTranscriptionOutput, AgentWorkspace, AiOperationId, ArtifactRef, ArtifactStore, Result,
    RunRecord, RunRequest, SessionId, Transcript,
};
use rsi_ai_protocol::{
    ImageRequest, MediaKind, RealtimeRequest, SpeechRequest, TranscriptionRequest,
};

const DEFAULT_MAX_CONCURRENT_RUNS: NonZeroU8 = NonZeroU8::new(8).unwrap();
const ADMITTED_RUN_FACTOR: usize = 4;

/// Host-wide deadlines applied to provider operations.
///
/// These limits are intentionally configured per host rather than per run so
/// callers joining one durable session cannot disagree about its execution
/// policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionLimits {
    handshake: Duration,
    model_response: Duration,
    tool_response: Duration,
    provider_turn: Duration,
}

impl ExecutionLimits {
    /// Creates a validated provider deadline policy.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::InvalidInput`] when a duration is zero, an
    /// operation timeout exceeds the whole provider turn, or a deadline cannot
    /// be represented by the platform clock.
    pub fn new(
        handshake_timeout: Duration,
        model_response_timeout: Duration,
        tool_response_timeout: Duration,
        provider_turn_timeout: Duration,
    ) -> Result<Self> {
        let limits = Self {
            handshake: handshake_timeout,
            model_response: model_response_timeout,
            tool_response: tool_response_timeout,
            provider_turn: provider_turn_timeout,
        };
        let operation_timeouts = [
            limits.handshake,
            limits.model_response,
            limits.tool_response,
        ];
        if limits.provider_turn.is_zero()
            || operation_timeouts.iter().any(Duration::is_zero)
            || operation_timeouts
                .iter()
                .any(|timeout| *timeout > limits.provider_turn)
        {
            return Err(AgentError::InvalidInput {
                field: "execution_limits",
                message: "timeouts must be nonzero and operation timeouts must not exceed the provider turn timeout".to_owned(),
            });
        }
        let now = std::time::Instant::now();
        if now.checked_add(limits.provider_turn).is_none()
            || operation_timeouts
                .iter()
                .any(|timeout| now.checked_add(*timeout).is_none())
        {
            return Err(AgentError::InvalidInput {
                field: "execution_limits",
                message: "timeouts exceed the platform deadline range".to_owned(),
            });
        }
        Ok(limits)
    }

    /// Returns the deadline for provider stream handshakes and individual
    /// Realtime command delivery. Local artifact I/O is outside this deadline.
    pub const fn handshake_timeout(self) -> Duration {
        self.handshake
    }

    pub const fn model_response_timeout(self) -> Duration {
        self.model_response
    }

    pub const fn tool_response_timeout(self) -> Duration {
        self.tool_response
    }

    /// Returns the deadline after which the turn admits no new provider work.
    /// Durable closure is not bounded by this value.
    pub const fn provider_turn_timeout(self) -> Duration {
        self.provider_turn
    }
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            model_response: Duration::from_mins(1),
            tool_response: Duration::from_secs(30),
            provider_turn: Duration::from_mins(5),
        }
    }
}

/// Dependencies, durable location, and bounded run concurrency used by
/// [`AgentHost::open`].
#[derive(Clone, Debug)]
pub struct OpenOptions {
    workspace: AgentWorkspace,
    composition: CompositionHost,
    consumer: InstanceId,
    max_concurrent_runs: NonZeroU8,
    execution_limits: ExecutionLimits,
}

impl OpenOptions {
    /// Creates host options with bounded default concurrency and execution limits.
    pub fn new(
        workspace: AgentWorkspace,
        composition: CompositionHost,
        consumer: InstanceId,
    ) -> Self {
        Self {
            workspace,
            composition,
            consumer,
            max_concurrent_runs: DEFAULT_MAX_CONCURRENT_RUNS,
            execution_limits: ExecutionLimits::default(),
        }
    }

    /// Sets the number of independent sessions that may execute concurrently.
    /// The host admits at most four times this many pending run calls, including
    /// calls that ultimately join the same session.
    #[must_use]
    pub const fn with_max_concurrent_runs(mut self, maximum: NonZeroU8) -> Self {
        self.max_concurrent_runs = maximum;
        self
    }

    /// Sets host-wide provider deadlines for newly accepted sessions.
    #[must_use]
    pub const fn with_execution_limits(mut self, limits: ExecutionLimits) -> Self {
        self.execution_limits = limits;
        self
    }
}

/// Sole online interface for durable agent runs and validated transcripts.
#[derive(Clone)]
pub struct AgentHost {
    inner: Arc<HostInner>,
}

struct HostInner {
    coordinator: mpsc::Sender<CoordinatorCommand>,
    reader: ColdReader,
    admissions: Arc<Semaphore>,
    health: HealthLatch,
    artifacts: ArtifactStore,
    ai: Option<AiRuntime>,
    ai_execution_slots: Arc<Semaphore>,
    writer: WriterHandle,
}

#[derive(Clone, Debug)]
pub(crate) struct AiRuntime {
    pub(crate) composition: CompositionHost,
    pub(crate) consumer: InstanceId,
    pub(crate) execution_limits: ExecutionLimits,
}

impl fmt::Debug for AgentHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentHost")
            .field("terminal", &!self.inner.health.is_healthy())
            .finish_non_exhaustive()
    }
}

impl AgentHost {
    /// Opens an exclusive product workspace and repairs interrupted sessions
    /// before accepting commands. It never shuts down the composition host.
    ///
    /// # Errors
    ///
    /// Returns an input, workspace, schema, persistence, or recovery error when
    /// the host cannot establish a trustworthy durable starting state.
    pub async fn open(options: OpenOptions) -> Result<Self> {
        let ai = AiRuntime {
            composition: options.composition.clone(),
            consumer: options.consumer.clone(),
            execution_limits: options.execution_limits,
        };
        let factory: Arc<dyn PortFactory> = Arc::new(CompositionPortFactory::new(
            options.composition,
            options.consumer,
        ));
        Self::open_inner(
            options.workspace,
            factory,
            options.max_concurrent_runs,
            options.execution_limits,
            Some(ai),
        )
        .await
    }

    async fn open_inner(
        workspace: AgentWorkspace,
        factory: Arc<dyn PortFactory>,
        max_concurrent_runs: NonZeroU8,
        execution_limits: ExecutionLimits,
        ai: Option<AiRuntime>,
    ) -> Result<Self> {
        let health = HealthLatch::new();
        let max_cold_reads = NonZeroU8::new(max_concurrent_runs.get().min(4))
            .expect("a nonzero execution limit has a nonzero cold-read limit");
        let (writer, reader) =
            WriterHandle::open(workspace, health.clone(), max_cold_reads).await?;
        let artifacts =
            ArtifactStore::open(&reader.workspace_root(), reader.workspace_lease()).await?;
        let admitted = usize::from(max_concurrent_runs.get()) * ADMITTED_RUN_FACTOR;
        let admissions = Arc::new(Semaphore::new(admitted));
        let execution_slots = Arc::new(Semaphore::new(usize::from(max_concurrent_runs.get())));
        let (coordinator, receiver) = mpsc::channel(admitted);
        let host_writer = writer.clone();
        tokio::spawn(coordinate_runs(
            receiver,
            writer,
            reader.clone(),
            factory,
            Arc::clone(&execution_slots),
            health.clone(),
            execution_limits,
        ));
        Ok(Self {
            inner: Arc::new(HostInner {
                coordinator,
                reader,
                admissions,
                health,
                artifacts,
                ai,
                ai_execution_slots: execution_slots,
                writer: host_writer,
            }),
        })
    }

    /// Runs, joins, or durably replays one session. Once the request reaches
    /// the coordinator, dropping this future does not cancel accepted work.
    ///
    /// # Errors
    ///
    /// Returns a session conflict, an untrustworthy durable outcome, or a
    /// terminal-host error after global health is lost.
    pub async fn run(&self, request: RunRequest) -> Result<RunRecord> {
        self.inner.health.check()?;
        let admission = Arc::clone(&self.inner.admissions)
            .acquire_owned()
            .await
            .map_err(|_| self.worker_stopped())?;
        self.inner.health.check()?;
        let (response, receiver) = oneshot::channel();
        self.inner
            .coordinator
            .send(CoordinatorCommand::Run(Candidate {
                request,
                response,
                admission,
                #[cfg(test)]
                accepted: None,
            }))
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?
    }

    /// Returns the workspace-owned content-addressed artifact store.
    pub fn artifacts(&self) -> &ArtifactStore {
        &self.inner.artifacts
    }

    /// Commits media bytes before they can be referenced by an AI operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the bounded CAS commit fails.
    pub async fn import_artifact(
        &self,
        kind: MediaKind,
        mime_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef> {
        self.inner.artifacts.ingest(kind, mime_type, bytes).await
    }

    /// Generates images and commits every returned image to the workspace CAS.
    ///
    /// # Errors
    ///
    /// Returns an error for admission, provider, protocol, persistence, or artifact failure.
    pub async fn generate_image(
        &self,
        operation_id: AiOperationId,
        model: impl Into<String>,
        request: ImageRequest,
    ) -> Result<AgentImageOutput> {
        let (runtime, permit) = self.ai_operation().await?;
        let writer = self.inner.writer.clone();
        let artifacts = self.inner.artifacts.clone();
        let model = model.into();
        let supervised_id = operation_id.clone();
        let deadline = runtime.execution_limits.provider_turn_timeout();
        self.supervise_ai_operation(supervised_id, deadline, async move {
            let _permit = permit;
            ai_operations::generate_image(
                &runtime,
                &writer,
                &artifacts,
                operation_id,
                model,
                request,
            )
            .await
        })
        .await
    }

    /// Transcribes a previously committed audio artifact.
    ///
    /// # Errors
    ///
    /// Returns an error for admission, provider, protocol, or persistence failure.
    pub async fn transcribe(
        &self,
        operation_id: AiOperationId,
        model: impl Into<String>,
        request: TranscriptionRequest,
    ) -> Result<AgentTranscriptionOutput> {
        let (runtime, permit) = self.ai_operation().await?;
        let writer = self.inner.writer.clone();
        let artifacts = self.inner.artifacts.clone();
        let model = model.into();
        let supervised_id = operation_id.clone();
        let deadline = runtime.execution_limits.provider_turn_timeout();
        self.supervise_ai_operation(supervised_id, deadline, async move {
            let _permit = permit;
            ai_operations::transcribe(&runtime, &writer, &artifacts, operation_id, model, request)
                .await
                .map(|(transcription, prepared)| AgentTranscriptionOutput {
                    transcription,
                    prepared,
                })
        })
        .await
    }

    /// Synthesizes speech and commits the returned audio to the workspace CAS.
    ///
    /// # Errors
    ///
    /// Returns an error for admission, provider, protocol, persistence, or artifact failure.
    pub async fn synthesize(
        &self,
        operation_id: AiOperationId,
        model: impl Into<String>,
        request: SpeechRequest,
    ) -> Result<AgentSpeechOutput> {
        let (runtime, permit) = self.ai_operation().await?;
        let writer = self.inner.writer.clone();
        let artifacts = self.inner.artifacts.clone();
        let model = model.into();
        let supervised_id = operation_id.clone();
        let deadline = runtime.execution_limits.provider_turn_timeout();
        self.supervise_ai_operation(supervised_id, deadline, async move {
            let _permit = permit;
            ai_operations::synthesize(&runtime, &writer, &artifacts, operation_id, model, request)
                .await
        })
        .await
    }

    /// Opens a live, non-replayable Realtime session. Its execution permit is
    /// held until the returned session is closed or dropped.
    ///
    /// # Errors
    ///
    /// Returns an error for admission, provider, protocol, or persistence failure.
    pub async fn open_realtime(
        &self,
        operation_id: AiOperationId,
        model: impl Into<String>,
        request: RealtimeRequest,
    ) -> Result<AgentRealtimeSession> {
        let (runtime, permit) = self.ai_operation().await?;
        let writer = self.inner.writer.clone();
        let artifacts = self.inner.artifacts.clone();
        let model = model.into();
        let supervised_id = operation_id.clone();
        let deadline = runtime.execution_limits.provider_turn_timeout();
        self.supervise_ai_operation(supervised_id, deadline, async move {
            ai_operations::open_realtime(
                &runtime,
                writer,
                artifacts,
                operation_id,
                model,
                request,
                permit,
            )
            .await
        })
        .await
    }

    async fn ai_operation(&self) -> Result<(AiRuntime, tokio::sync::OwnedSemaphorePermit)> {
        self.inner.health.check()?;
        let runtime = self.inner.ai.clone().ok_or_else(|| AgentError::Ai {
            operation: "open AI service",
            message: "this AgentHost has no rsi-meta AI runtime".to_owned(),
        })?;
        let permit = Arc::clone(&self.inner.ai_execution_slots)
            .acquire_owned()
            .await
            .map_err(|_| self.worker_stopped())?;
        self.inner.health.check()?;
        Ok((runtime, permit))
    }

    pub(crate) async fn supervise_ai_operation<T>(
        &self,
        operation_id: AiOperationId,
        timeout: Duration,
        operation: impl Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        if tokio::time::Instant::now().checked_add(timeout).is_none() {
            return Err(AgentError::Ai {
                operation: "execute AI operation",
                message: "AI operation deadline is no longer representable".to_owned(),
            });
        }
        let writer = self.inner.writer.clone();
        let supervisor = tokio::spawn(async move {
            writer.ai_reserve(operation_id.clone()).await?;
            let Some(deadline) = tokio::time::Instant::now().checked_add(timeout) else {
                let error = AgentError::Ai {
                    operation: "execute AI operation",
                    message: "AI operation deadline is no longer representable".to_owned(),
                };
                return abandon_after_error(&writer, operation_id, error).await;
            };
            let mut operation = tokio::spawn(operation);
            // Poll the operation first when completion and the deadline become
            // ready together, so a durably completed result is never discarded.
            let completed = tokio::select! {
                biased;
                result = &mut operation => Some(result),
                () = tokio::time::sleep_until(deadline) => None,
            };
            match completed {
                Some(Ok(Ok(result))) => Ok(result),
                Some(Ok(Err(error))) => abandon_after_error(&writer, operation_id, error).await,
                Some(Err(error)) => {
                    let error = AgentError::Ai {
                        operation: "execute AI operation",
                        message: format!("AI operation task failed: {error}"),
                    };
                    abandon_after_error(&writer, operation_id, error).await
                }
                None => {
                    operation.abort();
                    let _ = operation.await;
                    let error = AgentError::Ai {
                        operation: "execute AI operation",
                        message: "AI operation deadline elapsed".to_owned(),
                    };
                    abandon_after_error(&writer, operation_id, error).await
                }
            }
        });
        supervisor.await.map_err(|error| AgentError::Ai {
            operation: "supervise AI operation",
            message: format!("AI operation supervisor failed: {error}"),
        })?
    }

    /// Reads one closed transcript after durable revalidation. A read of an
    /// active session waits for only that session, never unrelated runs.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::CorruptSession`] when the selected transcript
    /// cannot be validated. Session-local corruption and transient read
    /// contention do not make unrelated sessions terminal.
    pub async fn transcript(&self, session_id: &SessionId) -> Result<Option<Transcript>> {
        self.inner.health.check()?;
        let (response, receiver) = oneshot::channel();
        self.inner
            .coordinator
            .send(CoordinatorCommand::Observe {
                session_id: session_id.clone(),
                response,
            })
            .await
            .map_err(|_| self.worker_stopped())?;
        if let Some(mut completed) = receiver.await.map_err(|_| self.worker_stopped())? {
            while !*completed.borrow() {
                completed
                    .changed()
                    .await
                    .map_err(|_| self.worker_stopped())?;
            }
        }
        self.inner.health.check()?;
        self.inner.reader.transcript(session_id.clone()).await
    }

    #[cfg(test)]
    pub(crate) async fn open_with_factory(
        workspace: AgentWorkspace,
        factory: Box<dyn PortFactory>,
    ) -> Result<Self> {
        Self::open_inner(
            workspace,
            Arc::from(factory),
            DEFAULT_MAX_CONCURRENT_RUNS,
            ExecutionLimits::default(),
            None,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn open_with_factory_and_concurrency(
        workspace: AgentWorkspace,
        factory: Box<dyn PortFactory>,
        max_concurrent_runs: NonZeroU8,
    ) -> Result<Self> {
        Self::open_inner(
            workspace,
            Arc::from(factory),
            max_concurrent_runs,
            ExecutionLimits::default(),
            None,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn open_with_factory_and_limits(
        workspace: AgentWorkspace,
        factory: Box<dyn PortFactory>,
        execution_limits: ExecutionLimits,
    ) -> Result<Self> {
        Self::open_inner(
            workspace,
            Arc::from(factory),
            DEFAULT_MAX_CONCURRENT_RUNS,
            execution_limits,
            None,
        )
        .await
    }

    fn worker_stopped(&self) -> AgentError {
        self.inner.health.poison();
        AgentError::WorkerStopped
    }

    #[cfg(test)]
    pub(crate) fn commit_count(&self) -> usize {
        self.inner.writer.commit_count()
    }

    #[cfg(test)]
    pub(crate) async fn make_writes_fail(&self) -> Result<()> {
        self.inner.writer.make_writes_fail().await
    }

    #[cfg(test)]
    pub(crate) fn gate_next_probe(&self) -> Result<crate::persistence::ThreadGate> {
        self.inner.reader.gate_next_probe()
    }

    #[cfg(test)]
    pub(crate) async fn gate_next_dispatch_commit(&self) -> Result<crate::persistence::ThreadGate> {
        self.inner.writer.gate_next_dispatch_commit().await
    }

    #[cfg(test)]
    pub(crate) async fn gate_next_ai_reserve(&self) -> Result<crate::persistence::ThreadGate> {
        self.inner.writer.gate_next_ai_reserve().await
    }

    #[cfg(test)]
    pub(crate) async fn fail_next_dispatch_commit_uncertain(&self) -> Result<()> {
        self.inner
            .writer
            .fail_next_dispatch_commit_uncertain()
            .await
    }

    #[cfg(test)]
    pub(crate) async fn run_with_acceptance_signal(
        &self,
        request: RunRequest,
        accepted: oneshot::Sender<()>,
    ) -> Result<RunRecord> {
        self.inner.health.check()?;
        let admission = Arc::clone(&self.inner.admissions)
            .acquire_owned()
            .await
            .map_err(|_| self.worker_stopped())?;
        let (response, receiver) = oneshot::channel();
        self.inner
            .coordinator
            .send(CoordinatorCommand::Run(Candidate {
                request,
                response,
                admission,
                accepted: Some(accepted),
            }))
            .await
            .map_err(|_| self.worker_stopped())?;
        receiver.await.map_err(|_| self.worker_stopped())?
    }
}

struct Candidate {
    request: RunRequest,
    response: oneshot::Sender<Result<RunRecord>>,
    admission: tokio::sync::OwnedSemaphorePermit,
    #[cfg(test)]
    accepted: Option<oneshot::Sender<()>>,
}

struct Waiter {
    response: oneshot::Sender<Result<RunRecord>>,
    _admission: tokio::sync::OwnedSemaphorePermit,
}

impl Candidate {
    fn into_parts(self) -> (SessionId, Arc<str>, Arc<str>, Waiter) {
        let (session_id, model, prompt) = self.request.into_parts();
        (
            session_id,
            model,
            prompt,
            Waiter {
                response: self.response,
                _admission: self.admission,
            },
        )
    }
}

type RequestKey = (Arc<str>, Arc<str>);
type RequestWaiters = HashMap<RequestKey, Vec<Waiter>>;

struct PromptGroups {
    first: RequestKey,
    by_request: RequestWaiters,
}

impl PromptGroups {
    fn new(candidate: Candidate) -> Self {
        let (_, model, prompt, waiter) = candidate.into_parts();
        let first = (model, prompt);
        let mut by_request = HashMap::with_capacity(1);
        by_request.insert(first.clone(), vec![waiter]);
        Self { first, by_request }
    }

    fn push(&mut self, candidate: Candidate) {
        let (_, model, prompt, waiter) = candidate.into_parts();
        self.by_request
            .entry((model, prompt))
            .or_default()
            .push(waiter);
    }

    fn into_first(mut self) -> (Arc<str>, Arc<str>, Vec<Waiter>, RequestWaiters) {
        let waiters = self
            .by_request
            .remove(&self.first)
            .expect("the first accepted request remains grouped");
        let (model, prompt) = self.first;
        (model, prompt, waiters, self.by_request)
    }
}

async fn abandon_after_error<T>(
    writer: &WriterHandle,
    operation_id: AiOperationId,
    original: AgentError,
) -> Result<T> {
    match writer.ai_abandon(operation_id).await {
        Ok(()) => Err(original),
        Err(abandonment)
            if matches!(
                abandonment.store_error_class(),
                StoreErrorClass::FatalStore | StoreErrorClass::CommitOutcomeUnknown
            ) || matches!(abandonment, AgentError::HostTerminal) =>
        {
            Err(abandonment)
        }
        Err(abandonment) => Err(AgentError::Ai {
            operation: "terminalize failed AI operation",
            message: format!(
                "operation failed: {original}; durable abandonment also failed: {abandonment}"
            ),
        }),
    }
}

enum CoordinatorCommand {
    Run(Candidate),
    Observe {
        session_id: SessionId,
        response: oneshot::Sender<Option<watch::Receiver<bool>>>,
    },
}

enum InFlight {
    Probing {
        groups: PromptGroups,
        completed: watch::Sender<bool>,
    },
    Running {
        model: Arc<str>,
        prompt: Arc<str>,
        waiters: Vec<Waiter>,
        completed: watch::Sender<bool>,
    },
}

impl InFlight {
    fn completed(&self) -> &watch::Sender<bool> {
        match self {
            Self::Probing { completed, .. } | Self::Running { completed, .. } => completed,
        }
    }
}

enum TaskCompletion {
    Probe {
        session_id: SessionId,
        result: Result<ProbeSession>,
    },
    Run {
        session_id: SessionId,
        result: Result<RunRecord>,
    },
}

#[derive(Clone, Copy)]
enum TaskKind {
    Probe,
    Run,
}

#[allow(clippy::too_many_lines)] // One local transition table keeps supervision auditable.
async fn coordinate_runs(
    mut receiver: mpsc::Receiver<CoordinatorCommand>,
    writer: WriterHandle,
    reader: ColdReader,
    factory: Arc<dyn PortFactory>,
    execution_slots: Arc<Semaphore>,
    health: HealthLatch,
    execution_limits: ExecutionLimits,
) {
    let mut accepting = true;
    let mut in_flight = HashMap::<SessionId, InFlight>::new();
    let mut tasks = JoinSet::<TaskCompletion>::new();
    let mut task_sessions = HashMap::<TaskId, (SessionId, TaskKind)>::new();

    while accepting || !tasks.is_empty() {
        tokio::select! {
            command = receiver.recv(), if accepting => {
                let Some(command) = command else {
                    accepting = false;
                    continue;
                };
                match command {
                    CoordinatorCommand::Run(candidate) => {
                        #[cfg(test)]
                        let mut candidate = candidate;
                        #[cfg(test)]
                        if let Some(accepted) = candidate.accepted.take() {
                            let _ = accepted.send(());
                        }
                        if !health.is_healthy() {
                            let _ = candidate.response.send(Err(AgentError::HostTerminal));
                            continue;
                        }
                        let session_id = candidate.request.session_id().clone();
                        match in_flight.get_mut(&session_id) {
                            Some(InFlight::Probing { groups, .. }) => {
                                groups.push(candidate);
                            }
                            Some(InFlight::Running { model, prompt, waiters, .. }) => {
                                if model.as_ref() == candidate.request.model()
                                    && prompt.as_ref() == candidate.request.prompt()
                                {
                                    let (_, _, _, waiter) = candidate.into_parts();
                                    waiters.push(waiter);
                                } else {
                                    let _ = candidate.response.send(Err(
                                        AgentError::SessionConflict { session_id },
                                    ));
                                }
                            }
                            None => {
                                let (completed, _) = watch::channel(false);
                                let groups = PromptGroups::new(candidate);
                                in_flight.insert(
                                    session_id.clone(),
                                    InFlight::Probing {
                                        groups,
                                        completed,
                                    },
                                );
                                let task_reader = reader.clone();
                                let task_session = session_id.clone();
                                let abort = tasks.spawn(async move {
                                    let result = task_reader.probe(task_session.clone()).await;
                                    TaskCompletion::Probe {
                                        session_id: task_session,
                                        result,
                                    }
                                });
                                task_sessions.insert(abort.id(), (session_id, TaskKind::Probe));
                            }
                        }
                    }
                    CoordinatorCommand::Observe { session_id, response } => {
                        let active = in_flight
                            .get(&session_id)
                            .map(|run| run.completed().subscribe());
                        let _ = response.send(active);
                    }
                }
            }
            joined = tasks.join_next_with_id(), if !tasks.is_empty() => {
                let completion = match joined {
                    Some(Ok((task_id, completion))) => {
                        task_sessions.remove(&task_id);
                        completion
                    }
                    Some(Err(error)) => {
                        let Some((session_id, kind)) = task_sessions.remove(&error.id()) else {
                            health.poison();
                            continue;
                        };
                        health.poison();
                        let result = Err(AgentError::RecoveryRequired {
                            session_id: session_id.clone(),
                            message: format!("agent task stopped unexpectedly: {error}"),
                        });
                        match kind {
                            TaskKind::Probe => TaskCompletion::Probe {
                                session_id,
                                result: result.map(|_| ProbeSession::Missing),
                            },
                            TaskKind::Run => TaskCompletion::Run { session_id, result },
                        }
                    }
                    None => continue,
                };
                match completion {
                    TaskCompletion::Probe { session_id, result } => {
                        resolve_probe(
                            &mut in_flight,
                            &mut tasks,
                            &mut task_sessions,
                            &writer,
                            &reader,
                            &factory,
                            &execution_slots,
                            &health,
                            execution_limits,
                            session_id,
                            result,
                        );
                    }
                    TaskCompletion::Run { session_id, result } => {
                        if matches!(
                            result,
                            Err(AgentError::RecoveryRequired { .. } | AgentError::WorkerStopped)
                        ) {
                            health.poison();
                        }
                        finish_run(&mut in_flight, &session_id, &result);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_probe(
    in_flight: &mut HashMap<SessionId, InFlight>,
    tasks: &mut JoinSet<TaskCompletion>,
    task_sessions: &mut HashMap<TaskId, (SessionId, TaskKind)>,
    writer: &WriterHandle,
    reader: &ColdReader,
    factory: &Arc<dyn PortFactory>,
    execution_slots: &Arc<Semaphore>,
    health: &HealthLatch,
    execution_limits: ExecutionLimits,
    session_id: SessionId,
    result: Result<ProbeSession>,
) {
    let Some(InFlight::Probing {
        mut groups,
        completed,
    }) = in_flight.remove(&session_id)
    else {
        health.poison();
        return;
    };

    match result {
        Ok(ProbeSession::Existing {
            model,
            prompt,
            record,
        }) => {
            completed.send_replace(true);
            let key = (Arc::from(model), Arc::from(prompt));
            if let Some(waiters) = groups.by_request.remove(&key) {
                for waiter in waiters {
                    let _ = waiter.response.send(Ok(record.clone()));
                }
            }
            for waiters in groups.by_request.into_values() {
                for waiter in waiters {
                    let _ = waiter.response.send(Err(AgentError::SessionConflict {
                        session_id: session_id.clone(),
                    }));
                }
            }
        }
        Ok(ProbeSession::Missing) => {
            let (model, prompt, waiters, conflicting) = groups.into_first();
            for conflicting_waiters in conflicting.into_values() {
                for waiter in conflicting_waiters {
                    let _ = waiter.response.send(Err(AgentError::SessionConflict {
                        session_id: session_id.clone(),
                    }));
                }
            }
            let task_model = Arc::clone(&model);
            let task_prompt = Arc::clone(&prompt);
            in_flight.insert(
                session_id.clone(),
                InFlight::Running {
                    model,
                    prompt,
                    waiters,
                    completed: completed.clone(),
                },
            );
            let task_writer = writer.clone();
            let task_reader = reader.clone();
            let task_factory = Arc::clone(factory);
            let task_slots = Arc::clone(execution_slots);
            let task_session = session_id.clone();
            let abort = tasks.spawn(async move {
                let result = crate::runner::run_new(
                    task_writer,
                    task_reader,
                    task_factory,
                    task_slots,
                    task_session.clone(),
                    task_model,
                    task_prompt,
                    execution_limits,
                )
                .await;
                TaskCompletion::Run {
                    session_id: task_session,
                    result,
                }
            });
            task_sessions.insert(abort.id(), (session_id, TaskKind::Run));
        }
        Ok(ProbeSession::Open) => {
            health.poison();
            let result = Err(AgentError::RecoveryRequired {
                session_id: session_id.clone(),
                message: "session remained open after startup recovery".to_owned(),
            });
            completed.send_replace(true);
            for waiters in groups.by_request.into_values() {
                for waiter in waiters {
                    let _ = waiter.response.send(result.clone());
                }
            }
        }
        Err(error) => {
            completed.send_replace(true);
            for waiters in groups.by_request.into_values() {
                for waiter in waiters {
                    let _ = waiter.response.send(Err(error.clone()));
                }
            }
        }
    }
}

fn finish_run(
    in_flight: &mut HashMap<SessionId, InFlight>,
    session_id: &SessionId,
    result: &Result<RunRecord>,
) {
    let Some(InFlight::Running {
        waiters, completed, ..
    }) = in_flight.remove(session_id)
    else {
        return;
    };
    completed.send_replace(true);
    for waiter in waiters {
        let _ = waiter.response.send(result.clone());
    }
}
