//! Ordinary durable Agent executor over exact Local dependencies.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_context::{ContextFold, ContextLimits};
use rsi_agent_session_protocol::{
    EffectId, EffectKind, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, SessionFact, SessionFactBody, TurnOutcome,
};
use rsi_agent_turn_protocol::{
    TurnClaim, TurnError, TurnExecution, TurnExecutionContract, TurnFinalization,
    TurnFinalizationContract, TurnFinalizationError,
};
use rsi_ai_protocol::{
    DispatchStatus, ErrorKind, FinishReason, ImageAssembler, ImageCall, ImageCallContract,
    ImageEvent, ImageRequest, LanguageAssembler, LanguageAssemblyError, LanguageCall,
    LanguageCallContract, LanguageEvent, ModelRef, PreparedCallSnapshot, ToolCall as ModelToolCall,
    ToolCallKind,
};
use rsi_approval_protocol::{
    Approval, ApprovalContract, ApprovalDecision, ApprovalError, ApprovalRequest,
};
use rsi_media_protocol::{Media, MediaContract, MediaRef};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_sandbox::{Sandbox, SandboxContract, SandboxMode};
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolFailureKind, RetainedToolResult, ToolCall, ToolError,
    ToolExecutionPolicy, ToolResult, ToolResultIdentity, ToolRuntime, ToolRuntimeContract,
    ToolStart, parse_tool_arguments,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const EXECUTOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(200);
const MAXIMUM_DURABILITY_WAIT_MS: u64 = 5 * 60 * 1_000;
const MAXIMUM_FINALIZATION_WAIT_MS: u64 = 5 * 60 * 1_000;

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
}

#[derive(Debug)]
struct Driver {
    turns: Arc<dyn TurnExecution>,
    finalization: Arc<dyn TurnFinalization>,
    language: Arc<dyn LanguageCall>,
    image: Arc<dyn ImageCall>,
    media: Arc<dyn Media>,
    tools: Arc<dyn ToolRuntime>,
    approval: Arc<dyn Approval>,
    sandbox: Arc<dyn Sandbox>,
    config: ExecutorConfig,
}

impl Driver {
    async fn run(self: Arc<Self>, stop: CancellationToken) {
        loop {
            let Ok(Some(claim)) = self
                .turns
                .claim(&self.config.executor_id, stop.clone())
                .await
            else {
                return;
            };
            match self.drive(&claim, &stop).await {
                Ok(()) => {
                    let _ignored = self.turns.release(&claim);
                }
                Err(DriveFailure::Stopped) => {
                    let _ignored = self.turns.release(&claim);
                    return;
                }
                Err(DriveFailure::Turn(outcome)) => {
                    let _ignored = self.finish(&claim, outcome).await;
                    let _ignored = self.turns.release(&claim);
                }
                Err(DriveFailure::SettledTool { outcome, identity }) => {
                    if self.finish(&claim, outcome).await.is_ok() {
                        let _ignored = self.tools.commit(&identity);
                    }
                    let _ignored = self.turns.release(&claim);
                }
                Err(DriveFailure::Fatal(message)) => {
                    let _ignored = self
                        .finish(
                            &claim,
                            TurnOutcome::Failed {
                                code: "executor.internal".into(),
                                message,
                            },
                        )
                        .await;
                    let _ignored = self.turns.release(&claim);
                }
            }
        }
    }

