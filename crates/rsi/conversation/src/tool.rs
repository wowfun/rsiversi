use crate::{BlockIdentity, FactField, SourceRef, ToolOutcome};
use rsi_agent_session_protocol::{SessionFact, SessionFactBody};
use serde::Serialize;

/// An issued completed-output identity, validated before retaining its bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OutputRef(String);
impl OutputRef {
    /// Rejects arbitrary JSON strings that do not satisfy the Process reader contract.
    pub fn parse(value: &str) -> Option<Self> {
        rsi_process::validate_output_read(value, 1).ok()?;
        Some(Self(value.into()))
    }
    /// Supplies the validated ID to an independently authorized output reader.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Latest known lifecycle phase; missing earlier facts do not imply execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    /// Prepared and pinned; execution has not been observed.
    Prepared,
    /// A durable started Fact was observed.
    Running,
    /// Rejected before intent/execution.
    Rejected,
    /// A complete outcome was observed.
    Settled(ToolOutcome),
}

/// Bounded per-block Tool metadata; never retains arguments, results or Fact leases.
#[derive(Clone, Debug, Serialize)]
pub struct ToolState {
    #[serde(skip)]
    key: String,
    /// Registered name if an intent or rejection is known.
    pub name: Option<String>,
    /// True only when the exact intent was observed.
    pub intent_present: bool,
    /// Exact arguments from intent or rejection.
    pub arguments: Option<SourceRef>,
    /// Complete structured result value.
    pub result: Option<SourceRef>,
    /// Pre-execution denial and provenance.
    pub rejection: Option<SourceRef>,
    /// Independently validated stdout/stderr cache references.
    pub outputs: [Option<OutputRef>; 2],
    /// Latest durable phase.
    pub phase: ToolPhase,
    #[serde(skip)]
    phase_seq: u64,
    #[serde(skip)]
    named_seq: u64,
}
impl ToolState {
    /// Starts partial metadata from any exact Tool lifecycle Fact.
    pub fn from_fact(fact: &SessionFact) -> Option<Self> {
        let key = BlockIdentity::tool(fact)?.key();
        let mut state = Self {
            key,
            name: None,
            intent_present: false,
            arguments: None,
            result: None,
            rejection: None,
            outputs: [None, None],
            phase: ToolPhase::Prepared,
            phase_seq: 0,
            named_seq: 0,
        };
        state.observe(fact);
        Some(state)
    }
    /// Returns the exact opaque block identity.
    pub fn key(&self) -> &str {
        &self.key
    }
    /// Incorporates missing history without regressing a newer phase.
    /// Returns false for another block or a non-Tool Fact.
    pub fn observe(&mut self, fact: &SessionFact) -> bool {
        if BlockIdentity::tool(fact).is_none_or(|identity| identity.key() != self.key) {
            return false;
        }
        let seq = fact.seq();
        let phase = match fact.body() {
            SessionFactBody::ToolIntent { name, .. } => {
                self.intent_present = true;
                self.named(seq, name);
                ToolPhase::Prepared
            }
            SessionFactBody::ToolRejected { name, .. } => {
                self.named(seq, name);
                self.rejection = Some(SourceRef {
                    seq,
                    field: FactField::ToolRejection,
                });
                ToolPhase::Rejected
            }
            SessionFactBody::ToolStarted { .. } => ToolPhase::Running,
            SessionFactBody::ToolResult { result, .. } => {
                if seq >= self.phase_seq {
                    self.result = Some(SourceRef {
                        seq,
                        field: FactField::ToolValue,
                    });
                    self.outputs = ["stdout", "stderr"].map(|stream| {
                        OutputRef::parse(result.value.get(stream)?.get("full_output")?.as_str()?)
                    });
                }
                ToolPhase::Settled(ToolOutcome::from_result(result))
            }
            _ => return false,
        };
        if seq >= self.phase_seq {
            self.phase = phase;
            self.phase_seq = seq;
        }
        true
    }
    fn named(&mut self, seq: u64, name: &str) {
        if seq >= self.named_seq {
            self.name = Some(name.into());
            self.arguments = Some(SourceRef {
                seq,
                field: FactField::ToolArguments,
            });
            self.named_seq = seq;
        }
    }
    /// Shared semantic title; renderers still apply their own text safety rules.
    pub fn title(&self) -> String {
        let status = match self.phase {
            ToolPhase::Prepared => "prepared",
            ToolPhase::Running => "running",
            ToolPhase::Rejected => "rejected",
            ToolPhase::Settled(ToolOutcome::Completed) => "completed",
            ToolPhase::Settled(ToolOutcome::ToolFailed) => "tool failed",
            ToolPhase::Settled(ToolOutcome::ProcessFailed) => "command failed",
        };
        format!("{} · {status}", self.name.as_deref().unwrap_or("Tool"))
    }
    /// Heap capacities retained in addition to this value's inline size.
    pub fn owned_bytes(&self) -> usize {
        self.key.capacity()
            + self.name.as_ref().map_or(0, String::capacity)
            + self
                .outputs
                .iter()
                .flatten()
                .map(|output| output.0.capacity())
                .sum::<usize>()
    }
}
