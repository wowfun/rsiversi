//! Process-local submit, cancel, observation, outcome, and executor contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::Stream;
use rsi_agent_composition_protocol::{AgentCompositionPin, PreparedFreshSession};
use rsi_agent_session_protocol::{
    ActivationId, AgentControlRecord, AgentMessage, AgentPath, BudgetDimension, ForkTurnSelection,
    MessageDiscardReason, MessageId, MessageTarget, SessionFact, SessionFactBody, SessionHeader,
    SessionId, StepId, TurnId, TurnOutcome, validate_identifier, validate_safe_diagnostic,
};
use rsi_meta_contract::LocalContract;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Kernel-owned issuer for exact resume admissions.
///
/// This public type is an integration seam between the Turn protocol and its
/// Kernel implementation. Application callers obtain tokens through
/// [`TurnService::prepare_resume`] and never need an issuer.
#[doc(hidden)]
#[derive(Clone)]
pub struct ResumeAdmissionIssuer {
    seal: Arc<()>,
}

impl ResumeAdmissionIssuer {
    /// Creates one issuer identity for one Turn-service instance.
    pub fn new() -> Self {
        Self { seal: Arc::new(()) }
    }

    /// Issues one move-only admission after validating Header/pin identity.
    pub fn issue(
        &self,
        header: SessionHeader,
        composition: AgentCompositionPin,
    ) -> Result<PreparedResumeSession> {
        header
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if header.agent_preset_id() != composition.preset_id() {
            return Err(TurnError::Invariant(
                "resume Header and composition preset identities differ".into(),
            ));
        }
        Ok(PreparedResumeSession {
            inner: Box::new(PreparedResumeSessionInner {
                header,
                composition,
                issuer_seal: Arc::clone(&self.seal),
            }),
        })
    }

    /// Borrows parts only when this issuer created the token.
    pub fn inspect<'a>(
        &self,
        prepared: &'a PreparedResumeSession,
    ) -> Result<(&'a SessionHeader, &'a AgentCompositionPin)> {
        if !Arc::ptr_eq(&self.seal, &prepared.inner.issuer_seal) {
            return Err(TurnError::Invalid(
                "resume admission belongs to a different Turn service".into(),
            ));
        }
        Ok((&prepared.inner.header, &prepared.inner.composition))
    }

    /// Consumes parts only when this issuer created the token.
    pub fn consume(
        &self,
        prepared: PreparedResumeSession,
    ) -> Result<(SessionHeader, AgentCompositionPin)> {
        if !Arc::ptr_eq(&self.seal, &prepared.inner.issuer_seal) {
            return Err(TurnError::Invalid(
                "resume admission belongs to a different Turn service".into(),
            ));
        }
        let inner = *prepared.inner;
        Ok((inner.header, inner.composition))
    }
}

impl Default for ResumeAdmissionIssuer {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ResumeAdmissionIssuer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResumeAdmissionIssuer(..)")
    }
}

/// Move-only authoritative Header and Agent-generation pin for one resume.
pub struct PreparedResumeSession {
    inner: Box<PreparedResumeSessionInner>,
}

struct PreparedResumeSessionInner {
    header: SessionHeader,
    composition: AgentCompositionPin,
    issuer_seal: Arc<()>,
}

impl PreparedResumeSession {
    /// Returns the authoritative durable Header selected by preparation.
    pub const fn header(&self) -> &SessionHeader {
        &self.inner.header
    }
}

impl fmt::Debug for PreparedResumeSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedResumeSession")
            .field("header", &self.inner.header)
            .field("composition", &self.inner.composition)
            .finish_non_exhaustive()
    }
}

/// Fresh or durable session selected by one submission.
#[derive(Debug)]
pub enum SubmitSession {
    /// First turn creates this prepared immutable header atomically on flush.
    Fresh(PreparedFreshSession),
    /// Existing durable session prepared by the same Turn-service instance.
    Resume(PreparedResumeSession),
}

impl SubmitSession {
    /// Returns the selected session identity.
    pub const fn session_id(&self) -> &SessionId {
        match self {
            Self::Fresh(prepared) => prepared.header().session_id(),
            Self::Resume(prepared) => prepared.header().session_id(),
        }
    }

