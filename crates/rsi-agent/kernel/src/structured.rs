//! Kernel ownership of output admission, terminal references and exact reads.
use super::*;
use rsi_agent_session_protocol::{AgentResultRef, OutputContract, REPORT_RESULT_TOOL};

pub(super) fn contract<'a>(
    header: &'a SessionHeader,
    turn: &TurnControl,
) -> Option<&'a OutputContract> {
    header
        .initial_output()
        .filter(|output| turn.initial_messages.contains(&output.message_id))
        .map(|output| &output.contract)
}
pub(super) fn validate_conclusion(
    header: &SessionHeader,
    turn: &TurnControl,
    body: &SessionFactBody,
) -> TurnResult<()> {
    if let SessionFactBody::TurnTerminal {
        turn_id,
        outcome,
        result,
    } = body
    {
        if let Some(reference) = result {
            let expected = turn
                .conclusion
                .as_ref()
                .and_then(|(seq, c)| c.structured.as_ref().map(|summary| (*seq, summary)));
            if !matches!(outcome, TurnOutcome::Completed)
                || reference.child_session_id != *header.session_id()
                || turn.activation_id.as_ref() != Some(&reference.activation_id)
                || &reference.turn_id != turn_id
                || expected != Some((reference.fact_seq, &reference.summary))
            {
                return Err(TurnError::Invalid(
                    "terminal output is not this Turn's accepted conclusion".into(),
                ));
            }
        }
        if matches!(outcome, TurnOutcome::Completed)
            && contract(header, turn).is_some()
            && (turn.conclusion.is_none() || result.is_none())
        {
            return Err(TurnError::Invalid(
                "successful structured Turn lacks its exact result".into(),
            ));
        }
        return Ok(());
    }
    let SessionFactBody::ToolResult {
        effect_id,
        identity,
        result,
        conclusion: Some(conclusion),
        ..
    } = body
    else {
        return Ok(());
    };
    if result.is_error || turn.effects.len() != 1 || turn.conclusion.is_some() {
        return Err(TurnError::Invalid(
            "conclusion requires a successful exclusive result".into(),
        ));
    }
    if conclusion.structured.is_some()
        && !matches!(turn.effects.get(effect_id), Some(ActiveEffect::Tool {
            name, identity: expected, started: true, ..
        }) if name == REPORT_RESULT_TOOL && expected == identity)
    {
        return Err(TurnError::Invalid(
            "structured conclusion requires its exact report_result Intent".into(),
        ));
    }
    match (contract(header, turn), &conclusion.structured) {
        (Some(contract), Some(summary))
            if contract
                .summarize(&result.value)
                .is_ok_and(|actual| &actual == summary) =>
        {
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(TurnError::Invalid(
            "conclusion differs from the frozen output contract".into(),
        )),
    }
}
pub(super) fn completion_outcome(
    header: &SessionHeader,
    turn: &TurnControl,
    proposed: &TurnOutcome,
) -> TurnOutcome {
    if matches!(proposed, TurnOutcome::Completed)
        && contract(header, turn).is_some()
        && turn.conclusion.is_none()
    {
        TurnOutcome::Failed {
            code: "structured_output.missing".into(),
            message: format!(
                "The initial activation ended without a valid {REPORT_RESULT_TOOL} submission."
            ),
        }
    } else {
        proposed.clone()
    }
}
pub(super) fn result_reference(
    claim: &TurnClaim,
    turn: &TurnControl,
    outcome: &TurnOutcome,
) -> Option<AgentResultRef> {
    if !matches!(outcome, TurnOutcome::Completed) || turn.cancel_requested {
        return None;
    }
    let (fact_seq, conclusion) = turn.conclusion.as_ref()?;
    Some(AgentResultRef {
        child_session_id: claim.session_id().clone(),
        activation_id: turn.activation_id.clone()?,
        turn_id: claim.turn_id().clone(),
        fact_seq: *fact_seq,
        summary: conclusion.structured.clone()?,
    })
}

impl AgentKernel {
    pub(super) async fn read_structured_result(
        &self,
        caller: &AgentCallerAuthority,
        locator: &rsi_agent_session_protocol::AgentResultLocator,
    ) -> TurnResult<rsi_agent_turn_protocol::AgentResult> {
        self.validate_agent_caller(caller)?;
        if locator.fact_seq == 0 {
            return Err(TurnError::Invalid(
                "result Fact sequence must be positive".into(),
            ));
        }
        let header = read_validated_header_bounded(&self.inner, &locator.child_session_id)
            .await
            .map_err(turn_store_error)?;
        if header
            .fork_origin()
            .is_none_or(|origin| &origin.parent_session_id != caller.session_id())
        {
            return Err(TurnError::Invalid(
                "only the immediate parent can read this result".into(),
            ));
        }
        let message_id = completion_message_id(&locator.child_session_id, &locator.activation_id)?;
        let reference = if matches!(
            header.execution_owner(),
            Some(rsi_agent_session_protocol::ExecutionOwner::ProgramRun { .. })
        ) {
            self.program_result_reference(&header, locator).await?
        } else {
            let (selected, _permit, _lease) = store_reads::read(
                &self.inner,
                caller.session_id(),
                MAXIMUM_SESSION_FACT_BYTES,
                true,
                move |store, id| async move { store.read_agent_message(&id, &message_id).await },
            )
            .await
            .map_err(turn_store_error)?;
            let selected = selected.ok_or_else(|| {
                TurnError::Invalid("activation has no final successful Completion".into())
            })?;
            let AgentMessageSource::Completion {
                child_session_id,
                activation_id,
                outcome:
                    ActivationOutcome::Completed {
                        result: Some(reference),
                    },
            } = selected.message.source
            else {
                return Err(TurnError::Invalid(
                    "activation has no structured successful Completion".into(),
                ));
            };
            if child_session_id != locator.child_session_id
                || activation_id != locator.activation_id
                || &reference.locator() != locator
            {
                return Err(TurnError::Invalid(
                    "result coordinates differ from the exact Completion".into(),
                ));
            }
            reference
        };
        let result = self
            .verify_structured_result(&header, locator, reference)
            .await?;
        self.validate_agent_caller(caller)?;
        Ok(result)
    }
    pub(super) async fn verify_structured_result(
        &self,
        header: &SessionHeader,
        locator: &rsi_agent_session_protocol::AgentResultLocator,
        reference: AgentResultRef,
    ) -> TurnResult<rsi_agent_turn_protocol::AgentResult> {
        let page = read_facts_bounded(
            &self.inner,
            &locator.child_session_id,
            locator.fact_seq - 1,
            1,
        )
        .await
        .map_err(turn_store_error)?;
        let fact = page
            .facts
            .first()
            .filter(|fact| fact.seq() == locator.fact_seq)
            .ok_or_else(|| TurnError::Invariant("result Fact is absent".into()))?;
        let SessionFactBody::ToolResult {
            turn_id,
            result,
            conclusion: Some(conclusion),
            ..
        } = fact.body()
        else {
            return Err(TurnError::Invariant(
                "result locator is not an accepted ToolResult".into(),
            ));
        };
        if turn_id != &locator.turn_id
            || result.is_error
            || conclusion.structured.as_ref() != Some(&reference.summary)
            || header.initial_output().is_none_or(|output| {
                !output
                    .contract
                    .summarize(&result.value)
                    .is_ok_and(|actual| actual == reference.summary)
            })
        {
            return Err(TurnError::Invariant(
                "result Fact disagrees with its frozen contract or Completion".into(),
            ));
        }
        Ok(rsi_agent_turn_protocol::AgentResult {
            reference,
            value: result.value.clone(),
        })
    }
}
