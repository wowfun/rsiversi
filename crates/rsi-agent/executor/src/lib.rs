//! Ordinary durable Agent executor over exact Local dependencies.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_composition_protocol::AgentCompositionPin;
use rsi_agent_context::{ContextFold, ContextLimits};
use rsi_agent_session_protocol::{
    BudgetDimension, EffectId, EffectKind, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, SessionFact,
    SessionFactBody, TurnOutcome,
};
use rsi_agent_turn_protocol::{
    ContextCheckpoint, PublishAttempt, TurnClaim, TurnError, TurnExecution, TurnExecutionContract,
    TurnFinalization, TurnFinalizationContext, TurnFinalizationContract, TurnFinalizationError,
};
use rsi_ai_protocol::{
    DispatchStatus, ErrorKind, FinishReason, ImageAssembler, ImageCall, ImageCallContract,
    ImageEvent, ImageRequest, LanguageAssembler, LanguageAssemblyError, LanguageCall,
    LanguageCallContract, LanguageEvent, ModelRef, PreparedCallSnapshot, ToolCall as ModelToolCall,
    ToolCallKind,
};
use rsi_approval_protocol::{
    Approval, ApprovalContract, ApprovalDecision, ApprovalError, ApprovalRequest, ApprovalSubject,
};
use rsi_jobs::{JobScopeAuthority, JobScopeId, Jobs, JobsContract};
use rsi_media_protocol::{Media, MediaContract, MediaRef};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_sandbox::{Sandbox, SandboxContract, SandboxMode};
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolFailureKind, RetainedToolResult, ToolCall, ToolError,
    ToolExecutionPolicy, ToolResult, ToolResultIdentity, ToolStart, parse_tool_arguments,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const EXECUTOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(200);
const MAXIMUM_DURABILITY_WAIT_MS: u64 = 5 * 60 * 1_000;
const MAXIMUM_FINALIZATION_WAIT_MS: u64 = 5 * 60 * 1_000;
const MAXIMUM_RETAINED_TOOL_WAIT_MS: u64 = 5 * 60 * 1_000;

/// Explicit executor instance configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorConfig {
    /// Exact registration identity, unique in one Kernel generation.
    pub executor_id: String,
    /// Maximum projected Language messages.
    #[serde(default = "default_context_messages")]
    pub max_context_messages: usize,
    /// Maximum projected canonical message bytes.
    #[serde(default = "default_context_bytes")]
    pub max_context_bytes: usize,
    /// Maximum wait for one exact Fact prefix to become durable.
    #[serde(default = "default_durability_wait_ms")]
    pub durability_wait_ms: u64,
    /// Maximum wait for the complete pre-terminal finalizer snapshot.
    #[serde(default = "default_finalization_wait_ms")]
    pub finalization_wait_ms: u64,
    /// Maximum background wait for a retained Tool to settle after terminal durability.
    #[serde(default = "default_retained_tool_wait_ms")]
    pub retained_tool_wait_ms: u64,
}

const fn default_context_messages() -> usize {
    rsi_agent_context::DEFAULT_CONTEXT_MESSAGES
}

const fn default_context_bytes() -> usize {
    rsi_agent_context::DEFAULT_CONTEXT_BYTES
}

const fn default_durability_wait_ms() -> u64 {
    30_000
}

const fn default_finalization_wait_ms() -> u64 {
    30_000
}

const fn default_retained_tool_wait_ms() -> u64 {
    30_000
}

impl ExecutorConfig {
    fn validate(&self) -> Result<()> {
        rsi_agent_session_protocol::validate_identifier("executor", &self.executor_id)
            .map_err(|error| ExecutorError::Invalid(error.to_string()))?;
        ContextLimits::new(self.max_context_messages, self.max_context_bytes)
            .map_err(|error| ExecutorError::Invalid(error.to_string()))?;
        if self.durability_wait_ms == 0 || self.durability_wait_ms > MAXIMUM_DURABILITY_WAIT_MS {
            return Err(ExecutorError::Invalid(format!(
                "durability_wait_ms must be within 1..={MAXIMUM_DURABILITY_WAIT_MS}"
            )));
        }
        if self.finalization_wait_ms == 0
            || self.finalization_wait_ms > MAXIMUM_FINALIZATION_WAIT_MS
        {
            return Err(ExecutorError::Invalid(format!(
                "finalization_wait_ms must be within 1..={MAXIMUM_FINALIZATION_WAIT_MS}"
            )));
        }
        if self.retained_tool_wait_ms == 0
            || self.retained_tool_wait_ms > MAXIMUM_RETAINED_TOOL_WAIT_MS
        {
            return Err(ExecutorError::Invalid(format!(
                "retained_tool_wait_ms must be within 1..={MAXIMUM_RETAINED_TOOL_WAIT_MS}"
            )));
        }
        Ok(())
    }

    fn limits(&self) -> ContextLimits {
        ContextLimits {
            max_messages: self.max_context_messages,
            max_bytes: self.max_context_bytes,
        }
    }

    const fn durability_wait(&self) -> Duration {
        Duration::from_millis(self.durability_wait_ms)
    }

    const fn finalization_wait(&self) -> Duration {
        Duration::from_millis(self.finalization_wait_ms)
    }

    const fn retained_tool_wait(&self) -> Duration {
        Duration::from_millis(self.retained_tool_wait_ms)
    }
}

#[derive(Debug)]
struct Driver {
    turns: Arc<dyn TurnExecution>,
    finalization: Arc<dyn TurnFinalization>,
    language: Arc<dyn LanguageCall>,
    image: Arc<dyn ImageCall>,
    media: Arc<dyn Media>,
    approval: Arc<dyn Approval>,
    sandbox: Arc<dyn Sandbox>,
    jobs: Arc<dyn Jobs>,
    active_tools: Mutex<BTreeMap<(String, String), TrackedTool>>,
    retirement_tasks: Mutex<Vec<JoinHandle<()>>>,
    checkpoint_tx: watch::Sender<Option<CheckpointRequest>>,
    config: ExecutorConfig,
}

#[derive(Clone, Debug)]
struct TrackedTool {
    composition: AgentCompositionPin,
    identity: ToolResultIdentity,
}

#[derive(Clone, Debug)]
struct CheckpointRequest {
    claim: TurnClaim,
    limits: ContextLimits,
}

async fn select_drive_or_stop(
    stop: &CancellationToken,
    drive: impl Future<Output = std::result::Result<(), DriveFailure>>,
) -> std::result::Result<(), DriveFailure> {
    tokio::select! {
        biased;
        result = drive => result,
        () = stop.cancelled() => Err(DriveFailure::Stopped),
    }
}

const fn elapsed_deadline_wins(
    deadline_fired: bool,
    drive: &std::result::Result<(), DriveFailure>,
) -> bool {
    deadline_fired && matches!(drive, Err(DriveFailure::Stopped))
}

