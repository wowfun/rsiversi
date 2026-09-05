//! Ordinary durable Agent executor over exact Local dependencies.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod checkpoint;

use checkpoint::{CheckpointRequest, CheckpointScheduler, run_checkpoint_writer};

use async_trait::async_trait;
use futures_util::{FutureExt as _, StreamExt as _, future::join_all};
use rsi_agent_composition_protocol::AgentCompositionPin;
use rsi_agent_context::{ContextFold, ContextLimits};
use rsi_agent_session_protocol::{
    BudgetDimension, EffectId, EffectKind, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, SessionFact,
    SessionFactBody, SessionId, TurnId, TurnOutcome,
};
use rsi_agent_turn_protocol::{
    PublishAttempt, TurnClaim, TurnError, TurnExecution, TurnExecutionContract, TurnFinalization,
    TurnFinalizationContext, TurnFinalizationContract, TurnFinalizationError,
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
    ToolExecutionPolicy, ToolLaneParkingAuthority, ToolLaneParkingService, ToolResult,
    ToolResultIdentity, ToolScheduling, ToolStart, parse_tool_arguments,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

const EXECUTOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(200);
const MAXIMUM_DURABILITY_WAIT_MS: u64 = 5 * 60 * 1_000;
const MAXIMUM_FINALIZATION_WAIT_MS: u64 = 5 * 60 * 1_000;
const MAXIMUM_RETAINED_TOOL_WAIT_MS: u64 = 5 * 60 * 1_000;
const MAXIMUM_EXECUTOR_ACTIVE_TURNS: usize = 256;

tokio::task_local! {
    static EXECUTOR_LANE_PARKING: ToolLaneParkingAuthority;
}

#[derive(Debug)]
struct ExecutorLaneParking {
    admission: Arc<Semaphore>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
    stop: CancellationToken,
    closed: CancellationToken,
}

impl ExecutorLaneParking {
    fn close(&self) {
        self.closed.cancel();
        self.permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[async_trait]
impl ToolLaneParkingService for ExecutorLaneParking {
    async fn park(&self) -> rsi_tools_protocol::Result<()> {
        let mut current = self
            .permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.closed.is_cancelled() {
            return Err(ToolError::ShuttingDown);
        }
        let permit = current
            .take()
            .ok_or_else(|| ToolError::InvalidInput("executor lane is already parked".into()))?;
        drop(current);
        drop(permit);
        Ok(())
    }

    async fn resume(&self, cancellation: CancellationToken) -> rsi_tools_protocol::Result<()> {
        let permit = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ToolError::Cancelled),
            () = self.closed.cancelled() => return Err(ToolError::ShuttingDown),
            () = self.stop.cancelled() => return Err(ToolError::ShuttingDown),
            permit = Arc::clone(&self.admission).acquire_owned() => {
                permit.map_err(|_| ToolError::ShuttingDown)?
            }
        };
        let mut current = self
            .permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if self.closed.is_cancelled() || self.stop.is_cancelled() {
            return Err(ToolError::ShuttingDown);
        }
        if current.is_some() {
            return Err(ToolError::InvalidInput(
                "executor lane resumed more than once".into(),
            ));
        }
        *current = Some(permit);
        Ok(())
    }
}

/// Explicit executor instance configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorConfig {
    /// Exact registration identity, unique in one Kernel generation.
    pub executor_id: String,
    /// Maximum turns active across distinct Sessions.
    #[serde(default = "default_maximum_active_turns")]
    pub maximum_active_turns: usize,
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

const fn default_maximum_active_turns() -> usize {
    1
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
        if self.maximum_active_turns == 0
            || self.maximum_active_turns > MAXIMUM_EXECUTOR_ACTIVE_TURNS
        {
            return Err(ExecutorError::Invalid(format!(
                "maximum_active_turns must be within 1..={MAXIMUM_EXECUTOR_ACTIVE_TURNS}"
            )));
        }
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
    active_tools: Mutex<BTreeMap<(SessionId, TurnId), BTreeMap<ToolResultIdentity, TrackedTool>>>,
    retirement_tasks: Mutex<Vec<JoinHandle<()>>>,
    checkpoints: Arc<CheckpointScheduler>,
    config: ExecutorConfig,
}

#[derive(Clone, Debug)]
struct TrackedTool {
    composition: AgentCompositionPin,
}

struct PreparedToolEffect {
    effect_id: EffectId,
    identity: ToolResultIdentity,
    prepared: Box<dyn PreparedToolCall>,
}

struct PendingToolEffect {
    effect_id: EffectId,
    identity: ToolResultIdentity,
    name: String,
    arguments: Value,
    approval: Option<rsi_approval_protocol::ApprovalOutcome>,
    parallel_safe: bool,
    prepared: Box<dyn PreparedToolCall>,
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

mod driver;
mod execution_support;

use execution_support::{
    CombinedCancellation, DriveFailure, ai_failure, apply_finalization_failure, bounded,
    combine_cancellation, failed, failure_outcome, fatal, image_ai_failure,
    image_operation_failure, next_effect_id, prepare_tool_effect, publish_budget_exhaustion,
    publish_nonterminal_with_capacity_retry, publish_terminal, retry_delay, run_executor_pool,
    settled_tool_budget, should_retry, tool_failure, unix_now_ms,
};

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
    effects: Vec<ResumeEffect>,
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
        effect_id: EffectId,
        started: bool,
    },
    Image {
        effect_id: EffectId,
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
            SessionFactBody::MessageTurnAccepted {
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
            SessionFactBody::ModelIntent {
                turn_id, effect_id, ..
            } if turn_id == claim.turn_id() => {
                state.completed_model_without_successor = false;
                state.effects.push(ResumeEffect::Model {
                    effect_id: effect_id.clone(),
                    started: false,
                });
            }
            SessionFactBody::ModelStarted { turn_id, effect_id } if turn_id == claim.turn_id() => {
                let Some(ResumeEffect::Model { started, .. }) = state.effects.iter_mut().find(
                    |effect| matches!(effect, ResumeEffect::Model { effect_id: current, .. } if current == effect_id),
                ) else {
                    return Err("model start lacks exact intent");
                };
                *started = true;
            }
            SessionFactBody::ImageIntent {
                turn_id, effect_id, ..
            } if turn_id == claim.turn_id() => {
                state.effects.push(ResumeEffect::Image {
                    effect_id: effect_id.clone(),
                    started: false,
                });
            }
            SessionFactBody::ImageStarted { turn_id, effect_id } if turn_id == claim.turn_id() => {
                let Some(ResumeEffect::Image { started, .. }) = state.effects.iter_mut().find(
                    |effect| matches!(effect, ResumeEffect::Image { effect_id: current, .. } if current == effect_id),
                ) else {
                    return Err("Image start lacks exact intent");
                };
                *started = true;
            }
            SessionFactBody::ModelEvent {
                turn_id,
                effect_id,
                event,
            } if turn_id == claim.turn_id()
                && matches!(
                    event,
                    LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
                ) =>
            {
                let Some(index) = state.effects.iter().position(
                    |effect| matches!(effect, ResumeEffect::Model { effect_id: current, .. } if current == effect_id),
                ) else {
                    return Err("model terminal event lacks exact start");
                };
                state.effects.remove(index);
                state.completed_model_without_successor = true;
            }
            SessionFactBody::ToolIntent {
                turn_id,
                effect_id,
                identity,
                ..
            } if turn_id == claim.turn_id() => {
                state.completed_model_without_successor = false;
                state.effects.push(ResumeEffect::Tool {
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                    started: false,
                });
            }
            SessionFactBody::ToolStarted {
                turn_id,
                effect_id,
                identity,
            } if turn_id == claim.turn_id() => {
                let Some(ResumeEffect::Tool { started, .. }) = state.effects.iter_mut().find(
                    |effect| matches!(effect, ResumeEffect::Tool { effect_id: current, identity: current_identity, .. } if current == effect_id && current_identity == identity),
                ) else {
                    return Err("Tool start lacks exact intent");
                };
                *started = true;
            }
            SessionFactBody::ToolResult {
                turn_id,
                effect_id,
                identity,
                ..
            } if turn_id == claim.turn_id() => {
                let Some(index) = state.effects.iter().position(
                    |effect| matches!(effect, ResumeEffect::Tool { effect_id: current, identity: current_identity, .. } if current == effect_id && current_identity == identity),
                ) else {
                    return Err("Tool result lacks exact start");
                };
                state.effects.remove(index);
            }
            SessionFactBody::TurnTerminal { turn_id, .. } if turn_id == claim.turn_id() => {
                state.terminal = true;
                state.completed_model_without_successor = false;
                state.effects.clear();
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
            | SessionFactBody::MessageTurnAccepted { .. }
            | SessionFactBody::StepStarted { .. }
            | SessionFactBody::InputMessageEntered { .. }
            | SessionFactBody::StepEnded { .. }
            | SessionFactBody::WorkspaceTouched { .. }
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
        let checkpoints = Arc::new(CheckpointScheduler::new());
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
            checkpoints: Arc::clone(&checkpoints),
            config,
        });
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let task_driver = Arc::clone(&driver);
        let mut worker = tokio::spawn(async move {
            let result = run_executor_pool(task_driver, task_stop).await;
            drop(lease);
            result
        });
        let checkpoint_scheduler = Arc::clone(&checkpoints);
        let mut checkpoint_worker = tokio::spawn(async move {
            run_checkpoint_writer(checkpoint_turns, checkpoint_scheduler).await;
        });
        plan.defer(
            "shutdown Agent executor",
            Box::new(move || {
                Box::pin(async move {
                    stop.cancel();
                    let deadline = tokio::time::Instant::now() + EXECUTOR_SHUTDOWN_TIMEOUT;
                    let mut failures = Vec::new();
                    let pool_cleaned = match tokio::time::timeout_at(deadline, &mut worker).await {
                        Ok(Ok(Ok(()))) => true,
                        Ok(Ok(Err(error))) => {
                            failures.push(error);
                            true
                        }
                        Ok(Err(error)) => {
                            failures.push(format!("Agent executor worker failed: {error}"));
                            false
                        }
                        Err(_) => {
                            failures.push("Agent executor worker shutdown timed out".into());
                            false
                        }
                    };
                    if !pool_cleaned {
                        worker.abort();
                        let _ = worker.await;
                        driver.abort_retirement_tasks().await;
                    }
                    checkpoints.close();
                    match tokio::time::timeout_at(deadline, &mut checkpoint_worker).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            failures.push(format!("Agent checkpoint worker failed: {error}"));
                        }
                        Err(_) => {
                            failures.push("Agent checkpoint worker shutdown timed out".into());
                            checkpoint_worker.abort();
                            let _ = checkpoint_worker.await;
                        }
                    }
                    if failures.is_empty() {
                        Ok(())
                    } else {
                        Err(failures.join("; "))
                    }
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests;
