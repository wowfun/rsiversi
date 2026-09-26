//! Mechanical cross-Session references, independent of Program execution policy.
use crate::{Result, StoreAgentMessage, StoreAgentMessageState, StoreError};
use rsi_agent_session_protocol::{
    ActivationId, ActivationOutcome, AgentControlRecord, AgentControlRecordBody as Body,
    AgentMessageSource, ExecutionOwner, MessageDelivery, MessageId, MessageTarget, ProgramOutcome,
    ProgramResultBinding, ProgramRunEvent as Event, ProgramRunId, SessionHeader, SessionId,
};

/// Exact canonical counterpart selected by a graph proof.
#[derive(Clone, Copy, Debug)]
#[allow(missing_docs)]
pub enum ProgramGraphQuery<'a> {
    Admission(&'a ProgramRunId, u32),
    Start(&'a ProgramRunId, u32),
    Terminal(&'a ProgramRunId),
    Sink(&'a ActivationId),
    Settlement(&'a ActivationId),
}
impl ProgramGraphQuery<'_> {
    /// Tests a decoded candidate without trusting an index's claimed kind.
    pub fn matches(self, body: &Body) -> bool {
        match (self, body) {
            (
                Self::Admission(run, ordinal),
                Body::ProgramRun {
                    run_id,
                    event: Event::ChildAdmitted { ordinal: found, .. },
                },
            )
            | (
                Self::Start(run, ordinal),
                Body::ProgramRun {
                    run_id,
                    event: Event::ChildStarted { ordinal: found, .. },
                },
            ) => run == run_id && ordinal == *found,
            (
                Self::Terminal(run),
                Body::ProgramRun {
                    run_id,
                    event: Event::Terminal { .. },
                },
            ) => run == run_id,
            (Self::Sink(id), Body::ProgramCompletionReserved { activation_id, .. })
            | (Self::Settlement(id), Body::ActivationSettled { activation_id, .. }) => {
                id == activation_id
            }
            _ => false,
        }
    }
}
/// Reads bounded canonical counterparts in the caller's atomic Store snapshot.
pub trait ProgramGraphRead {
    /// Reads an immutable validated Header.
    fn header(&self, session: &SessionId) -> Result<SessionHeader>;
    /// Requires exactly one canonical counterpart, rejecting missing or duplicate rows.
    fn control(
        &self,
        session: &SessionId,
        query: ProgramGraphQuery<'_>,
    ) -> Result<AgentControlRecord>;
    /// Reads the indexed message whose projection is also verified by its Store.
    fn message(&self, session: &SessionId, message: &MessageId) -> Result<StoreAgentMessage>;
}
fn invalid() -> StoreError {
    StoreError::Invalid("Program graph differs from its canonical counterpart".into())
}
/// Whether a control adds a mechanical reference requiring a graph proof.
pub fn needs_program_graph(body: &Body) -> bool {
    matches!(
        body,
        Body::ProgramRun {
            event: Event::ChildAdmitted { .. }
                | Event::ChildStarted { .. }
                | Event::ChildSettled { .. },
            ..
        } | Body::ProgramCompletionReserved { .. }
            | Body::MessageAccepted {
                message: rsi_agent_session_protocol::AgentMessage {
                    source: AgentMessageSource::Program { .. },
                    ..
                },
                ..
            }
    )
}
fn child(
    graph: &impl ProgramGraphRead,
    parent: &SessionId,
    run: &ProgramRunId,
    ordinal: u32,
) -> Result<(SessionId, StoreAgentMessage)> {
    let admission = graph.control(parent, ProgramGraphQuery::Admission(run, ordinal))?;
    let Body::ProgramRun {
        event:
            Event::ChildAdmitted {
                child_session_id,
                message_id,
                ..
            },
        ..
    } = admission.body()
    else {
        return Err(invalid());
    };
    let header = graph.header(child_session_id)?;
    if header.execution_owner()
        != Some(&ExecutionOwner::ProgramRun {
            session_id: parent.clone(),
            run_id: run.clone(),
            ordinal,
        })
    {
        return Err(invalid());
    }
    if header
        .initial_output()
        .is_some_and(|output| output.message_id != *message_id)
    {
        return Err(invalid());
    }
    let message = graph.message(child_session_id, message_id)?;
    if message.delivery != MessageDelivery::NextTurn
        || message.target != MessageTarget::NextTurn
        || !message.wake_required
    {
        return Err(invalid());
    }
    if !matches!(&message.message.source, AgentMessageSource::Agent { source_session_id } if source_session_id == parent)
    {
        return Err(invalid());
    }
    Ok((child_session_id.clone(), message))
}
/// Proves an immutable Program-owned child Header has its parent admission and input.
pub fn validate_program_header(
    graph: &impl ProgramGraphRead,
    header: &SessionHeader,
) -> Result<()> {
    if let Some(ExecutionOwner::ProgramRun {
        session_id,
        run_id,
        ordinal,
    }) = header.execution_owner()
        && child(graph, session_id, run_id, *ordinal)?.0 != *header.session_id()
    {
        return Err(invalid());
    }
    Ok(())
}
/// Proves the graph after all members of an atomic append are installed.
pub fn validate_program_graph(
    graph: &impl ProgramGraphRead,
    session: &SessionId,
    record: &AgentControlRecord,
) -> Result<()> {
    match record.body() {
        Body::MessageAccepted { message, .. } => {
            if let AgentMessageSource::Program { source } = &message.source {
                let terminal =
                    graph.control(session, ProgramGraphQuery::Terminal(&source.run_id))?;
                if terminal.seq() != source.terminal_control_seq || terminal.seq() >= record.seq() {
                    return Err(invalid());
                }
            }
        }
        Body::ProgramRun {
            run_id,
            event:
                Event::ChildAdmitted {
                    ordinal,
                    child_session_id,
                    ..
                },
        } => {
            if child(graph, session, run_id, *ordinal)?.0 != *child_session_id {
                return Err(invalid());
            }
        }
        Body::ProgramRun {
            run_id,
            event:
                Event::ChildStarted {
                    ordinal,
                    activation_id,
                },
        } => {
            validate_start(graph, session, run_id, *ordinal, activation_id)?;
        }
        Body::ProgramCompletionReserved {
            activation_id,
            run_id,
            ordinal,
        } => {
            let header = graph.header(session)?;
            let Some(ExecutionOwner::ProgramRun {
                session_id: parent, ..
            }) = header.execution_owner()
            else {
                return Err(invalid());
            };
            if child(graph, parent, run_id, *ordinal)?.0 != *session {
                return Err(invalid());
            }
            validate_start(graph, parent, run_id, *ordinal, activation_id)?;
        }
        Body::ProgramRun {
            run_id,
            event: Event::ChildSettled { receipt },
        } => {
            let (id, message) = child(graph, session, run_id, receipt.ordinal)?;
            if id != receipt.child_session_id {
                return Err(invalid());
            }
            if let Some(activation) = &receipt.activation_id {
                validate_start(graph, session, run_id, receipt.ordinal, activation)?;
                let settlement = graph.control(&id, ProgramGraphQuery::Settlement(activation))?;
                let Body::ActivationSettled { outcome, .. } = settlement.body() else {
                    return Err(invalid());
                };
                validate_receipt_outcome(outcome, &receipt.outcome, receipt.result.as_ref())?;
                if receipt.outcome == ProgramOutcome::Completed
                    && graph.header(&id)?.initial_output().is_some()
                    && receipt.result.is_none()
                {
                    return Err(invalid());
                }
            } else if !matches!(message.state, StoreAgentMessageState::Discarded { .. })
                || !matches!(
                    receipt.outcome,
                    ProgramOutcome::Cancelled | ProgramOutcome::Interrupted
                )
                || receipt.result.is_some()
            {
                return Err(invalid());
            }
        }
        _ => {}
    }
    Ok(())
}
fn validate_receipt_outcome(
    actual: &ActivationOutcome,
    claimed: &ProgramOutcome,
    result: Option<&ProgramResultBinding>,
) -> Result<()> {
    let valid = match (actual, claimed, result) {
        (ActivationOutcome::Completed { result: None }, ProgramOutcome::Completed, None)
        | (ActivationOutcome::Cancelled, ProgramOutcome::Cancelled, None) => true,
        (
            ActivationOutcome::Completed {
                result: Some(actual),
            },
            ProgramOutcome::Completed,
            Some(result),
        ) => {
            actual.locator() == result.locator
                && actual.summary.schema_sha256 == result.schema_sha256
                && actual.summary.value_sha256 == result.value_sha256
        }
        (ActivationOutcome::Failed { code, .. }, ProgramOutcome::Interrupted, None) => {
            code == "turn.interrupted"
        }
        (
            ActivationOutcome::Failed { code, message },
            ProgramOutcome::Failed {
                code: claimed_code,
                message: claimed_message,
            },
            None,
        ) => code == claimed_code && message == claimed_message,
        _ => false,
    };
    if valid { Ok(()) } else { Err(invalid()) }
}

fn validate_start(
    graph: &impl ProgramGraphRead,
    parent: &SessionId,
    run: &ProgramRunId,
    ordinal: u32,
    activation: &ActivationId,
) -> Result<()> {
    let (id, message) = child(graph, parent, run, ordinal)?;
    let start = graph.control(parent, ProgramGraphQuery::Start(run, ordinal))?;
    if !matches!(start.body(), Body::ProgramRun { event: Event::ChildStarted { activation_id, .. }, .. } if activation_id == activation)
        || !matches!(&message.state, StoreAgentMessageState::Claimed { activation_id, .. } if activation_id == activation)
    {
        return Err(invalid());
    }
    let sink = graph.control(&id, ProgramGraphQuery::Sink(activation))?;
    if !matches!(sink.body(), Body::ProgramCompletionReserved { run_id, ordinal: found, .. } if run_id == run && *found == ordinal)
    {
        return Err(invalid());
    }
    Ok(())
}

/// Retains only new cross-Session references while an atomic commit consumes its appends.
#[derive(Debug, Default)]
pub struct ProgramGraphChecks {
    headers: Vec<SessionHeader>,
    records: Vec<(SessionId, AgentControlRecord)>,
}
impl ProgramGraphChecks {
    /// Captures the bounded Program references from an already validated commit.
    pub fn capture(appends: &[crate::AtomicSessionAppend]) -> Self {
        let mut checks = Self::default();
        for append in appends {
            if let Some(header) = &append.header
                && matches!(
                    header.execution_owner(),
                    Some(ExecutionOwner::ProgramRun { .. })
                )
            {
                checks.headers.push(header.clone());
            }
            checks.records.extend(
                append
                    .controls
                    .iter()
                    .filter(|record| needs_program_graph(record.body()))
                    .map(|record| (append.session_id.clone(), record.clone())),
            );
        }
        checks
    }
    /// Validates references only after every paired append is installed.
    pub fn validate(&self, graph: &impl ProgramGraphRead) -> Result<()> {
        for header in &self.headers {
            validate_program_header(graph, header)?;
        }
        for (session, record) in &self.records {
            validate_program_graph(graph, session, record)?;
            if let Body::ProgramRun {
                run_id,
                event: Event::ChildAdmitted { ordinal, .. },
            } = record.body()
                && !matches!(
                    child(graph, session, run_id, *ordinal)?.1.state,
                    StoreAgentMessageState::Pending
                )
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts_cannot_forge_success_or_omit_or_replace_a_structured_result() {
        let failure = ActivationOutcome::Failed {
            code: "child.failed".into(),
            message: "failed".into(),
        };
        assert!(validate_receipt_outcome(&failure, &ProgramOutcome::Completed, None).is_err());
        assert!(
            validate_receipt_outcome(
                &ActivationOutcome::Cancelled,
                &ProgramOutcome::Completed,
                None
            )
            .is_err()
        );
        let actual = rsi_agent_session_protocol::AgentResultRef {
            child_session_id: SessionId::new("child").unwrap(),
            activation_id: ActivationId::new("activation").unwrap(),
            turn_id: rsi_agent_session_protocol::TurnId::new("turn").unwrap(),
            fact_seq: 7,
            summary: rsi_agent_session_protocol::StructuredResultSummary {
                schema_sha256: "a".repeat(64),
                value_sha256: "b".repeat(64),
                preview: "{}".into(),
            },
        };
        let mut binding = ProgramResultBinding {
            locator: actual.locator(),
            schema_sha256: actual.summary.schema_sha256.clone(),
            value_sha256: actual.summary.value_sha256.clone(),
        };
        let success = ActivationOutcome::Completed {
            result: Some(actual),
        };
        assert!(validate_receipt_outcome(&success, &ProgramOutcome::Completed, None).is_err());
        assert!(
            validate_receipt_outcome(&success, &ProgramOutcome::Completed, Some(&binding)).is_ok()
        );
        binding.value_sha256 = "c".repeat(64);
        assert!(
            validate_receipt_outcome(&success, &ProgramOutcome::Completed, Some(&binding)).is_err()
        );
        assert!(
            validate_receipt_outcome(
                &ActivationOutcome::Completed { result: None },
                &ProgramOutcome::Completed,
                Some(&binding)
            )
            .is_err()
        );
    }
}
