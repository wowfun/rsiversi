//! Exact-claim foreground program dispatch, independent of any script engine.
use async_trait::async_trait;
use rsi_tools_protocol::{ToolDefinition, ToolResult};
use std::{fmt, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Executor-owned dispatch implementation for one still-started coordinator.
#[async_trait]
pub trait ProgramToolDispatcher: fmt::Debug + Send + Sync + 'static {
    /// Exact eligible definitions in the frozen claim catalog.
    fn definitions(&self) -> Vec<ToolDefinition>;
    /// Admits one internal call; dropping a waiter does not erase admitted work.
    async fn call(
        &self,
        name: String,
        arguments: serde_json::Value,
        cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<ToolResult>;
}
/// Typed Local extension injected by the executor, never from model arguments.
#[derive(Clone, Debug)]
pub struct ProgramToolCalls(pub Arc<dyn ProgramToolDispatcher>);

/// One detached-capable workflow request, still authenticated by its creator Tool.
#[derive(Clone, Debug)]
pub struct PrepareProgram {
    /// Exact currently started model-origin workflow Tool.
    pub caller: crate::AgentCallerAuthority,
    /// Exact creator Tool cancellation, detached atomically with its observation lifetime.
    pub cancellation: CancellationToken,
    /// Complete frozen script bytes, at most 64 KiB.
    pub script: String,
    /// Completed parent history selected once for every initial child.
    pub fork_turns: rsi_agent_session_protocol::ForkTurnSelection,
    /// Trusted adapter-selected domain revision; a later mutation revokes the run.
    pub guard: Option<rsi_agent_session_protocol::ProgramDomainGuard>,
    /// Finite automatic-round domains selected by the trusted program contribution.
    pub continuation_domains: Vec<rsi_agent_session_protocol::DomainIdentity>,
}
/// One initial child task; model and permissions come from the run's frozen authority.
#[derive(Clone, Debug)]
pub struct ProgramAgentRequest {
    /// Self-contained task text.
    pub message: String,
    /// Optional validated result schema for this initial activation.
    pub output_contract: Option<rsi_agent_session_protocol::OutputContract>,
    /// Trusted, frozen delegation restriction; cannot widen parent permissions.
    pub role: Option<rsi_agent_session_protocol::DelegationRole>,
}
/// Complete initial-child settlement returned to program code.
#[derive(Clone, Debug)]
pub struct ProgramAgentResult {
    /// Exact exclusive completion receipt.
    pub receipt: rsi_agent_session_protocol::ProgramChildReceipt,
    /// Full verified structured value and metadata, when requested.
    pub structured: Option<crate::AgentResult>,
    /// Bounded public final reply when no output schema was requested.
    pub reply: Option<String>,
}
/// One bounded, revision-bound observation; full JSON is for Local consumers.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProgramSnapshot {
    /// Exact stable run identity.
    pub run_id: rsi_agent_session_protocol::ProgramRunId,
    /// Latest canonical run control, used to reject mixed-revision pages.
    pub control_seq: u64,
    /// Whether the process start was accepted.
    pub started: bool,
    /// Whether creator observation was detached.
    pub detached: bool,
    /// Whether cancellation was durably requested.
    pub cancelling: bool,
    /// Number of admitted initial children (at most 128).
    pub children: usize,
    /// Number of exact initial receipts.
    pub settled_children: usize,
    /// Last named phase, at most 256 UTF-8 bytes.
    pub phase: Option<String>,
    /// Last progress data, at most 16 KiB.
    pub progress: Option<String>,
    /// Terminal outcome, absent while live.
    pub outcome: Option<rsi_agent_session_protocol::ProgramOutcome>,
    /// Content binding for the complete curated result.
    pub result_ref: Option<rsi_agent_session_protocol::ProgramBlob>,
    /// Complete verified curated result, at most 256 KiB encoded.
    pub result: Option<serde_json::Value>,
}
/// Opaque live run owner. Only its issuing Kernel implementation can admit work.
#[async_trait]
pub trait ProgramRun: fmt::Debug + Send + Sync + 'static {
    /// Frozen, non-authorizing durable metadata.
    fn descriptor(&self) -> &rsi_agent_session_protocol::ProgramRunDescriptor;
    /// Cancellation for the independently owned process and its admitted RPCs.
    fn cancellation(&self) -> CancellationToken;
    /// Accepts the run after Jobs admission, rechecking exact creator authority.
    async fn accept(&self, caller: &crate::AgentCallerAuthority) -> crate::Result<()>;
    /// Records start after acceptance and before external process launch.
    async fn start(&self) -> crate::Result<()>;
    /// Atomically detaches unless cancellation already won.
    async fn detach(&self) -> crate::Result<()>;
    /// Cancels only while still owned by the creator; false means detachment won.
    async fn cancel_from_creator(&self) -> crate::Result<bool>;
    /// Revokes this run and cancels every owned child, including unclaimed input.
    async fn cancel(&self) -> crate::Result<()>;
    /// Durably admits one stable initial child and waits for its exclusive receipt.
    /// Run cancellation drains admitted source mutations, then releases this waiter
    /// with `TurnError::Cancelled`; the run owner still settles actual child terminals.
    async fn agent(&self, request: ProgramAgentRequest) -> crate::Result<ProgramAgentResult>;
    /// Records bounded progress, charged to the independent run budget.
    async fn progress(&self, phase: Option<String>, message: String) -> crate::Result<()>;
    /// Joins child settlement and returns the authoritative durable terminal outcome.
    /// An existing terminal wins over the proposed outcome; effects are never replayed.
    async fn finish(
        &self,
        outcome: rsi_agent_session_protocol::ProgramOutcome,
        value: Option<serde_json::Value>,
    ) -> crate::Result<rsi_agent_session_protocol::ProgramOutcome>;
}
