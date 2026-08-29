//! Process-local submit, cancel, observation, outcome, and executor contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::Stream;
use rsi_agent_session_protocol::{
    SessionFact, SessionFactBody, SessionHeader, SessionId, TurnId, TurnOutcome,
};
use rsi_meta_contract::LocalContract;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Fresh or durable session selected by one submission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubmitSession {
    /// First turn creates this immutable header atomically on flush.
    Fresh(SessionHeader),
    /// Existing durable session whose creation-time header is authoritative.
    Resume(SessionId),
}

impl SubmitSession {
    /// Returns the selected session identity.
    pub const fn session_id(&self) -> &SessionId {
        match self {
            Self::Fresh(header) => header.session_id(),
            Self::Resume(session_id) => session_id,
        }
    }
}

/// One user turn submission without a durability receipt promise.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmitTurn {
    /// Fresh header or existing durable identity.
    pub session: SubmitSession,
    /// Exact user text.
    pub text: String,
    /// Optional exact model override for this turn only.
    pub model: Option<rsi_ai_protocol::ModelRef>,
    /// Optional sandbox override for this invocation only.
    pub sandbox: Option<rsi_sandbox::SandboxMode>,
}

/// One direct Image request submitted as its own durable turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmitImage {
    /// Fresh header or existing durable identity.
    pub session: SubmitSession,
    /// Exact invocation-scoped Image route.
    pub model: rsi_ai_protocol::ModelRef,
    /// Complete bounded provider-neutral request.
    pub request: rsi_ai_protocol::ImageRequest,
}

/// Accepted live turn identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmittedTurn {
    /// Exact session identity.
    pub session_id: SessionId,
    /// Exact turn identity.
    pub turn_id: TurnId,
    /// Live Fact sequence assigned to `TurnAccepted`.
    pub accepted_seq: u64,
}

/// Idempotent cancellation result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelResult {
    /// Whether a new cancellation Fact entered the live stream.
    pub accepted: bool,
    /// Whether the target was already terminal at call time.
    pub already_terminal: bool,
}

/// One live observation update.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnUpdate {
    /// A nonterminal Fact entered the live interval, or a terminal Fact became durable.
    /// `durable_seq` may lag only a nonterminal Fact.
    Fact {
        /// Exact Fact.
        fact: Box<SessionFact>,
        /// Durable watermark at publication time.
        durable_seq: u64,
    },
    /// Store durability advanced without introducing a new live Fact.
    Durable {
        /// Exact contiguous durable watermark.
        durable_seq: u64,
    },
}

/// Detachable stream of live updates. Dropping it does not cancel the turn.
pub type TurnObservation = Pin<Box<dyn Stream<Item = Result<TurnUpdate>> + Send + 'static>>;

/// Application-facing process-local Turn service.
#[async_trait]
pub trait TurnService: fmt::Debug + Send + Sync + 'static {
    /// Accepts one turn into a fresh or existing session's live interval.
    async fn submit(&self, request: SubmitTurn) -> Result<SubmittedTurn>;
    /// Accepts one direct Image operation into a fresh or existing session.
    async fn submit_image(&self, request: SubmitImage) -> Result<SubmittedTurn>;
    /// Idempotently requests cancellation of one exact accepted turn.
    async fn cancel(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        reason: Option<String>,
    ) -> Result<CancelResult>;
    /// Observes Facts and watermarks strictly after one live sequence.
    async fn observe(&self, session_id: &SessionId, after_seq: u64) -> Result<TurnObservation>;
    /// Returns a terminal outcome only after its complete prefix is durable.
    async fn outcome(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> Result<Option<TurnOutcome>>;
    /// Reads the immutable durable or current-process lazy header.
    async fn session_header(&self, session_id: &SessionId) -> Result<SessionHeader>;
}

/// Nominal application-facing Turn service contract.
#[derive(Debug)]
pub struct TurnServiceContract;

impl LocalContract for TurnServiceContract {
    const KEY: &'static str = "rsi.agent.turns";
    type Service = dyn TurnService;
}

/// Exact executor claim over one oldest nonterminal turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnClaim {
    /// Registered executor identity.
    pub executor_id: String,
    /// Kernel-issued process-local claim generation.
    pub claim_id: u64,
    /// Exact session identity.
    pub session_id: SessionId,
    /// Exact turn identity.
    pub turn_id: TurnId,
    /// Immutable session header.
    pub header: SessionHeader,
    /// Highest Fact sequence in the live interval when claimed.
    pub live_seq: u64,
}

/// One claim-filtered page plus the exact live sequence scanned through.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimFactPage {
    /// Facts visible to the claimed turn in increasing sequence order.
    pub facts: Vec<SessionFact>,
    /// Highest live sequence examined, including Facts hidden by claim isolation.
    pub through_seq: u64,
}

