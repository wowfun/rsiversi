//! Initial acceptance classification shared by ordinary automatic-work adapters.
use crate::{Result, TurnClaim, TurnError, TurnExecution};
use rsi_agent_session_protocol::{DomainIdentity, InputMessageSource, MessageId, SessionFactBody};
use std::collections::BTreeSet;

/// Original acceptance policy and input provenance, never later steering.
#[derive(Clone, Debug)]
pub struct InitialTurnInput {
    /// Exact resolved sandbox policy from acceptance.
    pub sandbox: rsi_sandbox::SandboxMode,
    /// Exact resolved approval policy from acceptance.
    pub require_approval: bool,
    /// At least one accepted input came from a human.
    pub human: bool,
    /// Distinct continuation domains of accepted inputs.
    pub continuation_domains: Vec<DomainIdentity>,
}
struct InitialInput {
    input: InitialTurnInput,
    pending: BTreeSet<MessageId>,
}
impl InitialInput {
    fn accepted(body: &SessionFactBody) -> Result<Self> {
        let (sandbox, require_approval, human, pending) = match body {
            SessionFactBody::TurnAccepted {
                sandbox,
                require_approval,
                ..
            } => (*sandbox, *require_approval, true, BTreeSet::new()),
            SessionFactBody::MessageTurnAccepted {
                sandbox,
                require_approval,
                message_ids,
                ..
            } => (
                *sandbox,
                *require_approval,
                false,
                message_ids.iter().cloned().collect(),
            ),
            _ => {
                return Err(TurnError::Invariant(
                    "initial Turn acceptance is absent".into(),
                ));
            }
        };
        Ok(Self {
            input: InitialTurnInput {
                sandbox,
                require_approval,
                human,
                continuation_domains: vec![],
            },
            pending,
        })
    }
    fn enter(&mut self, source: &InputMessageSource) {
        let (InputMessageSource::Human { message_id: id }
        | InputMessageSource::Continuation { message_id: id, .. }
        | InputMessageSource::Agent { message_id: id, .. }
        | InputMessageSource::Completion { message_id: id, .. }
        | InputMessageSource::Program { message_id: id, .. }) = source
        else {
            return;
        };
        if !self.pending.remove(id) {
            return;
        }
        match source {
            InputMessageSource::Human { .. } => self.input.human = true,
            InputMessageSource::Continuation { source, .. }
                if !self.input.continuation_domains.contains(&source.domain) =>
            {
                self.input.continuation_domains.push(source.domain.clone());
            }
            _ => {}
        }
    }
}
/// Reads the original acceptance through the authenticated claim's paged horizon.
/// Later human steering cannot authorize an initially automatic Turn.
pub async fn initial_turn_input(
    execution: &dyn TurnExecution,
    claim: &TurnClaim,
) -> Result<InitialTurnInput> {
    let mut cursor = claim.accepted_seq().saturating_sub(1);
    let mut state: Option<InitialInput> = None;
    loop {
        let page = execution.read_facts(claim, cursor, 128).await?;
        for fact in &page.facts {
            if fact.body().turn_id() != claim.turn_id() {
                continue;
            }
            if state.is_none() {
                state = Some(InitialInput::accepted(fact.body())?);
            }
            let initial = state
                .as_mut()
                .ok_or_else(|| TurnError::Invariant("initial Turn acceptance is absent".into()))?;
            if let SessionFactBody::InputMessageEntered { source, .. } = fact.body() {
                initial.enter(source);
            }
            if initial.pending.is_empty()
                || matches!(fact.body(), SessionFactBody::ModelIntent { .. })
            {
                return Ok(initial.input.clone());
            }
        }
        if page.through_seq <= cursor || page.facts.is_empty() {
            return state
                .map(|state| state.input)
                .ok_or_else(|| TurnError::Invariant("initial Turn acceptance is absent".into()));
        }
        cursor = page.through_seq;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{ActivationId, TurnId};
    #[test]
    fn only_exact_initial_messages_classify_acceptance() {
        let initial = MessageId::new("initial").unwrap();
        let mut state = InitialInput::accepted(&SessionFactBody::MessageTurnAccepted {
            turn_id: TurnId::new("turn").unwrap(),
            activation_id: ActivationId::new("activation").unwrap(),
            message_ids: vec![initial.clone()],
            model: None,
            reasoning_effort: None,
            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
            require_approval: true,
        })
        .unwrap();
        state.enter(&InputMessageSource::Human {
            message_id: MessageId::new("later-steering").unwrap(),
        });
        assert!(!state.input.human);
        assert_eq!(state.pending.len(), 1);
        state.enter(&InputMessageSource::Human {
            message_id: initial,
        });
        assert!(state.input.human);
        assert!(state.input.require_approval);
        assert!(state.pending.is_empty());
    }
}
