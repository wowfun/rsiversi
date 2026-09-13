//! Host Goal controls and the standard Session-owned admission bridge.

use async_trait::async_trait;
use futures_util::Stream;
use rsi_agent_goal::{GoalAction, GoalState, RoundSettlement};
use rsi_agent_session_protocol::{
    CommandRevision, ContinuationInput, ContinuationProvenance, DomainRequestId, DomainStateView,
    MessageId, SessionCommandInvocation, SessionCommandReceipt, SessionHeader, SessionId,
};
use rsi_agent_turn_protocol::{
    ContinuationBinding, ContinuationLease, DomainMutationReceipt, MessageReceipt,
};
use serde::{Deserialize, Serialize};
use std::{fmt, pin::Pin, sync::Arc};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// One explicit logical application control, frozen across reply reconciliation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalControl {
    /// Caller-preallocated idempotency identity.
    pub request_id: DomainRequestId,
    /// Exact draft or durable predecessor.
    pub expected_revision: CommandRevision,
    /// Bounded explicit state action.
    pub action: GoalAction,
}

impl GoalControl {
    /// Constructs the exact ordinary domain invocation for receipt authentication.
    ///
    /// # Errors
    /// Rejects arguments exceeding the command protocol's bound.
    pub fn invocation(&self) -> GoalResult<SessionCommandInvocation> {
        use rsi_agent_session_protocol::{CommandArguments, ContributionId};
        Ok(SessionCommandInvocation {
            command: ContributionId::new(rsi_agent_goal::GOAL_COMMAND)
                .map_err(|error| GoalError::Invalid(error.to_string()))?,
            request_id: self.request_id.clone(),
            expected_revision: self.expected_revision,
            arguments: CommandArguments::new(
                serde_json::to_value(&self.action)
                    .map_err(|error| GoalError::Invalid(error.to_string()))?,
            )
            .map_err(|error| GoalError::Invalid(error.to_string()))?,
        })
    }
}

/// Application command receipt with a separate live owner snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalControlReceipt {
    /// Canonical state command result; it never encodes live authority.
    pub command: SessionCommandReceipt,
    /// Current process-local driver observation.
    pub live: GoalLiveState,
}

/// Small complete driver snapshot, independent of the durable Goal projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalLiveState {
    /// Whether the owning Host generation currently exposes this capability.
    pub available: bool,
    /// Whether new allocations and automatic input remain explicitly authorized.
    pub armed: bool,
    /// Current controller stage.
    pub stage: GoalDriverStage,
    /// Exact latest automatic input being reconciled, if any.
    pub message_id: Option<MessageId>,
    /// Safe bounded failure or stopping explanation.
    pub detail: Option<String>,
}

impl Default for GoalLiveState {
    fn default() -> Self {
        Self {
            available: true,
            armed: false,
            stage: GoalDriverStage::Disarmed,
            message_id: None,
            detail: None,
        }
    }
}

impl GoalLiveState {
    /// Checks the small process-local observation at an external boundary.
    ///
    /// # Errors
    /// Rejects oversized diagnostics and contradictory live authority.
    pub fn validate(&self) -> GoalResult<()> {
        if self.detail.as_ref().is_some_and(|text| {
            text.len() > 4096
                || text
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        }) || (self.armed
            && (!self.available
                || matches!(
                    self.stage,
                    GoalDriverStage::Disarmed | GoalDriverStage::Failed | GoalDriverStage::Stopping
                )))
        {
            return Err(GoalError::Invalid("invalid live Goal state".into()));
        }
        Ok(())
    }
}

/// Controller stages describe live activity only.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalDriverStage {
    /// No live authorization; explicit create/resume is required.
    Disarmed,
    /// Reserving or accepting one frozen automatic input.
    Reserving,
    /// Waiting for that exact input's claim or canonical Turn outcome.
    Waiting,
    /// Reconciling the canonical source Turn with Goal state.
    Settling,
    /// Scheduling revoked while already admitted work settles.
    Stopping,
    /// A controller failure disarmed execution without inventing durable state.
    Failed,
}

/// Complete typed state at one command predecessor.
#[derive(Clone, Debug)]
pub struct GoalSnapshot {
    /// Exact candidate or durable Header.
    pub header: SessionHeader,
    /// Captured draft or control revision.
    pub revision: CommandRevision,
    /// Opaque complete domain and its exact CAS revision.
    pub domain: DomainStateView,
    /// Semantically decoded Goal state from that same domain snapshot.
    pub state: GoalState,
}

