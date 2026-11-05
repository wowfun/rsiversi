//! Captured execution inputs and narrow, effect-free contribution callbacks.

use crate::ValidatedDomainProposal;
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    ContributionId, DomainStateView, SessionFact, SessionHeader, StepId, TurnId,
};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::ToolResultIdentity;
use std::{fmt, sync::Arc};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

mod batch;
mod catalog;
mod fork;
mod publication;
pub use batch::{ContributionBatch, ContributionInput};
pub use catalog::{
    ContributionCatalog, ContributionKind, ContributionRegistrar, ContributionRegistrarContract,
    ContributionRegistration, ContributionStage,
};
pub use fork::{ForkTransform, ForkTransformContext};
pub use publication::{
    DraftPublicationReceipt, Publication, PublicationObserver, PublicationReceipt,
};

/// Maximum callbacks in one immutable Agent composition.
pub const MAXIMUM_AGENT_CONTRIBUTIONS: usize = 64;
/// Maximum entered inputs returned by a complete contribution stage.
pub const MAXIMUM_CONTRIBUTION_INPUTS: usize = 512;
/// Maximum encoded input bytes staged before one business commit.
pub const MAXIMUM_CONTRIBUTION_INPUT_BYTES: usize = 16 * 1024 * 1024;

/// Failures at the contribution admission or execution seam.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContributionError {
    /// Invalid returned data or callback failure.
    #[error("invalid contribution: {0}")]
    Invalid(String),
    /// Two registrations claim one contribution identity.
    #[error("duplicate contribution: {0:?}")]
    Duplicate(ContributionId),
    /// A bounded contribution set or output is full.
    #[error("contribution capacity exceeded")]
    Capacity,
    /// The unpublished registration stage has closed.
    #[error("contribution stage is closed")]
    Closed,
    /// The registering generation is unavailable or from another Runtime.
    #[error("contribution registration ownership is unavailable")]
    RegistrationUnavailable,
}

/// Result across the process-local contribution seam.
pub type ContributionResult<T> = Result<T, ContributionError>;

/// One immutable pair of durable watermarks captured before callbacks run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContributionHorizon {
    /// Inclusive canonical Fact watermark.
    pub fact_seq: u64,
    /// Inclusive canonical control watermark.
    pub control_seq: u64,
}

/// Bounded claim-visible page; the cursor may pass Facts invisible to this claim.
#[derive(Clone, Debug)]
pub struct ContributionFactPage {
    /// Ordered Facts at or below the captured horizon.
    pub facts: Vec<Arc<SessionFact>>,
    /// Inclusive scanned cursor, including omitted claim-invisible Facts.
    pub through_seq: u64,
}

/// Read-only access to one claim's captured history. No Store write authority.
#[async_trait]
pub trait ContributionFactReader: fmt::Debug + Send + Sync {
    /// Reads a bounded page after a cursor, never crossing the captured horizon.
    async fn read(&self, after_seq: u64, limit: usize) -> ContributionResult<ContributionFactPage>;
}

/// Consistent immutable inputs shared by every callback in one stage.
#[derive(Clone, Debug)]
pub struct ContributionContext {
    /// Frozen execution Header.
    pub header: Arc<SessionHeader>,
    /// Exact non-durable execution authority captured from the live claim.
    pub execution: Option<rsi_execution::ExecutionLease>,
    /// Exact executing Turn.
    pub turn_id: TurnId,
    /// Exact acceptance Fact sequence; later queued acceptances may precede this Step.
    pub accepted_fact_seq: u64,
    /// Current open Step.
    pub step_id: StepId,
    /// Watermarks captured with these domain states.
    pub horizon: ContributionHorizon,
    /// Complete bounded current domain set.
    pub domains: Arc<[DomainStateView]>,
    /// Bounded claim-scoped historical reader.
    pub facts: Arc<dyn ContributionFactReader>,
}

/// Proposed inputs and typed domain replacements; the framework commits the whole stage.
#[derive(Debug, Default)]
pub struct ContributionOutput {
    /// Actual model-visible text to validate and persist.
    pub inputs: Vec<ContributionInput>,
    /// Complete replacements carrying exact generation handles and CAS revisions.
    pub domains: Vec<ValidatedDomainProposal>,
}

/// Context contribution before a new provider retry series.
#[async_trait]
pub trait ContextContributor: fmt::Debug + Send + Sync + 'static {
    /// Samples inputs once; retries replay the committed inputs.
    async fn contribute(
        &self,
        context: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput>;
}