    /// Returns the exact immutable header selected for submission.
    pub const fn header(&self) -> &SessionHeader {
        match self {
            Self::Fresh(prepared) => prepared.header(),
            Self::Resume(prepared) => prepared.header(),
        }
    }
}

/// One idempotent user turn submission.
#[derive(Debug)]
pub struct SubmitTurn {
    /// Fresh header or existing durable identity.
    pub session: SubmitSession,
    /// Caller-preallocated durable turn identity.
    pub turn_id: TurnId,
    /// Exact user text.
    pub text: String,
    /// Optional exact model override for this turn only.
    pub model: Option<rsi_ai_protocol::ModelRef>,
    /// Optional sandbox override for this invocation only.
    pub sandbox: Option<rsi_sandbox::SandboxMode>,
}

/// One direct Image request submitted as its own durable turn.
#[derive(Debug)]
pub struct SubmitImage {
    /// Fresh header or existing durable identity.
    pub session: SubmitSession,
    /// Caller-preallocated durable turn identity.
    pub turn_id: TurnId,
    /// Exact invocation-scoped Image route.
    pub model: rsi_ai_protocol::ModelRef,
    /// Complete bounded provider-neutral request.
    pub request: rsi_ai_protocol::ImageRequest,
}

/// Durable idempotent turn-acceptance receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmittedTurn {
    /// Exact session identity.
    pub session_id: SessionId,
    /// Exact turn identity.
    pub turn_id: TurnId,
    /// Durable Fact sequence assigned to `TurnAccepted`.
    pub accepted_seq: u64,
}

/// Durable state of one admitted mailbox message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessageState {
    /// Accepted but not yet claimed or discarded.
    Pending,
    /// Entered one exact execution boundary.
    Claimed {
        /// Owning Agent activation.
        activation_id: ActivationId,
        /// Turn created or resumed by the claim.
        turn_id: TurnId,
        /// Step which received the input.
        step_id: StepId,
        /// Fact sequence containing the model-visible input.
        entered_fact_seq: u64,
    },
    /// Will never enter model context.
    Discarded {
        /// Closed durable reason.
        reason: MessageDiscardReason,
        /// Control sequence containing the discard.
        control_seq: u64,
    },
}

/// Durable receipt for one accepted mailbox message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageReceipt {
    /// Exact target session.
    pub session_id: SessionId,
    /// Caller-preallocated message identity.
    pub message_id: MessageId,
    /// Durable control sequence containing acceptance.
    pub accepted_control_seq: u64,
    /// Durable Fact tail observed when this receipt was produced.
    pub observed_fact_seq: u64,
    /// State observed when the receipt was produced.
    pub state: MessageState,
}

impl MessageReceipt {
    /// Revalidates the cross-field sequence relationships in one durable receipt.
    pub fn validate(&self) -> Result<()> {
        if self.accepted_control_seq == 0 {
            return Err(TurnError::Invariant(
                "message receipt has no durable acceptance sequence".into(),
            ));
        }
        match &self.state {
            MessageState::Claimed {
                entered_fact_seq, ..
            } if *entered_fact_seq == 0 => Err(TurnError::Invariant(
                "claimed message receipt has no entered Fact sequence".into(),
            )),
            MessageState::Claimed {
                entered_fact_seq, ..
            } if *entered_fact_seq > self.observed_fact_seq => Err(TurnError::Invariant(
                "claimed message receipt entered after its observed Fact tail".into(),
            )),
            MessageState::Discarded { control_seq, .. }
                if *control_seq <= self.accepted_control_seq =>
            {
                Err(TurnError::Invariant(
                    "discarded message receipt precedes its acceptance".into(),
                ))
            }
            MessageState::Pending
            | MessageState::Claimed { .. }
            | MessageState::Discarded { .. } => Ok(()),
        }
    }
}

