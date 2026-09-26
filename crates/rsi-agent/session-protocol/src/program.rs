//! Durable program ownership and bounded initial-child completion receipts.
use crate::{
    ActivationId, AgentResultLocator, EffectId, ForkTurnSelection, ModelSelection, ProgramRunId,
    Result, SessionError, SessionId, TurnId, validate_safe_diagnostic, validate_sha256,
};
use rsi_sandbox::SandboxMode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum in-flight foreground nested calls and native Program RPC requests.
/// Context replay enforces this same durable active-effect bound.
pub const MAXIMUM_PROGRAM_OUTSTANDING_CALLS: usize = 16;
/// Maximum UTF-8 script bytes.
pub const MAXIMUM_PROGRAM_SCRIPT_BYTES: usize = 64 * 1024;
/// Maximum encoded curated result bytes.
pub const MAXIMUM_PROGRAM_RESULT_BYTES: usize = 256 * 1024;
/// Maximum continuation domains admitted by Program preparation.
pub const MAXIMUM_PROGRAM_CONTINUATION_DOMAINS: usize = 64;
const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Maximum initial child admissions for one program run.
pub const MAXIMUM_PROGRAM_CHILDREN: u32 = 128;
/// Total canonical run-record allowance, independent of each child's Turn budget.
pub const MAXIMUM_PROGRAM_RECORD_BYTES: u64 = 8 * 1024 * 1024;
/// Close-path space unavailable to ordinary progress and child admission.
pub const PROGRAM_TERMINAL_RESERVE_BYTES: u64 = 4 * 1024 * 1024;
/// One bounded progress payload.
pub const MAXIMUM_PROGRAM_PROGRESS_BYTES: usize = 16 * 1024;

