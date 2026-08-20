use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use rsi_agent_protocol::{ToolResult as WireToolResult, ToolsInvokeRequest};
use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::adapter::{
    CommittedModelRequest, PortBundle, PortError, PortFactory, ValidatedAssistantMessage,
};
use crate::domain::{
    AssistantMessage, BoundaryOutcome, CallId, ContextSnapshot, EventSeq, Failure, FailureKind,
    ModelRequestSnapshot, RunRecord, RunStatus, SessionId, StepId, ToolCall, ToolOutcome,
    TranscriptEventKind,
};
use crate::persistence::{
    ColdReader, CommitCursor, CommitReceipt, CreateSession, ProbeSession, WriterHandle,
    preflight_appended_events,
};
use crate::tool_validation::{ArgumentError, PreparedTool, prepare_catalog, validate_arguments};
use crate::transcript::{AssistantAssessment, SessionMachine};
use crate::{AgentError, ExecutionLimits, MAX_STEPS, Result, SYSTEM_PROMPT};

#[allow(clippy::too_many_arguments)] // One admitted run carries its explicit owned dependencies.
pub(crate) async fn run_new(
    writer: WriterHandle,
    reader: ColdReader,
    factory: Arc<dyn PortFactory>,
    execution_slots: Arc<Semaphore>,
    session_id: SessionId,
    model: Arc<str>,
    prompt: Arc<str>,
    execution_limits: ExecutionLimits,
) -> Result<RunRecord> {
    let _execution =
        execution_slots
            .acquire_owned()
            .await
            .map_err(|_| AgentError::RecoveryRequired {
                session_id: session_id.clone(),
                message: "agent execution limiter stopped".to_owned(),
            })?;
    let created = writer
        .create(
            session_id.clone(),
            model.as_ref().to_owned(),
            prompt.as_ref().to_owned(),
        )
        .await
        .map_err(|error| recovery(&session_id, error))?;
    let (cursor, initial_events) = match created {
        CreateSession::Created { cursor, events } => (cursor, events),
        CreateSession::Exists => {
            return match reader.probe(session_id.clone()).await {
                Ok(ProbeSession::Existing {
                    model: durable_model,
                    prompt: durable_prompt,
                    record,
                }) if durable_model == model.as_ref() && durable_prompt == prompt.as_ref() => {
                    Ok(record)
                }
                Ok(ProbeSession::Existing { .. }) => {
                    Err(AgentError::SessionConflict { session_id })
                }
                Ok(ProbeSession::Missing | ProbeSession::Open) => {
                    Err(AgentError::RecoveryRequired {
                        session_id,
                        message: "session appeared without a validated terminal outcome".to_owned(),
                    })
                }
                Err(error) => Err(recovery(&session_id, error)),
            };
        }
    };

    let mut state =
        CommittedRunState::new(session_id.clone(), model, prompt, cursor, &initial_events)?;
    failpoint("after_session_created");
    run_loop(&writer, factory.as_ref(), &mut state, execution_limits)
        .await
        .map_err(|error| match error {
            AgentError::SessionConflict { .. } | AgentError::RecoveryRequired { .. } => error,
            error => recovery(&session_id, error),
        })
}