/// One message admission before execution preparation and claim.
#[derive(Debug)]
pub struct SubmitMessage {
    /// Fresh header or existing durable identity.
    pub session: SubmitSession,
    /// Validated mixed-content message.
    pub message: AgentMessage,
    /// Delivery horizon.
    pub target: MessageTarget,
    /// Whether an idle activation must be made ready.
    pub wake_required: bool,
}

/// Exact durable boundary which claims one pending next-Turn message.
#[derive(Debug)]
pub struct ClaimMessage {
    /// Prepared authoritative target session.
    pub session: PreparedResumeSession,
    /// Pending message selected by its durable identity.
    pub message_id: MessageId,
    /// Caller-preallocated activation identity.
    pub activation_id: ActivationId,
    /// Durable path of this activation within its Agent tree.
    pub path: AgentPath,
    /// Caller-preallocated Turn identity.
    pub turn_id: TurnId,
    /// Caller-preallocated first Step identity.
    pub step_id: StepId,
}

/// One source-authorized durable child creation request.
#[derive(Clone, Debug)]
pub struct SpawnAgentRequest {
    /// Exact live calling Agent authority.
    pub caller: AgentCallerAuthority,
    /// Deterministic preallocated child session identity.
    pub child_session_id: SessionId,
    /// Stable sibling-unique model-facing task name.
    pub task_name: String,
    /// Deterministic initial mailbox message identity.
    pub message_id: MessageId,
    /// Self-contained initial child task.
    pub message: String,
    /// Completed parent turns inherited before the invoking Turn.
    pub fork_turns: ForkTurnSelection,
}

/// Durable receipt for one ready continuable child.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnedAgent {
    /// Exact child session identity.
    pub session_id: SessionId,
    /// Stable path assigned within the tree.
    pub path: AgentPath,
    /// Initial durable mailbox receipt.
    pub message: MessageReceipt,
}

/// One source-authorized Agent-to-Agent message.
#[derive(Clone, Debug)]
pub struct SendAgentMessage {
    /// Exact live calling Agent authority.
    pub caller: AgentCallerAuthority,
    /// Adjacent target session.
    pub target_session_id: SessionId,
    /// Deterministic preallocated mailbox identity.
    pub message_id: MessageId,
    /// Exact text delivered to the target.
    pub message: String,
    /// `true` queues a waking next Turn; `false` injects at the next Step and stays held while idle.
    pub start_new_turn: bool,
}

/// Agent roster traversal scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentListScope {
    /// Direct children only.
    Children,
    /// Stable pre-order descendant traversal.
    Descendants,
}

/// Observable durable scheduling state of one continuable child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentNodeState {
    /// A durable Turn is currently open.
    Running,
    /// No Turn is open and a waking message is pending.
    Ready,
    /// No Turn is open and no waking message is pending.
    Idle,
}

/// One bounded durable Agent roster row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentNode {
    /// Exact child session.
    pub session_id: SessionId,
    /// Exact durable direct parent.
    pub parent_session_id: SessionId,
    /// Stable tree path.
    pub path: AgentPath,
    /// Stable sibling-unique task name.
    pub task_name: String,
    /// Current durable scheduling state.
    pub state: AgentNodeState,
}

/// Result of waiting for a descendant control-state change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentWaitResult {
    /// At least one descendant control watermark advanced.
    Changed,
    /// The bounded deadline elapsed without a change.
    TimedOut,
    /// No descendant could currently produce a change.
    NoProgress,
}

/// Cursor spanning independent Agent-control and Fact streams.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObservationCursor {
    /// Last observed durable Agent-control sequence.
    pub control_seq: u64,
    /// Last observed durable Fact sequence.
    pub fact_seq: u64,
}

/// One durable reconnectable session observation.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionObservation {
    /// Agent-control record and its durable sequence.
    Control {
        /// Exact record.
        record: Arc<AgentControlRecord>,
        /// Durable control watermark.
        durable_control_seq: u64,
    },
    /// Model-visible Fact and its durable sequence.
    Fact {
        /// Exact Fact.
        fact: Arc<SessionFact>,
        /// Durable Fact watermark.
        durable_fact_seq: u64,
    },
}

