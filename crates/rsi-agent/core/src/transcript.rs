use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use crate::digest::sha256_hex;
use crate::domain::{
    BoundaryOutcome, CallId, ContextSnapshot, EventSeq, ModelRequestSnapshot, RunStatus, StepId,
    ToolCall, ToolOutcome, TranscriptEvent, TranscriptEventKind,
};
use crate::error::corrupt;
use crate::tool_validation::{ArgumentError, PreparedTool, prepare_catalog, validate_arguments};
use crate::{
    AgentError, MAX_STEPS, MAX_TOOL_CALLS_PER_STEP, MAX_TOOL_CALLS_PER_TURN, MAX_TRANSCRIPT_EVENTS,
    Result,
};
use rsi_agent_protocol::ToolResult as WireToolResult;
use rsi_agent_protocol::is_wire_identifier;
use rsi_ai_protocol::{LanguageRequest, Message, MessageContent, ProviderExtension, ToolChoice};

pub(crate) fn prepare_projected_model_request(
    context: &ContextSnapshot,
    projection: ProjectedRequest,
    source_through: EventSeq,
    request_id: &str,
) -> std::result::Result<ModelRequestSnapshot, PrepareRequestError> {
    let request = model_request(context, projection).map_err(PrepareRequestError::ContextLimit)?;
    let bytes = request
        .canonical_bytes()
        .map_err(|error| PrepareRequestError::ContextLimit(error.to_string()))?;
    let canonical_json = String::from_utf8(bytes).map_err(|error| {
        PrepareRequestError::Corrupt(AgentError::CorruptStore {
            message: format!("model protocol emitted non-UTF-8 JSON: {error}"),
        })
    })?;
    let canonical_json: Arc<str> = canonical_json.into();
    let sha256 = sha256_hex(canonical_json.as_bytes());
    Ok(ModelRequestSnapshot {
        request_id: request_id.to_owned(),
        model: context.model.clone(),
        source_through,
        canonical_json,
        sha256,
    })
}