#[allow(clippy::too_many_lines)]
async fn run_loop(
    writer: &WriterHandle,
    factory: &dyn PortFactory,
    state: &mut CommittedRunState,
    execution_limits: ExecutionLimits,
) -> Result<RunRecord> {
    let Some(deadline) = Instant::now().checked_add(execution_limits.provider_turn_timeout())
    else {
        return close_failure(writer, state, StepId::new(1), provider_deadline_overflow()).await;
    };
    writer.check_health()?;
    let mut ports = match factory.open(&state.session_id) {
        Ok(ports) => ports,
        Err(error) => {
            return close_failure(writer, state, StepId::new(1), error.failure).await;
        }
    };
    writer.check_health()?;
    if let Err(failure) = initialize_ports(&mut ports, deadline, execution_limits).await {
        return close_failure(writer, state, StepId::new(1), failure).await;
    }
    writer.check_health()?;
    let catalog = match call_port(
        deadline,
        execution_limits.tool_response_timeout(),
        ports.tools.catalog(),
    )
    .await
    {
        Ok(catalog) => catalog.into_inner(),
        Err(failure) => return close_failure(writer, state, StepId::new(1), failure).await,
    };
    let mut definitions = catalog.tools;
    definitions.sort_by(|left, right| left.name.cmp(&right.name));
    let prepared_tools = match prepare_catalog(&definitions) {
        Ok(tools) => tools,
        Err(message) => {
            return close_failure(
                writer,
                state,
                StepId::new(1),
                Failure::new(FailureKind::ToolProtocol, message),
            )
            .await;
        }
    };
    let context = ContextSnapshot {
        system_prompt: SYSTEM_PROMPT.to_owned(),
        model: state.model.to_string(),
        model_provider: ports.model.provider().to_owned(),
        model_protocol_version: rsi_agent_protocol::WIRE_VERSION,
        tools_provider: ports.tools.provider().to_owned(),
        tools_protocol_version: rsi_agent_protocol::WIRE_VERSION,
        tools: definitions,
    };
    let context_event = TranscriptEventKind::ContextSnapshot {
        context: context.clone(),
    };
    let first_request =
        match state.prepare_request(&context, std::slice::from_ref(&context_event), "model-1") {
            Ok(request) => request,
            Err(crate::transcript::PrepareRequestError::ContextLimit(message)) => {
                commit_events_with_catalog(writer, state, vec![context_event], prepared_tools)
                    .await?;
                return close_failure(
                    writer,
                    state,
                    StepId::new(1),
                    Failure::new(FailureKind::ContextLimitExceeded, message),
                )
                .await;
            }
            Err(crate::transcript::PrepareRequestError::Corrupt(error)) => return Err(error),
        };
    let mut exact_request = commit_request(
        writer,
        state,
        vec![context_event],
        first_request,
        Some(prepared_tools),
    )
    .await?;
    failpoint("after_model_request");

    let mut dispatched_calls = 0_usize;
    for step_number in 1..=MAX_STEPS {
        let step = StepId::new(u64::from(step_number));
        let wire_message = match run_model_attempts(
            writer,
            state,
            ports.model.as_mut(),
            &exact_request,
            deadline,
            execution_limits,
        )
        .await?
        {
            Ok(message) => message,
            Err(failure) => return close_failure(writer, state, step, failure).await,
        };
        let message = convert_assistant(wire_message);
        let assessment = match state.machine()?.assess_validated_assistant(&message, step) {
            Ok(assessment) => assessment,
            Err(failure) => return close_failure(writer, state, step, failure).await,
        };
        let calls = message.tool_calls.clone();
        let mut response_events = vec![TranscriptEventKind::AssistantMessage {
            message: message.clone(),
        }];
        response_events.extend(
            calls
                .iter()
                .cloned()
                .map(|call| TranscriptEventKind::ToolCallPrepared { call }),
        );
        if let Err(error) =
            preflight_appended_events(&state.session_id, state.cursor, &response_events)
        {
            return close_failure(
                writer,
                state,
                step,
                Failure::new(
                    FailureKind::ContextLimitExceeded,
                    format!("assistant response cannot be committed: {error}"),
                ),
            )
            .await;
        }
        commit_events(writer, state, response_events).await?;
        failpoint("after_tool_prepared");

        if calls.is_empty() {
            let final_message = message.content.ok_or_else(|| AgentError::CorruptStore {
                message: "validated final assistant message lost its content".to_owned(),
            })?;
            failpoint("after_final_assistant");
            writer.check_health()?;
            if let Err(failure) = finish_ports(&mut ports, deadline, execution_limits).await {
                return close_failure(writer, state, step, failure).await;
            }
            return terminal(
                writer,
                state,
                vec![
                    TranscriptEventKind::StepEnded {
                        step,
                        outcome: BoundaryOutcome::Completed,
                    },
                    TranscriptEventKind::TurnEnded {
                        outcome: BoundaryOutcome::Completed,
                    },
                ],
                RunStatus::Completed { final_message },
            )
            .await;
        }

        if let AssistantAssessment::Limit {
            failure,
            not_started_reason,
        } = assessment
        {
            failpoint("after_call_limit_prepared");
            let mut events = calls
                .into_iter()
                .map(|call| TranscriptEventKind::ToolResult {
                    call_id: call.id,
                    outcome: ToolOutcome::NotStarted {
                        reason: not_started_reason.to_owned(),
                    },
                })
                .collect::<Vec<_>>();
            return close_failure_with_prefix(writer, state, step, failure, &mut events).await;
        }
        let mut plans = plan_calls(
            calls,
            state
                .machine()?
                .tools()
                .expect("captured tools were committed"),
        );
        let mut pending = Vec::new();
        while let Some(plan) = plans.pop_front() {
            match plan {
                PlannedCall::Rejected { call_id, outcome } => {
                    pending.push(TranscriptEventKind::ToolResult { call_id, outcome });
                }
                PlannedCall::Invoke { call_id, request } => {
                    pending.push(TranscriptEventKind::ToolDispatchStarted {
                        call_id: call_id.clone(),
                    });
                    commit_events(writer, state, std::mem::take(&mut pending)).await?;
                    dispatched_calls += 1;
                    failpoint("after_tool_dispatch");
                    if dispatched_calls == 2 {
                        failpoint("after_second_tool_dispatch");
                    }
                    writer.check_health()?;
                    let response = match call_port(
                        deadline,
                        execution_limits.tool_response_timeout(),
                        ports.tools.invoke(request),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(failure) => {
                            let mut events = vec![TranscriptEventKind::ToolResult {
                                call_id,
                                outcome: ToolOutcome::OutcomeUnknown,
                            }];
                            events.extend(plans.into_iter().map(|remaining| {
                                TranscriptEventKind::ToolResult {
                                    call_id: remaining.call_id(),
                                    outcome: ToolOutcome::NotStarted {
                                        reason: "preceding_tool_infrastructure_failure".to_owned(),
                                    },
                                }
                            }));
                            return close_failure_with_prefix(
                                writer,
                                state,
                                step,
                                failure,
                                &mut events,
                            )
                            .await;
                        }
                    }
                    .into_inner();
                    let outcome = match response.result {
                        WireToolResult::Ok { value } => ToolOutcome::Succeeded { value },
                        WireToolResult::Error { code, message } => {
                            ToolOutcome::Failed { code, message }
                        }
                    };
                    pending.push(TranscriptEventKind::ToolResult { call_id, outcome });
                }
            }
        }

        pending.push(TranscriptEventKind::StepEnded {
            step,
            outcome: BoundaryOutcome::Continued,
        });
        pending.push(TranscriptEventKind::StepStarted {
            step: StepId::new(u64::from(step_number + 1)),
        });
        let request_id = format!("model-{}", step_number + 1);
        let snapshot = match state.prepare_request(
            state.machine()?.context().expect("context was committed"),
            &pending,
            &request_id,
        ) {
            Ok(snapshot) => snapshot,
            Err(crate::transcript::PrepareRequestError::ContextLimit(message)) => {
                commit_events(writer, state, pending).await?;
                return close_failure(
                    writer,
                    state,
                    StepId::new(u64::from(step_number + 1)),
                    Failure::new(FailureKind::ContextLimitExceeded, message),
                )
                .await;
            }
            Err(crate::transcript::PrepareRequestError::Corrupt(error)) => return Err(error),
        };
        exact_request = commit_request(writer, state, pending, snapshot, None).await?;
        failpoint("after_followup_request");
        failpoint("after_model_request");
    }
    close_failure(
        writer,
        state,
        StepId::new(u64::from(MAX_STEPS) + 1),
        Failure::new(
            FailureKind::StepLimitExceeded,
            "agent step limit was exhausted",
        ),
    )
    .await
}