/// Execution ownership is independent of inherited history and tree presentation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)]
pub enum ExecutionOwner {
    /// Normal activation-owned child, with its exact creator Turn.
    TurnActivation {
        session_id: SessionId,
        turn_id: TurnId,
    },
    /// Initial activation belongs to a detached-capable program generation.
    ProgramRun {
        session_id: SessionId,
        run_id: ProgramRunId,
        ordinal: u32,
    },
}
impl ExecutionOwner {
    /// Rejects invalid durable program ordinals.
    pub fn validate(&self) -> Result<()> {
        if let Self::ProgramRun { ordinal, .. } = self
            && (*ordinal == 0 || *ordinal > MAXIMUM_PROGRAM_CHILDREN)
        {
            return Err(invalid("program execution owner ordinal is out of bounds"));
        }
        Ok(())
    }
}
/// Fixed completed-parent interval reused by every initial child in one run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramForkBoundary {
    pub requested_turns: ForkTurnSelection,
    pub resolved_after_seq: u64,
    pub resolved_terminal_seq: u64,
    pub terminal_prefix_sha256: String,
    pub resolved_terminal_control_seq: u64,
    pub terminal_control_prefix_sha256: String,
    pub effective_turns: u64,
}
impl ProgramForkBoundary {
    /// Revalidates the same finite paired horizon used by ordinary forks.
    pub fn validate(&self) -> Result<()> {
        self.requested_turns.validate()?;
        validate_sha256("program fork Fact prefix", &self.terminal_prefix_sha256)?;
        validate_sha256(
            "program fork control prefix",
            &self.terminal_control_prefix_sha256,
        )?;
        let empty = self.effective_turns == 0;
        if empty != (self.resolved_after_seq == 0 && self.resolved_terminal_seq == 0)
            || empty != (self.resolved_terminal_control_seq == 0)
            || (!empty && self.resolved_after_seq >= self.resolved_terminal_seq)
            || (matches!(self.requested_turns, ForkTurnSelection::None) && !empty)
            || (empty
                && (self.terminal_prefix_sha256 != ZERO_SHA256
                    || self.terminal_control_prefix_sha256 != ZERO_SHA256))
        {
            return Err(invalid("program fork horizon is inconsistent"));
        }
        Ok(())
    }
}
/// Immutable CAS binding for a script or final curated JSON result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramBlob {
    pub sha256: String,
    pub bytes: u64,
}
impl ProgramBlob {
    /// Validates the digest and caller-selected content limit.
    pub fn validate(&self, maximum: u64) -> Result<()> {
        validate_sha256("program blob", &self.sha256)?;
        if self.bytes == 0 || self.bytes > maximum {
            return Err(invalid("program blob exceeds its content bound"));
        }
        Ok(())
    }
}
/// Frozen durable authorization facts. This value alone is never live authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramRunDescriptor {
    pub run_id: ProgramRunId,
    pub session_id: SessionId,
    pub creator_turn_id: TurnId,
    pub creator_effect_id: EffectId,
    pub parent_header_sha256: String,
    pub fork: ProgramForkBoundary,
    pub selection: ModelSelection,
    pub sandbox: SandboxMode,
    pub require_approval: bool,
    pub script: ProgramBlob,
    pub guard: Option<ProgramDomainGuard>,
}
impl ProgramRunDescriptor {
    /// Rejects invalid frozen permissions, horizons and script bounds.
    pub fn validate(&self) -> Result<()> {
        validate_sha256("program parent Header", &self.parent_header_sha256)?;
        self.fork.validate()?;
        self.selection.validate()?;
        if let Some(guard) = &self.guard {
            guard.validate()?;
        }
        self.script.validate(MAXIMUM_PROGRAM_SCRIPT_BYTES as u64)?;
        if self.sandbox == SandboxMode::DangerFullAccess && !self.require_approval {
            return Err(invalid("program danger-full-access requires approval"));
        }
        Ok(())
    }
    /// Stable child identity, independent of the retired creator Tool's call ID.
    pub fn child_session_id(&self, ordinal: u32) -> Result<SessionId> {
        if ordinal == 0 || ordinal > MAXIMUM_PROGRAM_CHILDREN {
            return Err(invalid("program child ordinal is out of bounds"));
        }
        let encoded =
            serde_json::to_vec(&(self.session_id.as_str(), self.run_id.as_str(), ordinal))
                .map_err(|error| invalid(&error.to_string()))?;
        let mut hash = Sha256::new();
        hash.update(b"rsi-program-child-v1\0");
        hash.update(encoded);
        SessionId::new(format!("program-child-{}", hex::encode(hash.finalize())))
    }
}
/// Digest-bound output receipt, excluding the display preview's repeated bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramResultBinding {
    pub locator: AgentResultLocator,
    pub schema_sha256: String,
    pub value_sha256: String,
}
impl ProgramResultBinding {
    /// Verifies exact output coordinates and both authoritative digests.
    pub fn validate(&self) -> Result<()> {
        if self.locator.fact_seq == 0 {
            return Err(invalid("program result has no exact Fact"));
        }
        validate_sha256("program result schema", &self.schema_sha256)?;
        validate_sha256("program result value", &self.value_sha256)
    }
}
/// Terminal child receipt written only by exact initial-activation settlement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramChildReceipt {
    pub ordinal: u32,
    pub child_session_id: SessionId,
    pub activation_id: Option<ActivationId>,
    pub outcome: ProgramOutcome,
    pub result: Option<ProgramResultBinding>,
}
/// Closed run/child outcome. Interruptions never authorize replay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)]
pub enum ProgramOutcome {
    Completed,
    Cancelled,
    Interrupted,
    Failed { code: String, message: String },
}
impl ProgramOutcome {
    /// Revalidates bounded terminal diagnostics.
    pub fn validate(&self) -> Result<()> {
        if let Self::Failed { code, message } = self {
            crate::validate_identifier("program failure", code)?;
            validate_safe_diagnostic("program failure", message)?;
        }
        Ok(())
    }
}
impl ProgramChildReceipt {
    /// Validates receipt identity independently of the owning run's admission ledger.
    pub fn validate(&self) -> Result<()> {
        if self.ordinal == 0 || self.ordinal > MAXIMUM_PROGRAM_CHILDREN {
            return Err(invalid("program receipt ordinal is out of bounds"));
        }
        self.outcome.validate()?;
        if let Some(result) = &self.result {
            result.validate()?;
            if self.outcome != ProgramOutcome::Completed
                || result.locator.child_session_id != self.child_session_id
                || self.activation_id.as_ref() != Some(&result.locator.activation_id)
            {
                return Err(invalid("program completion and result identities differ"));
            }
        }
        Ok(())
    }
}
fn invalid(message: &str) -> SessionError {
    SessionError::Invalid(message.into())
}