#[cfg(test)]
pub(crate) fn project_model_visible(events: &[TranscriptEventKind]) -> Result<ProjectedRequest> {
    let mut projection = ModelProjection::new();
    for event in events {
        projection.apply(event)?;
    }
    projection.with_prefix(&[]).map_err(|error| match error {
        PrepareRequestError::ContextLimit(message) => corrupt(message),
        PrepareRequestError::Corrupt(error) => error,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectedRequest {
    messages: Vec<Message>,
    replay: Option<ProviderExtension>,
}

struct ModelProjection {
    messages: Vec<Message>,
    replay: Option<ProviderExtension>,
    encoded_messages_bytes: usize,
    overflowed: bool,
}

impl ModelProjection {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            replay: None,
            encoded_messages_bytes: 2,
            overflowed: false,
        }
    }

    fn apply(&mut self, event: &TranscriptEventKind) -> Result<()> {
        if self.overflowed {
            return Ok(());
        }
        let Some(message) = model_visible_message(event)? else {
            return Ok(());
        };
        if !push_bounded_message(
            &mut self.messages,
            &mut self.encoded_messages_bytes,
            message,
        )? {
            self.messages.clear();
            self.encoded_messages_bytes = 0;
            self.overflowed = true;
        }
        if let TranscriptEventKind::AssistantMessage { message } = event {
            self.replay.clone_from(&message.replay);
        }
        Ok(())
    }

    fn with_prefix(
        &self,
        prefix: &[TranscriptEventKind],
    ) -> std::result::Result<ProjectedRequest, PrepareRequestError> {
        if self.overflowed {
            return Err(context_limit());
        }
        let mut messages = self.messages.clone();
        let mut replay = self.replay.clone();
        let mut encoded_messages_bytes = self.encoded_messages_bytes;
        for event in prefix {
            let Some(message) =
                model_visible_message(event).map_err(PrepareRequestError::Corrupt)?
            else {
                continue;
            };
            if !push_bounded_message(&mut messages, &mut encoded_messages_bytes, message)
                .map_err(PrepareRequestError::Corrupt)?
            {
                return Err(context_limit());
            }
            if let TranscriptEventKind::AssistantMessage { message } = event {
                replay.clone_from(&message.replay);
            }
        }
        Ok(ProjectedRequest { messages, replay })
    }
}

fn push_bounded_message(
    messages: &mut Vec<Message>,
    encoded_messages_bytes: &mut usize,
    message: Message,
) -> Result<bool> {
    let message_bytes = rsi_agent_protocol::encoded_json_len(&message).map_err(|error| {
        corrupt(format!(
            "model-visible message could not be encoded: {error}"
        ))
    })?;
    let separator = usize::from(!messages.is_empty());
    let projected = encoded_messages_bytes
        .checked_add(separator)
        .and_then(|bytes| bytes.checked_add(message_bytes))
        .ok_or_else(|| corrupt("model-visible context size overflowed"))?;
    if projected > rsi_agent_protocol::MAX_DATA_BYTES {
        return Ok(false);
    }
    messages.push(message);
    *encoded_messages_bytes = projected;
    Ok(true)
}

fn model_visible_message(event: &TranscriptEventKind) -> Result<Option<Message>> {
    let message = match event {
        TranscriptEventKind::UserMessage { content } => Message::user_text(content.clone()),
        TranscriptEventKind::AssistantMessage { message } => {
            let mut blocks = Vec::new();
            let replay_handles_reasoning = message
                .replay
                .as_ref()
                .is_some_and(|replay| replay.namespace == "openai.responses.replay");
            if !replay_handles_reasoning && let Some(reasoning) = &message.reasoning {
                blocks.push(MessageContent::Reasoning {
                    text: reasoning.clone(),
                    evidence: None,
                });
            }
            if let Some(content) = &message.content {
                blocks.push(MessageContent::Text {
                    text: content.clone(),
                });
            }
            blocks.extend(message.tool_calls.iter().map(|call| {
                MessageContent::ToolCall(rsi_ai_protocol::ToolCall {
                    id: call.id.as_str().to_owned(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
            }));
            Message::assistant(blocks)
        }
        TranscriptEventKind::ToolResult { call_id, outcome } => {
            let result = wire_tool_result(outcome);
            let content = serde_json::to_string(&result)
                .map_err(|error| corrupt(format!("tool result cannot be projected: {error}")))?;
            Message::tool_result(
                call_id.as_str(),
                vec![MessageContent::Text { text: content }],
                matches!(result, WireToolResult::Error { .. }),
            )
        }
        _ => return Ok(None),
    };
    message
        .map(Some)
        .map_err(|error| corrupt(format!("model-visible message is invalid: {error}")))
}

fn context_limit() -> PrepareRequestError {
    PrepareRequestError::ContextLimit(format!(
        "model-visible context exceeds the {}-byte DATA limit",
        rsi_agent_protocol::MAX_DATA_BYTES
    ))
}

#[derive(Debug)]
pub(crate) enum PrepareRequestError {
    ContextLimit(String),
    Corrupt(AgentError),
}

pub(crate) struct RecoveryPlan {
    pub(crate) open_step: StepId,
    pub(crate) unfinished_calls: Vec<(CallId, bool)>,
}

pub(crate) enum AssistantAssessment {
    Continue,
    Limit {
        failure: crate::Failure,
        not_started_reason: &'static str,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PayloadTrust {
    Durable,
    ValidatedLive,
}

/// The single transcript grammar used incrementally by execution and by
/// durable replay during recovery and terminal reads.
#[allow(clippy::struct_excessive_bools)] // Independent grammar facts are clearer than coupled phase enums.
pub(crate) struct SessionMachine {
    prompt: Arc<str>,
    model: Option<String>,
    next_seq: u64,
    turn_open: bool,
    step_open: Option<StepId>,
    next_step: u64,
    saw_session: bool,
    saw_user: bool,
    saw_context: bool,
    context: Option<ContextSnapshot>,
    projection: ModelProjection,
    prepared_tools: Option<BTreeMap<String, PreparedTool>>,
    request_pending: bool,
    model_call_prepared: bool,
    model_attempt: u8,
    prepared_snapshot: Option<rsi_ai_meta::PreparedCallSnapshot>,
    request_seen_in_step: bool,
    expected_calls: VecDeque<ToolCall>,
    execution_order: VecDeque<CallId>,
    active_dispatch: Option<CallId>,
    calls: BTreeMap<CallId, CallState>,
    seen_call_ids: std::collections::BTreeSet<CallId>,
    assistant_calls_in_step: Option<usize>,
    assistant_final_in_step: Option<String>,
    step_has_noncontinuable_tool_outcome: bool,
    total_tool_calls: usize,
    call_limit_exceeded_in_step: bool,
    last_step_outcome: Option<BoundaryOutcome>,
    final_boundary: Option<BoundaryOutcome>,
}

impl SessionMachine {
    pub(crate) fn new(prompt: impl Into<Arc<str>>) -> Result<Self> {
        let prompt = prompt.into();
        if prompt.trim().is_empty()
            || prompt.len() > crate::MAX_PROMPT_BYTES
            || prompt
                .chars()
                .any(|character| character == '\0' || character == '\u{007f}')
        {
            return Err(corrupt("stored prompt is invalid"));
        }
        Ok(Self {
            prompt,
            model: None,
            next_seq: 1,
            turn_open: false,
            step_open: None,
            next_step: 1,
            saw_session: false,
            saw_user: false,
            saw_context: false,
            context: None,
            projection: ModelProjection::new(),
            prepared_tools: None,
            request_pending: false,
            model_call_prepared: false,
            model_attempt: 0,
            prepared_snapshot: None,
            request_seen_in_step: false,
            expected_calls: VecDeque::new(),
            execution_order: VecDeque::new(),
            active_dispatch: None,
            calls: BTreeMap::new(),
            seen_call_ids: std::collections::BTreeSet::new(),
            assistant_calls_in_step: None,
            assistant_final_in_step: None,
            step_has_noncontinuable_tool_outcome: false,
            total_tool_calls: 0,
            call_limit_exceeded_in_step: false,
            last_step_outcome: None,
            final_boundary: None,
        })
    }

    pub(crate) fn replay(prompt: impl Into<Arc<str>>, events: &[TranscriptEvent]) -> Result<Self> {
        if events.len() > MAX_TRANSCRIPT_EVENTS {
            return Err(corrupt("transcript exceeds the event limit"));
        }
        let mut machine = Self::new(prompt)?;
        let mut supplied_catalog = None;
        for event in events {
            machine.apply(
                event.seq(),
                event.kind(),
                &mut supplied_catalog,
                PayloadTrust::Durable,
            )?;
        }
        Ok(machine)
    }

    pub(crate) fn apply_batch(
        &mut self,
        first_seq: EventSeq,
        events: &[TranscriptEventKind],
    ) -> Result<()> {
        self.apply_batch_inner(first_seq, events, None, PayloadTrust::Durable)
    }

    pub(crate) fn apply_validated_batch(
        &mut self,
        first_seq: EventSeq,
        events: &[TranscriptEventKind],
        prepared: Option<BTreeMap<String, PreparedTool>>,
    ) -> Result<()> {
        self.apply_batch_inner(first_seq, events, prepared, PayloadTrust::ValidatedLive)
    }

    fn apply_batch_inner(
        &mut self,
        first_seq: EventSeq,
        events: &[TranscriptEventKind],
        prepared: Option<BTreeMap<String, PreparedTool>>,
        trust: PayloadTrust,
    ) -> Result<()> {
        let mut prepared = prepared;
        for (offset, event) in events.iter().enumerate() {
            let seq = first_seq
                .get()
                .checked_add(u64::try_from(offset).expect("bounded event offset fits u64"))
                .map(EventSeq::new)
                .ok_or_else(|| corrupt("event sequence exhausted"))?;
            self.apply(seq, event, &mut prepared, trust)?;
        }
        if prepared.is_some() {
            return Err(corrupt(
                "precompiled catalog receipt contained no context snapshot",
            ));
        }
        Ok(())
    }

    pub(crate) fn context(&self) -> Option<&ContextSnapshot> {
        self.context.as_ref()
    }

    pub(crate) fn projected_messages(
        &self,
        prefix: &[TranscriptEventKind],
    ) -> std::result::Result<ProjectedRequest, PrepareRequestError> {
        self.projection.with_prefix(prefix)
    }

    pub(crate) fn tools(&self) -> Option<&BTreeMap<String, PreparedTool>> {
        self.prepared_tools.as_ref()
    }

    pub(crate) fn assess_validated_assistant(
        &self,
        message: &crate::AssistantMessage,
        step: StepId,
    ) -> std::result::Result<AssistantAssessment, crate::Failure> {
        if message.tool_calls.is_empty() && message.content.is_none() {
            return Err(crate::Failure::new(
                crate::FailureKind::ModelProtocol,
                "model completed without user-visible text or tool calls",
            ));
        }
        if message.tool_calls.len() > MAX_TOOL_CALLS_PER_STEP {
            return Err(crate::Failure::new(
                crate::FailureKind::ModelProtocol,
                "model response exceeded the per-step tool-call limit",
            ));
        }
        let mut response_ids = BTreeSet::new();
        if let Some(reused) = message.tool_calls.iter().find(|call| {
            self.seen_call_ids.contains(&call.id) || !response_ids.insert(call.id.clone())
        }) {
            return Err(crate::Failure::new(
                crate::FailureKind::ModelProtocol,
                format!("model reused tool call id `{}`", reused.id),
            ));
        }
        let projected_total = self
            .total_tool_calls
            .checked_add(message.tool_calls.len())
            .ok_or_else(|| {
                crate::Failure::new(
                    crate::FailureKind::CallLimitExceeded,
                    "model response overflowed the bounded tool-call budget",
                )
            })?;
        if projected_total > MAX_TOOL_CALLS_PER_TURN {
            return Ok(AssistantAssessment::Limit {
                failure: crate::Failure::new(
                    crate::FailureKind::CallLimitExceeded,
                    "model response exceeded the bounded tool-call budget",
                ),
                not_started_reason: "turn_limit_exceeded",
            });
        }
        if step.get() == u64::from(MAX_STEPS) && !message.tool_calls.is_empty() {
            return Ok(AssistantAssessment::Limit {
                failure: crate::Failure::new(
                    crate::FailureKind::StepLimitExceeded,
                    format!("turn reached the {MAX_STEPS}-step limit"),
                ),
                not_started_reason: "step_limit_exceeded",
            });
        }
        Ok(AssistantAssessment::Continue)
    }

    pub(crate) fn prepare_current_request(
        &self,
        source_through: EventSeq,
        request_id: &str,
    ) -> std::result::Result<ModelRequestSnapshot, PrepareRequestError> {
        let context = self.context.as_ref().ok_or_else(|| {
            PrepareRequestError::Corrupt(corrupt("model request has no context snapshot"))
        })?;
        prepare_projected_model_request(
            context,
            self.projection.with_prefix(&[])?,
            source_through,
            request_id,
        )
    }

    pub(crate) fn recovery_plan(&self) -> Result<RecoveryPlan> {
        if !self.turn_open || self.final_boundary.is_some() || !self.expected_calls.is_empty() {
            return Err(corrupt(
                "open session is not at a recoverable event boundary",
            ));
        }
        let open_step = self
            .step_open
            .ok_or_else(|| corrupt("open session has no open step"))?;
        let unfinished_calls = self
            .execution_order
            .iter()
            .map(|call_id| {
                self.calls
                    .get(call_id)
                    .map(|state| (call_id.clone(), state.dispatch_started))
                    .ok_or_else(|| corrupt("pending call disappeared from session state"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(RecoveryPlan {
            open_step,
            unfinished_calls,
        })
    }

    pub(crate) fn validate_terminal(&self, status: &RunStatus) -> Result<()> {
        if !self.saw_session || !self.saw_user || self.turn_open || self.step_open.is_some() {
            return Err(corrupt("terminal transcript has open or missing brackets"));
        }
        if let RunStatus::Failed { failure } = status {
            validate_failure(failure)?;
        }
        match (status, self.final_boundary.as_ref()) {
            (RunStatus::Completed { final_message }, Some(BoundaryOutcome::Completed))
                if self.assistant_final_in_step.as_ref() == Some(final_message) => {}
            (
                RunStatus::Failed { failure },
                Some(BoundaryOutcome::Failed { failure: boundary }),
            ) if failure == boundary => {}
            (RunStatus::Interrupted, Some(BoundaryOutcome::Interrupted)) => {}
            _ => return Err(corrupt("session status does not match turn/end")),
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn apply(
        &mut self,
        seq: EventSeq,
        event: &TranscriptEventKind,
        supplied_catalog: &mut Option<BTreeMap<String, PreparedTool>>,
        trust: PayloadTrust,
    ) -> Result<()> {
        if seq.get() != self.next_seq {
            return Err(corrupt(format!(
                "event sequence {} appeared where {} was required",
                seq.get(),
                self.next_seq
            )));
        }
        if self.final_boundary.is_some() {
            return Err(corrupt("events appear after turn/end"));
        }
        let index = usize::try_from(self.next_seq - 1).expect("bounded event index fits usize");
        match event {
            TranscriptEventKind::SessionStarted {
                model,
                prompt_sha256,
            } => {
                if index != 0
                    || self.saw_session
                    || prompt_sha256 != &sha256_hex(self.prompt.as_bytes())
                    || !is_wire_identifier(model)
                {
                    return Err(corrupt("invalid session/start event"));
                }
                self.model = Some(model.clone());
                self.saw_session = true;
            }
            TranscriptEventKind::TurnStarted => {
                if !self.saw_session || self.turn_open {
                    return Err(corrupt("turn/start is not the single outer turn"));
                }
                self.turn_open = true;
            }
            TranscriptEventKind::StepStarted { step } => {
                if !self.turn_open
                    || self.step_open.is_some()
                    || step.get() != self.next_step
                    || step.get() > u64::from(MAX_STEPS)
                    || (step.get() > 1
                        && !matches!(self.last_step_outcome, Some(BoundaryOutcome::Continued)))
                {
                    return Err(corrupt("step/start is not continuous and strictly nested"));
                }
                self.next_step += 1;
                self.step_open = Some(*step);
                self.last_step_outcome = None;
                self.request_seen_in_step = false;
                self.call_limit_exceeded_in_step = false;
                self.assistant_calls_in_step = None;
                self.assistant_final_in_step = None;
                self.step_has_noncontinuable_tool_outcome = false;
            }
            TranscriptEventKind::UserMessage { content } => {
                if self.saw_user
                    || self.step_open != Some(StepId::new(1))
                    || content != self.prompt.as_ref()
                {
                    return Err(corrupt("user message is not the exact first-step input"));
                }
                self.saw_user = true;
            }
            TranscriptEventKind::ContextSnapshot { context } => {
                if self.saw_context
                    || !self.saw_user
                    || self.step_open.is_none()
                    || self.request_pending
                    || self.model.as_deref() != Some(context.model.as_str())
                {
                    return Err(corrupt("context snapshot is misplaced or duplicated"));
                }
                self.prepared_tools =
                    Some(validate_context(context, supplied_catalog.take(), trust)?);
                self.context = Some(context.clone());
                self.saw_context = true;
            }
            TranscriptEventKind::ModelRequestPrepared { request } => {
                if !self.saw_context
                    || self.step_open.is_none()
                    || self.request_pending
                    || self.request_seen_in_step
                    || !self.expected_calls.is_empty()
                    || !self.execution_order.is_empty()
                    || self.active_dispatch.is_some()
                    || self.calls.values().any(|state| !state.finished)
                {
                    return Err(corrupt(
                        "model request was prepared outside a quiescent step",
                    ));
                }
                let source = seq
                    .get()
                    .checked_sub(1)
                    .filter(|source| *source != 0)
                    .map(EventSeq::new)
                    .ok_or_else(|| corrupt("model request has no durable source prefix"))?;
                if request.source_through != source {
                    return Err(corrupt(
                        "model request source cursor is not its exact prefix",
                    ));
                }
                let step = self.step_open.expect("model request requires an open step");
                if request.request_id != format!("model-{}", step.get()) {
                    return Err(corrupt(
                        "model request id is not deterministic for its step",
                    ));
                }
                if trust == PayloadTrust::Durable {
                    let context = self
                        .context
                        .as_ref()
                        .ok_or_else(|| corrupt("model request has no context snapshot"))?;
                    let derived = prepare_projected_model_request(
                        context,
                        self.projection
                            .with_prefix(&[])
                            .map_err(|error| match error {
                                PrepareRequestError::ContextLimit(message) => corrupt(format!(
                                    "stored model request no longer encodes: {message}"
                                )),
                                PrepareRequestError::Corrupt(error) => error,
                            })?,
                        source,
                        &request.request_id,
                    )
                    .map_err(|error| match error {
                        PrepareRequestError::ContextLimit(message) => {
                            corrupt(format!("stored model request no longer encodes: {message}"))
                        }
                        PrepareRequestError::Corrupt(error) => error,
                    })?;
                    if &derived != request {
                        return Err(corrupt(
                            "stored model request does not match its log projection",
                        ));
                    }
                }
                self.request_pending = true;
                self.model_call_prepared = false;
                self.model_attempt = 0;
                self.prepared_snapshot = None;
                self.request_seen_in_step = true;
            }
            TranscriptEventKind::ModelCallPrepared {
                request_id,
                snapshot,
            } => {
                let context = self
                    .context
                    .as_ref()
                    .ok_or_else(|| corrupt("prepared model call has no context"))?;
                let step = self
                    .step_open
                    .ok_or_else(|| corrupt("prepared model call has no open step"))?;
                if !self.request_pending
                    || self.model_call_prepared
                    || request_id != &format!("model-{}", step.get())
                    || snapshot.capability != rsi_ai_meta::Capability::Language
                    || snapshot.model != context.model
                {
                    return Err(corrupt(
                        "prepared model call does not match the pending request",
                    ));
                }
                snapshot
                    .validate()
                    .map_err(|error| corrupt(format!("prepared snapshot is invalid: {error}")))?;
                self.model_attempt = self
                    .model_attempt
                    .checked_add(1)
                    .ok_or_else(|| corrupt("model attempt counter overflowed"))?;
                self.model_call_prepared = true;
                self.prepared_snapshot = Some(snapshot.clone());
            }
            TranscriptEventKind::ModelRetryScheduled {
                request_id,
                failed_attempt,
                error,
                delay_ms,
            } => {
                let step = self
                    .step_open
                    .ok_or_else(|| corrupt("model retry has no open step"))?;
                let snapshot = self
                    .prepared_snapshot
                    .as_ref()
                    .ok_or_else(|| corrupt("model retry has no prepared attempt"))?;
                error
                    .validate()
                    .map_err(|reason| corrupt(format!("model retry error is invalid: {reason}")))?;
                if !self.request_pending
                    || !self.model_call_prepared
                    || request_id != &format!("model-{}", step.get())
                    || *failed_attempt != self.model_attempt
                    || self.model_attempt > snapshot.retry_policy.max_retries()
                    || !snapshot.retry_policy.retries(error.kind())
                    || *delay_ms == 0
                    || *delay_ms > snapshot.retry_policy.max_delay_ms()
                {
                    return Err(corrupt("model retry does not match its prepared attempt"));
                }
                self.model_call_prepared = false;
                self.prepared_snapshot = None;
            }
            TranscriptEventKind::AssistantMessage { message } => {
                if !self.request_pending || !self.model_call_prepared || self.step_open.is_none() {
                    return Err(corrupt("assistant message has no pending model request"));
                }
                if trust == PayloadTrust::Durable {
                    validate_assistant(message)?;
                }
                self.request_pending = false;
                self.model_call_prepared = false;
                self.prepared_snapshot = None;
                self.assistant_calls_in_step = Some(message.tool_calls.len());
                self.total_tool_calls = self
                    .total_tool_calls
                    .checked_add(message.tool_calls.len())
                    .ok_or_else(|| corrupt("tool call count overflowed"))?;
                if message.tool_calls.len() > MAX_TOOL_CALLS_PER_STEP {
                    return Err(corrupt("assistant tool calls exceed the step limit"));
                }
                self.call_limit_exceeded_in_step = self.total_tool_calls > MAX_TOOL_CALLS_PER_TURN;
                if message.tool_calls.is_empty() {
                    self.assistant_final_in_step.clone_from(&message.content);
                }
                self.expected_calls
                    .extend(message.tool_calls.iter().cloned());
            }
            TranscriptEventKind::ToolCallPrepared { call } => {
                let expected = self
                    .expected_calls
                    .pop_front()
                    .ok_or_else(|| corrupt("prepared call is absent from assistant message"))?;
                if &expected != call
                    || self.calls.contains_key(&call.id)
                    || !self.seen_call_ids.insert(call.id.clone())
                {
                    return Err(corrupt(
                        "prepared call does not exactly match assistant output",
                    ));
                }
                self.calls.insert(
                    call.id.clone(),
                    CallState {
                        call: call.clone(),
                        dispatch_started: false,
                        finished: false,
                    },
                );
                self.execution_order.push_back(call.id.clone());
            }
            TranscriptEventKind::ToolDispatchStarted { call_id } => {
                if !self.expected_calls.is_empty() {
                    return Err(corrupt(
                        "tool dispatch began before every call was prepared",
                    ));
                }
                let state = self
                    .calls
                    .get_mut(call_id)
                    .ok_or_else(|| corrupt("dispatch has no prepared call"))?;
                if state.dispatch_started || state.finished {
                    return Err(corrupt("tool dispatch was duplicated"));
                }
                if self.call_limit_exceeded_in_step {
                    return Err(corrupt("tool dispatch occurred after the turn call limit"));
                }
                if self.step_open == Some(StepId::new(u64::from(MAX_STEPS))) {
                    return Err(corrupt("tool dispatch occurred on the final model step"));
                }
                if self.execution_order.front() != Some(call_id) || self.active_dispatch.is_some() {
                    return Err(corrupt("tool dispatch is not strictly serial model order"));
                }
                if trust == PayloadTrust::Durable {
                    let tools = self
                        .prepared_tools
                        .as_ref()
                        .ok_or_else(|| corrupt("tool dispatch has no captured catalog"))?;
                    let tool = tools
                        .get(&state.call.name)
                        .ok_or_else(|| corrupt("unknown tool was durably dispatched"))?;
                    validate_arguments(tool, &state.call.arguments).map_err(
                        |error| match error {
                            ArgumentError::InvalidJson => {
                                corrupt("dispatched tool arguments are not strict bounded JSON")
                            }
                            ArgumentError::LossyNumber => {
                                corrupt("dispatched tool arguments contain a lossy number")
                            }
                            ArgumentError::SchemaMismatch => corrupt(
                                "dispatched tool arguments do not match the captured schema",
                            ),
                        },
                    )?;
                }
                state.dispatch_started = true;
                self.active_dispatch = Some(call_id.clone());
            }
            TranscriptEventKind::ToolResult { call_id, outcome } => {
                if !self.expected_calls.is_empty() {
                    return Err(corrupt("tool result preceded preparation of sibling calls"));
                }
                let state = self
                    .calls
                    .get_mut(call_id)
                    .ok_or_else(|| corrupt("tool result has no prepared call"))?;
                if state.finished {
                    return Err(corrupt("tool call has more than one terminal result"));
                }
                if self.execution_order.front() != Some(call_id) {
                    return Err(corrupt("tool result is not in model call order"));
                }
                if !state.dispatch_started && trust == PayloadTrust::Durable {
                    validate_undispatched_outcome(
                        self.prepared_tools.as_ref(),
                        &state.call,
                        outcome,
                    )?;
                }
                if self.call_limit_exceeded_in_step
                    && !matches!(outcome, ToolOutcome::NotStarted { .. })
                {
                    return Err(corrupt(
                        "call-limit rejection has a result other than not-started",
                    ));
                }
                match (state.dispatch_started, outcome) {
                    (true, ToolOutcome::NotStarted { .. }) => {
                        return Err(corrupt("dispatched tool call is marked not started"));
                    }
                    (false, ToolOutcome::Succeeded { .. } | ToolOutcome::OutcomeUnknown) => {
                        return Err(corrupt(
                            "tool result implies a dispatch that was not logged",
                        ));
                    }
                    _ => {}
                }
                if state.dispatch_started {
                    if self.active_dispatch.as_ref() != Some(call_id) {
                        return Err(corrupt("tool result does not close the active dispatch"));
                    }
                    self.active_dispatch = None;
                } else if self.active_dispatch.is_some() {
                    return Err(corrupt("undispatched result overlaps an active dispatch"));
                }
                if trust == PayloadTrust::Durable {
                    validate_tool_outcome(call_id, outcome)?;
                }
                if matches!(
                    outcome,
                    ToolOutcome::NotStarted { .. } | ToolOutcome::OutcomeUnknown
                ) {
                    self.step_has_noncontinuable_tool_outcome = true;
                }
                state.finished = true;
                self.execution_order.pop_front();
            }
            TranscriptEventKind::StepEnded { step, outcome } => {
                if self.step_open != Some(*step)
                    || !self.expected_calls.is_empty()
                    || !self.execution_order.is_empty()
                    || self.active_dispatch.is_some()
                    || self.calls.values().any(|state| !state.finished)
                {
                    return Err(corrupt("step/end does not close a quiescent open step"));
                }
                if let BoundaryOutcome::Failed { failure } = outcome {
                    validate_failure(failure)?;
                }
                if self.step_has_noncontinuable_tool_outcome
                    && matches!(
                        outcome,
                        BoundaryOutcome::Continued | BoundaryOutcome::Completed
                    )
                {
                    return Err(corrupt(
                        "uncertain or unstarted tool work continued into model-visible history",
                    ));
                }
                match outcome {
                    BoundaryOutcome::Continued | BoundaryOutcome::Completed
                        if self.request_pending =>
                    {
                        return Err(corrupt(
                            "successful step ended with a pending model request",
                        ));
                    }
                    BoundaryOutcome::Continued
                        if self.assistant_calls_in_step.unwrap_or(0) == 0 =>
                    {
                        return Err(corrupt("continued step contains no tool calls"));
                    }
                    BoundaryOutcome::Completed
                        if self.assistant_calls_in_step != Some(0)
                            || self
                                .assistant_final_in_step
                                .as_deref()
                                .is_none_or(str::is_empty) =>
                    {
                        return Err(corrupt("completed step has no final assistant text"));
                    }
                    _ => {}
                }
                match (self.call_limit_exceeded_in_step, outcome) {
                    (true, BoundaryOutcome::Failed { failure })
                        if failure.kind == crate::FailureKind::CallLimitExceeded => {}
                    (true, BoundaryOutcome::Interrupted) => {}
                    (true, _) => {
                        return Err(corrupt(
                            "call-limit step did not close with call-limit failure",
                        ));
                    }
                    (false, BoundaryOutcome::Failed { failure })
                        if failure.kind == crate::FailureKind::CallLimitExceeded =>
                    {
                        return Err(corrupt("call-limit failure has no exceeded call budget"));
                    }
                    (false, _) => {}
                }
                let step_limit_reached = step.get() == u64::from(MAX_STEPS)
                    && self.assistant_calls_in_step.unwrap_or(0) > 0
                    && !self.call_limit_exceeded_in_step;
                match (step_limit_reached, outcome) {
                    (true, BoundaryOutcome::Failed { failure })
                        if failure.kind == crate::FailureKind::StepLimitExceeded => {}
                    (true, BoundaryOutcome::Interrupted) => {}
                    (true, _) => {
                        return Err(corrupt(
                            "final-step tool calls did not close with step-limit failure",
                        ));
                    }
                    (false, BoundaryOutcome::Failed { failure })
                        if failure.kind == crate::FailureKind::StepLimitExceeded =>
                    {
                        return Err(corrupt("step-limit failure occurred before the final step"));
                    }
                    (false, _) => {}
                }
                self.request_pending = false;
                self.model_call_prepared = false;
                self.prepared_snapshot = None;
                self.calls.clear();
                self.step_open = None;
                self.last_step_outcome = Some(outcome.clone());
            }
            TranscriptEventKind::TurnEnded { outcome } => {
                if !self.turn_open
                    || self.step_open.is_some()
                    || matches!(outcome, BoundaryOutcome::Continued)
                    || self.last_step_outcome.as_ref() != Some(outcome)
                {
                    return Err(corrupt("turn/end is misplaced or duplicated"));
                }
                if let BoundaryOutcome::Failed { failure } = outcome {
                    validate_failure(failure)?;
                }
                self.turn_open = false;
                self.final_boundary = Some(outcome.clone());
            }
        }
        self.projection.apply(event)?;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or_else(|| corrupt("event sequence exhausted"))?;
        Ok(())
    }
}

pub(crate) fn terminal_status(events: &[TranscriptEvent]) -> Result<RunStatus> {
    let Some(TranscriptEventKind::TurnEnded { outcome }) = events.last().map(TranscriptEvent::kind)
    else {
        return Err(corrupt("terminal transcript does not end with turn/end"));
    };
    match outcome {
        BoundaryOutcome::Completed => {
            let final_message = events
                .iter()
                .rev()
                .find_map(|event| match event.kind() {
                    TranscriptEventKind::AssistantMessage { message }
                        if message.tool_calls.is_empty() =>
                    {
                        message.content.clone()
                    }
                    _ => None,
                })
                .filter(|message| !message.is_empty())
                .ok_or_else(|| corrupt("completed transcript has no final assistant message"))?;
            Ok(RunStatus::Completed { final_message })
        }
        BoundaryOutcome::Failed { failure } => Ok(RunStatus::Failed {
            failure: failure.clone(),
        }),
        BoundaryOutcome::Interrupted => Ok(RunStatus::Interrupted),
        BoundaryOutcome::Continued => Err(corrupt("terminal turn has a continued outcome")),
    }
}

fn model_request(
    context: &ContextSnapshot,
    projection: ProjectedRequest,
) -> std::result::Result<LanguageRequest, String> {
    let mut rich = vec![
        Message::system_text(context.system_prompt.clone()).map_err(|error| error.to_string())?,
    ];
    rich.extend(projection.messages);
    let tools = context
        .tools
        .iter()
        .map(|tool| {
            rsi_ai_protocol::ToolDefinition::new(
                tool.name.clone(),
                tool.description.clone(),
                tool.input_schema.clone(),
            )
            .map_err(|error| error.to_string())
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    LanguageRequest::new(rich)
        .and_then(|request| request.with_tools(tools, ToolChoice::Auto))
        .and_then(|request| request.with_extensions(projection.replay.into_iter().collect()))
        .map_err(|error| error.to_string())
}

fn wire_tool_result(outcome: &ToolOutcome) -> WireToolResult {
    match outcome {
        ToolOutcome::Succeeded { value } => WireToolResult::Ok {
            value: value.clone(),
        },
        ToolOutcome::Failed { code, message } => WireToolResult::Error {
            code: code.clone(),
            message: message.clone(),
        },
        ToolOutcome::NotStarted { reason } => WireToolResult::Error {
            code: "not_started".to_owned(),
            message: reason.clone(),
        },
        ToolOutcome::OutcomeUnknown => WireToolResult::Error {
            code: "outcome_unknown".to_owned(),
            message: "the tool may have run, but no durable result was recorded".to_owned(),
        },
    }
}

fn validate_context(
    context: &ContextSnapshot,
    prepared: Option<BTreeMap<String, PreparedTool>>,
    trust: PayloadTrust,
) -> Result<BTreeMap<String, PreparedTool>> {
    validate_context_versions(
        context,
        prepared,
        trust,
        rsi_ai_meta::AiService::Language.version(),
        rsi_agent_protocol::WIRE_VERSION,
    )
}

fn validate_context_versions(
    context: &ContextSnapshot,
    prepared: Option<BTreeMap<String, PreparedTool>>,
    trust: PayloadTrust,
    model_protocol_version: u32,
    tools_protocol_version: u32,
) -> Result<BTreeMap<String, PreparedTool>> {
    if context.system_prompt.is_empty()
        || context.system_prompt.chars().count() > crate::MAX_SYSTEM_PROMPT_CHARS
        || context
            .system_prompt
            .chars()
            .any(|character| character == '\0' || character == '\u{007f}')
        || !is_wire_identifier(&context.model_provider)
        || !is_wire_identifier(&context.model)
        || !is_wire_identifier(&context.tools_provider)
        || context.model_protocol_version != model_protocol_version
        || context.tools_protocol_version != tools_protocol_version
    {
        return Err(corrupt(
            "context snapshot has invalid identity or system prompt",
        ));
    }
    if trust == PayloadTrust::Durable {
        let catalog = rsi_agent_protocol::ToolsCatalogResponse {
            tools: context.tools.clone(),
        };
        catalog
            .validate("context.tools")
            .map_err(|error| corrupt(format!("context tool catalog is invalid: {error}")))?;
    }
    if let Some(prepared) = prepared {
        let expected = context
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if expected.len() != context.tools.len()
            || !prepared.keys().map(String::as_str).eq(expected)
        {
            return Err(corrupt(
                "precompiled catalog does not match committed context",
            ));
        }
        Ok(prepared)
    } else if trust == PayloadTrust::Durable {
        prepare_catalog(&context.tools)
            .map_err(|error| corrupt(format!("context tool catalog cannot be compiled: {error}")))
    } else {
        Err(corrupt(
            "validated context transition has no compiled tool catalog",
        ))
    }
}

fn validate_assistant(message: &crate::AssistantMessage) -> Result<()> {
    let mut content = Vec::new();
    if let Some(text) = &message.content {
        content.push(rsi_ai_protocol::ContentBlock::Text { text: text.clone() });
    }
    if let Some(text) = &message.reasoning {
        content.push(rsi_ai_protocol::ContentBlock::Reasoning { text: text.clone() });
    }
    content.extend(message.tool_calls.iter().map(|call| {
        rsi_ai_protocol::ContentBlock::ToolCall(rsi_ai_protocol::ToolCall {
            id: call.id.as_str().to_owned(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
    }));
    let output = rsi_ai_protocol::LanguageOutput {
        content,
        finish_reason: message.finish_reason.clone(),
        usage: message.usage,
        replay: message.replay.clone(),
        warnings: message.warnings.clone(),
        sources: message.sources.clone(),
    };
    output
        .validate()
        .map_err(|error| corrupt(format!("assistant message is invalid: {error}")))
}

fn validate_tool_outcome(_call_id: &CallId, outcome: &ToolOutcome) -> Result<()> {
    let result = wire_tool_result(outcome);
    result
        .validate("result")
        .map_err(|error| corrupt(format!("tool result is invalid: {error}")))
}

fn validate_undispatched_outcome(
    tools: Option<&BTreeMap<String, PreparedTool>>,
    call: &ToolCall,
    outcome: &ToolOutcome,
) -> Result<()> {
    let tools = tools.ok_or_else(|| corrupt("tool result has no captured catalog"))?;
    match outcome {
        ToolOutcome::Failed { code, .. } if code == "unknown_tool" => {
            if tools.contains_key(&call.name) {
                return Err(corrupt("known tool is recorded as unknown"));
            }
        }
        ToolOutcome::Failed { code, .. } if code == "invalid_arguments" => {
            let tool = tools
                .get(&call.name)
                .ok_or_else(|| corrupt("unknown tool is recorded with invalid arguments"))?;
            if validate_arguments(tool, &call.arguments).is_ok() {
                return Err(corrupt("valid tool arguments are recorded as invalid"));
            }
        }
        ToolOutcome::Failed { .. } => {
            return Err(corrupt(
                "undispatched tool failure has no runtime rejection reason",
            ));
        }
        ToolOutcome::NotStarted { .. } => {}
        ToolOutcome::Succeeded { .. } | ToolOutcome::OutcomeUnknown => {
            return Err(corrupt("undispatched tool outcome implies execution"));
        }
    }
    Ok(())
}

fn validate_failure(failure: &crate::Failure) -> Result<()> {
    if failure.message.is_empty()
        || failure.message.len() > crate::domain::MAX_FAILURE_MESSAGE_BYTES
        || failure
            .message
            .chars()
            .any(|character| character == '\0' || character == '\u{007f}')
    {
        return Err(corrupt("terminal failure message is invalid or unbounded"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn model_and_tool_protocol_versions_evolve_independently() {
        let context = ContextSnapshot {
            system_prompt: "system".to_owned(),
            model: "model".to_owned(),
            model_provider: "provider".to_owned(),
            model_protocol_version: 7,
            tools_provider: "tools".to_owned(),
            tools_protocol_version: 0,
            tools: Vec::new(),
        };

        validate_context_versions(
            &context,
            Some(BTreeMap::new()),
            PayloadTrust::ValidatedLive,
            7,
            0,
        )
        .expect("one service version can advance without changing the other");
        assert!(
            validate_context_versions(
                &context,
                Some(BTreeMap::new()),
                PayloadTrust::ValidatedLive,
                0,
                0,
            )
            .is_err()
        );
    }

    #[test]
    fn oversized_projection_releases_retained_model_messages() {
        let mut projection = ModelProjection::new();
        projection
            .apply(&TranscriptEventKind::UserMessage {
                content: "hello".to_owned(),
            })
            .expect("user message");
        for index in 0..8 {
            projection
                .apply(&TranscriptEventKind::ToolResult {
                    call_id: CallId::new(format!("projection-{index}")).expect("call id"),
                    outcome: ToolOutcome::Succeeded {
                        value: json!({"blob":"x".repeat(100 * 1024)}),
                    },
                })
                .expect("tool result");
        }

        assert!(projection.overflowed);
        assert!(projection.messages.is_empty());
        assert!(matches!(
            projection.with_prefix(&[]),
            Err(PrepareRequestError::ContextLimit(_))
        ));
    }
}

#[derive(Debug)]
struct CallState {
    call: ToolCall,
    dispatch_started: bool,
    finished: bool,
}