async fn run_model_attempts(
    writer: &WriterHandle,
    state: &mut CommittedRunState,
    model: &mut dyn crate::adapter::ModelPort,
    request: &CommittedModelRequest,
    deadline: Instant,
    limits: ExecutionLimits,
) -> Result<std::result::Result<ValidatedAssistantMessage, Failure>> {
    let mut attempt = 0_u8;
    let mut pinned = None::<rsi_ai_meta::PreparedCallSnapshot>;
    loop {
        attempt = attempt
            .checked_add(1)
            .ok_or_else(|| AgentError::CorruptStore {
                message: "model attempt counter overflowed".to_owned(),
            })?;
        writer.check_health()?;
        let prepared = match call_model_port(
            deadline,
            limits.model_response_timeout(),
            model.prepare(request),
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => return Ok(Err(error.failure)),
        };
        if let Some(first) = &pinned {
            if !same_retry_route(first, prepared.snapshot()) {
                return Ok(Err(Failure::new(
                    FailureKind::ModelProtocol,
                    "provider changed the prepared route within one model request",
                )));
            }
        } else {
            pinned = Some(prepared.snapshot().clone());
        }
        let snapshot = prepared.snapshot().clone();
        commit_events(
            writer,
            state,
            vec![TranscriptEventKind::ModelCallPrepared {
                request_id: request.request_id().to_owned(),
                snapshot: snapshot.clone(),
            }],
        )
        .await?;
        failpoint("after_model_call_prepared");
        writer.check_health()?;
        match call_model_port(
            deadline,
            limits.model_response_timeout(),
            model.start(prepared),
        )
        .await
        {
            Ok(message) => return Ok(Ok(message)),
            Err(error) => {
                let Some(ai_error) = error.retry_error() else {
                    return Ok(Err(error.failure));
                };
                let Some(delay_ms) = retry_delay_ms(
                    &snapshot.retry_policy,
                    attempt,
                    ai_error,
                    request.request_id(),
                ) else {
                    return Ok(Err(error.failure));
                };
                commit_events(
                    writer,
                    state,
                    vec![TranscriptEventKind::ModelRetryScheduled {
                        request_id: request.request_id().to_owned(),
                        failed_attempt: attempt,
                        error: ai_error.clone(),
                        delay_ms,
                    }],
                )
                .await?;
                failpoint("after_model_retry_scheduled");
                let delay = Duration::from_millis(delay_ms);
                if Instant::now()
                    .checked_add(delay)
                    .is_none_or(|wake| wake >= deadline)
                {
                    return Ok(Err(provider_timeout()));
                }
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn same_retry_route(
    first: &rsi_ai_meta::PreparedCallSnapshot,
    next: &rsi_ai_meta::PreparedCallSnapshot,
) -> bool {
    first.deployment_id == next.deployment_id
        && first.provider_family == next.provider_family
        && first.capability == next.capability
        && first.model == next.model
        && first.protocol == next.protocol
        && first.transport == next.transport
        && first.endpoint_fingerprint == next.endpoint_fingerprint
        && first.config_generation == next.config_generation
        && first.credential_source == next.credential_source
        && first.retry_policy == next.retry_policy
        && first.request_sha256 == next.request_sha256
}

fn retry_delay_ms(
    policy: &rsi_ai_meta::RetryPolicy,
    failed_attempt: u8,
    error: &rsi_ai_protocol::AiError,
    request_id: &str,
) -> Option<u64> {
    if failed_attempt > policy.max_retries() || !policy.retries(error.kind()) {
        return None;
    }
    if let Some(provider_delay) = error.retry_after_ms() {
        return (provider_delay > 0 && provider_delay <= policy.max_delay_ms())
            .then_some(provider_delay);
    }
    let exponent = u32::from(failed_attempt.saturating_sub(1));
    let base = policy
        .initial_delay_ms()
        .saturating_mul(2_u64.saturating_pow(exponent))
        .min(policy.max_delay_ms());
    let spread = base.saturating_mul(u64::from(policy.jitter_per_mille())) / 1_000;
    if spread == 0 {
        return Some(base);
    }
    let digest = crate::digest::sha256_hex(format!("{request_id}:{failed_attempt}").as_bytes());
    let sample = u64::from_str_radix(&digest[..16], 16).ok()?;
    let width = spread.saturating_mul(2).saturating_add(1);
    Some(base.saturating_sub(spread).saturating_add(sample % width))
}

struct CommittedRunState {
    session_id: SessionId,
    model: Arc<str>,
    cursor: CommitCursor,
    machine: Option<SessionMachine>,
    #[cfg(test)]
    request_derivations: std::cell::RefCell<std::collections::BTreeMap<String, usize>>,
}

impl CommittedRunState {
    fn new(
        session_id: SessionId,
        model: Arc<str>,
        prompt: Arc<str>,
        cursor: CommitCursor,
        initial_events: &[TranscriptEventKind],
    ) -> Result<Self> {
        let mut machine = SessionMachine::new(prompt)?;
        machine.apply_batch(EventSeq::new(1), initial_events)?;
        Ok(Self {
            session_id,
            model,
            cursor,
            machine: Some(machine),
            #[cfg(test)]
            request_derivations: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        })
    }

    fn machine(&self) -> Result<&SessionMachine> {
        self.machine
            .as_ref()
            .ok_or_else(|| AgentError::CorruptStore {
                message: "committed session state is unavailable during a pending transition"
                    .to_owned(),
            })
    }

    fn prepare_request(
        &self,
        context: &ContextSnapshot,
        prefix: &[TranscriptEventKind],
        request_id: &str,
    ) -> std::result::Result<ModelRequestSnapshot, crate::transcript::PrepareRequestError> {
        let messages = self
            .machine()
            .map_err(crate::transcript::PrepareRequestError::Corrupt)?
            .projected_messages(prefix)?;
        let offset = u64::try_from(prefix.len()).expect("bounded event count fits u64");
        let source = self
            .cursor
            .last()
            .ok_or_else(|| {
                crate::transcript::PrepareRequestError::Corrupt(AgentError::CorruptStore {
                    message: "committed session has no source event".to_owned(),
                })
            })?
            .get()
            .checked_add(offset)
            .map(EventSeq::new)
            .ok_or_else(|| {
                crate::transcript::PrepareRequestError::Corrupt(AgentError::CorruptStore {
                    message: "event sequence exhausted while preparing request".to_owned(),
                })
            })?;
        let request = crate::transcript::prepare_projected_model_request(
            context, messages, source, request_id,
        )?;
        #[cfg(test)]
        self.note_request_derivation(request_id);
        Ok(request)
    }

    fn transaction(&mut self) -> Result<SessionTxn<'_>> {
        let machine = self
            .machine
            .take()
            .ok_or_else(|| AgentError::CorruptStore {
                message: "another durable transition is already pending".to_owned(),
            })?;
        let session_id = self.session_id.clone();
        let cursor = self.cursor;
        Ok(SessionTxn {
            target: self,
            session_id,
            cursor,
            machine,
        })
    }

    fn verify_committed_request(
        &self,
        committed: &ModelRequestSnapshot,
    ) -> Result<CommittedModelRequest> {
        let last = self.cursor.last().ok_or_else(|| AgentError::CorruptStore {
            message: "committed request has no transcript sequence".to_owned(),
        })?;
        if last.get() != committed.source_through.get().saturating_add(1) {
            return Err(AgentError::CorruptStore {
                message: "committed model request is not the latest event".to_owned(),
            });
        }
        let derived = self
            .machine()?
            .prepare_current_request(committed.source_through, &committed.request_id)
            .map_err(|error| match error {
                crate::transcript::PrepareRequestError::ContextLimit(message) => {
                    AgentError::CorruptStore {
                        message: format!("committed model request no longer encodes: {message}"),
                    }
                }
                crate::transcript::PrepareRequestError::Corrupt(error) => error,
            })?;
        #[cfg(test)]
        self.note_request_derivation(&committed.request_id);
        #[cfg(test)]
        assert_eq!(
            self.request_derivations
                .borrow()
                .get(&committed.request_id)
                .copied(),
            Some(2),
            "each live model request must be derived once before and once after its commit",
        );
        if &derived != committed {
            return Err(AgentError::CorruptStore {
                message: "committed model request differs from its installed state projection"
                    .to_owned(),
            });
        }
        Ok(CommittedModelRequest::new(
            derived.request_id,
            derived.model,
            derived.canonical_json,
        ))
    }

    #[cfg(test)]
    fn note_request_derivation(&self, request_id: &str) {
        *self
            .request_derivations
            .borrow_mut()
            .entry(request_id.to_owned())
            .or_default() += 1;
    }
}

struct SessionTxn<'state> {
    target: &'state mut CommittedRunState,
    session_id: SessionId,
    cursor: CommitCursor,
    machine: SessionMachine,
}

struct CommitIntent {
    events: Vec<TranscriptEventKind>,
    catalog: Option<std::collections::BTreeMap<String, PreparedTool>>,
    terminal_status: Option<RunStatus>,
}

impl<'state> SessionTxn<'state> {
    fn prepare(mut self, intent: CommitIntent) -> Result<PendingCommit<'state>> {
        self.machine.apply_validated_batch(
            EventSeq::new(self.cursor.next_seq),
            &intent.events,
            intent.catalog,
        )?;
        if let Some(status) = intent.terminal_status.as_ref() {
            self.machine.validate_terminal(status)?;
        }
        Ok(PendingCommit {
            target: self.target,
            session_id: self.session_id,
            cursor: self.cursor,
            events: Some(intent.events),
            terminal_status: intent.terminal_status,
            machine: self.machine,
        })
    }
}

struct PendingCommit<'state> {
    target: &'state mut CommittedRunState,
    session_id: SessionId,
    cursor: CommitCursor,
    events: Option<Vec<TranscriptEventKind>>,
    terminal_status: Option<RunStatus>,
    machine: SessionMachine,
}