/// Detachable stream of reconnectable control and Fact observations.
pub type SessionObservationStream =
    Pin<Box<dyn Stream<Item = Result<SessionObservation>> + Send + 'static>>;

/// Idempotent cancellation target before or after message claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CancelTarget {
    /// Discard one accepted, unclaimed message.
    Message(MessageId),
    /// Request cancellation of one accepted Turn.
    Turn(TurnId),
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
        fact: Arc<SessionFact>,
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
    /// Pins the authoritative resident or current-cold generation for resume.
    ///
    /// Preparation does not reserve resident capacity or materialize Facts.
    /// Dropping the returned token has no Store semantics.
    async fn prepare_resume(&self, session_id: &SessionId) -> Result<PreparedResumeSession>;
    /// Lists the exact root followed by every durable descendant in stable tree order.
    async fn tree_sessions(&self, session_id: &SessionId) -> Result<Vec<SessionId>> {
        let _ = session_id;
        Err(TurnError::Invalid(
            "this Turn service does not expose Agent tree membership".into(),
        ))
    }
    /// Creates one durable, ready, continuable fork child.
    async fn spawn_agent(&self, request: SpawnAgentRequest) -> Result<SpawnedAgent> {
        let _ = request;
        Err(TurnError::Invalid(
            "this Turn service does not support subagents".into(),
        ))
    }
    /// Sends one durable message across an authorized adjacent Agent edge.
    async fn send_agent_message(&self, request: SendAgentMessage) -> Result<MessageReceipt> {
        let _ = request;
        Err(TurnError::Invalid(
            "this Turn service does not support Agent messaging".into(),
        ))
    }
    /// Lists direct children or all descendants below one live caller.
    async fn list_agents(
        &self,
        caller: &AgentCallerAuthority,
        scope: AgentListScope,
    ) -> Result<Vec<AgentNode>> {
        let _ = (caller, scope);
        Err(TurnError::Invalid(
            "this Turn service does not support Agent listing".into(),
        ))
    }
    /// Waits for a descendant state change after the call begins.
    async fn wait_agent(
        &self,
        caller: &AgentCallerAuthority,
        timeout: std::time::Duration,
        cancellation: CancellationToken,
    ) -> Result<AgentWaitResult> {
        let _ = (caller, timeout, cancellation);
        Err(TurnError::Invalid(
            "this Turn service does not support Agent waiting".into(),
        ))
    }
    /// Requests cancellation of one exact descendant's current Turn only.
    async fn interrupt_agent(
        &self,
        caller: &AgentCallerAuthority,
        target_session_id: &SessionId,
    ) -> Result<CancelResult> {
        let _ = (caller, target_session_id);
        Err(TurnError::Invalid(
            "this Turn service does not support Agent interruption".into(),
        ))
    }
    /// Durably accepts one mailbox message without promising a Turn identity.
    async fn submit_message(&self, request: SubmitMessage) -> Result<MessageReceipt> {
        let _ = request;
        Err(TurnError::Invalid(
            "this Turn service does not support durable messages".into(),
        ))
    }
    /// Reads the latest durable state for one message.
    async fn message_status(
        &self,
        session_id: &SessionId,
        message_id: &MessageId,
    ) -> Result<MessageReceipt> {
        let _ = (session_id, message_id);
        Err(TurnError::Invalid(
            "this Turn service does not support durable messages".into(),
        ))
    }
    /// Atomically makes one pending next-Turn message model-visible.
    async fn claim_message(&self, request: ClaimMessage) -> Result<SubmittedTurn> {
        let _ = request;
        Err(TurnError::Invalid(
            "this Turn service does not support durable message claims".into(),
        ))
    }
    /// Observes durable control and Fact streams after independent cursors.
    async fn observe_session(
        &self,
        session_id: &SessionId,
        cursor: ObservationCursor,
    ) -> Result<SessionObservationStream> {
        let _ = (session_id, cursor);
        Err(TurnError::Invalid(
            "this Turn service does not support session observations".into(),
        ))
    }
    /// Cancels either an unclaimed message or an accepted Turn.
    async fn cancel_target(
        &self,
        session_id: &SessionId,
        target: CancelTarget,
        reason: Option<String>,
    ) -> Result<CancelResult> {
        match target {
            CancelTarget::Turn(turn_id) => self.cancel(session_id, &turn_id, reason).await,
            CancelTarget::Message(_) => Err(TurnError::Invalid(
                "this Turn service cannot cancel unclaimed messages".into(),
            )),
        }
    }
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
    executor_id: String,
    claim_id: u64,
    session_id: SessionId,
    turn_id: TurnId,
    header: Arc<SessionHeader>,
    accepted_at_ms: u64,
    accepted_seq: u64,
    live_seq: u64,
    authority: TurnClaimAuthority,
}