    async fn drive(
        &self,
        claim: &TurnClaim,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let mut fold = ContextFold::with_limits(
            claim.header.clone(),
            ContextLimits::new(
                self.config.max_context_messages,
                self.config.max_context_bytes,
            )
            .map_err(|error| failed("context.invalid", error.to_string()))?,
        )
        .map_err(|error| failed("context.invalid", error.to_string()))?;
        let state = self.load_claim(claim, &mut fold).await?;
        if state.terminal {
            return Ok(());
        }
        if state.completed_model_without_successor {
            return Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                effect: Some(EffectKind::Model),
                reason: "a completed model effect lacks a durable terminal or successor intent"
                    .into(),
            }));
        }
        self.resume_effect(claim, &mut fold, state.effect, stop)
            .await?;

        if let Some((model, request)) = state.image {
            return self.run_image(claim, &mut fold, model, request, stop).await;
        }

        let turn_policy = state.turn_policy.ok_or_else(|| {
            failed(
                "executor.invalid_history",
                "Language turn lacks a resolved execution policy",
            )
        })?;
        let model = state
            .model
            .unwrap_or_else(|| claim.header.profile().default_model().clone());
        self.run_language(claim, &mut fold, model, turn_policy, stop)
            .await
    }

    async fn run_language(
        &self,
        claim: &TurnClaim,
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
                .run_model_attempt(claim, fold, &model, retry_attempt, &cancellation, stop)
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
                self.run_tool(claim, fold, call, turn_policy, &cancellation, stop)
                    .await?;
            }
        }
    }

    async fn resume_effect(
        &self,
        claim: &TurnClaim,
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
                self.recover_tool(claim, fold, effect_id, identity, stop)
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
                    turn_id: claim.turn_id.clone(),
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
                    turn_id: claim.turn_id.clone(),
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
                            turn_id: claim.turn_id.clone(),
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

    async fn run_model_attempt(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
        model: &ModelRef,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        self.sync_fold(claim, fold).await?;
        let request = fold
            .request(self.config.limits(), self.tools.definitions())
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
                    turn_id: claim.turn_id.clone(),
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
                    turn_id: claim.turn_id.clone(),
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
                        turn_id: claim.turn_id.clone(),
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

    async fn run_tool(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
        call: ModelToolCall,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
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
        let prepared = self
            .tools
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
                    turn_id: claim.turn_id.clone(),
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
                    turn_id: claim.turn_id.clone(),
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &started).await?;
        let result = self
            .start_tool(
                prepared,
                &identity,
                claim,
                turn_policy.sandbox,
                cancellation,
                stop,
            )
            .await?;
        let returned = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ToolResult {
                    turn_id: claim.turn_id.clone(),
                    effect_id,
                    identity: identity.clone(),
                    result,
                }],
            )
            .await?;
        self.flush_last(claim, &returned).await?;
        self.tools
            .commit(&identity)
            .map_err(|error| tool_failure(&error))
    }

    async fn start_tool(
        &self,
        prepared: Box<dyn PreparedToolCall>,
        identity: &ToolResultIdentity,
        claim: &TurnClaim,
        sandbox_mode: SandboxMode,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ToolResult, DriveFailure> {
        let combined = combine_cancellation(cancellation, stop);
        let cwd = std::path::PathBuf::from(claim.header.canonical_cwd());
        let result = prepared
            .start(ToolStart {
                cancellation: combined.token(),
                policy: ToolExecutionPolicy {
                    mode: sandbox_mode,
                    cwd: cwd.clone(),
                    workspace: cwd,
                },
                sandbox: Arc::clone(&self.sandbox),
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
                    self.tools.query(identity),
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
            id: effect_id.as_str().to_owned(),
            action: format!("run tool {tool_name}"),
            reason: format!(
                "Agent turn {} requested this Tool effect",
                claim.turn_id.as_str()
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
        fold: &mut ContextFold,
        effect_id: EffectId,
        identity: ToolResultIdentity,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let retained = self.tools.wait(&identity, stop.clone()).await;
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
                            turn_id: claim.turn_id.clone(),
                            effect_id: effect_id.clone(),
                            identity: identity.clone(),
                            result,
                        }],
                    )
                    .await?;
                self.flush_last(claim, &facts).await?;
                self.tools
                    .commit(&identity)
                    .map_err(|error| tool_failure(&error))?;
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
                    turn_id: claim.turn_id.clone(),
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
        outcome: TurnOutcome,
    ) -> std::result::Result<(), DriveFailure> {
        let outcome = match tokio::time::timeout(
            self.config.finalization_wait(),
            self.finalization
                .finalize(&claim.session_id, &claim.turn_id),
        )
        .await
        {
            Ok(Ok(())) => outcome,
            Ok(Err(TurnFinalizationError::Failed { code, message })) => TurnOutcome::Failed {
                code: bounded(&code),
                message: bounded(&message),
            },
            Ok(Err(TurnFinalizationError::Invalid(message))) => TurnOutcome::Failed {
                code: "turn.finalization".into(),
                message: bounded(&message),
            },
            Err(_) => TurnOutcome::Failed {
                code: "turn.finalization_timeout".into(),
                message: format!(
                    "turn finalization exceeded {} ms",
                    self.config.finalization_wait_ms
                ),
            },
        };
        publish_terminal(self.turns.as_ref(), &self.config, claim, outcome).await
    }

    async fn publish_apply(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
        bodies: Vec<SessionFactBody>,
    ) -> std::result::Result<Vec<SessionFact>, DriveFailure> {
        let facts = match self.turns.publish(claim, bodies.clone()).await {
            Ok(facts) => facts,
            Err(TurnError::Flush(_)) if fold.through_seq() > 0 => {
                self.flush_durable(claim, fold.through_seq()).await?;
                self.turns.publish(claim, bodies).await.map_err(fatal)?
            }
            Err(error) => return Err(fatal(error)),
        };
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
        facts: &[SessionFact],
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

async fn publish_terminal(
    turns: &dyn TurnExecution,
    config: &ExecutorConfig,
    claim: &TurnClaim,
    outcome: TurnOutcome,
) -> std::result::Result<(), DriveFailure> {
    let mut last_capacity_flush = None;
    let facts = loop {
        match turns
            .publish(
                claim,
                vec![SessionFactBody::TurnTerminal {
                    turn_id: claim.turn_id.clone(),
                    outcome: outcome.clone(),
                }],
            )
            .await
        {
            Ok(facts) => break facts,
            Err(TurnError::Flush(_)) => {
                let tail = live_tail(turns, claim).await?;
                if tail == 0 || last_capacity_flush.is_some_and(|flushed| flushed >= tail) {
                    return Err(fatal(
                        "terminal publication remained full without new flushable Facts",
                    ));
                }
                flush_execution_prefix(turns, config, claim, tail).await?;
                last_capacity_flush = Some(tail);
            }
            Err(error) => return Err(fatal(error)),
        }
    };
    let seq = facts
        .last()
        .ok_or_else(|| failed("executor.empty_publish", "publication returned no Facts"))?
        .seq();
    flush_execution_prefix(turns, config, claim, seq).await?;
    Ok(())
}

async fn live_tail(
    turns: &dyn TurnExecution,
    claim: &TurnClaim,
) -> std::result::Result<u64, DriveFailure> {
    // Facts at or before the claim were already represented by its live watermark.
    // Only scan the executor-owned suffix when recovering publication capacity.
    let mut cursor = claim.live_seq;
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

fn scan_turn(
    claim: &TurnClaim,
    state: &mut ScannedTurn,
    facts: &[SessionFact],
) -> std::result::Result<(), &'static str> {
    for fact in facts {
        match fact.body() {
            SessionFactBody::TurnAccepted {
                turn_id,
                model,
                sandbox,
                require_approval,
                ..
            } if turn_id == &claim.turn_id => {
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
            } if turn_id == &claim.turn_id => {
                state.image = Some((model.clone(), request.clone()));
            }
            SessionFactBody::ModelIntent { turn_id, .. } if turn_id == &claim.turn_id => {
                state.completed_model_without_successor = false;
                state.effect = Some(ResumeEffect::Model { started: false });
            }
            SessionFactBody::ModelStarted { turn_id, .. } if turn_id == &claim.turn_id => {
                match &mut state.effect {
                    Some(ResumeEffect::Model { started }) => *started = true,
                    _ => {
                        return Err("model start lacks intent");
                    }
                }
            }
            SessionFactBody::ImageIntent { turn_id, .. } if turn_id == &claim.turn_id => {
                state.effect = Some(ResumeEffect::Image { started: false });
            }
            SessionFactBody::ImageStarted { turn_id, .. } if turn_id == &claim.turn_id => {
                match &mut state.effect {
                    Some(ResumeEffect::Image { started }) => *started = true,
                    _ => {
                        return Err("Image start lacks intent");
                    }
                }
            }
            SessionFactBody::ModelEvent { turn_id, event, .. }
                if turn_id == &claim.turn_id
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
            } if turn_id == &claim.turn_id => {
                state.completed_model_without_successor = false;
                state.effect = Some(ResumeEffect::Tool {
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                    started: false,
                });
            }
            SessionFactBody::ToolStarted { turn_id, .. } if turn_id == &claim.turn_id => {
                match &mut state.effect {
                    Some(ResumeEffect::Tool { started, .. }) => *started = true,
                    _ => {
                        return Err("Tool start lacks intent");
                    }
                }
            }
            SessionFactBody::ToolResult { turn_id, .. } if turn_id == &claim.turn_id => {
                state.effect = None;
            }
            SessionFactBody::TurnTerminal { turn_id, .. } if turn_id == &claim.turn_id => {
                state.terminal = true;
                state.completed_model_without_successor = false;
                state.effect = None;
            }
            SessionFactBody::TurnAccepted { .. }
            | SessionFactBody::ImageRequested { .. }
            | SessionFactBody::CancelRequested { .. }
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
        ToolError::InvalidInput(_)
        | ToolError::Duplicate(_)
        | ToolError::Unknown(_)
        | ToolError::Execution(_) => failed("tool.execution", error.to_string()),
    }
}

fn failed(code: impl Into<String>, message: impl Into<String>) -> DriveFailure {
    let code = code.into();
    let message = message.into();
    DriveFailure::Turn(TurnOutcome::Failed {
        code: bounded(&code),
        message: bounded(&message),
    })
}

fn fatal(error: impl fmt::Display) -> DriveFailure {
    DriveFailure::Fatal(bounded(&error.to_string()))
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

#[derive(Debug)]
enum DriveFailure {
    Stopped,
    Turn(TurnOutcome),
    SettledTool {
        outcome: TurnOutcome,
        identity: ToolResultIdentity,
    },
    Fatal(String),
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

/// Ordinary executor factory over Turn, Language, Image, Media, and Tool Local contracts.
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
                .requiring_local::<ToolRuntimeContract>()
                .requiring_local::<ApprovalContract>()
                .requiring_local::<SandboxContract>(),
        )
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<ExecutorConfig>()?;
        let turns = plan.local::<TurnExecutionContract>()?;
        let finalization = plan.local::<TurnFinalizationContract>()?;
        let language = plan.local::<LanguageCallContract>()?;
        let image = plan.local::<ImageCallContract>()?;
        let media = plan.local::<MediaContract>()?;
        let tools = plan.local::<ToolRuntimeContract>()?;
        let approval = plan.local::<ApprovalContract>()?;
        let sandbox = plan.local::<SandboxContract>()?;
        let lease = turns
            .register(config.executor_id.clone())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let driver = Arc::new(Driver {
            turns,
            finalization,
            language,
            image,
            media,
            tools,
            approval,
            sandbox,
            config,
        });
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let mut worker = tokio::spawn(async move { driver.run(task_stop).await });
        plan.defer(
            "shutdown Agent executor",
            Box::new(move || {
                Box::pin(async move {
                    stop.cancel();
                    if let Ok(joined) =
                        tokio::time::timeout(EXECUTOR_SHUTDOWN_TIMEOUT, &mut worker).await
                    {
                        drop(lease);
                        match joined {
                            Ok(()) => Ok(()),
                            Err(error) => Err(format!("Agent executor worker failed: {error}")),
                        }
                    } else {
                        worker.abort();
                        let _ = worker.await;
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
    use rsi_agent_session_protocol::{FrozenAgentProfile, SessionHeader, SessionId, TurnId};
    use rsi_agent_turn_protocol::ExecutorLease;
    use rsi_sandbox::SandboxMode;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn prepared_executor_charge_includes_inline_and_dynamic_config_state() {
        let config = ExecutorConfig {
            executor_id: "executor".into(),
            max_context_messages: default_context_messages(),
            max_context_bytes: default_context_bytes(),
            durability_wait_ms: default_durability_wait_ms(),
            finalization_wait_ms: default_finalization_wait_ms(),
        };

        assert_eq!(
            executor_config_retained_bytes(&config).unwrap(),
            std::mem::size_of::<ExecutorConfig>() + config.executor_id.len()
        );
    }

    #[derive(Debug)]
    struct FullBeforeTerminal {
        accepted: SessionFact,
        publish_calls: AtomicUsize,
        flushes: Mutex<Vec<u64>>,
    }

    #[async_trait]
    impl TurnExecution for FullBeforeTerminal {
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

        async fn read_facts(
            &self,
            _claim: &TurnClaim,
            after_seq: u64,
            _limit: usize,
        ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::ClaimFactPage> {
            let facts = (after_seq == 0)
                .then(|| self.accepted.clone())
                .into_iter()
                .collect::<Vec<_>>();
            Ok(rsi_agent_turn_protocol::ClaimFactPage {
                through_seq: facts.last().map_or(after_seq, SessionFact::seq),
                facts,
            })
        }

        async fn publish(
            &self,
            claim: &TurnClaim,
            bodies: Vec<SessionFactBody>,
        ) -> rsi_agent_turn_protocol::Result<Vec<SessionFact>> {
            if self.publish_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(TurnError::Flush("speculative buffer is full".into()));
            }
            assert!(matches!(
                bodies.as_slice(),
                [SessionFactBody::TurnTerminal { turn_id, .. }] if turn_id == &claim.turn_id
            ));
            Ok(vec![
                SessionFact::new(2, 2, bodies.into_iter().next().unwrap()).unwrap(),
            ])
        }

        async fn flush(
            &self,
            _claim: &TurnClaim,
            through_seq: u64,
        ) -> rsi_agent_turn_protocol::Result<u64> {
            self.flushes.lock().unwrap().push(through_seq);
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
            FrozenAgentProfile::new(
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
            TurnClaim {
                executor_id: "executor".into(),
                claim_id: 1,
                session_id,
                turn_id,
                header,
                live_seq: 1,
            },
            accepted,
        )
    }

    #[tokio::test]
    async fn terminal_publication_flushes_and_retries_a_full_speculative_suffix() {
        let (claim, accepted) = claim();
        let turns = FullBeforeTerminal {
            accepted,
            publish_calls: AtomicUsize::new(0),
            flushes: Mutex::new(Vec::new()),
        };
        let config = ExecutorConfig {
            executor_id: "executor".into(),
            max_context_messages: default_context_messages(),
            max_context_bytes: default_context_bytes(),
            durability_wait_ms: 1_000,
            finalization_wait_ms: 1_000,
        };

        publish_terminal(&turns, &config, &claim, TurnOutcome::Completed)
            .await
            .unwrap();
        assert_eq!(turns.publish_calls.load(Ordering::SeqCst), 2);
        assert_eq!(turns.flushes.lock().unwrap().as_slice(), [1, 2]);
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
                    turn_id: claim.turn_id.clone(),
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
                    turn_id: claim.turn_id.clone(),
                    effect_id: effect_id.clone(),
                },
            )
            .unwrap(),
            SessionFact::new(
                4,
                4,
                SessionFactBody::ModelEvent {
                    turn_id: claim.turn_id.clone(),
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
}