impl Driver {
    async fn run(self: Arc<Self>, stop: CancellationToken) {
        loop {
            let Some(claim) = self.claim_next(&stop).await else {
                break;
            };
            let job_scope = match self.acquire_job_scope(&claim) {
                Ok(scope) => Some(scope),
                Err(message) => {
                    let _ignored = self
                        .finish(
                            &claim,
                            None,
                            failure_outcome("jobs.scope", bounded(&message)),
                        )
                        .await;
                    let _ignored = self.turns.release(&claim);
                    continue;
                }
            };
            let composition = match self.turns.composition(&claim) {
                Ok(composition) => composition,
                Err(error) => {
                    self.finish_context_error(&claim, job_scope.as_ref(), error.to_string())
                        .await;
                    let _ignored = self.turns.release(&claim);
                    continue;
                }
            };
            let claim_stop = stop.child_token();
            let deadline_fired = Arc::new(AtomicBool::new(false));
            let elapsed = unix_now_ms().saturating_sub(claim.accepted_at_ms());
            let limit = claim.header().settings().turn_budget().maximum_elapsed_ms();
            let remaining = limit.saturating_sub(elapsed);
            let deadline_task = tokio::spawn({
                let deadline_fired = Arc::clone(&deadline_fired);
                let claim_stop = claim_stop.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(remaining)).await;
                    deadline_fired.store(true, Ordering::Release);
                    claim_stop.cancel();
                }
            });
            let limits = self.context_limits();
            let mut fold = match ContextFold::with_limits(claim.header().clone(), limits) {
                Ok(fold) => fold,
                Err(error) => {
                    deadline_task.abort();
                    self.finish_context_error(&claim, job_scope.as_ref(), error.to_string())
                        .await;
                    let _ignored = self.turns.release(&claim);
                    continue;
                }
            };
            let drive = select_drive_or_stop(
                &claim_stop,
                self.drive(
                    &claim,
                    &composition,
                    job_scope.as_ref(),
                    &claim_stop,
                    &mut fold,
                ),
            )
            .await;
            deadline_task.abort();
            if elapsed_deadline_wins(deadline_fired.load(Ordering::Acquire), &drive) {
                let consumed = unix_now_ms()
                    .saturating_sub(claim.accepted_at_ms())
                    .max(limit);
                if self
                    .finish_budget(
                        &claim,
                        job_scope.as_ref(),
                        BudgetDimension::Elapsed,
                        consumed,
                        limit,
                    )
                    .await
                    .is_ok()
                {
                    self.request_checkpoint(&claim);
                    self.retire_tracked_tool(&claim, &stop);
                }
                let _ignored = self.turns.release(&claim);
                continue;
            }
            let stopped = self
                .settle_drive(&claim, &composition, job_scope.as_ref(), drive, &stop)
                .await;
            let _ignored = self.turns.release(&claim);
            if stopped {
                break;
            }
        }
        self.abort_retirement_tasks().await;
    }

    async fn claim_next(&self, stop: &CancellationToken) -> Option<TurnClaim> {
        self.reap_retirement_tasks();
        self.turns
            .claim(&self.config.executor_id, stop.clone())
            .await
            .ok()
            .flatten()
    }

    fn acquire_job_scope(
        &self,
        claim: &TurnClaim,
    ) -> std::result::Result<JobScopeAuthority, String> {
        let id = JobScopeId::new(
            "rsi.agent.turn",
            [claim.session_id().as_str(), claim.turn_id().as_str()],
        )
        .map_err(|error| error.to_string())?;
        self.jobs
            .acquire_scope(id)
            .map_err(|error| error.to_string())
    }

    async fn settle_drive(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        drive: std::result::Result<(), DriveFailure>,
        stop: &CancellationToken,
    ) -> bool {
        match drive {
            Ok(()) => false,
            Err(DriveFailure::Stopped) => true,
            Err(DriveFailure::Turn(outcome)) => {
                if self.finish(claim, job_scope, outcome).await.is_ok() {
                    self.request_checkpoint(claim);
                    self.retire_tracked_tool(claim, stop);
                }
                false
            }
            Err(DriveFailure::SettledTool { outcome, identity }) => {
                if self.finish(claim, job_scope, outcome).await.is_ok() {
                    self.request_checkpoint(claim);
                    let _ignored = composition.tools().commit(&identity);
                    self.clear_tracked_tool(claim, &identity);
                }
                false
            }
            Err(DriveFailure::Budget {
                dimension,
                consumed,
                limit,
            }) => {
                if self
                    .finish_budget(claim, job_scope, dimension, consumed, limit)
                    .await
                    .is_ok()
                {
                    self.request_checkpoint(claim);
                    self.retire_tracked_tool(claim, stop);
                }
                false
            }
            Err(DriveFailure::DurableBudget {
                dimension,
                consumed,
                limit,
            }) => {
                if publish_terminal(
                    self.turns.as_ref(),
                    &self.config,
                    claim,
                    TurnOutcome::BudgetExceeded {
                        dimension,
                        consumed,
                        limit,
                    },
                )
                .await
                .is_ok()
                {
                    self.request_checkpoint(claim);
                    self.retire_tracked_tool(claim, stop);
                }
                false
            }
            Err(DriveFailure::SettledToolBudget {
                dimension,
                consumed,
                limit,
                identity,
            }) => {
                if self
                    .finish_budget(claim, job_scope, dimension, consumed, limit)
                    .await
                    .is_ok()
                {
                    self.request_checkpoint(claim);
                    let _ignored = composition.tools().commit(&identity);
                    self.clear_tracked_tool(claim, &identity);
                }
                false
            }
            Err(DriveFailure::Fatal(message)) => {
                if self
                    .finish(
                        claim,
                        job_scope,
                        TurnOutcome::Failed {
                            code: "executor.internal".into(),
                            message,
                        },
                    )
                    .await
                    .is_ok()
                {
                    self.request_checkpoint(claim);
                    self.retire_tracked_tool(claim, stop);
                }
                false
            }
        }
    }

    async fn drive(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        stop: &CancellationToken,
        fold: &mut ContextFold,
    ) -> std::result::Result<(), DriveFailure> {
        let state = self.load_claim(claim, fold).await?;
        if state.terminal {
            return Ok(());
        }
        if let Some((dimension, consumed, limit)) = state.budget_exhausted {
            return Err(DriveFailure::DurableBudget {
                dimension,
                consumed,
                limit,
            });
        }
        if state.completed_model_without_successor {
            return Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                effect: Some(EffectKind::Model),
                reason: "a completed model effect lacks a durable terminal or successor intent"
                    .into(),
            }));
        }
        self.resume_effect(claim, composition, fold, state.effect, stop)
            .await?;

        if let Some((model, request)) = state.image {
            return self.run_image(claim, fold, model, request, stop).await;
        }

        let turn_policy = state.turn_policy.ok_or_else(|| {
            failed(
                "executor.invalid_history",
                "Language turn lacks a resolved execution policy",
            )
        })?;
        let model = state
            .model
            .unwrap_or_else(|| claim.header().settings().default_model().clone());
        self.run_language(
            claim,
            composition,
            job_scope,
            fold,
            model,
            turn_policy,
            stop,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // Keep the exact claim generation, durable fold, policy, Jobs authority, and cancellation owner explicit.
    async fn run_language(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        fold: &mut ContextFold,
        model: ModelRef,
        turn_policy: ResolvedTurnPolicy,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let mut retry_attempt = 0_u8;
        loop {
            if stop.is_cancelled() {
                return Err(DriveFailure::Stopped);
            }
            let cancellation = self.turns.cancellation(claim).map_err(fatal)?;
            if cancellation.is_cancelled() {
                return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
            }
            let output = match self
                .run_model_attempt(
                    claim,
                    composition,
                    fold,
                    &model,
                    retry_attempt,
                    &cancellation,
                    stop,
                )
                .await?
            {
                ModelAttempt::Retry => {
                    retry_attempt = retry_attempt.saturating_add(1);
                    continue;
                }
                ModelAttempt::Output(output) => *output,
            };
            if cancellation.is_cancelled()
                || matches!(output.finish_reason, FinishReason::Cancelled)
            {
                return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
            }
            if !matches!(output.finish_reason, FinishReason::ToolCalls) {
                return Err(DriveFailure::Turn(TurnOutcome::Completed));
            }
            retry_attempt = 0;
            let calls = output
                .content
                .into_iter()
                .filter_map(|content| match content {
                    rsi_ai_protocol::ContentBlock::ToolCall(call) => Some(call),
                    rsi_ai_protocol::ContentBlock::Text { .. }
                    | rsi_ai_protocol::ContentBlock::Reasoning { .. } => None,
                })
                .collect::<Vec<_>>();
            if calls.is_empty() {
                return Err(failed(
                    "provider.tool_calls_missing",
                    "Tool-call finish reason contained no Tool call",
                ));
            }
            for call in calls {
                self.run_tool(
                    claim,
                    composition,
                    job_scope,
                    fold,
                    call,
                    turn_policy,
                    &cancellation,
                    stop,
                )
                .await?;
            }
        }
    }

    async fn resume_effect(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
        effect: Option<ResumeEffect>,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        match effect {
            None => Ok(()),
            Some(ResumeEffect::Model { started }) => {
                Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                    effect: started.then_some(EffectKind::Model),
                    reason: "executor generation changed during a prepared model effect".into(),
                }))
            }
            Some(ResumeEffect::Image { started }) => {
                Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                    effect: started.then_some(EffectKind::Image),
                    reason: "executor generation changed during a prepared Image effect".into(),
                }))
            }
            Some(ResumeEffect::Tool { started: false, .. }) => {
                Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                    effect: None,
                    reason: "executor generation changed before a prepared Tool started".into(),
                }))
            }
            Some(ResumeEffect::Tool {
                effect_id,
                identity,
                started: true,
            }) => {
                self.recover_tool(claim, composition, fold, effect_id, identity, stop)
                    .await
            }
        }
    }

    async fn run_image(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
        model: ModelRef,
        request: ImageRequest,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        let cancellation = self.turns.cancellation(claim).map_err(fatal)?;
        if cancellation.is_cancelled() {
            return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
        }
        let expected_outputs = usize::from(request.count());
        let prepared = self
            .image
            .prepare(model, request)
            .await
            .map_err(|error| image_ai_failure(&error, Vec::new()))?;
        let snapshot = prepared.snapshot().clone();
        let effect_id = next_effect_id().map_err(fatal)?;
        let intent = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ImageIntent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    snapshot,
                }],
            )
            .await?;
        self.flush_last(claim, &intent).await?;
        let started = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ImageStarted {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &started).await?;

        let combined = combine_cancellation(&cancellation, stop);
        let stream = match prepared.start(combined.token()).await {
            Ok(stream) => stream,
            Err(error) => {
                combined.cancel();
                if stop.is_cancelled() {
                    return Err(DriveFailure::Stopped);
                }
                return Err(image_ai_failure(&error, Vec::new()));
            }
        };
        self.consume_image_stream(
            fold,
            ImageStreamContext {
                claim,
                effect_id: &effect_id,
                expected_outputs,
                stop,
                combined,
            },
            stream,
        )
        .await
    }

    async fn consume_image_stream(
        &self,
        fold: &mut ContextFold,
        attempt: ImageStreamContext<'_>,
        mut stream: rsi_ai_protocol::ImageStream,
    ) -> std::result::Result<(), DriveFailure> {
        let ImageStreamContext {
            claim,
            effect_id,
            expected_outputs,
            stop,
            combined,
        } = attempt;
        let mut assembler = ImageAssembler::new();
        let mut media = Vec::with_capacity(expected_outputs);
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    combined.cancel();
                    if stop.is_cancelled() {
                        return Err(DriveFailure::Stopped);
                    }
                    return Err(image_ai_failure(&error, media));
                }
                None => {
                    combined.cancel();
                    return Err(image_operation_failure(
                        media,
                        "provider.missing_terminal",
                        "Image stream ended without a terminal event",
                    ));
                }
            };
            let completed_index = match &event {
                ImageEvent::OutputFinished { index } => Some(*index),
                ImageEvent::OutputStarted { .. }
                | ImageEvent::OutputChunk { .. }
                | ImageEvent::Usage { .. }
                | ImageEvent::Finished => None,
            };
            assembler.push(&event).map_err(|error| {
                image_operation_failure(media.clone(), error.code(), error.to_string())
            })?;
            if let Some(index) = completed_index {
                let output = assembler.take_completed(index).ok_or_else(|| {
                    image_operation_failure(
                        media.clone(),
                        "stream.missing_output",
                        "closed Image output was not retained",
                    )
                })?;
                let reference = self
                    .media
                    .import_image(Arc::from(output.bytes))
                    .await
                    .map_err(|error| {
                        image_operation_failure(media.clone(), "media.commit", error.to_string())
                    })?;
                let published = self
                    .publish_apply(
                        claim,
                        fold,
                        vec![SessionFactBody::ImageOutput {
                            turn_id: claim.turn_id().clone(),
                            effect_id: effect_id.clone(),
                            index,
                            media: reference.clone(),
                        }],
                    )
                    .await?;
                self.flush_last(claim, &published).await?;
                media.push(reference);
            }
            if matches!(event, ImageEvent::Finished) {
                combined.cancel();
                let completed_outputs = assembler.completed_count();
                let _output = assembler.finish().map_err(|error| {
                    image_operation_failure(media.clone(), error.code(), error.to_string())
                })?;
                if completed_outputs != expected_outputs || media.len() != expected_outputs {
                    return Err(image_operation_failure(
                        media,
                        "provider.output_count",
                        "Image provider returned a different number of outputs than requested",
                    ));
                }
                return Err(DriveFailure::Turn(TurnOutcome::Completed));
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // One attempt binds the resident generation to its durable fold, model, retry ordinal, and cancellation fences.
    async fn run_model_attempt(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
        model: &ModelRef,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        self.sync_fold(claim, fold).await?;
        let request = fold
            .request(self.config.limits(), composition.tools().definitions())
            .map_err(|error| failed("context.projection", error.to_string()))?;
        let prepared = self
            .language
            .prepare(model.clone(), request)
            .await
            .map_err(|error| ai_failure(&error))?;
        let snapshot = prepared.snapshot().clone();
        let effect_id = next_effect_id().map_err(fatal)?;
        let intent = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ModelIntent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    snapshot: snapshot.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &intent).await?;
        let started = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ModelStarted {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &started).await?;

        let combined = combine_cancellation(cancellation, stop);
        let stream = match prepared.start(combined.token()).await {
            Ok(stream) => stream,
            Err(error) => {
                combined.cancel();
                if stop.is_cancelled() {
                    return Err(DriveFailure::Stopped);
                }
                self.record_model_failure(claim, fold, &effect_id, error.clone())
                    .await?;
                return self
                    .retry_or_fail(&snapshot, &error, retry_attempt, cancellation, stop)
                    .await;
            }
        };
        self.consume_model_stream(
            fold,
            ModelStreamContext {
                claim,
                effect_id: &effect_id,
                snapshot: &snapshot,
                retry_attempt,
                cancellation,
                stop,
                combined,
            },
            stream,
        )
        .await
    }

    async fn consume_model_stream(
        &self,
        fold: &mut ContextFold,
        attempt: ModelStreamContext<'_>,
        mut stream: rsi_ai_protocol::LanguageStream,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        let ModelStreamContext {
            claim,
            effect_id,
            snapshot,
            retry_attempt,
            cancellation,
            stop,
            combined,
        } = attempt;
        let mut assembler = LanguageAssembler::new();
        let mut last_flush = tokio::time::Instant::now();
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    combined.cancel();
                    if stop.is_cancelled() {
                        return Err(DriveFailure::Stopped);
                    }
                    self.record_model_failure(claim, fold, effect_id, error.clone())
                        .await?;
                    return self
                        .retry_or_fail(snapshot, &error, retry_attempt, cancellation, stop)
                        .await;
                }
                None => {
                    combined.cancel();
                    return Err(failed(
                        "provider.missing_terminal",
                        "Language stream ended without a terminal event",
                    ));
                }
            };
            assembler
                .push(&event)
                .map_err(|error| failed(error.code(), error.to_string()))?;
            let terminal = matches!(
                event,
                LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
            );
            let facts = self
                .publish_apply(
                    claim,
                    fold,
                    vec![SessionFactBody::ModelEvent {
                        turn_id: claim.turn_id().clone(),
                        effect_id: effect_id.clone(),
                        event,
                    }],
                )
                .await?;
            if terminal || last_flush.elapsed() >= STREAM_FLUSH_INTERVAL {
                self.flush_last(claim, &facts).await?;
                last_flush = tokio::time::Instant::now();
            }
            if terminal {
                combined.cancel();
                return match assembler.finish() {
                    Ok(output) => Ok(ModelAttempt::Output(Box::new(output))),
                    Err(LanguageAssemblyError::Provider { error, .. }) => {
                        self.retry_or_fail(snapshot, &error, retry_attempt, cancellation, stop)
                            .await
                    }
                    Err(LanguageAssemblyError::Protocol(error)) => {
                        Err(failed(error.code(), error.to_string()))
                    }
                };
            }
        }
    }

    async fn retry_or_fail(
        &self,
        snapshot: &PreparedCallSnapshot,
        error: &rsi_ai_protocol::AiError,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        if !should_retry(snapshot, error, retry_attempt) {
            return Err(ai_failure(error));
        }
        self.wait_retry(snapshot, error, retry_attempt, cancellation, stop)
            .await?;
        Ok(ModelAttempt::Retry)
    }

    #[allow(clippy::too_many_arguments)] // Keep durable claim/fold, live Jobs authority, policy, and both cancellation owners explicit.
    async fn run_tool(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        fold: &mut ContextFold,
        call: ModelToolCall,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let tools = composition.tools();
        let (effect_id, arguments) = prepare_tool_effect(&call).map_err(|failure| *failure)?;
        let approval = self
            .request_tool_approval(
                claim,
                &effect_id,
                &call.name,
                turn_policy.require_approval,
                cancellation,
                stop,
            )
            .await?;
        let prepared = tools
            .prepare(
                effect_id.as_str(),
                ToolCall {
                    id: call.id,
                    name: call.name.clone(),
                    arguments: arguments.clone(),
                },
            )
            .map_err(|error| tool_failure(&error))?;
        let identity = prepared.identity().clone();
        let intent = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ToolIntent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                    name: call.name,
                    arguments,
                    approval,
                }],
            )
            .await?;
        self.flush_last(claim, &intent).await?;
        let started = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ToolStarted {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &started).await?;
        self.track_tool(claim, composition.clone(), identity.clone());
        let result = match self
            .start_tool(
                prepared,
                &identity,
                composition,
                claim,
                job_scope,
                turn_policy.sandbox,
                cancellation,
                stop,
            )
            .await
        {
            Ok(result) => result,
            Err(failure) => {
                if matches!(tools.query(&identity), Ok(RetainedToolResult::Absent)) {
                    self.clear_tracked_tool(claim, &identity);
                }
                return Err(failure);
            }
        };
        let returned = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ToolResult {
                    turn_id: claim.turn_id().clone(),
                    effect_id,
                    identity: identity.clone(),
                    result,
                }],
            )
            .await
            .map_err(|failure| settled_tool_budget(failure, identity.clone()))?;
        self.flush_last(claim, &returned).await?;
        tools
            .commit(&identity)
            .map_err(|error| tool_failure(&error))?;
        self.clear_tracked_tool(claim, &identity);
        Ok(())
    }

    fn track_tool(
        &self,
        claim: &TurnClaim,
        composition: AgentCompositionPin,
        identity: ToolResultIdentity,
    ) {
        self.active_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (
                    claim.session_id().as_str().to_owned(),
                    claim.turn_id().as_str().to_owned(),
                ),
                TrackedTool {
                    composition,
                    identity,
                },
            );
    }

    fn clear_tracked_tool(&self, claim: &TurnClaim, identity: &ToolResultIdentity) {
        let key = (
            claim.session_id().as_str().to_owned(),
            claim.turn_id().as_str().to_owned(),
        );
        let mut active = self
            .active_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active
            .get(&key)
            .is_some_and(|tracked| &tracked.identity == identity)
        {
            active.remove(&key);
        }
    }

    fn retire_tracked_tool(&self, claim: &TurnClaim, stop: &CancellationToken) {
        let key = (
            claim.session_id().as_str().to_owned(),
            claim.turn_id().as_str().to_owned(),
        );
        let tracked = self
            .active_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        let Some(tracked) = tracked else {
            return;
        };
        let composition = tracked.composition;
        let tools = composition.tools();
        let identity = tracked.identity;
        let stop = stop.clone();
        let wait = self.config.retained_tool_wait();
        let task = tokio::spawn(async move {
            let _composition = composition;
            let settlement = tools.wait(&identity, stop.clone());
            tokio::pin!(settlement);
            let retained = tokio::select! {
                biased;
                () = stop.cancelled() => None,
                () = tokio::time::sleep(wait) => None,
                retained = &mut settlement => Some(retained),
            };
            if matches!(
                retained,
                Some(Ok(
                    RetainedToolResult::Returned(_) | RetainedToolResult::Failed(_)
                ))
            ) {
                let _ignored = tools.commit(&identity);
            }
        });
        self.retirement_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(task);
    }

    fn reap_retirement_tasks(&self) {
        self.retirement_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|task| !task.is_finished());
    }

    async fn abort_retirement_tasks(&self) {
        let tasks = std::mem::take(
            &mut *self
                .retirement_tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ignored = task.await;
        }
    }

    #[allow(clippy::too_many_arguments)] // Start binds one prepared effect to its durable identity and exact turn-scoped authorities.
    async fn start_tool(
        &self,
        prepared: Box<dyn PreparedToolCall>,
        identity: &ToolResultIdentity,
        composition: &AgentCompositionPin,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        sandbox_mode: SandboxMode,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ToolResult, DriveFailure> {
        let combined = combine_cancellation(cancellation, stop);
        let cwd = std::path::PathBuf::from(claim.header().canonical_cwd());
        let result = prepared
            .start(ToolStart {
                cancellation: combined.token(),
                policy: ToolExecutionPolicy {
                    mode: sandbox_mode,
                    cwd: cwd.clone(),
                    workspace: cwd,
                },
                sandbox: Arc::clone(&self.sandbox),
                job_scope: job_scope.cloned(),
            })
            .await;
        combined.cancel();
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                let failure = tool_failure(&error);
                if matches!(
                    composition.tools().query(identity),
                    Ok(RetainedToolResult::Failed(_))
                ) {
                    let DriveFailure::Turn(outcome) = failure else {
                        return Err(failure);
                    };
                    return Err(DriveFailure::SettledTool {
                        outcome,
                        identity: identity.clone(),
                    });
                }
                Err(failure)
            }
        }
    }

    async fn request_tool_approval(
        &self,
        claim: &TurnClaim,
        effect_id: &EffectId,
        tool_name: &str,
        required: bool,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<Option<rsi_approval_protocol::ApprovalOutcome>, DriveFailure> {
        if !required {
            return Ok(None);
        }
        let request = ApprovalRequest {
            subject: ApprovalSubject::new(
                claim.session_id().as_str(),
                claim.turn_id().as_str(),
                effect_id.as_str(),
            )
            .map_err(|error| failed("approval.invalid_subject", error.to_string()))?,
            id: effect_id.as_str().to_owned(),
            action: format!("run tool {tool_name}"),
            reason: format!(
                "Agent turn {} requested this Tool effect",
                claim.turn_id().as_str()
            ),
        };
        let outcome = tokio::select! {
            outcome = self.approval.ask(request, cancellation.clone()) => outcome,
            () = stop.cancelled() => return Err(DriveFailure::Stopped),
        };
        match outcome {
            Ok(outcome) if outcome.decision == ApprovalDecision::AllowOnce => Ok(Some(outcome)),
            Ok(_) => Err(failed(
                "approval.denied",
                "live approval denied the Tool effect",
            )),
            Err(ApprovalError::Cancelled) if cancellation.is_cancelled() => {
                Err(DriveFailure::Turn(TurnOutcome::Cancelled))
            }
            Err(error) => Err(failed("approval.failed", error.to_string())),
        }
    }

    async fn recover_tool(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
        effect_id: EffectId,
        identity: ToolResultIdentity,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let tools = composition.tools();
        self.track_tool(claim, composition.clone(), identity.clone());
        let retained = tools.wait(&identity, stop.clone()).await;
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        match retained.map_err(|error| tool_failure(&error))? {
            RetainedToolResult::Returned(result) => {
                let facts = self
                    .publish_apply(
                        claim,
                        fold,
                        vec![SessionFactBody::ToolResult {
                            turn_id: claim.turn_id().clone(),
                            effect_id: effect_id.clone(),
                            identity: identity.clone(),
                            result,
                        }],
                    )
                    .await
                    .map_err(|failure| settled_tool_budget(failure, identity.clone()))?;
                self.flush_last(claim, &facts).await?;
                tools
                    .commit(&identity)
                    .map_err(|error| tool_failure(&error))?;
                self.clear_tracked_tool(claim, &identity);
                Ok(())
            }
            RetainedToolResult::Failed(failure) => {
                let outcome = match failure.kind {
                    RetainedToolFailureKind::Cancelled => {
                        if self
                            .turns
                            .cancellation(claim)
                            .map_err(fatal)?
                            .is_cancelled()
                        {
                            TurnOutcome::Cancelled
                        } else {
                            TurnOutcome::Interrupted {
                                    effect: Some(EffectKind::Tool),
                                    reason: "retained Tool call was cancelled by an executor generation change".into(),
                                }
                        }
                    }
                    RetainedToolFailureKind::Timeout | RetainedToolFailureKind::Execution => {
                        TurnOutcome::Failed {
                            code: "tool.execution".into(),
                            message: bounded(&failure.summary),
                        }
                    }
                };
                Err(DriveFailure::SettledTool { outcome, identity })
            }
            RetainedToolResult::Absent => Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                effect: Some(EffectKind::Tool),
                reason: "exact retained Tool result is absent from its owner generation".into(),
            })),
            RetainedToolResult::Pending => Err(fatal(
                "Tool Runtime wait returned before the retained invocation settled",
            )),
        }
    }

    async fn record_model_failure(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
        effect_id: &EffectId,
        error: rsi_ai_protocol::AiError,
    ) -> std::result::Result<(), DriveFailure> {
        let facts = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ModelEvent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    event: LanguageEvent::Failed {
                        error,
                        replay: None,
                    },
                }],
            )
            .await?;
        self.flush_last(claim, &facts).await
    }

    async fn wait_retry(
        &self,
        snapshot: &PreparedCallSnapshot,
        error: &rsi_ai_protocol::AiError,
        attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let delay = retry_delay(snapshot, error, attempt);
        tokio::select! {
            () = tokio::time::sleep(delay) => Ok(()),
            () = cancellation.cancelled() => Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            () = stop.cancelled() => Err(DriveFailure::Stopped),
        }
    }

    async fn finish(
        &self,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        outcome: TurnOutcome,
    ) -> std::result::Result<Arc<SessionFact>, DriveFailure> {
        let outcome = self.resolve_outcome(claim, job_scope, outcome).await;
        publish_terminal(self.turns.as_ref(), &self.config, claim, outcome).await
    }

    async fn resolve_outcome(
        &self,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        outcome: TurnOutcome,
    ) -> TurnOutcome {
        let context = TurnFinalizationContext {
            session_id: claim.session_id().clone(),
            turn_id: claim.turn_id().clone(),
            job_scope: job_scope.cloned(),
        };
        match tokio::time::timeout(
            self.config.finalization_wait(),
            self.finalization.finalize(&context),
        )
        .await
        {
            Ok(Ok(report)) => match report.completion_blocker() {
                Some(blocker) => {
                    apply_finalization_failure(outcome, blocker.code(), blocker.message(), false)
                }
                None => outcome,
            },
            Ok(Err(TurnFinalizationError::Failed { code, message })) => {
                apply_finalization_failure(outcome, &bounded(&code), &bounded(&message), true)
            }
            Ok(Err(TurnFinalizationError::Invalid(message))) => {
                apply_finalization_failure(outcome, "turn.finalization", &bounded(&message), true)
            }
            Err(_) => apply_finalization_failure(
                outcome,
                "turn.finalization_timeout",
                &format!(
                    "turn finalization exceeded {} ms",
                    self.config.finalization_wait_ms
                ),
                true,
            ),
        }
    }

    async fn finish_budget(
        &self,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        dimension: BudgetDimension,
        consumed: u64,
        limit: u64,
    ) -> std::result::Result<Vec<Arc<SessionFact>>, DriveFailure> {
        let outcome = self
            .resolve_outcome(
                claim,
                job_scope,
                TurnOutcome::BudgetExceeded {
                    dimension,
                    consumed,
                    limit,
                },
            )
            .await;
        if !matches!(
            outcome,
            TurnOutcome::BudgetExceeded {
                dimension: outcome_dimension,
                consumed: outcome_consumed,
                limit: outcome_limit,
            } if outcome_dimension == dimension
                && outcome_consumed == consumed
                && outcome_limit == limit
        ) {
            let terminal =
                publish_terminal(self.turns.as_ref(), &self.config, claim, outcome).await?;
            return Ok(vec![terminal]);
        }
        let exhausted = publish_budget_exhaustion(
            self.turns.as_ref(),
            &self.config,
            claim,
            dimension,
            consumed,
            limit,
        )
        .await?;
        let terminal = publish_terminal(self.turns.as_ref(), &self.config, claim, outcome).await?;
        Ok(vec![exhausted, terminal])
    }

    const fn context_limits(&self) -> ContextLimits {
        ContextLimits {
            max_messages: self.config.max_context_messages,
            max_bytes: self.config.max_context_bytes,
        }
    }

    async fn finish_context_error(
        &self,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        message: String,
    ) {
        let _ignored = self
            .finish(
                claim,
                job_scope,
                failure_outcome("context.invalid", message),
            )
            .await;
    }

    fn request_checkpoint(&self, claim: &TurnClaim) {
        self.checkpoint_tx.send_replace(Some(CheckpointRequest {
            claim: claim.clone(),
            limits: self.context_limits(),
        }));
    }

    async fn publish_apply(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
        bodies: Vec<SessionFactBody>,
    ) -> std::result::Result<Vec<Arc<SessionFact>>, DriveFailure> {
        let facts = publish_nonterminal_with_capacity_retry(
            self.turns.as_ref(),
            &self.config,
            claim,
            bodies,
        )
        .await?;
        if facts
            .first()
            .is_some_and(|fact| fact.seq() != fold.through_seq() + 1)
        {
            self.sync_fold(claim, fold).await?;
        }
        if let Some(unapplied) = facts
            .iter()
            .position(|fact| fact.seq() > fold.through_seq())
        {
            fold.apply(&facts[unapplied..])
                .map_err(|error| failed("context.incremental", error.to_string()))?;
        }
        Ok(facts)
    }

    async fn flush_last(
        &self,
        claim: &TurnClaim,
        facts: &[Arc<SessionFact>],
    ) -> std::result::Result<(), DriveFailure> {
        let seq = facts
            .last()
            .ok_or_else(|| failed("executor.empty_publish", "publication returned no Facts"))?
            .seq();
        self.flush_durable(claim, seq).await?;
        Ok(())
    }

    async fn flush_durable(
        &self,
        claim: &TurnClaim,
        through_seq: u64,
    ) -> std::result::Result<u64, DriveFailure> {
        tokio::time::timeout(
            self.config.durability_wait(),
            self.turns.flush(claim, through_seq),
        )
        .await
        .map_err(|_| {
            fatal(format!(
                "Fact durability wait exceeded {} ms",
                self.config.durability_wait_ms
            ))
        })?
        .map_err(fatal)
    }

    async fn sync_fold(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
    ) -> std::result::Result<(), DriveFailure> {
        loop {
            let after_seq = fold.through_seq();
            let page = self
                .turns
                .read_facts(
                    claim,
                    after_seq,
                    rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
                )
                .await
                .map_err(fatal)?;
            if page.through_seq == after_seq {
                return Ok(());
            }
            fold.apply_page(&page.facts, page.through_seq)
                .map_err(|error| failed("context.incremental", error.to_string()))?;
        }
    }

    async fn load_claim(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
    ) -> std::result::Result<ScannedTurn, DriveFailure> {
        let mut state = ScannedTurn::default();
        let mut cursor = 0;
        if let Ok(Some(checkpoint)) = self.turns.read_context_checkpoint(claim.session_id()).await
            && checkpoint.through_seq < claim.accepted_seq()
            && checkpoint.through_seq <= claim.live_seq()
            && let Ok(restored) = ContextFold::from_checkpoint(
                claim.header().clone(),
                self.context_limits(),
                &checkpoint.bytes,
            )
            && restored.through_seq() == checkpoint.through_seq
            && restored.fact_prefix_sha256() == checkpoint.fact_prefix_sha256
            && claim
                .header()
                .fingerprint()
                .is_ok_and(|fingerprint| fingerprint == checkpoint.header_fingerprint)
        {
            *fold = restored;
            cursor = checkpoint.through_seq;
        }
        loop {
            let page = self
                .turns
                .read_facts(
                    claim,
                    cursor,
                    rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
                )
                .await
                .map_err(fatal)?;
            if page.through_seq == cursor {
                return Ok(state);
            }
            cursor = page.through_seq;
            fold.apply_page(&page.facts, page.through_seq)
                .map_err(|error| failed("context.invalid", error.to_string()))?;
            scan_turn(claim, &mut state, &page.facts)
                .map_err(|message| failed("executor.invalid_history", message))?;
        }
    }
}