impl TurnClaim {
    /// Returns the registered executor identity.
    pub fn executor_id(&self) -> &str {
        &self.executor_id
    }

    /// Returns the Kernel-issued process-local claim generation.
    pub const fn claim_id(&self) -> u64 {
        self.claim_id
    }

    /// Returns the exact session identity.
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the exact turn identity.
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Borrows the immutable session header without exposing its shared owner.
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// Returns the durable acceptance timestamp used by elapsed budgets.
    pub const fn accepted_at_ms(&self) -> u64 {
        self.accepted_at_ms
    }

    /// Returns the exact Fact sequence of the acceptance boundary.
    pub const fn accepted_seq(&self) -> u64 {
        self.accepted_seq
    }

    /// Returns the highest live Fact sequence captured by this claim.
    pub const fn live_seq(&self) -> u64 {
        self.live_seq
    }

    fn agent_caller(&self) -> AgentCallerAuthority {
        AgentCallerAuthority {
            claim: self.clone(),
        }
    }
}

#[derive(Clone)]
struct TurnClaimAuthority {
    issuer_seal: Arc<()>,
}

impl fmt::Debug for TurnClaimAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TurnClaimAuthority(..)")
    }
}

impl PartialEq for TurnClaimAuthority {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.issuer_seal, &other.issuer_seal)
    }
}

impl Eq for TurnClaimAuthority {}

/// Kernel-only issuer for sealed process-local claims.
#[doc(hidden)]
#[derive(Clone)]
pub struct TurnClaimIssuer {
    seal: Arc<()>,
}

impl TurnClaimIssuer {
    /// Creates one issuer identity for one Turn service instance.
    pub fn new() -> Self {
        Self { seal: Arc::new(()) }
    }

    /// Issues one immutable claim sharing the resident session Header owner.
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        &self,
        executor_id: String,
        claim_id: u64,
        session_id: SessionId,
        turn_id: TurnId,
        header: Arc<SessionHeader>,
        accepted_at_ms: u64,
        accepted_seq: u64,
        live_seq: u64,
    ) -> TurnClaim {
        TurnClaim {
            executor_id,
            claim_id,
            session_id,
            turn_id,
            header,
            accepted_at_ms,
            accepted_seq,
            live_seq,
            authority: TurnClaimAuthority {
                issuer_seal: Arc::clone(&self.seal),
            },
        }
    }

    /// Revalidates the private issuer identity.
    pub fn validates(&self, claim: &TurnClaim) -> bool {
        Arc::ptr_eq(&self.seal, &claim.authority.issuer_seal)
    }

    /// Revalidates issuer identity and shared resident Header ownership.
    pub fn validates_header(&self, claim: &TurnClaim, header: &Arc<SessionHeader>) -> bool {
        self.validates(claim) && Arc::ptr_eq(&claim.header, header)
    }

    /// Derives a caller authority only from this issuer's exact live claim.
    pub fn agent_caller(&self, claim: &TurnClaim) -> Result<AgentCallerAuthority> {
        if !self.validates(claim) {
            return Err(TurnError::StaleClaim);
        }
        Ok(claim.agent_caller())
    }
}

impl Default for TurnClaimIssuer {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TurnClaimIssuer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TurnClaimIssuer(..)")
    }
}

/// One claim-filtered page plus the exact live sequence scanned through.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimFactPage {
    /// Facts visible to the claimed turn in increasing sequence order.
    pub facts: Vec<Arc<SessionFact>>,
    /// Highest live sequence examined, including Facts hidden by claim isolation.
    pub through_seq: u64,
}