/// Executor-facing Kernel port.
#[async_trait]
pub trait TurnExecution: fmt::Debug + Send + Sync + 'static {
    /// Registers executor availability until the returned lease drops.
    fn register(&self, executor_id: String) -> Result<ExecutorLease>;
    /// Waits for and claims one oldest available nonterminal turn.
    async fn claim(
        &self,
        executor_id: &str,
        cancellation: CancellationToken,
    ) -> Result<Option<TurnClaim>>;
    /// Reads bounded live Facts after a cursor, including a speculative suffix.
    async fn read_facts(
        &self,
        claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> Result<ClaimFactPage>;
    /// Publishes validated bodies as the next live Facts without claiming durability.
    async fn publish(
        &self,
        claim: &TurnClaim,
        bodies: Vec<SessionFactBody>,
    ) -> Result<Vec<SessionFact>>;
    /// Waits until the exact live prefix is durable or returns its flush failure.
    async fn flush(&self, claim: &TurnClaim, through_seq: u64) -> Result<u64>;
    /// Returns a cancellation token that fires after a durable cancel request.
    fn cancellation(&self, claim: &TurnClaim) -> Result<CancellationToken>;
    /// Releases one exact nonterminal claim for another registered executor.
    fn release(&self, claim: &TurnClaim) -> Result<()>;
}

/// Nominal executor-facing Kernel contract.
#[derive(Debug)]
pub struct TurnExecutionContract;

impl LocalContract for TurnExecutionContract {
    const KEY: &'static str = "rsi.agent.turn_execution";
    type Service = dyn TurnExecution;
}

/// One effect-owned pre-terminal hook.
#[async_trait]
pub trait TurnFinalizer: fmt::Debug + Send + Sync + 'static {
    /// Settles invocation-scoped resources before the sole terminal Fact is published.
    async fn finalize(&self, session_id: &SessionId, turn_id: &TurnId) -> FinalizationResult<()>;
}

/// Ordered process-local finalizer registry invoked by the Agent executor.
#[async_trait]
pub trait TurnFinalization: fmt::Debug + Send + Sync + 'static {
    /// Registers one exact finalizer name until the returned lease drops.
    fn register(
        &self,
        name: String,
        finalizer: Arc<dyn TurnFinalizer>,
    ) -> FinalizationResult<TurnFinalizerLease>;

    /// Runs an immutable snapshot in registration order until one finalizer fails.
    ///
    /// The caller owns the deadline for the complete snapshot.
    async fn finalize(&self, session_id: &SessionId, turn_id: &TurnId) -> FinalizationResult<()>;
}

/// Nominal Local contract for [`TurnFinalization`].
#[derive(Debug)]
pub struct TurnFinalizationContract;

impl LocalContract for TurnFinalizationContract {
    const KEY: &'static str = "rsi.agent.turn_finalization";
    type Service = dyn TurnFinalization;
}

/// Effect-owned exact finalizer registration.
pub struct TurnFinalizerLease {
    cleanup: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

impl TurnFinalizerLease {
    /// Creates a lease from one exact deregistration action.
    pub fn new(cleanup: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl fmt::Debug for TurnFinalizerLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TurnFinalizerLease(..)")
    }
}

impl Drop for TurnFinalizerLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

/// Closed pre-terminal finalization failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TurnFinalizationError {
    /// Registration input or identity is invalid.
    #[error("invalid turn finalizer: {0}")]
    Invalid(String),
    /// One registered finalizer could not settle its owned resources.
    #[error("turn finalization failed ({code}): {message}")]
    Failed {
        /// Stable bounded failure category.
        code: String,
        /// Safe bounded summary.
        message: String,
    },
}

/// Pre-terminal finalization result.
pub type FinalizationResult<T> = std::result::Result<T, TurnFinalizationError>;

/// Effect-owned exact executor registration.
pub struct ExecutorLease {
    cleanup: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

impl ExecutorLease {
    /// Creates a lease from one deregistration action.
    pub fn new(cleanup: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl fmt::Debug for ExecutorLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExecutorLease(..)")
    }
}

impl Drop for ExecutorLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

/// Closed Turn runtime failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TurnError {
    /// Malformed, oversized, or state-incompatible request.
    #[error("invalid Agent turn operation: {0}")]
    Invalid(String),
    /// Session is not durable or process-locally reserved.
    #[error("Agent session `{0}` was not found")]
    SessionNotFound(String),
    /// Turn does not belong to the selected session.
    #[error("Agent turn `{turn}` was not found in session `{session}`")]
    TurnNotFound {
        /// Session identity.
        session: String,
        /// Turn identity.
        turn: String,
    },
    /// The session already has its bounded number of live turns.
    #[error("Agent session live-turn capacity is exhausted")]
    Capacity,
    /// Exact executor or claim lease is stale.
    #[error("Agent executor claim is stale")]
    StaleClaim,
    /// Store flush failed and execution is paused.
    #[error("Agent durable flush failed: {0}")]
    Flush(String),
    /// Kernel is shutting down and accepts no new work.
    #[error("Agent Kernel is shutting down")]
    ShuttingDown,
    /// Kernel detected corrupt or contradictory state.
    #[error("Agent Kernel invariant failed: {0}")]
    Invariant(String),
}

/// Turn runtime result.
pub type Result<T> = std::result::Result<T, TurnError>;