async fn publish_nonterminal_with_capacity_retry(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    bodies: Vec<SessionFactBody>,
) -> std::result::Result<Vec<Arc<SessionFact>>, DriveFailure> {
    match turns.publish(claim, bodies).await {
        Ok(PublishAttempt::Published(facts)) => Ok(facts),
        Ok(PublishAttempt::FlushRequired { unpublished }) => {
            let tail = live_tail(turns, claim).await?;
            if tail == 0 {
                return Err(fatal(
                    "Fact publication requires a nonempty flushable prefix",
                ));
            }
            flush_execution_prefix(turns, config, claim, tail).await?;
            match turns.publish(claim, unpublished).await {
                Ok(PublishAttempt::Published(facts)) => Ok(facts),
                Ok(PublishAttempt::FlushRequired { .. }) => Err(fatal(
                    "Fact publication remained full after its durable flush",
                )),
                Err(error) => Err(turn_failure(error)),
            }
        }
        Err(error) => Err(turn_failure(error)),
    }
}

async fn publish_terminal(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    outcome: TurnOutcome,
) -> std::result::Result<Arc<SessionFact>, DriveFailure> {
    let mut last_capacity_flush = None;
    let mut bodies = vec![SessionFactBody::TurnTerminal {
        turn_id: claim.turn_id().clone(),
        outcome,
    }];
    let facts = loop {
        match turns.publish(claim, bodies).await {
            Ok(PublishAttempt::Published(facts)) => break facts,
            Ok(PublishAttempt::FlushRequired { unpublished }) => {
                let tail = live_tail(turns, claim).await?;
                if tail == 0 || last_capacity_flush.is_some_and(|flushed| flushed >= tail) {
                    return Err(fatal(
                        "terminal publication remained full without new flushable Facts",
                    ));
                }
                flush_execution_prefix(turns, config, claim, tail).await?;
                last_capacity_flush = Some(tail);
                bodies = unpublished;
            }
            Err(error) => return Err(turn_failure(error)),
        }
    };
    let fact = facts
        .last()
        .ok_or_else(|| failed("executor.empty_publish", "publication returned no Facts"))?
        .clone();
    flush_execution_prefix(turns, config, claim, fact.seq()).await?;
    Ok(fact)
}