/// One bounded inherited parent-Fact page for a forked claim.
#[derive(Clone, Debug, PartialEq)]
pub struct ForkFactPage {
    /// Parent Facts after the supplied parent cursor.
    pub facts: Vec<Arc<SessionFact>>,
    /// Highest parent sequence examined through the immutable fork boundary.
    pub through_parent_seq: u64,
    /// Exact immutable terminal sequence of the inherited interval.
    pub terminal_parent_seq: u64,
}

/// Unforgeable caller identity injected into trusted Agent control Tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentCallerAuthority {
    claim: TurnClaim,
}

impl AgentCallerAuthority {
    /// Returns the exact calling Agent session.
    pub const fn session_id(&self) -> &SessionId {
        self.claim.session_id()
    }

    /// Returns the exact calling Turn.
    pub const fn turn_id(&self) -> &TurnId {
        self.claim.turn_id()
    }

    /// Returns the immutable calling session Header.
    pub fn header(&self) -> &SessionHeader {
        self.claim.header()
    }

    /// Borrows the sealed live Turn claim for its issuing service only.
    #[doc(hidden)]
    pub const fn claim(&self) -> &TurnClaim {
        &self.claim
    }
}

/// Explicit result of attempting to publish one owned body batch.
#[derive(Debug, PartialEq)]
pub enum PublishAttempt {
    /// The complete batch entered the live interval.
    Published(Vec<Arc<SessionFact>>),
    /// Capacity requires a durable flush; no body entered the live interval.
    FlushRequired {
        /// Canonical bodies recovered from the unpublished candidate Facts.
        unpublished: Vec<SessionFactBody>,
    },
}

/// Opaque Context-owned checkpoint crossing only the process-local Kernel seam.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextCheckpoint {
    /// Immutable Session header fingerprint.
    pub header_fingerprint: String,
    /// Exact durable sequence folded into the checkpoint.
    pub through_seq: u64,
    /// SHA-256 chain of the exact canonical Fact prefix folded by Context.
    pub fact_prefix_sha256: String,
    /// Versioned Context-owned bytes.
    pub bytes: Arc<[u8]>,
}