/// Captured `BeforeStep` input plus exact publication identity and frozen catalog.
#[derive(Debug)]
pub struct BeforeStepContext<'a> {
    /// Resolved provider, model and effort for this request series.
    pub selection: &'a rsi_agent_session_protocol::ModelSelection,
    /// Exact registered producer assigned to its durable context inputs.
    pub producer: &'a ContributionId,
    /// Read-only claim-scoped snapshot.
    pub captured: &'a ContributionContext,
    /// Result of preceding preset input editors, over the same captured claim set.
    pub previous: Option<&'a rsi_agent_session_protocol::StepInputDecision>,
    /// Framework identity allocated before any process callback begins.
    pub request_id: &'a rsi_agent_session_protocol::DomainRequestId,
    /// Complete admitted catalog names; a producer can only narrow this set.
    pub tools: &'a [String],
}
/// One staged contribution to the complete atomic `BeforeStep` publication.
#[derive(Debug, Default)]
pub struct BeforeStepOutput {
    /// Ordered edit of the captured inputs. None preserves the preceding decision.
    pub decision: Option<rsi_agent_session_protocol::StepInputDecision>,
    /// Current system guidance, replaced by the next preparation and cleared when the Step closes.
    pub instructions: String,
    /// Durable context inputs and typed domain replacements.
    pub contribution: ContributionOutput,
    /// Retained external resources awaiting the exact canonical receipt.
    pub publications: Vec<Publication>,
    /// Optional subset of the captured names; an empty set disables all Tools.
    pub tools: Option<Vec<String>>,
}
/// Staged preparation without ownership of Step acceptance or effect execution.
#[async_trait]
pub trait BeforeStepContributor: fmt::Debug + Send + Sync + 'static {
    /// Runs once per retry series; only canonical publication admits its output.
    async fn prepare(
        &self,
        context: &BeforeStepContext<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<BeforeStepOutput>;
}

/// Contribution after a source-ordered Tool batch has durably settled.
#[async_trait]
pub trait PostToolContributor: fmt::Debug + Send + Sync + 'static {
    /// Proposes the next model-visible inputs from actual settled Facts.
    async fn contribute(
        &self,
        context: &ContributionContext,
        settled: &[Arc<SessionFact>],
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput>;
}

/// Read-only inputs for one prepublication Tool result settlement.
#[derive(Debug)]
pub struct ToolSettlementContext<'a> {
    /// Immutable Session settings.
    pub header: &'a SessionHeader,
    /// Exact durable intent of the started Tool.
    pub intent: &'a SessionFact,
    /// Its already settled retained result; no callback can rerun the Tool.
    pub result: &'a rsi_tools_protocol::ToolResult,
    /// Complete bounded current domain set; revisions fence the atomic commit.
    pub domains: &'a [DomainStateView],
}

/// Pure typed state proposals committed atomically with one `ToolResult`.
#[derive(Debug, Default)]
pub struct ToolSettlement {
    /// Retained resources and post-publication observers; no callback runs during proposal capture.
    pub publications: Vec<Publication>,
    /// Requires an uncancelled Turn when Kernel admits the atomic mutation.
    /// Ordinary effect settlement may otherwise publish after cancellation.
    pub require_uncancelled_turn: bool,
    /// Atomic typed domain replacements.
    pub domains: Vec<ValidatedDomainProposal>,
    /// Optional request to conclude after committing this exact result.
    pub conclusion: Option<rsi_agent_session_protocol::ToolConclusion>,
}

/// Pure callback over one retained result and exact current domains.
pub trait ToolSettlementContributor: fmt::Debug + Send + Sync + 'static {
    /// Has no external effects and returns only validated domain replacements.
    ///
    /// # Errors
    /// Returns an error when the matching Tool result or current domain state
    /// cannot produce a valid replacement.
    fn settle(&self, context: &ToolSettlementContext<'_>) -> ContributionResult<ToolSettlement>;
}

/// Exact prepared Tool call and already-resolved constraints.
#[derive(Debug)]
pub struct ToolPolicyRequest<'a> {
    /// Identity from the pinned Tool runtime's successful preparation.
    pub identity: &'a ToolResultIdentity,
    /// Prepared model-visible Tool name.
    pub name: &'a str,
    /// Exact arguments subject to the decision.
    pub arguments: &'a serde_json::Value,
    /// Already resolved sandbox requirement.
    pub sandbox: SandboxMode,
    /// Whether earlier policy already requires approval.
    pub require_approval: bool,
}

/// Monotone policy result; no callback can relax existing constraints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolPolicyDecision {
    /// Preserve current constraints.
    Abstain,
    /// Add an approval requirement.
    RequireApproval,
    /// Reject the prepared call before any Tool intent or start.
    Deny {
        /// Persisted bounded, nonempty diagnostic.
        reason: String,
    },
}

/// Read-only Tool policy evaluated before approval and execution.
#[async_trait]
pub trait ToolPolicy: fmt::Debug + Send + Sync + 'static {
    /// Adds constraints or denies; it cannot submit mutations or execute the Tool.
    async fn decide(
        &self,
        context: &ContributionContext,
        request: &ToolPolicyRequest<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<ToolPolicyDecision>;
}

/// Read-only history at one owner-validated durable Fact horizon.
#[derive(Clone, Debug)]
pub struct CapturedFactHistory {
    /// Highest visible durable Fact sequence.
    pub through_seq: u64,
    /// Cancellation-bound reader, never a Store writer.
    pub facts: Arc<dyn ContributionFactReader>,
}