async fn publish_budget_exhaustion(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    dimension: BudgetDimension,
    consumed: u64,
    limit: u64,
) -> std::result::Result<Arc<SessionFact>, DriveFailure> {
    let mut bodies = vec![SessionFactBody::BudgetExhausted {
        turn_id: claim.turn_id().clone(),
        dimension,
        consumed,
        limit,
    }];
    let mut last_capacity_flush = None;
    let facts = loop {
        match turns.publish(claim, bodies).await {
            Ok(PublishAttempt::Published(facts)) => break facts,
            Ok(PublishAttempt::FlushRequired { unpublished }) => {
                let tail = live_tail(turns, claim).await?;
                if tail == 0 || last_capacity_flush.is_some_and(|flushed| flushed >= tail) {
                    return Err(fatal(
                        "budget publication remained full without new flushable Facts",
                    ));
                }
                flush_execution_prefix(turns, config, claim, tail).await?;
                last_capacity_flush = Some(tail);
                bodies = unpublished;
            }
            Err(error) => return Err(turn_failure(error)),
        }
    };
    let fact = facts
        .last()
        .ok_or_else(|| failed("executor.empty_publish", "publication returned no Facts"))?
        .clone();
    flush_execution_prefix(turns, config, claim, fact.seq()).await?;
    Ok(fact)
}