enum CommittedAction {
    Continue,
    Terminal(RunRecord),
}

impl CommittedAction {
    fn into_record(self) -> Option<RunRecord> {
        match self {
            Self::Continue => None,
            Self::Terminal(record) => Some(record),
        }
    }
}

impl PendingCommit<'_> {
    fn take_write(
        &mut self,
    ) -> (
        SessionId,
        CommitCursor,
        Vec<TranscriptEventKind>,
        Option<RunStatus>,
    ) {
        (
            self.session_id.clone(),
            self.cursor,
            self.events
                .take()
                .expect("a pending commit is written exactly once"),
            self.terminal_status.take(),
        )
    }

    fn install(self, receipt: CommitReceipt) -> CommittedAction {
        let action = match receipt.record {
            None => CommittedAction::Continue,
            Some(record) => CommittedAction::Terminal(record),
        };
        self.target.cursor = receipt.cursor;
        self.target.machine = Some(self.machine);
        action
    }
}

enum PlannedCall {
    Rejected {
        call_id: CallId,
        outcome: ToolOutcome,
    },
    Invoke {
        call_id: CallId,
        request: ToolsInvokeRequest,
    },
}

impl PlannedCall {
    fn call_id(self) -> CallId {
        match self {
            Self::Rejected { call_id, .. } | Self::Invoke { call_id, .. } => call_id,
        }
    }
}

