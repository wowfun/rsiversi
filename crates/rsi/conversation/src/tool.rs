use crate::{BlockIdentity, FactField, SourceRef, ToolOutcome};
use rsi_agent_session_protocol::{SessionFact, SessionFactBody, ToolOrigin};
use serde::Serialize;

/// An issued completed-output identity, validated before retaining its bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OutputRef(rsi_spill::SpillLocator);
impl OutputRef {
    /// Rejects arbitrary JSON strings that do not satisfy the Spill reader contract.
    pub fn parse(value: &str) -> Option<Self> {
        rsi_spill::SpillLocator::parse(value.to_owned())
            .ok()
            .map(Self)
    }
    /// Supplies the validated ID to an independently authorized output reader.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
    /// Borrows the admitted locator without reparsing it.
    pub const fn locator(&self) -> &rsi_spill::SpillLocator {
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
    /// Recorded producer; absent when the intent or rejection has not been loaded.
    pub origin: Option<ToolOrigin>,
    /// Bounded semantic preview of known Tool arguments, without owning their source.
    pub argument_summary: Option<String>,
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
    /// Generic typed text references carried by the settled Fact.
    pub spills: Vec<OutputRef>,
    /// Latest durable phase.
    pub phase: ToolPhase,
    #[serde(skip)]
    phase_seq: u64,
    #[serde(skip)]
    named_seq: u64,
    #[serde(skip)]
    external_candidate: Option<String>,
    /// Exact detached run locator; it is not execution authority.
    #[serde(default)]
    pub workflow_run: Option<rsi_agent_session_protocol::ProgramRunId>,
    #[serde(skip)]
    workflow_candidate: Option<rsi_agent_session_protocol::ProgramRunId>,
    #[serde(skip)]
    schedule_candidate: Option<Box<ScheduleCreation>>,
}

/// Compact creation receipt displayed independently of the Tool disclosure.
#[derive(Clone, Debug, Serialize)]
pub struct ScheduleCreation {
    /// ID returned by the creating call.
    pub id: String,
    /// First nonempty instruction line, bounded for the transcript.
    pub title: String,
    /// Interval at creation. A one-shot has no interval.
    pub interval_ms: Option<u64>,
}
impl ScheduleCreation {
    fn from_result(value: &serde_json::Value) -> Option<Box<Self>> {
        if !matches!(
            value.get("status")?.as_str()?,
            "created_armed" | "created_disarmed"
        ) {
            return None;
        }
        let id = value.get("request_id")?.as_str()?;
        rsi_agent_session_protocol::validate_identifier("reminder", id).ok()?;
        let rows = value.get("state")?.get("reminders")?.as_array()?;
        let row = rows
            .iter()
            .find(|row| row.get("id").and_then(serde_json::Value::as_str) == Some(id))?;
        let line = row
            .get("prompt")?
            .as_str()?
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        let interval_ms = match row.get("interval_ms")? {
            serde_json::Value::Null => None,
            interval => Some(interval.as_u64().filter(|ms| *ms > 0)?),
        };
        Some(Box::new(Self {
            id: id.into(),
            title: line.chars().take(128).collect(),
            interval_ms,
        }))
    }
}
impl ToolState {
    /// Starts partial metadata from any exact Tool lifecycle Fact.
    pub fn from_fact(fact: &SessionFact) -> Option<Self> {
        let key = BlockIdentity::tool(fact)?.key();
        let mut state = Self {
            key,
            name: None,
            origin: None,
            argument_summary: None,
            intent_present: false,
            arguments: None,
            result: None,
            rejection: None,
            outputs: [None, None],
            spills: Vec::new(),
            phase: ToolPhase::Prepared,
            phase_seq: 0,
            named_seq: 0,
            external_candidate: None,
            workflow_run: None,
            workflow_candidate: None,
            schedule_candidate: None,
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
            SessionFactBody::ToolIntent {
                name,
                arguments,
                origin,
                ..
            } => {
                self.intent_present = true;
                self.named(seq, name, arguments, origin);
                ToolPhase::Prepared
            }
            SessionFactBody::ToolRejected {
                name,
                arguments,
                origin,
                ..
            } => {
                self.named(seq, name, arguments, origin);
                self.rejection = Some(SourceRef {
                    seq,
                    field: FactField::ToolRejection,
                });
                ToolPhase::Rejected
            }
            SessionFactBody::ToolStarted { .. } => ToolPhase::Running,
            SessionFactBody::ToolResult { result, .. } => {
                if seq >= self.phase_seq {
                    self.external_candidate = (!result.is_error)
                        .then(|| {
                            let value = result.value.get("conversation")?.as_str()?;
                            rsi_acp_protocol::observation::ConversationId::new(value)
                                .ok()
                                .map(|id| id.as_str().to_owned())
                        })
                        .flatten();
                    self.workflow_candidate = (!result.is_error
                        && self.name.as_deref().is_none_or(|name| {
                            name == rsi_agent_session_protocol::RUN_WORKFLOW_TOOL_NAME
                        }))
                    .then(|| {
                        rsi_agent_session_protocol::WorkflowInvocationResult::detached_run(
                            &result.value,
                        )
                        .ok()
                        .flatten()
                    })
                    .flatten();
                    self.result = Some(SourceRef {
                        seq,
                        field: FactField::ToolValue,
                    });
                    self.schedule_candidate = if !result.is_error
                        && self
                            .name
                            .as_deref()
                            .is_none_or(|name| name == "schedule_create")
                    {
                        ScheduleCreation::from_result(&result.value)
                    } else {
                        None
                    };
                    self.spills = result
                        .spills
                        .iter()
                        .map(|reference| OutputRef(reference.locator.clone()))
                        .collect();
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
        self.workflow_run = (self.intent_present
            && self.name.as_deref() == Some(rsi_agent_session_protocol::RUN_WORKFLOW_TOOL_NAME))
        .then(|| self.workflow_candidate.clone())
        .flatten();
        true
    }
    fn named(&mut self, seq: u64, name: &str, arguments: &serde_json::Value, origin: &ToolOrigin) {
        if seq >= self.named_seq {
            self.name = Some(name.into());
            if name != rsi_agent_session_protocol::RUN_WORKFLOW_TOOL_NAME {
                self.workflow_candidate = None;
            }
            if name != "schedule_create" {
                self.schedule_candidate = None;
            }
            self.origin = Some(origin.clone());
            self.argument_summary = argument_summary(name, arguments);
            self.arguments = Some(SourceRef {
                seq,
                field: FactField::ToolArguments,
            });
            self.named_seq = seq;
        }
    }
    /// Navigation hint from a successful result paired with its exact delegation intent.
    /// Opening the hint still requires authorization by the external conversation owner.
    pub fn external_conversation(&self) -> Option<&str> {
        (self.name.as_deref() == Some(rsi_acp_protocol::EXTERNAL_AGENT_TOOL_NAME)
            && self.intent_present)
            .then_some(self.external_candidate.as_deref())
            .flatten()
    }
    /// A creation card requires a successful result and its matching creation intent.
    pub fn schedule_creation(&self) -> Option<&ScheduleCreation> {
        (self.intent_present && self.name.as_deref() == Some("schedule_create"))
            .then_some(self.schedule_candidate.as_deref())
            .flatten()
    }
    /// Shared semantic title; renderers still apply their own text safety rules.
    pub fn title(&self) -> String {
        let status = match self.phase {
            ToolPhase::Prepared => "prepared",
            ToolPhase::Running => "running",
            ToolPhase::Rejected => "rejected",
            ToolPhase::Settled(ToolOutcome::Completed) if self.workflow_run.is_some() => {
                "invocation returned · detached"
            }
            ToolPhase::Settled(ToolOutcome::Completed) => "completed",
            ToolPhase::Settled(ToolOutcome::ToolFailed) => "tool failed",
            ToolPhase::Settled(ToolOutcome::ProcessFailed) => "command failed",
        };
        let prefix = if matches!(self.origin, Some(ToolOrigin::Program { .. })) {
            "Program · "
        } else {
            ""
        };
        let title = format!(
            "{prefix}{} · {status}",
            self.name.as_deref().unwrap_or("Tool")
        );
        self.argument_summary
            .as_ref()
            .map_or(title.clone(), |summary| format!("{title} · {summary}"))
    }
    /// Heap capacities retained in addition to this value's inline size.
    pub fn owned_bytes(&self) -> usize {
        self.key.capacity()
            + self.schedule_candidate.as_deref().map_or(0, |task| {
                std::mem::size_of::<ScheduleCreation>() + task.id.capacity() + task.title.capacity()
            })
            + self.origin.as_ref().map_or(0, |origin| match origin {
                ToolOrigin::Model { effect_id } => effect_id.as_str().len(),
                ToolOrigin::Program {
                    parent_effect_id, ..
                } => parent_effect_id.as_str().len(),
            })
            + self.workflow_run.as_ref().map_or(0, |id| id.as_str().len())
            + self
                .workflow_candidate
                .as_ref()
                .map_or(0, |id| id.as_str().len())
            + self.external_candidate.as_ref().map_or(0, String::capacity)
            + self.name.as_ref().map_or(0, String::capacity)
            + self.argument_summary.as_ref().map_or(0, String::capacity)
            + self.spills.capacity() * std::mem::size_of::<OutputRef>()
            + self
                .outputs
                .iter()
                .flatten()
                .chain(self.spills.iter())
                .map(|output| output.as_str().len())
                .sum::<usize>()
    }
}

fn argument_summary(name: &str, arguments: &serde_json::Value) -> Option<String> {
    let string = |key| arguments.get(key).and_then(serde_json::Value::as_str);
    let text = match name {
        "bash" | "pwsh" => string("command")?,
        "file_read" | "directory_list" => {
            string("path").or_else(|| string("path_hex")).unwrap_or(".")
        }
        "spawn_agent" => string("task_name")?,
        "send_message" | "followup_task" | "interrupt_agent" => string("target")?,
        "todo_write" => {
            return arguments
                .get("todos")
                .and_then(serde_json::Value::as_array)
                .map(|items| format!("{} tasks", items.len()));
        }
        _ => return None,
    };
    let mut summary = String::new();
    for character in text.chars() {
        let character = if character.is_whitespace() {
            ' '
        } else {
            character
        };
        if summary.len() + character.len_utf8() > 508 {
            summary.push('…');
            break;
        }
        summary.push(character);
    }
    Some(summary)
}