async fn live_tail(
    turns: &dyn TurnExecution,
    claim: &TurnClaim,
) -> std::result::Result<u64, DriveFailure> {
    // Facts at or before the claim were already represented by its live watermark.
    // Only scan the executor-owned suffix when recovering publication capacity.
    let mut cursor = claim.live_seq();
    loop {
        let page = turns
            .read_facts(
                claim,
                cursor,
                rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
            )
            .await
            .map_err(fatal)?;
        let tail = page.through_seq;
        if tail == cursor {
            return Ok(cursor);
        }
        if tail <= cursor {
            return Err(fatal("Fact scan made no progress before terminal retry"));
        }
        cursor = tail;
    }
}

async fn flush_execution_prefix(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    through_seq: u64,
) -> std::result::Result<u64, DriveFailure> {
    tokio::time::timeout(config.durability_wait(), turns.flush(claim, through_seq))
        .await
        .map_err(|_| {
            fatal(format!(
                "Fact durability wait exceeded {} ms",
                config.durability_wait_ms
            ))
        })?
        .map_err(fatal)
}

#[derive(Debug)]
enum ModelAttempt {
    Output(Box<rsi_ai_protocol::LanguageOutput>),
    Retry,
}

struct ModelStreamContext<'a> {
    claim: &'a TurnClaim,
    effect_id: &'a EffectId,
    snapshot: &'a PreparedCallSnapshot,
    retry_attempt: u8,
    cancellation: &'a CancellationToken,
    stop: &'a CancellationToken,
    combined: CombinedCancellation,
}

struct ImageStreamContext<'a> {
    claim: &'a TurnClaim,
    effect_id: &'a EffectId,
    expected_outputs: usize,
    stop: &'a CancellationToken,
    combined: CombinedCancellation,
}

#[derive(Debug, Default)]
struct ScannedTurn {
    model: Option<ModelRef>,
    image: Option<(ModelRef, ImageRequest)>,
    terminal: bool,
    completed_model_without_successor: bool,
    effect: Option<ResumeEffect>,
    turn_policy: Option<ResolvedTurnPolicy>,
    budget_exhausted: Option<(BudgetDimension, u64, u64)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolvedTurnPolicy {
    sandbox: SandboxMode,
    require_approval: bool,
}

#[derive(Debug)]
enum ResumeEffect {
    Model {
        started: bool,
    },
    Image {
        started: bool,
    },
    Tool {
        effect_id: EffectId,
        identity: ToolResultIdentity,
        started: bool,
    },
}

#[allow(clippy::too_many_lines)]
fn scan_turn(
    claim: &TurnClaim,
    state: &mut ScannedTurn,
    facts: &[Arc<SessionFact>],
) -> std::result::Result<(), &'static str> {
    for fact in facts {
        match fact.body() {
            SessionFactBody::TurnAccepted {
                turn_id,
                model,
                sandbox,
                require_approval,
                ..
            } if turn_id == claim.turn_id() => {
                state.model.clone_from(model);
                state.turn_policy = Some(ResolvedTurnPolicy {
                    sandbox: *sandbox,
                    require_approval: *require_approval,
                });
            }
            SessionFactBody::ImageRequested {
                turn_id,
                model,
                request,
            } if turn_id == claim.turn_id() => {
                state.image = Some((model.clone(), request.clone()));
            }
            SessionFactBody::ModelIntent { turn_id, .. } if turn_id == claim.turn_id() => {
                state.completed_model_without_successor = false;
                state.effect = Some(ResumeEffect::Model { started: false });
            }
            SessionFactBody::ModelStarted { turn_id, .. } if turn_id == claim.turn_id() => {
                match &mut state.effect {
                    Some(ResumeEffect::Model { started }) => *started = true,
                    _ => {
                        return Err("model start lacks intent");
                    }
                }
            }
            SessionFactBody::ImageIntent { turn_id, .. } if turn_id == claim.turn_id() => {
                state.effect = Some(ResumeEffect::Image { started: false });
            }
            SessionFactBody::ImageStarted { turn_id, .. } if turn_id == claim.turn_id() => {
                match &mut state.effect {
                    Some(ResumeEffect::Image { started }) => *started = true,
                    _ => {
                        return Err("Image start lacks intent");
                    }
                }
            }
            SessionFactBody::ModelEvent { turn_id, event, .. }
                if turn_id == claim.turn_id()
                    && matches!(
                        event,
                        LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
                    ) =>
            {
                state.effect = None;
                state.completed_model_without_successor = true;
            }
            SessionFactBody::ToolIntent {
                turn_id,
                effect_id,
                identity,
                ..
            } if turn_id == claim.turn_id() => {
                state.completed_model_without_successor = false;
                state.effect = Some(ResumeEffect::Tool {
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                    started: false,
                });
            }
            SessionFactBody::ToolStarted { turn_id, .. } if turn_id == claim.turn_id() => {
                match &mut state.effect {
                    Some(ResumeEffect::Tool { started, .. }) => *started = true,
                    _ => {
                        return Err("Tool start lacks intent");
                    }
                }
            }
            SessionFactBody::ToolResult { turn_id, .. } if turn_id == claim.turn_id() => {
                state.effect = None;
            }
            SessionFactBody::TurnTerminal { turn_id, .. } if turn_id == claim.turn_id() => {
                state.terminal = true;
                state.completed_model_without_successor = false;
                state.effect = None;
            }
            SessionFactBody::BudgetExhausted {
                turn_id,
                dimension,
                consumed,
                limit,
            } if turn_id == claim.turn_id() => {
                state.budget_exhausted = Some((*dimension, *consumed, *limit));
            }
            SessionFactBody::TurnAccepted { .. }
            | SessionFactBody::ImageRequested { .. }
            | SessionFactBody::CancelRequested { .. }
            | SessionFactBody::BudgetExhausted { .. }
            | SessionFactBody::ModelIntent { .. }
            | SessionFactBody::ModelStarted { .. }
            | SessionFactBody::ImageIntent { .. }
            | SessionFactBody::ImageStarted { .. }
            | SessionFactBody::ImageOutput { .. }
            | SessionFactBody::ModelEvent { .. }
            | SessionFactBody::ToolIntent { .. }
            | SessionFactBody::ToolStarted { .. }
            | SessionFactBody::ToolResult { .. }
            | SessionFactBody::TurnTerminal { .. } => {}
        }
    }
    Ok(())
}

fn prepare_tool_effect(
    call: &ModelToolCall,
) -> std::result::Result<(EffectId, Value), Box<DriveFailure>> {
    let arguments = match call.kind {
        ToolCallKind::Function => parse_tool_arguments(&call.arguments)
            .map_err(|error| Box::new(failed("tool.invalid_arguments", error.to_string())))?,
        ToolCallKind::Freeform => Value::String(call.arguments.clone()),
    };
    Ok((
        next_effect_id().map_err(|error| Box::new(fatal(error)))?,
        arguments,
    ))
}

fn next_effect_id() -> Result<EffectId> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy)
        .map_err(|error| ExecutorError::Invalid(format!("OS entropy failed: {error}")))?;
    EffectId::new(format!("effect-{:032x}", u128::from_le_bytes(entropy)))
        .map_err(|error| ExecutorError::Invalid(error.to_string()))
}

