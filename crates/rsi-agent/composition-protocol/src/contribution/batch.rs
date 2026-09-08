use super::{
    ContributionError, ContributionOutput, ContributionResult, MAXIMUM_CONTRIBUTION_INPUT_BYTES,
    MAXIMUM_CONTRIBUTION_INPUTS,
};
use crate::ValidatedDomainProposal;
use rsi_agent_session_protocol::{
    AgentMessageContent, ContributionId, InputMessageSource, MAXIMUM_SESSION_DOMAINS, SessionFact,
    SessionFactBody, StepId, TurnId,
};
use std::collections::BTreeSet;

/// Unvalidated text output. Ordinary context obtains its provenance from the registrar.
#[derive(Debug)]
pub struct ContributionInput {
    source: Option<InputMessageSource>,
    text: String,
}

impl ContributionInput {
    /// Returns context text with framework-assigned contribution identity.
    pub fn context(text: impl Into<String>) -> Self {
        Self {
            source: None,
            text: text.into(),
        }
    }

    /// Returns a closed workspace source; transport and custom plugin sources are rejected
    /// when the complete output is admitted to a batch.
    pub fn sourced(source: InputMessageSource, text: impl Into<String>) -> Self {
        Self {
            source: Some(source),
            text: text.into(),
        }
    }

    fn into_body(
        self,
        producer: &ContributionId,
        turn: &TurnId,
        step: &StepId,
    ) -> ContributionResult<SessionFactBody> {
        let source = match self.source {
            None => InputMessageSource::PluginContext {
                contribution_id: producer.clone(),
            },
            Some(
                source @ (InputMessageSource::AgentInstructions { .. }
                | InputMessageSource::SkillCatalog { .. }
                | InputMessageSource::UserSkillInvocation { .. }),
            ) => source,
            Some(_) => {
                return Err(ContributionError::Invalid(
                    "contributor cannot assign transport or another plugin's provenance".into(),
                ));
            }
        };
        Ok(SessionFactBody::InputMessageEntered {
            turn_id: turn.clone(),
            step_id: step.clone(),
            source,
            content: vec![AgentMessageContent::Text { text: self.text }],
        })
    }
}

/// Complete bounded stage, validated before any business mutation is submitted.
#[derive(Debug, Default)]
pub struct ContributionBatch {
    facts: Vec<SessionFactBody>,
    domains: Vec<ValidatedDomainProposal>,
    input_bytes: usize,
}

impl ContributionBatch {
    /// Validates one producer's whole output before changing this batch.
    ///
    /// # Errors
    /// Rejects forged provenance, invalid Facts, duplicate domains, or aggregate limits.
    pub fn append(
        &mut self,
        producer: &ContributionId,
        turn: &TurnId,
        step: &StepId,
        output: ContributionOutput,
    ) -> ContributionResult<()> {
        if output.inputs.len() > MAXIMUM_CONTRIBUTION_INPUTS.saturating_sub(self.facts.len())
            || output.domains.len() > MAXIMUM_SESSION_DOMAINS.saturating_sub(self.domains.len())
        {
            return Err(ContributionError::Capacity);
        }
        let mut ids = BTreeSet::new();
        for proposal in self.domains.iter().chain(&output.domains) {
            if !ids.insert(proposal.snapshot().identity().id()) {
                return Err(ContributionError::Invalid(
                    "duplicate domain proposal in one stage".into(),
                ));
            }
        }
        let mut input_bytes = self.input_bytes;
        let mut facts = Vec::with_capacity(output.inputs.len());
        for input in output.inputs {
            let body = input.into_body(producer, turn, step)?;
            // Reserve the largest sequence/timestamp envelope. Kernel still charges actual bytes.
            let fact = SessionFact::new(u64::MAX, u64::MAX, body)
                .map_err(|error| ContributionError::Invalid(error.to_string()))?;
            input_bytes = input_bytes
                .checked_add(fact.encoded_len())
                .ok_or(ContributionError::Capacity)?;
            if input_bytes > MAXIMUM_CONTRIBUTION_INPUT_BYTES {
                return Err(ContributionError::Capacity);
            }
            facts.push(fact.into_body());
        }
        self.facts.extend(facts);
        self.domains.extend(output.domains);
        self.input_bytes = input_bytes;
        Ok(())
    }

    /// Transfers the entire validated stage to the owning commit boundary.
    pub fn into_parts(self) -> (Vec<SessionFactBody>, Vec<ValidatedDomainProposal>) {
        (self.facts, self.domains)
    }
}