fn plan_calls(
    calls: Vec<ToolCall>,
    tools: &std::collections::BTreeMap<String, PreparedTool>,
) -> VecDeque<PlannedCall> {
    calls
        .into_iter()
        .map(|call| {
            let Some(tool) = tools.get(&call.name) else {
                return PlannedCall::Rejected {
                    call_id: call.id,
                    outcome: ToolOutcome::Failed {
                        code: "unknown_tool".to_owned(),
                        message: format!("tool `{}` is not in the captured catalog", call.name),
                    },
                };
            };
            let arguments = match validate_arguments(tool, &call.arguments) {
                Ok(arguments) => arguments.canonical_json,
                Err(error) => {
                    let message = match error {
                        ArgumentError::InvalidJson => {
                            "arguments are not bounded JSON with unique object keys"
                        }
                        ArgumentError::LossyNumber => {
                            "arguments contain a number that is not exactly representable for schema validation"
                        }
                        ArgumentError::SchemaMismatch => {
                            "arguments do not match the captured schema"
                        }
                    };
                    return PlannedCall::Rejected {
                        call_id: call.id,
                        outcome: ToolOutcome::Failed {
                            code: "invalid_arguments".to_owned(),
                            message: message.to_owned(),
                        },
                    };
                }
            };
            PlannedCall::Invoke {
                call_id: call.id.clone(),
                request: ToolsInvokeRequest {
                    call_id: call.id.as_str().to_owned(),
                    name: call.name,
                    arguments,
                },
            }
        })
        .collect()
}