/// Closed controller error classes; reply uncertainty never means command rejection.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GoalError {
    /// Live scheduling was revoked or its guarded revision changed.
    #[error("Goal scheduling is disarmed")]
    Disarmed,
    /// Bounded argument, codec or state transition failure.
    #[error("invalid Goal control: {0}")]
    Invalid(String),
    /// A capability, Goal or referenced message is absent.
    #[error("Goal capability or state is unavailable")]
    Unavailable,
    /// An identity or revision no longer matches.
    #[error("Goal control conflicted with its current state")]
    Conflict,
    /// A known command rejection at its exact captured predecessor.
    #[error("Goal command revision conflict: expected {expected:?}, actual {actual:?}")]
    RevisionConflict {
        /// Revision captured by the rejected control.
        expected: CommandRevision,
        /// Canonical revision observed at rejection.
        actual: CommandRevision,
    },
    /// The Host's bounded controller set is full.
    #[error("Goal controller capacity is exhausted")]
    Capacity,
    /// Host withdrawal stopped this operation.
    #[error("Goal controller is shutting down")]
    ShuttingDown,
    /// The original action may have committed; do not allocate another identity.
    #[error("Goal control outcome is unknown: {0}")]
    OutcomeUnknown(String),
    /// Safe infrastructure diagnostic; it does not assert a durable Goal phase.
    #[error("Goal controller failed: {0}")]
    Backend(String),
}

/// Goal capability result.
pub type GoalResult<T> = Result<T, GoalError>;
/// Detachable coalesced complete live snapshots.
pub type GoalLiveStream = Pin<Box<dyn Stream<Item = GoalResult<GoalLiveState>> + Send + 'static>>;

/// Standard Session-owned bridge, retained by a live Host driver across GUI detach.
#[async_trait]
pub trait GoalSession: fmt::Debug + Send + Sync + 'static {
    /// Exact Session identity; no caller-selected alias.
    fn session_id(&self) -> &SessionId;
    /// Captures the current Header, command predecessor and semantically decoded state.
    async fn snapshot(&self) -> GoalResult<GoalSnapshot>;
    /// Runs an ordinary Goal command under the existing draft/durable command owner.
    async fn application_command(
        &self,
        invocation: SessionCommandInvocation,
    ) -> GoalResult<SessionCommandReceipt>;
    /// Queries the original ordinary receipt without replaying its callback.
    async fn command_status(
        &self,
        request: &DomainRequestId,
    ) -> GoalResult<Option<SessionCommandReceipt>>;
    /// Arms the exact current draft or durable composition after state reconciliation.
    async fn arm(&self, binding: ContinuationBinding) -> GoalResult<ContinuationLease>;
    /// Retains a disarmed lease for an explicit pause/cancel action after restart.
    async fn retain_for_settlement(
        &self,
        binding: ContinuationBinding,
    ) -> GoalResult<ContinuationLease>;
    /// Delegates one frozen internal reserve or settlement to Kernel admission.
    async fn internal_command(
        &self,
        lease: &ContinuationLease,
        invocation: SessionCommandInvocation,
        input: Option<ContinuationInput>,
    ) -> GoalResult<DomainMutationReceipt>;
    /// Queries the exact internal receipt without granting new live authority.
    async fn internal_status(
        &self,
        lease: &ContinuationLease,
        request: &DomainRequestId,
    ) -> GoalResult<Option<DomainMutationReceipt>>;
    /// Prepares Workspace access and atomically publishes a fresh draft if necessary.
    async fn submit(
        &self,
        lease: &ContinuationLease,
        input: ContinuationInput,
        provenance: ContinuationProvenance,
    ) -> GoalResult<MessageReceipt>;
    /// Canonical message state; `None` means a successful read proved absence.
    async fn message_status(&self, message: &MessageId) -> GoalResult<Option<MessageReceipt>>;
    /// Waits for this exact input's canonical discard or Turn terminal, with bounded reads.
    async fn wait_round(
        &self,
        message: &MessageId,
        cancellation: CancellationToken,
    ) -> GoalResult<RoundSettlement>;
    /// Discards only pending automatic input; claimed state is returned unchanged.
    async fn discard_if_pending(
        &self,
        lease: &ContinuationLease,
        message: &MessageId,
    ) -> GoalResult<Option<MessageReceipt>>;
    /// Cancels only the exact allocated input or its already claimed Turn.
    async fn cancel(&self, message: &MessageId) -> GoalResult<()>;
}

/// Host-owned application control and disposable live observation.
#[async_trait]
pub trait GoalController: fmt::Debug + Send + Sync + 'static {
    /// Executes/reconciles one explicit action and arms only a current create/resume result.
    async fn control(
        &self,
        session: Arc<dyn GoalSession>,
        request: GoalControl,
    ) -> GoalResult<GoalControlReceipt>;
    /// Reads a small live snapshot without creating or arming a driver.
    ///
    /// # Errors
    /// Returns `ShuttingDown` after its Host owner withdraws.
    fn status(&self, session: &SessionId) -> GoalResult<GoalLiveState>;
    /// Observes complete coalesced live snapshots; detach leaves execution owned by Host.
    ///
    /// # Errors
    /// Returns `Capacity` when observation retention is full, or `ShuttingDown`.
    fn observe(&self, session: &SessionId) -> GoalResult<GoalLiveStream>;
}

/// Host-generation controller supplied before the standard Session adapter.
#[derive(Debug)]
pub struct GoalControllerContract;
impl rsi_meta::LocalContract for GoalControllerContract {
    const KEY: &'static str = "rsi.goal.controller";
    type Service = dyn GoalController;
}