fn should_retry(
    snapshot: &PreparedCallSnapshot,
    error: &rsi_ai_protocol::AiError,
    attempt: u8,
) -> bool {
    attempt < snapshot.retry_policy.max_retries()
        && snapshot.retry_policy.retries(error.kind())
        && matches!(
            error.dispatch_status(),
            DispatchStatus::NotStarted | DispatchStatus::NotDispatched
        )
}

fn retry_delay(
    snapshot: &PreparedCallSnapshot,
    error: &rsi_ai_protocol::AiError,
    attempt: u8,
) -> Duration {
    let policy = &snapshot.retry_policy;
    let multiplier = 1_u64 << u32::from(attempt.min(16));
    let exponential = policy
        .initial_delay_ms()
        .saturating_mul(multiplier)
        .min(policy.max_delay_ms());
    let requested = error
        .retry_after_ms()
        .unwrap_or(0)
        .min(policy.max_delay_ms());
    let base = exponential.max(requested);
    let spread = base.saturating_mul(u64::from(policy.jitter_per_mille())) / 1_000;
    let seed = u64::from_str_radix(snapshot.request_sha256.get(..16).unwrap_or("0"), 16)
        .unwrap_or(0)
        ^ u64::from(attempt);
    let sampled = if spread == 0 {
        0
    } else {
        seed % spread.saturating_mul(2).saturating_add(1)
    };
    Duration::from_millis(base.saturating_sub(spread).saturating_add(sampled))
}

struct CombinedCancellation {
    token: CancellationToken,
    listener: JoinHandle<()>,
}

impl CombinedCancellation {
    fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    fn cancel(&self) {
        self.token.cancel();
    }
}

impl Drop for CombinedCancellation {
    fn drop(&mut self) {
        self.token.cancel();
        self.listener.abort();
    }
}

fn combine_cancellation(
    turn: &CancellationToken,
    stop: &CancellationToken,
) -> CombinedCancellation {
    let combined = CancellationToken::new();
    let output = combined.clone();
    let done = combined.clone();
    let turn = turn.clone();
    let stop = stop.clone();
    let listener = tokio::spawn(async move {
        tokio::select! {
            () = turn.cancelled() => output.cancel(),
            () = stop.cancelled() => output.cancel(),
            () = done.cancelled() => {}
        }
    });
    CombinedCancellation {
        token: combined,
        listener,
    }
}

fn ai_failure(error: &rsi_ai_protocol::AiError) -> DriveFailure {
    if error.kind() == ErrorKind::Cancelled {
        return DriveFailure::Turn(TurnOutcome::Cancelled);
    }
    if error.dispatch_status() == DispatchStatus::Unknown {
        return DriveFailure::Turn(TurnOutcome::Interrupted {
            effect: Some(EffectKind::Model),
            reason: bounded(error.safe_summary()),
        });
    }
    DriveFailure::Turn(TurnOutcome::Failed {
        code: error.kind().code().into(),
        message: bounded(error.safe_summary()),
    })
}

fn image_ai_failure(error: &rsi_ai_protocol::AiError, media: Vec<MediaRef>) -> DriveFailure {
    if media.is_empty() {
        if error.kind() == ErrorKind::Cancelled {
            return DriveFailure::Turn(TurnOutcome::Cancelled);
        }
        if error.dispatch_status() == DispatchStatus::Unknown {
            return DriveFailure::Turn(TurnOutcome::Interrupted {
                effect: Some(EffectKind::Image),
                reason: bounded(error.safe_summary()),
            });
        }
    }
    image_operation_failure(media, error.kind().code(), error.safe_summary())
}

fn image_operation_failure(
    media: Vec<MediaRef>,
    code: impl Into<String>,
    message: impl Into<String>,
) -> DriveFailure {
    let code = bounded(&code.into());
    let message = bounded(&message.into());
    if media.is_empty() {
        DriveFailure::Turn(TurnOutcome::Failed { code, message })
    } else {
        DriveFailure::Turn(TurnOutcome::PartialFailed {
            media,
            code,
            message,
        })
    }
}

fn tool_failure(error: &ToolError) -> DriveFailure {
    match error {
        ToolError::Cancelled => DriveFailure::Turn(TurnOutcome::Cancelled),
        ToolError::Timeout => failed("tool.timeout", "Tool invocation timed out"),
        ToolError::Capacity => failed("tool.capacity", "Tool capacity is exhausted"),
        ToolError::ShuttingDown => failed("tool.shutting_down", "Tool provider is shutting down"),
        ToolError::InvalidInput(_)
        | ToolError::Duplicate(_)
        | ToolError::Unknown(_)
        | ToolError::Withdrawn(_)
        | ToolError::Sealed
        | ToolError::Sandbox(_)
        | ToolError::Execution(_) => failed("tool.execution", error.to_string()),
    }
}

fn failed(code: impl Into<String>, message: impl Into<String>) -> DriveFailure {
    DriveFailure::Turn(failure_outcome(code, message))
}

fn failure_outcome(code: impl Into<String>, message: impl Into<String>) -> TurnOutcome {
    let code = code.into();
    let message = message.into();
    TurnOutcome::Failed {
        code: bounded(&code),
        message: bounded(&message),
    }
}

fn fatal(error: impl fmt::Display) -> DriveFailure {
    DriveFailure::Fatal(bounded(&error.to_string()))
}

fn turn_failure(error: TurnError) -> DriveFailure {
    match error {
        TurnError::ShuttingDown => DriveFailure::Stopped,
        TurnError::BudgetExceeded {
            dimension,
            consumed,
            limit,
        } => DriveFailure::Budget {
            dimension,
            consumed,
            limit,
        },
        other => fatal(other),
    }
}

fn apply_finalization_failure(
    outcome: TurnOutcome,
    code: &str,
    message: &str,
    cleanup_failed: bool,
) -> TurnOutcome {
    let code = bounded(code);
    let message = bounded(message);
    match outcome {
        TurnOutcome::PartialFailed { media, .. } => TurnOutcome::PartialFailed {
            media,
            code,
            message,
        },
        TurnOutcome::Completed => TurnOutcome::Failed { code, message },
        _ if cleanup_failed => TurnOutcome::Failed { code, message },
        original => original,
    }
}

fn bounded(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        let character = if matches!(character, '\0' | '\u{7f}') {
            '\u{fffd}'
        } else {
            character
        };
        if output.len().saturating_add(character.len_utf8()) > MAXIMUM_AGENT_DIAGNOSTIC_BYTES {
            break;
        }
        output.push(character);
    }
    if output.is_empty() {
        output.push_str("Agent executor failed");
    }
    output
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[derive(Debug)]
enum DriveFailure {
    Stopped,
    Turn(TurnOutcome),
    SettledTool {
        outcome: TurnOutcome,
        identity: ToolResultIdentity,
    },
    Fatal(String),
    Budget {
        dimension: BudgetDimension,
        consumed: u64,
        limit: u64,
    },
    DurableBudget {
        dimension: BudgetDimension,
        consumed: u64,
        limit: u64,
    },
    SettledToolBudget {
        dimension: BudgetDimension,
        consumed: u64,
        limit: u64,
        identity: ToolResultIdentity,
    },
}

fn settled_tool_budget(failure: DriveFailure, identity: ToolResultIdentity) -> DriveFailure {
    match failure {
        DriveFailure::Budget {
            dimension,
            consumed,
            limit,
        } => DriveFailure::SettledToolBudget {
            dimension,
            consumed,
            limit,
            identity,
        },
        failure => failure,
    }
}

async fn run_checkpoint_writer(
    turns: Arc<dyn TurnExecution>,
    mut requests: watch::Receiver<Option<CheckpointRequest>>,
) {
    while requests.changed().await.is_ok() {
        let Some(request) = requests.borrow_and_update().clone() else {
            continue;
        };
        let Some(checkpoint) = rebuild_context_checkpoint(&turns, &request).await else {
            continue;
        };
        let _ignored = turns
            .write_context_checkpoint(&request.claim, checkpoint)
            .await;
    }
}

async fn rebuild_context_checkpoint(
    turns: &Arc<dyn TurnExecution>,
    request: &CheckpointRequest,
) -> Option<ContextCheckpoint> {
    let mut fold = ContextFold::with_limits(request.claim.header().clone(), request.limits).ok()?;
    let mut cursor = 0;
    if let Ok(Some(checkpoint)) = turns
        .read_context_checkpoint(request.claim.session_id())
        .await
        && let Ok(restored) = ContextFold::from_checkpoint(
            request.claim.header().clone(),
            request.limits,
            &checkpoint.bytes,
        )
        && restored.through_seq() == checkpoint.through_seq
        && restored.fact_prefix_sha256() == checkpoint.fact_prefix_sha256
        && request
            .claim
            .header()
            .fingerprint()
            .is_ok_and(|fingerprint| fingerprint == checkpoint.header_fingerprint)
    {
        fold = restored;
        cursor = checkpoint.through_seq;
    }
    loop {
        let page = turns
            .read_checkpoint_facts(
                &request.claim,
                cursor,
                rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
            )
            .await
            .ok()??;
        if page.through_seq == cursor {
            break;
        }
        fold.apply(&page.facts).ok()?;
        cursor = page.through_seq;
    }
    Some(ContextCheckpoint {
        header_fingerprint: request.claim.header().fingerprint().ok()?,
        through_seq: fold.through_seq(),
        fact_prefix_sha256: fold.fact_prefix_sha256(),
        bytes: fold.checkpoint_bytes().ok()?,
    })
}

/// Closed executor preparation failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ExecutorError {
    /// Invalid bounded configuration or identity generation.
    #[error("invalid Agent executor: {0}")]
    Invalid(String),
}

/// Executor result.
pub type Result<T> = std::result::Result<T, ExecutorError>;

/// Ordinary executor factory over Turn, AI, Media, and effect-owner Local contracts.
#[derive(Clone, Debug, Default)]
pub struct ExecutorFactory;

fn executor_config_retained_bytes(config: &ExecutorConfig) -> rsi_meta::Result<usize> {
    std::mem::size_of::<ExecutorConfig>()
        .checked_add(config.executor_id.len())
        .ok_or_else(|| MetaError::InvalidInput("executor retained byte count overflowed".into()))
}