async fn initialize_ports(
    ports: &mut PortBundle,
    deadline: Instant,
    limits: ExecutionLimits,
) -> std::result::Result<(), Failure> {
    let (model, tools) = (&mut ports.model, &mut ports.tools);
    let (model, tools) = tokio::join!(
        call_port(deadline, limits.handshake_timeout(), model.initialize()),
        call_port(deadline, limits.handshake_timeout(), tools.initialize())
    );
    merge_port_results(model, tools)
}

async fn finish_ports(
    ports: &mut PortBundle,
    deadline: Instant,
    limits: ExecutionLimits,
) -> std::result::Result<(), Failure> {
    let (model, tools) = (&mut ports.model, &mut ports.tools);
    let (model, tools) = tokio::join!(
        call_port(deadline, limits.handshake_timeout(), model.finish()),
        call_port(deadline, limits.handshake_timeout(), tools.finish())
    );
    merge_port_results(model, tools)
}

fn merge_port_results(
    model: std::result::Result<(), Failure>,
    tools: std::result::Result<(), Failure>,
) -> std::result::Result<(), Failure> {
    model.and(tools)
}

async fn call_port<T>(
    turn_deadline: Instant,
    operation_timeout: Duration,
    future: impl Future<Output = std::result::Result<T, PortError>>,
) -> std::result::Result<T, Failure> {
    let now = Instant::now();
    if now >= turn_deadline {
        return Err(provider_timeout());
    }
    let operation_deadline = now
        .checked_add(operation_timeout)
        .unwrap_or(turn_deadline)
        .min(turn_deadline);
    match tokio::time::timeout_at(operation_deadline, future).await {
        Ok(result) => result.map_err(|error| error.failure),
        Err(_) => Err(provider_timeout()),
    }
}