/// Executor-facing Kernel port.
#[async_trait]
pub trait TurnExecution: fmt::Debug + Send + Sync + 'static {
    /// Registers executor availability until the returned lease drops.
    fn register(&self, executor_id: String) -> Result<ExecutorLease>;
    /// Waits for and claims one oldest available nonterminal turn.
    ///
    /// `Ok(None)` is the orderly registration-close result. Any error is
    /// terminal for this exact executor registration, so callers stop every
    /// sibling claim lane rather than retrying an unspecified failure.
    async fn claim(
        &self,
        executor_id: &str,
        cancellation: CancellationToken,
    ) -> Result<Option<TurnClaim>>;
    /// Returns the exact immutable Agent composition pinned by this claim.
    fn composition(&self, claim: &TurnClaim) -> Result<AgentCompositionPin>;
    /// Derives the unforgeable Agent Tool caller from one live exact claim.
    fn agent_caller(&self, claim: &TurnClaim) -> Result<AgentCallerAuthority> {
        let _ = claim;
        Err(TurnError::Invalid(
            "this Turn executor does not expose Agent Tool caller authority".into(),
        ))
    }
    /// Reads one bounded immutable inherited parent-history page.
    async fn read_fork_facts(
        &self,
        claim: &TurnClaim,
        after_parent_seq: u64,
        limit: usize,
    ) -> Result<Option<ForkFactPage>>;
    /// Atomically enters every pending next-Step message at one safe model boundary.
    async fn enter_pending_step_messages(&self, claim: &TurnClaim) -> Result<usize>;
    /// Refreshes complete trust-bound workspace context before provider I/O.
    async fn refresh_workspace_context(&self, claim: &TurnClaim) -> Result<usize>;
    /// Closes the current Agent Step before its Turn terminal boundary.
    async fn close_current_step(&self, claim: &TurnClaim, outcome: &TurnOutcome) -> Result<()>;
    /// Atomically closes an activation-owned Turn and advances tree settlement.
    ///
    /// `None` means the claimed Turn is not owned by a mailbox activation and
    /// should use ordinary terminal publication.
    async fn finish_activation_turn(
        &self,
        claim: &TurnClaim,
        outcome: &TurnOutcome,
    ) -> Result<Option<Arc<SessionFact>>>;
    /// Reads bounded live Facts after a cursor, including a speculative suffix.
    async fn read_facts(
        &self,
        claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> Result<ClaimFactPage>;
    /// Reads an unfiltered durable session page for checkpoint maintenance
    /// only after the claimed turn is terminal and no speculative suffix exists.
    /// Returns `Ok(None)` when checkpoint maintenance is unavailable or stale.
    async fn read_checkpoint_facts(
        &self,
        claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> Result<Option<ClaimFactPage>> {
        let _ = (claim, after_seq, limit);
        Ok(None)
    }
    /// Reads inherited parent history for terminal-turn checkpoint maintenance.
    /// The implementation must authorize the immutable child Header and reject
    /// any session with a speculative local suffix.
    async fn read_checkpoint_fork_facts(
        &self,
        claim: &TurnClaim,
        after_parent_seq: u64,
        limit: usize,
    ) -> Result<Option<ForkFactPage>> {
        let _ = (claim, after_parent_seq, limit);
        Ok(None)
    }
    /// Reads one optional Context checkpoint cache.
    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ContextCheckpoint>> {
        let _ = session_id;
        Ok(None)
    }
    /// Installs a checkpoint only at an unchanged durable and live tail.
    ///
    /// `Ok(false)` means checkpoint maintenance is disabled, the tail changed,
    /// or the claim is not durably terminal;
    /// Store failures remain typed errors so callers can choose an explicit
    /// fail-soft cache policy without losing diagnostics at this seam.
    async fn write_context_checkpoint(
        &self,
        claim: &TurnClaim,
        checkpoint: ContextCheckpoint,
    ) -> Result<bool> {
        let _ = (claim, checkpoint);
        Ok(false)
    }
    /// Publishes validated bodies as the next live Facts without claiming durability.
    async fn publish(
        &self,
        claim: &TurnClaim,
        bodies: Vec<SessionFactBody>,
    ) -> Result<PublishAttempt>;
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
#[derive(Clone, Debug)]
pub struct TurnFinalizationContext {
    /// Exact session identity.
    pub session_id: SessionId,
    /// Exact turn identity.
    pub turn_id: TurnId,
    /// Exact optional Jobs authority shared with Tool execution.
    pub job_scope: Option<rsi_jobs::JobScopeAuthority>,
}

/// Bounded reason why otherwise completed work must not publish success.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnCompletionBlocker {
    code: String,
    message: String,
}

impl TurnCompletionBlocker {
    /// Creates one validated stable completion blocker.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> FinalizationResult<Self> {
        let blocker = Self {
            code: code.into(),
            message: message.into(),
        };
        validate_identifier("turn completion blocker", &blocker.code)
            .map_err(|error| TurnFinalizationError::Invalid(error.to_string()))?;
        validate_safe_diagnostic("turn completion blocker message", &blocker.message)
            .map_err(|error| TurnFinalizationError::Invalid(error.to_string()))?;
        Ok(blocker)
    }

    /// Returns the stable blocker code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the bounded safe blocker summary.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Successful result from one hook or a complete finalizer snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TurnFinalizationReport {
    completion_blocker: Option<TurnCompletionBlocker>,
}

impl TurnFinalizationReport {
    /// Returns an unblocked finalization report.
    pub const fn complete() -> Self {
        Self {
            completion_blocker: None,
        }
    }

    /// Returns a report containing one validated completion blocker.
    pub const fn blocked(blocker: TurnCompletionBlocker) -> Self {
        Self {
            completion_blocker: Some(blocker),
        }
    }

    /// Returns the optional blocker selected by registration order.
    pub const fn completion_blocker(&self) -> Option<&TurnCompletionBlocker> {
        self.completion_blocker.as_ref()
    }
}

