//! Pure typed canonical reference projection, independent of Store implementations.
use super::{Result, StoreError};
use rsi_agent_session_protocol::{
    ActivationOutcome, AgentControlRecord, AgentControlRecordBody, AgentMessageContent,
    AgentMessageSource, DomainIdentity, DomainReferenceCodec, DomainSnapshot, ExecutionOwner,
    InputMessageSource, MAXIMUM_SESSION_DOMAINS, ProgramRunEvent, ReferenceCodecRegistration,
    ReferenceCollector, SessionFact, SessionFactBody, SessionHeader,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

/// Bounded exact-version codec registry retained by one Store generation.
#[derive(Debug, Default)]
pub struct ReferenceCodecs(RwLock<BTreeMap<DomainIdentity, Arc<dyn DomainReferenceCodec>>>);
impl ReferenceCodecRegistration for ReferenceCodecs {
    fn install(
        &self,
        codecs: Vec<Arc<dyn DomainReferenceCodec>>,
    ) -> std::result::Result<(), String> {
        if codecs.len() > MAXIMUM_SESSION_DOMAINS {
            return Err("reference codec batch exceeds capacity".into());
        }
        let mut known = self
            .0
            .write()
            .map_err(|_| "reference codec registry poisoned")?;
        let mut next = known.clone();
        for codec in codecs {
            if let Some(previous) = next.get(codec.identity()) {
                if previous.definition_key() != codec.definition_key() {
                    return Err("exact Domain version cannot change reference semantics".into());
                }
            } else {
                if next.len() == 256 {
                    return Err("reference codec registry exceeds capacity".into());
                }
                next.insert(codec.identity().clone(), codec);
            }
        }
        *known = next;
        Ok(())
    }
}
impl ReferenceCodecs {
    /// Reads one exact codec without retaining a composition owner.
    pub fn collect(
        &self,
        snapshot: &DomainSnapshot,
        output: &mut ReferenceCollector,
    ) -> Result<()> {
        let codec = self
            .0
            .read()
            .map_err(|_| StoreError::Corrupt("reference codec registry poisoned".into()))?
            .get(snapshot.identity())
            .cloned()
            .ok_or_else(|| {
                StoreError::Unsupported(format!(
                    "missing reference codec {} version {}",
                    snapshot.identity().id(),
                    snapshot.identity().version()
                ))
            })?;
        codec
            .references(snapshot.state(), output)
            .map_err(StoreError::Corrupt)
    }
}

/// Projects the immutable Header's typed lineage and execution ownership.
pub fn header_references(header: &SessionHeader) -> Result<ReferenceCollector> {
    let mut output = ReferenceCollector::default();
    if let Some(origin) = header.fork_origin() {
        output
            .session(&origin.parent_session_id)
            .map_err(StoreError::Invalid)?;
        output
            .session(&origin.root_session_id)
            .map_err(StoreError::Invalid)?;
    }
    if let Some(owner) = header.execution_owner() {
        let (ExecutionOwner::TurnActivation { session_id, .. }
        | ExecutionOwner::ProgramRun { session_id, .. }) = owner;
        output.session(session_id).map_err(StoreError::Invalid)?;
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
            AgentMessageContent::Text { .. } | AgentMessageContent::Image { .. } => {}
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
        AgentMessageSource::Human
        | AgentMessageSource::Continuation { .. }
        | AgentMessageSource::Program { .. } => {}
    }
    Ok(())
}
/// Projects every typed control edge; missing Domain codecs remain Unsupported.
pub fn control_references(
    record: &AgentControlRecord,
    codecs: &ReferenceCodecs,
) -> Result<ReferenceCollector> {
    let mut output = native_control_references(record)?;
    if let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() {
        for update in commit.updates() {
            codecs.collect(update.snapshot(), &mut output)?;
        }
    }
    Ok(output)
}
/// Projects built-in control references only; plugin Domain state needs exact codecs.
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
            | AgentControlRecordBody::ProgramCompletionReserved { .. } => {}
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
            SessionFactBody::InputMessageEntered {
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
                    | InputMessageSource::Continuation { .. }
                    | InputMessageSource::Program { .. }
                    | InputMessageSource::AgentInstructions { .. }
                    | InputMessageSource::SkillCatalog { .. }
                    | InputMessageSource::UserSkillInvocation { .. }
                    | InputMessageSource::PluginContext { .. } => {}
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
            | SessionFactBody::TurnTerminal { result: None, .. } => {}
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
        DomainStateValue, DurableReference, MAXIMUM_RECORD_REFERENCES, SessionId,
    };
    #[derive(Debug)]
    struct Codec {
        id: DomainIdentity,
        key: usize,
    }
    impl DomainReferenceCodec for Codec {
        fn identity(&self) -> &DomainIdentity {
            &self.id
        }
        fn definition_key(&self) -> (std::any::TypeId, usize, usize) {
            (std::any::TypeId::of::<Self>(), 0, self.key)
        }
        fn references(
            &self,
            value: &DomainStateValue,
            output: &mut ReferenceCollector,
        ) -> std::result::Result<(), String> {
            let id = value
                .value()
                .as_str()
                .ok_or("expected typed Session identity")?;
            output.session(&SessionId::new(id).map_err(|e| e.to_string())?)
        }
    }
    #[test]
    fn exact_codec_coverage_is_bounded_atomic_and_cannot_change_semantics() {
        let codecs = ReferenceCodecs::default();
        let id = DomainIdentity::new("references", 1).unwrap();
        let snapshot = DomainSnapshot::new(
            id.clone(),
            DomainStateValue::new(serde_json::json!("retained-session")).unwrap(),
        );
        assert!(matches!(
            codecs.collect(&snapshot, &mut ReferenceCollector::default()),
            Err(StoreError::Unsupported(_))
        ));
        codecs
            .install(vec![Arc::new(Codec {
                id: id.clone(),
                key: 1,
            })])
            .unwrap();
        let mut refs = ReferenceCollector::default();
        codecs.collect(&snapshot, &mut refs).unwrap();
        assert_eq!(
            refs.iter().collect::<Vec<_>>(),
            vec![&DurableReference::Session(
                SessionId::new("retained-session").unwrap()
            )]
        );
        assert!(
            codecs
                .install(vec![Arc::new(Codec {
                    id: id.clone(),
                    key: 2
                })])
                .is_err()
        );
        assert!(
            codecs
                .collect(
                    &DomainSnapshot::new(
                        DomainIdentity::new("references", 2).unwrap(),
                        snapshot.state().clone()
                    ),
                    &mut ReferenceCollector::default()
                )
                .is_err()
        );
        assert!(
            codecs
                .collect(
                    &DomainSnapshot::new(
                        id,
                        DomainStateValue::new(serde_json::json!(false)).unwrap()
                    ),
                    &mut ReferenceCollector::default()
                )
                .is_err()
        );
        for n in 0..MAXIMUM_RECORD_REFERENCES {
            refs.session(&SessionId::new(format!("session-{n}")).unwrap())
                .unwrap_or_else(|_| assert_eq!(n, MAXIMUM_RECORD_REFERENCES - 1));
        }
        assert!(refs.session(&SessionId::new("overflow").unwrap()).is_err());
    }
}