async fn call_model_port<T>(
    turn_deadline: Instant,
    operation_timeout: Duration,
    future: impl Future<Output = std::result::Result<T, PortError>>,
) -> std::result::Result<T, PortError> {
    let now = Instant::now();
    if now >= turn_deadline {
        return Err(PortError {
            failure: provider_timeout(),
            retry: None,
        });
    }
    let operation_deadline = now
        .checked_add(operation_timeout)
        .unwrap_or(turn_deadline)
        .min(turn_deadline);
    match tokio::time::timeout_at(operation_deadline, future).await {
        Ok(result) => result,
        Err(_) => Err(PortError {
            failure: provider_timeout(),
            retry: None,
        }),
    }
}

fn provider_timeout() -> Failure {
    Failure::new(
        FailureKind::TimedOut,
        "agent service operation exceeded its deadline",
    )
}

fn provider_deadline_overflow() -> Failure {
    Failure::new(
        FailureKind::TimedOut,
        "agent provider turn deadline exceeds the platform clock range",
    )
}

async fn commit_events(
    writer: &WriterHandle,
    state: &mut CommittedRunState,
    events: Vec<TranscriptEventKind>,
) -> Result<()> {
    commit_transition(writer, state, events, None, None).await?;
    Ok(())
}

async fn terminal(
    writer: &WriterHandle,
    state: &mut CommittedRunState,
    events: Vec<TranscriptEventKind>,
    status: RunStatus,
) -> Result<RunRecord> {
    let record = commit_transition(writer, state, events, None, Some(status))
        .await?
        .ok_or_else(|| AgentError::CorruptStore {
            message: "terminal commit returned no run record".to_owned(),
        })?;
    failpoint("after_terminal_commit");
    Ok(record)
}