/// One append-only run event. External execution and replay decisions remain Kernel-owned.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
#[allow(missing_docs)]
pub enum ProgramRunEvent {
    Accepted {
        descriptor: Box<ProgramRunDescriptor>,
    },
    Started,
    Detached,
    CancellationRequested,
    ChildAdmitted {
        ordinal: u32,
        child_session_id: SessionId,
        message_id: crate::MessageId,
    },
    ChildStarted {
        ordinal: u32,
        activation_id: ActivationId,
    },
    ChildSettled {
        receipt: ProgramChildReceipt,
    },
    Progress {
        phase: Option<String>,
        message: String,
    },
    Terminal {
        outcome: ProgramOutcome,
        result: Option<ProgramBlob>,
    },
}
impl ProgramRunEvent {
    /// Revalidates event-local bounds without interpreting the run state machine.
    pub fn validate(&self, run_id: &ProgramRunId) -> Result<()> {
        match self {
            Self::Accepted { descriptor } => {
                descriptor.validate()?;
                if &descriptor.run_id != run_id {
                    return Err(invalid("program acceptance identity mismatch"));
                }
            }
            Self::ChildAdmitted { ordinal, .. } | Self::ChildStarted { ordinal, .. } => {
                if *ordinal == 0 || *ordinal > MAXIMUM_PROGRAM_CHILDREN {
                    return Err(invalid("program child ordinal is out of bounds"));
                }
            }
            Self::ChildSettled { receipt } => receipt.validate()?,
            Self::Progress { phase, message } => {
                if message.len() > MAXIMUM_PROGRAM_PROGRESS_BYTES || message.contains('\0') {
                    return Err(invalid("program progress exceeds its bound"));
                }
                if let Some(phase) = phase {
                    crate::validate_safe_text("program phase", phase, 256, false)?;
                }
            }
            Self::Terminal { outcome, result } => {
                outcome.validate()?;
                if let Some(result) = result {
                    result.validate(MAXIMUM_PROGRAM_RESULT_BYTES as u64)?;
                    if *outcome != ProgramOutcome::Completed {
                        return Err(invalid("only a completed program can retain a result"));
                    }
                }
            }
            Self::Started | Self::Detached | Self::CancellationRequested => {}
        }
        Ok(())
    }
    /// Closure records can consume the reserved tail of the record allowance.
    pub const fn is_closure(&self) -> bool {
        matches!(
            self,
            Self::ChildSettled { .. } | Self::CancellationRequested | Self::Terminal { .. }
        )
    }
}

/// Exact optional policy snapshot whose mutation revokes live run admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramDomainGuard {
    pub domain: crate::DomainIdentity,
    pub revision: crate::DomainRevision,
    pub snapshot_sha256: String,
}
impl ProgramDomainGuard {
    /// Requires one exact durable revision and a valid snapshot binding.
    pub fn validate(&self) -> Result<()> {
        if self.revision.get() == 0 {
            return Err(invalid("program guard requires a durable domain revision"));
        }
        validate_sha256("program domain guard", &self.snapshot_sha256)
    }
}
/// One bounded notification coordinate, claimable only in its live Kernel generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)]
pub struct ProgramCompletionSource {
    pub run_id: ProgramRunId,
    pub generation: String,
    pub terminal_control_seq: u64,
}
impl ProgramCompletionSource {
    /// Checks coordinates without granting scheduling authority.
    pub fn validate(&self) -> Result<()> {
        validate_sha256("program notice generation", &self.generation)?;
        if self.terminal_control_seq == 0 {
            return Err(invalid("program notice has no terminal control"));
        }
        Ok(())
    }
}
