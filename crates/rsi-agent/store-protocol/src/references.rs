//! Pure typed canonical reference projection, independent of Store implementations.
use super::{Result, StoreError};
use rsi_agent_session_protocol::{
    ActivationOutcome, AgentControlRecord, AgentControlRecordBody, AgentMessageContent,
    AgentMessageSource, InputMessageSource, ProgramRunEvent, ReferenceCollector, SessionFact,
    SessionFactBody, SessionHeader,
};

/// Projects the immutable Header's typed lineage and execution ownership.
pub fn header_references(header: &SessionHeader) -> Result<ReferenceCollector> {
    let mut output = ReferenceCollector::default();
    if let Some(origin) = header.history_origin() {
        output
            .session(&origin.parent_session_id)
            .map_err(StoreError::Invalid)?;
    }
    if let Some(origin) = header.fork_origin() {
        output
            .session(&origin.root_session_id)
            .map_err(StoreError::Invalid)?;
    }
    if let Some(owner) = header.execution_owner() {
        output
            .session(owner.session_id())
            .map_err(StoreError::Invalid)?;
    }
    Ok(output)
}

fn contents(
    items: &[AgentMessageContent],
    output: &mut ReferenceCollector,
) -> std::result::Result<(), String> {
    for item in items {
        match item {
            AgentMessageContent::Reference { reference } => {
                output.cas(&reference.snapshot.sha256, reference.snapshot.byte_len)?;
                output.session(&reference.metadata.target.session_id)?;
                if let Some(source) = reference.metadata.source.native() {
                    output.session(&source.session_id)?;
                }
            }
            AgentMessageContent::Text { .. }
            | AgentMessageContent::Image { .. }
            | AgentMessageContent::File { .. } => {}
        }
    }
    Ok(())
}
fn outcome(
    value: &ActivationOutcome,
    output: &mut ReferenceCollector,
) -> std::result::Result<(), String> {
    if let ActivationOutcome::Completed {
        result: Some(result),
    } = value
    {
        output.session(&result.child_session_id)?;
    }
    Ok(())
}
fn source(
    value: &AgentMessageSource,
    output: &mut ReferenceCollector,
) -> std::result::Result<(), String> {
    match value {
        AgentMessageSource::Agent { source_session_id } => output.session(source_session_id)?,
        AgentMessageSource::Completion {
            child_session_id,
            outcome: value,
            ..
        } => {
            output.session(child_session_id)?;
            outcome(value, output)?;
        }
        AgentMessageSource::Human { .. }
        | AgentMessageSource::Application { .. }
        | AgentMessageSource::Command { .. }
        | AgentMessageSource::Continuation { .. }
        | AgentMessageSource::Program { .. } => {}
    }
    Ok(())
}
/// Projects native edges and canonical Domain references without plugin code.
pub fn control_references(record: &AgentControlRecord) -> Result<ReferenceCollector> {
    let mut output = native_control_references(record)?;
    if let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() {
        for update in commit.updates() {
            output
                .extend(update.snapshot().references())
                .map_err(StoreError::Invalid)?;
        }
    }
    Ok(output)
}
/// Projects built-in control references independently of Domain state.
pub fn native_control_references(record: &AgentControlRecord) -> Result<ReferenceCollector> {
    let mut output = ReferenceCollector::default();
    let mut visit = || -> std::result::Result<(), String> {
        match record.body() {
            AgentControlRecordBody::MessageAccepted {
                message,
                root_session_id,
                ..
            } => {
                output.session(root_session_id)?;
                source(&message.source, &mut output)?;
                contents(&message.content, &mut output)?;
            }
            AgentControlRecordBody::ActivationStarted {
                root_session_id,
                parent_session_id,
                ..
            } => {
                output.session(root_session_id)?;
                if let Some(parent) = parent_session_id {
                    output.session(parent)?;
                }
            }
            AgentControlRecordBody::CompletionReserved {
                parent_session_id, ..
            } => output.session(parent_session_id)?,
            AgentControlRecordBody::ActivationSettled { outcome: value, .. } => {
                outcome(value, &mut output)?;
            }
            AgentControlRecordBody::ProgramRun { event, .. } => match event {
                ProgramRunEvent::Accepted { descriptor } => {
                    output.session(&descriptor.session_id)?;
                    output.cas(&descriptor.script.sha256, descriptor.script.bytes)?;
                }
                ProgramRunEvent::ChildAdmitted {
                    child_session_id, ..
                } => output.session(child_session_id)?,
                ProgramRunEvent::ChildSettled { receipt } => {
                    output.session(&receipt.child_session_id)?;
                    if let Some(result) = &receipt.result {
                        output.session(&result.locator.child_session_id)?;
                    }
                }
                ProgramRunEvent::Terminal { result, .. } => {
                    if let Some(result) = result {
                        output.cas(&result.sha256, result.bytes)?;
                    }
                }
                ProgramRunEvent::Started
                | ProgramRunEvent::Detached
                | ProgramRunEvent::CancellationRequested
                | ProgramRunEvent::ChildStarted { .. }
                | ProgramRunEvent::Progress { .. } => {}
            },
            AgentControlRecordBody::DomainStateCommitted { .. }
            | AgentControlRecordBody::TurnBoundaryRecorded { .. }
            | AgentControlRecordBody::MessageSuccessor { .. }
            | AgentControlRecordBody::QueueMutationRecorded { .. }
            | AgentControlRecordBody::MessageClaimed { .. }
            | AgentControlRecordBody::MessagePromoted { .. }
            | AgentControlRecordBody::MessageDiscarded { .. }
            | AgentControlRecordBody::ActivationWaitingForDescendants { .. }
            | AgentControlRecordBody::WaitParked { .. }
            | AgentControlRecordBody::WaitResumed { .. }
            | AgentControlRecordBody::ProgramCompletionReserved { .. }
            | AgentControlRecordBody::MessageFeedbackChanged { .. }
            | AgentControlRecordBody::WorkingDirectoryChanged { .. }
            | AgentControlRecordBody::InboxPaused
            | AgentControlRecordBody::InboxResumed => {}
        }
        Ok(())
    };
    visit().map_err(StoreError::Invalid)?;
    Ok(output)
}
/// Projects Fact provenance and captured references, never arbitrary Tool JSON or model text.
pub fn fact_references(record: &SessionFact) -> Result<ReferenceCollector> {
    let mut output = ReferenceCollector::default();
    let mut visit = || -> std::result::Result<(), String> {
        match record.body() {
            SessionFactBody::StepInputsPrepared { decision, .. } => {
                for input in decision.messages().unwrap_or_default() {
                    match input {
                        rsi_agent_session_protocol::PreparedInput::Claimed { .. } => {}
                        rsi_agent_session_protocol::PreparedInput::Replacement {
                            message, ..
                        }
                        | rsi_agent_session_protocol::PreparedInput::Added { message } => {
                            contents(message.content(), &mut output)?;
                        }
                    }
                }
            }
            SessionFactBody::InputMessageClaimed {
                source, content, ..
            } => {
                contents(content, &mut output)?;
                match source {
                    InputMessageSource::Agent {
                        source_session_id, ..
                    } => output.session(source_session_id)?,
                    InputMessageSource::Completion {
                        child_session_id,
                        outcome: value,
                        ..
                    } => {
                        output.session(child_session_id)?;
                        outcome(value, &mut output)?;
                    }
                    InputMessageSource::Human { .. }
                    | InputMessageSource::Application { .. }
                    | InputMessageSource::Continuation { .. }
                    | InputMessageSource::Program { .. }
                    | InputMessageSource::AgentInstructions { .. }
                    | InputMessageSource::SkillCatalog { .. }
                    | InputMessageSource::UserSkillInvocation { .. }
                    | InputMessageSource::RuntimeContext
                    | InputMessageSource::PluginContext { .. }
                    | InputMessageSource::Command { .. } => {}
                }
            }
            SessionFactBody::TurnTerminal {
                result: Some(result),
                ..
            } => output.session(&result.child_session_id)?,
            SessionFactBody::TurnAccepted { .. }
            | SessionFactBody::MessageTurnAccepted { .. }
            | SessionFactBody::StepStarted { .. }
            | SessionFactBody::StepEnded { .. }
            | SessionFactBody::WorkspaceTouched { .. }
            | SessionFactBody::ImageRequested { .. }
            | SessionFactBody::CancelRequested { .. }
            | SessionFactBody::ManualCompaction { .. }
            | SessionFactBody::ContextCompactionInstalled { .. }
            | SessionFactBody::ModelIntent { .. }
            | SessionFactBody::ModelStarted { .. }
            | SessionFactBody::ImageIntent { .. }
            | SessionFactBody::ImageStarted { .. }
            | SessionFactBody::ImageOutput { .. }
            | SessionFactBody::ModelAttemptSettled { .. }
            | SessionFactBody::ToolIntent { .. }
            | SessionFactBody::ToolCallsSuperseded { .. }
            | SessionFactBody::ToolRejected { .. }
            | SessionFactBody::ToolStarted { .. }
            | SessionFactBody::ToolResult { .. }
            | SessionFactBody::TurnTerminal { result: None, .. }
            | SessionFactBody::ToolContextsClaimed { .. }
            | SessionFactBody::StepPrepared { .. } => {}
        }
        Ok(())
    };
    visit().map_err(StoreError::Invalid)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        DomainIdentity, DomainMutationSource, DomainRevision, DomainSnapshot, DomainStateCommit,
        DomainStateUpdate, DomainStateValue, SessionId,
    };

    #[test]
    fn unknown_domain_history_retains_explicit_roots_without_executable_code() {
        let mut references = ReferenceCollector::default();
        references
            .session(&SessionId::new("retained-child").unwrap())
            .unwrap();
        references.cas(&"b".repeat(64), 27).unwrap();
        let snapshot = DomainSnapshot::new(
            DomainIdentity::new("unloaded-plugin", 987).unwrap(),
            DomainStateValue::new(serde_json::json!({"opaque":"not interpreted"})).unwrap(),
        )
        .with_references(references.clone());
        let commit = DomainStateCommit::new(
            None,
            DomainMutationSource::Baseline,
            vec![DomainStateUpdate::new(DomainRevision::new(0), snapshot).unwrap()],
        )
        .unwrap();
        let record = AgentControlRecord::new(
            1,
            1,
            AgentControlRecordBody::DomainStateCommitted { commit },
        )
        .unwrap();
        let restored = serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(control_references(&restored).unwrap(), references);
    }
}