async fn close_failure(
    writer: &WriterHandle,
    state: &mut CommittedRunState,
    step: StepId,
    failure: Failure,
) -> Result<RunRecord> {
    let mut events = Vec::new();
    close_failure_with_prefix(writer, state, step, failure, &mut events).await
}

async fn close_failure_with_prefix(
    writer: &WriterHandle,
    state: &mut CommittedRunState,
    step: StepId,
    failure: Failure,
    events: &mut Vec<TranscriptEventKind>,
) -> Result<RunRecord> {
    events.extend([
        TranscriptEventKind::StepEnded {
            step,
            outcome: BoundaryOutcome::Failed {
                failure: failure.clone(),
            },
        },
        TranscriptEventKind::TurnEnded {
            outcome: BoundaryOutcome::Failed {
                failure: failure.clone(),
            },
        },
    ]);
    terminal(
        writer,
        state,
        std::mem::take(events),
        RunStatus::Failed { failure },
    )
    .await
}

async fn commit_request(
    writer: &WriterHandle,
    state: &mut CommittedRunState,
    mut events: Vec<TranscriptEventKind>,
    prepared: ModelRequestSnapshot,
    catalog: Option<std::collections::BTreeMap<String, PreparedTool>>,
) -> Result<CommittedModelRequest> {
    events.push(TranscriptEventKind::ModelRequestPrepared {
        request: prepared.clone(),
    });
    commit_transition(writer, state, events, catalog, None).await?;
    state.verify_committed_request(&prepared)
}

async fn commit_events_with_catalog(
    writer: &WriterHandle,
    state: &mut CommittedRunState,
    events: Vec<TranscriptEventKind>,
    catalog: std::collections::BTreeMap<String, PreparedTool>,
) -> Result<()> {
    commit_transition(writer, state, events, Some(catalog), None).await?;
    Ok(())
}

async fn commit_transition(
    writer: &WriterHandle,
    state: &mut CommittedRunState,
    events: Vec<TranscriptEventKind>,
    catalog: Option<std::collections::BTreeMap<String, PreparedTool>>,
    terminal_status: Option<RunStatus>,
) -> Result<Option<RunRecord>> {
    let session_id = state.session_id.clone();
    let mut pending = state.transaction()?.prepare(CommitIntent {
        events,
        catalog,
        terminal_status,
    })?;
    let (write_session, cursor, events, terminal_status) = pending.take_write();
    let receipt = writer
        .commit(write_session, cursor, events, terminal_status)
        .await
        .map_err(|error| recovery(&session_id, error))?;
    Ok(pending.install(receipt).into_record())
}

fn convert_assistant(message: ValidatedAssistantMessage) -> AssistantMessage {
    let message = message.into_inner();
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    for block in message.content {
        match block {
            rsi_ai_protocol::ContentBlock::Text { text: value } => text.push_str(&value),
            rsi_ai_protocol::ContentBlock::Reasoning { text: value } => {
                reasoning.push_str(&value);
            }
            rsi_ai_protocol::ContentBlock::ToolCall(call) => calls.push(ToolCall {
                id: CallId::from_validated(call.id),
                name: call.name,
                arguments: call.arguments,
            }),
        }
    }
    AssistantMessage {
        content: (!text.is_empty()).then_some(text),
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: calls,
        finish_reason: message.finish_reason,
        usage: message.usage,
        replay: message.replay,
        warnings: message.warnings,
        sources: message.sources,
    }
}

fn recovery(session_id: &SessionId, error: AgentError) -> AgentError {
    match error {
        error @ AgentError::SessionConflict { .. } => error,
        AgentError::RecoveryRequired { message, .. } => AgentError::RecoveryRequired {
            session_id: session_id.clone(),
            message,
        },
        error => AgentError::RecoveryRequired {
            session_id: session_id.clone(),
            message: error.to_string(),
        },
    }
}

#[cfg(all(test, feature = "test-failpoints"))]
fn failpoint(stage: &str) {
    if std::env::var("RSI_AGENT_CRASH_AT").as_deref() == Ok(stage) {
        std::process::exit(86);
    }
}

#[cfg(not(all(test, feature = "test-failpoints")))]
fn failpoint(_stage: &str) {}
