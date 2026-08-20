//! Durable, replayable agent host layered above `rsi-meta`.
//!
//! [`AgentHost`] is the sole online interface. It owns one product workspace,
//! runs independent sessions concurrently, and exposes only validated durable
//! transcripts.
//! Model and tool protocols, `SQLite` rows, recovery, and service-stream flow
//! control remain implementation details.
//!
//! This is an experimental v0 interface with no cross-release compatibility
//! promise.

#![deny(unsafe_code)]

mod adapter;
mod ai_operations;
mod artifact;
mod digest;
mod domain;
mod error;
mod host;
mod persistence;
mod runner;
mod tool_validation;
mod transcript;
mod workspace;

#[cfg(test)]
mod tests;

pub use ai_operations::{
    AgentImageOutput, AgentRealtimeEvent, AgentRealtimeSession, AgentSpeechOutput,
    AgentTranscriptionOutput,
};
pub use artifact::{ArtifactRef, ArtifactStore};
pub use domain::{
    AgentWorkspace, AiOperationId, AssistantMessage, BoundaryOutcome, CallId, ContextSnapshot,
    EventSeq, Failure, FailureKind, ModelRequestSnapshot, RunRecord, RunRequest, RunStatus,
    SessionId, StepId, ToolCall, ToolOutcome, Transcript, TranscriptEvent, TranscriptEventKind,
};
pub use error::{AgentError, Result};
pub use host::{AgentHost, ExecutionLimits, OpenOptions};
pub use rsi_agent_protocol::ToolDefinition;

/// Default model-visible system instruction captured in every new session.
pub const SYSTEM_PROMPT: &str =
    "You may use the supplied tools. After observing tool results, return a final answer.";

/// Maximum accepted user prompt size in UTF-8 bytes.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;
/// Maximum Unicode scalar values in the captured system instruction.
pub const MAX_SYSTEM_PROMPT_CHARS: usize = 4 * 1024;
/// Maximum number of model completions in one turn.
pub const MAX_STEPS: u32 = 8;
/// Maximum number of calls accepted from one assistant message.
pub const MAX_TOOL_CALLS_PER_STEP: usize = 8;
/// Maximum number of calls admitted across one turn.
pub const MAX_TOOL_CALLS_PER_TURN: usize = 16;
/// Maximum number of durable events retained by one session.
pub const MAX_TRANSCRIPT_EVENTS: usize = 256;