/// One effect-owned pre-terminal hook.
#[async_trait]
pub trait TurnFinalizer: fmt::Debug + Send + Sync + 'static {
    /// Settles invocation-scoped resources before the sole terminal Fact is published.
    async fn finalize(
        &self,
        context: &TurnFinalizationContext,
    ) -> FinalizationResult<TurnFinalizationReport>;
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

    /// Starts an immutable snapshot concurrently and resolves errors and blockers by registration order.
    ///
    /// The caller owns the deadline for the complete snapshot.
    async fn finalize(
        &self,
        context: &TurnFinalizationContext,
    ) -> FinalizationResult<TurnFinalizationReport>;
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
    /// A preallocated turn identity already names a different canonical submission.
    #[error("Agent turn `{turn}` in session `{session}` conflicts with an existing submission")]
    SubmissionConflict {
        /// Session identity.
        session: String,
        /// Turn identity.
        turn: String,
    },
    /// A preallocated message identity already names different canonical input.
    #[error("Agent message `{message}` in session `{session}` conflicts with accepted input")]
    MessageConflict {
        /// Session identity.
        session: String,
        /// Message identity.
        message: String,
    },
    /// The session already has its bounded number of live turns.
    #[error("Agent session live-turn capacity is exhausted")]
    Capacity,
    /// Process-wide live-observer admission is exhausted.
    #[error("Agent active-observer capacity is exhausted")]
    ObserverCapacity,
    /// The session's durable Agent preset cannot produce a healthy generation.
    #[error("Agent composition is unavailable: {0}")]
    Composition(String),
    /// One frozen turn budget prevented further work admission.
    #[error("Agent turn budget exceeded for {dimension:?}: consumed {consumed}, limit {limit}")]
    BudgetExceeded {
        /// Exhausted dimension.
        dimension: BudgetDimension,
        /// Proposed or elapsed consumption at rejection.
        consumed: u64,
        /// Immutable turn limit.
        limit: u64,
    },
    /// The caller cancelled one interruptible Agent operation.
    #[error("Agent turn operation was cancelled")]
    Cancelled,
    /// Exact executor or claim lease is stale.
    #[error("Agent executor claim is stale")]
    StaleClaim,
    /// A requested durable flush or shutdown failed.
    #[error("Agent durable flush failed: {0}")]
    Flush(String),
    /// Store access failed outside a requested durability barrier.
    #[error("Agent Store access failed: {0}")]
    Store(String),
    /// Kernel is shutting down and accepts no new work.
    #[error("Agent Kernel is shutting down")]
    ShuttingDown,
    /// Kernel detected corrupt or contradictory state.
    #[error("Agent Kernel invariant failed: {0}")]
    Invariant(String),
}

/// Turn runtime result.
pub type Result<T> = std::result::Result<T, TurnError>;

#[cfg(test)]
mod tests {
    use super::{MessageReceipt, MessageState, TurnCompletionBlocker};
    use rsi_agent_session_protocol::{ActivationId, MessageId, SessionId, StepId, TurnId};

    #[test]
    fn completion_blocker_rejects_unsafe_diagnostic_characters() {
        for message in ["contains\0nul", "contains\u{7f}delete"] {
            assert!(
                TurnCompletionBlocker::new("jobs_active", message).is_err(),
                "accepted unsafe blocker message {message:?}"
            );
        }
    }

    #[test]
    fn claimed_message_receipt_must_observe_its_entered_fact() {
        let receipt = MessageReceipt {
            session_id: SessionId::new("session-receipt").unwrap(),
            message_id: MessageId::new("message-receipt").unwrap(),
            accepted_control_seq: 1,
            observed_fact_seq: 4,
            state: MessageState::Claimed {
                activation_id: ActivationId::new("activation-receipt").unwrap(),
                turn_id: TurnId::new("turn-receipt").unwrap(),
                step_id: StepId::new("step-receipt").unwrap(),
                entered_fact_seq: 5,
            },
        };

        assert!(receipt.validate().is_err());
    }
}