#[async_trait]
impl PluginFactory for ExecutorFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: ExecutorConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = executor_config_retained_bytes(&config)?;
        Ok(
            PreparedActivation::with_state(desired.clone(), config, retained)
                .requiring_local::<TurnExecutionContract>()
                .requiring_local::<TurnFinalizationContract>()
                .requiring_local::<LanguageCallContract>()
                .requiring_local::<ImageCallContract>()
                .requiring_local::<MediaContract>()
                .requiring_local::<ApprovalContract>()
                .requiring_local::<SandboxContract>()
                .requiring_local::<JobsContract>(),
        )
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<ExecutorConfig>()?;
        let turns = plan.local::<TurnExecutionContract>()?;
        let finalization = plan.local::<TurnFinalizationContract>()?;
        let language = plan.local::<LanguageCallContract>()?;
        let image = plan.local::<ImageCallContract>()?;
        let media = plan.local::<MediaContract>()?;
        let approval = plan.local::<ApprovalContract>()?;
        let sandbox = plan.local::<SandboxContract>()?;
        let jobs = plan.local::<JobsContract>()?;
        let lease = turns
            .register(config.executor_id.clone())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let checkpoint_turns = Arc::clone(&turns);
        let (checkpoint_tx, checkpoint_rx) = watch::channel(None);
        let driver = Arc::new(Driver {
            turns,
            finalization,
            language,
            image,
            media,
            approval,
            sandbox,
            jobs,
            active_tools: Mutex::new(BTreeMap::new()),
            retirement_tasks: Mutex::new(Vec::new()),
            checkpoint_tx,
            config,
        });
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let mut worker = tokio::spawn(async move { driver.run(task_stop).await });
        let mut checkpoint_worker = tokio::spawn(async move {
            run_checkpoint_writer(checkpoint_turns, checkpoint_rx).await;
        });
        plan.defer(
            "shutdown Agent executor",
            Box::new(move || {
                Box::pin(async move {
                    stop.cancel();
                    if let Ok(joined) =
                        tokio::time::timeout(EXECUTOR_SHUTDOWN_TIMEOUT, &mut worker).await
                    {
                        let checkpoint_joined =
                            tokio::time::timeout(EXECUTOR_SHUTDOWN_TIMEOUT, &mut checkpoint_worker)
                                .await;
                        drop(lease);
                        match (joined, checkpoint_joined) {
                            (Ok(()), Ok(Ok(()))) => Ok(()),
                            (Err(error), _) => {
                                Err(format!("Agent executor worker failed: {error}"))
                            }
                            (Ok(()), Ok(Err(error))) => {
                                Err(format!("Agent checkpoint worker failed: {error}"))
                            }
                            (Ok(()), Err(_)) => {
                                checkpoint_worker.abort();
                                let _ = checkpoint_worker.await;
                                Err("Agent checkpoint worker shutdown timed out".into())
                            }
                        }
                    } else {
                        worker.abort();
                        let _ = worker.await;
                        checkpoint_worker.abort();
                        let _ = checkpoint_worker.await;
                        drop(lease);
                        Err("Agent executor worker shutdown timed out".into())
                    }
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId, TurnId,
    };
    use rsi_agent_turn_protocol::ExecutorLease;
    use rsi_media_protocol::MediaId;
    use rsi_sandbox::SandboxMode;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    #[test]
    fn prepared_executor_charge_includes_inline_and_dynamic_config_state() {
        let config = ExecutorConfig {
            executor_id: "executor".into(),
            max_context_messages: default_context_messages(),
            max_context_bytes: default_context_bytes(),
            durability_wait_ms: default_durability_wait_ms(),
            finalization_wait_ms: default_finalization_wait_ms(),
            retained_tool_wait_ms: default_retained_tool_wait_ms(),
        };

        assert_eq!(
            executor_config_retained_bytes(&config).unwrap(),
            std::mem::size_of::<ExecutorConfig>() + config.executor_id.len()
        );
    }

    #[test]
    fn context_failure_diagnostic_is_utf8_safe_and_protocol_bounded() {
        let outcome = failure_outcome(
            "context.invalid",
            "界".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES),
        );
        outcome.validate().unwrap();
        let TurnOutcome::Failed { message, .. } = outcome else {
            panic!("context failure must preserve its typed terminal class");
        };
        assert!(message.len() <= MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    }

    #[test]
    fn tool_admission_failures_keep_their_stable_terminal_codes() {
        for (error, expected_code, expected_message) in [
            (
                ToolError::Capacity,
                "tool.capacity",
                "Tool capacity is exhausted",
            ),
            (
                ToolError::ShuttingDown,
                "tool.shutting_down",
                "Tool provider is shutting down",
            ),
        ] {
            let DriveFailure::Turn(TurnOutcome::Failed { code, message }) = tool_failure(&error)
            else {
                panic!("tool admission failure changed terminal class")
            };
            assert_eq!(code, expected_code);
            assert_eq!(message, expected_message);
        }
    }

    #[test]
    fn finalization_priority_matrix_preserves_partial_media_and_ignores_blockers_for_failures() {
        let media = MediaRef {
            id: MediaId::new("a".repeat(64)).unwrap(),
            mime: "image/png".into(),
            bytes: 1,
            width: 1,
            height: 1,
        };
        assert_eq!(
            apply_finalization_failure(
                TurnOutcome::PartialFailed {
                    media: vec![media.clone()],
                    code: "image.failure".into(),
                    message: "image failed".into(),
                },
                "jobs.cleanup",
                "cleanup failed",
                true,
            ),
            TurnOutcome::PartialFailed {
                media: vec![media],
                code: "jobs.cleanup".into(),
                message: "cleanup failed".into(),
            }
        );
        assert_eq!(
            apply_finalization_failure(
                TurnOutcome::Cancelled,
                "jobs.unreported",
                "output was not collected",
                false,
            ),
            TurnOutcome::Cancelled
        );
        assert!(matches!(
            apply_finalization_failure(
                TurnOutcome::BudgetExceeded {
                    dimension: BudgetDimension::Elapsed,
                    consumed: 10,
                    limit: 10,
                },
                "jobs.cleanup",
                "cleanup failed",
                true,
            ),
            TurnOutcome::Failed { code, .. } if code == "jobs.cleanup"
        ));
    }

    #[tokio::test]
    async fn completed_drive_wins_when_the_elapsed_deadline_is_also_ready() {
        let stop = CancellationToken::new();
        stop.cancel();

        let drive = select_drive_or_stop(
            &stop,
            std::future::ready(Err(DriveFailure::Turn(TurnOutcome::Completed))),
        )
        .await;

        assert!(matches!(
            drive,
            Err(DriveFailure::Turn(TurnOutcome::Completed))
        ));
        assert!(!elapsed_deadline_wins(true, &drive));
    }

    #[derive(Debug)]
    struct FullBeforePublish {
        facts: Vec<Arc<SessionFact>>,
        required_flush_seq: u64,
        durable_seq: AtomicU64,
        publish_calls: AtomicUsize,
        flushes: Mutex<Vec<u64>>,
        shutdown_on_publish: bool,
    }

    #[derive(Debug)]
    struct CheckpointFixture {
        facts: Vec<Arc<SessionFact>>,
        writes: Mutex<Vec<ContextCheckpoint>>,
    }

    #[async_trait]
    impl TurnExecution for CheckpointFixture {
        fn register(&self, _executor_id: String) -> rsi_agent_turn_protocol::Result<ExecutorLease> {
            unreachable!("checkpoint writer test does not register")
        }

        async fn claim(
            &self,
            _executor_id: &str,
            _cancellation: CancellationToken,
        ) -> rsi_agent_turn_protocol::Result<Option<TurnClaim>> {
            unreachable!("checkpoint writer test does not claim")
        }

        fn composition(
            &self,
            _claim: &TurnClaim,
        ) -> rsi_agent_turn_protocol::Result<AgentCompositionPin> {
            unreachable!("checkpoint writer test does not resolve composition")
        }

        async fn read_facts(
            &self,
            _claim: &TurnClaim,
            _after_seq: u64,
            _limit: usize,
        ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::ClaimFactPage> {
            unreachable!("checkpoint writer uses only maintenance reads")
        }

        async fn read_checkpoint_facts(
            &self,
            _claim: &TurnClaim,
            after_seq: u64,
            limit: usize,
        ) -> rsi_agent_turn_protocol::Result<Option<rsi_agent_turn_protocol::ClaimFactPage>>
        {
            let facts = self
                .facts
                .iter()
                .filter(|fact| fact.seq() > after_seq)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            Ok(Some(rsi_agent_turn_protocol::ClaimFactPage {
                through_seq: facts.last().map_or(after_seq, |fact| fact.seq()),
                facts,
            }))
        }

        async fn write_context_checkpoint(
            &self,
            _claim: &TurnClaim,
            checkpoint: ContextCheckpoint,
        ) -> rsi_agent_turn_protocol::Result<bool> {
            self.writes.lock().unwrap().push(checkpoint);
            Ok(true)
        }

        async fn publish(
            &self,
            _claim: &TurnClaim,
            _bodies: Vec<SessionFactBody>,
        ) -> rsi_agent_turn_protocol::Result<PublishAttempt> {
            unreachable!("checkpoint writer test does not publish")
        }

        async fn flush(
            &self,
            _claim: &TurnClaim,
            _through_seq: u64,
        ) -> rsi_agent_turn_protocol::Result<u64> {
            unreachable!("checkpoint writer test does not flush")
        }

        fn cancellation(
            &self,
            _claim: &TurnClaim,
        ) -> rsi_agent_turn_protocol::Result<CancellationToken> {
            unreachable!("checkpoint writer test does not cancel")
        }

        fn release(&self, _claim: &TurnClaim) -> rsi_agent_turn_protocol::Result<()> {
            unreachable!("checkpoint writer test does not release")
        }
    }

    #[async_trait]
    impl TurnExecution for FullBeforePublish {
        fn register(&self, _executor_id: String) -> rsi_agent_turn_protocol::Result<ExecutorLease> {
            unreachable!("terminal publication test does not register")
        }

        async fn claim(
            &self,
            _executor_id: &str,
            _cancellation: CancellationToken,
        ) -> rsi_agent_turn_protocol::Result<Option<TurnClaim>> {
            unreachable!("terminal publication test does not claim")
        }

        fn composition(
            &self,
            _claim: &TurnClaim,
        ) -> rsi_agent_turn_protocol::Result<AgentCompositionPin> {
            unreachable!("terminal publication test does not resolve composition")
        }

        async fn read_facts(
            &self,
            _claim: &TurnClaim,
            after_seq: u64,
            limit: usize,
        ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::ClaimFactPage> {
            let facts = self
                .facts
                .iter()
                .filter(|fact| fact.seq() > after_seq)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            Ok(rsi_agent_turn_protocol::ClaimFactPage {
                through_seq: facts.last().map_or(after_seq, |fact| fact.seq()),
                facts,
            })
        }

        async fn publish(
            &self,
            _claim: &TurnClaim,
            bodies: Vec<SessionFactBody>,
        ) -> rsi_agent_turn_protocol::Result<PublishAttempt> {
            if self.shutdown_on_publish {
                return Err(TurnError::ShuttingDown);
            }
            if self.publish_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(PublishAttempt::FlushRequired {
                    unpublished: bodies,
                });
            }
            if self.durable_seq.load(Ordering::SeqCst) < self.required_flush_seq {
                return Ok(PublishAttempt::FlushRequired {
                    unpublished: bodies,
                });
            }
            let next_seq = self
                .facts
                .last()
                .map_or(1, |fact| fact.seq().saturating_add(1));
            Ok(PublishAttempt::Published(vec![Arc::new(
                SessionFact::new(next_seq, next_seq, bodies.into_iter().next().unwrap()).unwrap(),
            )]))
        }

        async fn flush(
            &self,
            _claim: &TurnClaim,
            through_seq: u64,
        ) -> rsi_agent_turn_protocol::Result<u64> {
            self.flushes.lock().unwrap().push(through_seq);
            self.durable_seq.store(through_seq, Ordering::SeqCst);
            Ok(through_seq)
        }

        fn cancellation(
            &self,
            _claim: &TurnClaim,
        ) -> rsi_agent_turn_protocol::Result<CancellationToken> {
            unreachable!("terminal publication test does not cancel")
        }

        fn release(&self, _claim: &TurnClaim) -> rsi_agent_turn_protocol::Result<()> {
            unreachable!("terminal publication test does not release")
        }
    }

    fn claim() -> (TurnClaim, SessionFact) {
        let session_id = SessionId::new("session-terminal-retry").unwrap();
        let turn_id = TurnId::new("turn-terminal-retry").unwrap();
        let header = SessionHeader::new(
            session_id.clone(),
            1,
            "/tmp",
            AgentPresetId::new("test-agent").unwrap(),
            FrozenAgentSettings::new(
                "test",
                "system",
                ModelRef::new("test", "model").unwrap(),
                SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let accepted = SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: turn_id.clone(),
                text: "task".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap();
        (
            rsi_agent_turn_protocol::TurnClaimIssuer::new().issue(
                "executor".into(),
                1,
                session_id,
                turn_id,
                Arc::new(header),
                1,
                1,
                1,
            ),
            accepted,
        )
    }

    #[tokio::test]
    async fn terminal_publication_flushes_and_retries_a_full_speculative_suffix() {
        let (claim, accepted) = claim();
        let turns = FullBeforePublish {
            facts: vec![Arc::new(accepted)],
            required_flush_seq: 1,
            durable_seq: AtomicU64::new(0),
            publish_calls: AtomicUsize::new(0),
            flushes: Mutex::new(Vec::new()),
            shutdown_on_publish: false,
        };
        let config = ExecutorConfig {
            executor_id: "executor".into(),
            max_context_messages: default_context_messages(),
            max_context_bytes: default_context_bytes(),
            durability_wait_ms: 1_000,
            finalization_wait_ms: 1_000,
            retained_tool_wait_ms: 1_000,
        };

        publish_terminal(&turns, &config, &claim, TurnOutcome::Completed)
            .await
            .unwrap();
        assert_eq!(turns.publish_calls.load(Ordering::SeqCst), 2);
        assert_eq!(turns.flushes.lock().unwrap().as_slice(), [1, 2]);
    }

    #[tokio::test]
    async fn terminal_publication_treats_kernel_shutdown_as_driver_stop() {
        let (claim, accepted) = claim();
        let turns = FullBeforePublish {
            facts: vec![Arc::new(accepted)],
            required_flush_seq: 1,
            durable_seq: AtomicU64::new(0),
            publish_calls: AtomicUsize::new(0),
            flushes: Mutex::new(Vec::new()),
            shutdown_on_publish: true,
        };
        let config = ExecutorConfig {
            executor_id: "executor".into(),
            max_context_messages: default_context_messages(),
            max_context_bytes: default_context_bytes(),
            durability_wait_ms: 1_000,
            finalization_wait_ms: 1_000,
            retained_tool_wait_ms: 1_000,
        };

        assert!(matches!(
            publish_terminal(&turns, &config, &claim, TurnOutcome::Completed).await,
            Err(DriveFailure::Stopped)
        ));
    }

    #[tokio::test]
    async fn nonterminal_publication_flushes_the_live_tail_when_the_fold_lags() {
        let (claim, accepted) = claim();
        let later = SessionFact::new(
            2,
            2,
            SessionFactBody::CancelRequested {
                turn_id: claim.turn_id().clone(),
                reason: Some("published outside the fold".into()),
            },
        )
        .unwrap();
        let turns = FullBeforePublish {
            facts: vec![Arc::new(accepted), Arc::new(later)],
            required_flush_seq: 2,
            durable_seq: AtomicU64::new(0),
            publish_calls: AtomicUsize::new(0),
            flushes: Mutex::new(Vec::new()),
            shutdown_on_publish: false,
        };
        let config = ExecutorConfig {
            executor_id: "executor".into(),
            max_context_messages: default_context_messages(),
            max_context_bytes: default_context_bytes(),
            durability_wait_ms: 1_000,
            finalization_wait_ms: 1_000,
            retained_tool_wait_ms: 1_000,
        };

        let facts = publish_nonterminal_with_capacity_retry(
            &turns,
            &config,
            &claim,
            vec![SessionFactBody::CancelRequested {
                turn_id: claim.turn_id().clone(),
                reason: Some("retry".into()),
            }],
        )
        .await
        .unwrap();

        assert_eq!(facts.last().unwrap().seq(), 3);
        assert_eq!(turns.flushes.lock().unwrap().as_slice(), [2]);
    }

    #[tokio::test]
    async fn checkpoint_writer_coalesces_and_preserves_a_queued_turn() {
        let (claim, accepted) = claim();
        let queued_turn = TurnId::new("turn-queued").unwrap();
        let fixture = Arc::new(CheckpointFixture {
            facts: vec![
                Arc::new(accepted),
                Arc::new(
                    SessionFact::new(
                        2,
                        2,
                        SessionFactBody::TurnTerminal {
                            turn_id: claim.turn_id().clone(),
                            outcome: TurnOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ),
                Arc::new(
                    SessionFact::new(
                        3,
                        3,
                        SessionFactBody::TurnAccepted {
                            turn_id: queued_turn,
                            text: "queued task".into(),
                            model: None,
                            sandbox: SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                    )
                    .unwrap(),
                ),
            ],
            writes: Mutex::new(Vec::new()),
        });
        let turns: Arc<dyn TurnExecution> = fixture.clone();
        let (sender, receiver) = watch::channel(None);
        let request = CheckpointRequest {
            claim: claim.clone(),
            limits: ContextLimits::default(),
        };
        sender.send_replace(Some(request.clone()));
        sender.send_replace(Some(request));
        drop(sender);

        run_checkpoint_writer(turns, receiver).await;

        let writes = fixture.writes.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].through_seq, 3);
        let restored = ContextFold::from_checkpoint(
            claim.header().clone(),
            ContextLimits::default(),
            &writes[0].bytes,
        )
        .unwrap();
        assert_eq!(restored.through_seq(), 3);
        assert!(
            serde_json::to_string(&restored.project(ContextLimits::default()).unwrap().messages)
                .unwrap()
                .contains("queued task")
        );
    }

    #[test]
    fn completed_model_without_a_successor_is_not_classified_as_fresh_work() {
        let (claim, accepted) = claim();
        let effect_id = EffectId::new("effect-model-complete").unwrap();
        let facts = vec![
            accepted,
            SessionFact::new(
                2,
                2,
                SessionFactBody::ModelIntent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    snapshot: PreparedCallSnapshot {
                        call_id: "call-1".into(),
                        deployment_id: "test".into(),
                        provider_family: "test".into(),
                        capability: rsi_ai_protocol::AiCapability::Language,
                        model: "model".into(),
                        protocol: "test".into(),
                        transport: "memory".into(),
                        endpoint_fingerprint: "endpoint".into(),
                        config_generation: 1,
                        credential_source: None,
                        retry_policy: rsi_ai_protocol::RetryPolicy::default(),
                        request_sha256: "a".repeat(64),
                    },
                },
            )
            .unwrap(),
            SessionFact::new(
                3,
                3,
                SessionFactBody::ModelStarted {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                },
            )
            .unwrap(),
            SessionFact::new(
                4,
                4,
                SessionFactBody::ModelEvent {
                    turn_id: claim.turn_id().clone(),
                    effect_id,
                    event: LanguageEvent::Finished {
                        reason: FinishReason::Stop,
                        replay: None,
                    },
                },
            )
            .unwrap(),
        ];
        let mut state = ScannedTurn::default();
        let facts = facts.into_iter().map(Arc::new).collect::<Vec<_>>();
        scan_turn(&claim, &mut state, &facts).unwrap();
        assert!(state.completed_model_without_successor);
        assert!(state.effect.is_none());
    }

    #[test]
    fn finalization_deadline_is_bounded_during_factory_preparation() {
        let factory = ExecutorFactory;
        for wait in [0, MAXIMUM_FINALIZATION_WAIT_MS + 1] {
            let error = factory
                .prepare(&serde_json::json!({
                    "executor_id": "executor",
                    "finalization_wait_ms": wait
                }))
                .expect_err("unbounded finalization deadline");
            assert!(error.to_string().contains("finalization_wait_ms"));
        }
    }

    #[test]
    fn retained_tool_deadline_is_bounded_during_factory_preparation() {
        let factory = ExecutorFactory;
        for wait in [0, MAXIMUM_RETAINED_TOOL_WAIT_MS + 1] {
            let error = factory
                .prepare(&serde_json::json!({
                    "executor_id": "executor",
                    "retained_tool_wait_ms": wait
                }))
                .expect_err("unbounded retained Tool deadline");
            assert!(error.to_string().contains("retained_tool_wait_ms"));
        }
    }
}
