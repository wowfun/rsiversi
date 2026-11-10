//! Native prompt preparation stays inside the admitted Agent generation.

use crate::ContributionResult;
use async_trait::async_trait;
use rsi_agent_session_protocol::{ModelSelection, SessionHeader, StepInstruction};
use rsi_meta::LocalContract;
use rsi_meta_scope::{ScopeKey, ScopeRoot};
use std::fmt;
use tokio_util::sync::CancellationToken;

/// Immutable values borrowed from one admitted request series.
#[derive(Debug)]
pub struct PromptInput<'a> {
    /// Immutable Session defaults and delegation persona.
    pub header: &'a SessionHeader,
    /// Actual selected route and reasoning effort.
    pub model: &'a ModelSelection,
    /// Explicit exact scope, absent for a global-only assembly.
    pub scope: Option<&'a ScopeKey>,
    /// Ordered guidance from the existing native `BeforeStep` contributors.
    pub instructions: &'a [StepInstruction],
    /// Already admitted names; the Tool owner retains schemas and execution authority.
    pub tools: &'a [String],
    /// Names before `BeforeStep` restrictions, used to validate configured ordering.
    pub known_tools: &'a [String],
}

/// Resolved output; only the existing Step and input publication persists it.
#[derive(Debug)]
pub struct PreparedPrompt {
    /// Complete system sections, including any selected Header/persona defaults.
    pub sections: Vec<StepInstruction>,
    /// Rendered snapshot; the executor claims only a changed retained value.
    pub runtime_context: String,
    /// Ordered admitted names, independent of execution-subset authority.
    pub tool_order: Vec<String>,
}

/// Native preparation service frozen with the selected Agent generation.
#[async_trait]
pub trait PromptAssembler: fmt::Debug + Send + Sync + 'static {
    /// Evaluates once per request series, outside framework locks.
    async fn assemble(
        &self,
        input: PromptInput<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<PreparedPrompt>;
}

/// Optional generation-local native prompt implementation.
#[derive(Debug)]
pub struct PromptAssemblerContract;
impl LocalContract for PromptAssemblerContract {
    const KEY: &'static str = "rsi.agent.prompt-assembler";
    type Service = dyn PromptAssembler;
}

/// Scope identity supplied to prompt providers by the generation's registrar owner.
#[derive(Clone, Debug)]
pub struct AgentPromptScope {
    /// Existing composition Scope root; no second Runtime or ownership graph.
    pub root: ScopeRoot,
    /// Exact hidden Agent generation scope.
    pub key: ScopeKey,
}

/// Narrow authoring input for a generation's scoped prompt registry.
#[derive(Debug)]
pub struct AgentPromptScopeContract;
impl LocalContract for AgentPromptScopeContract {
    const KEY: &'static str = "rsi.agent.prompt-scope";
    type Service = AgentPromptScope;
}
