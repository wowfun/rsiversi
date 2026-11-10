//! Claimed input is retained separately from the model's entered messages.

use crate::{ContextError, ContextFold, Result, input_content, input_message};
use rsi_agent_session_protocol::{
    ContributionId, InputClaimRef, InputMessageSource, PreparedInput, PreparedInputMessage,
    PreparedInputRole, SessionFact, SessionFactBody, StepInputDecision, TurnId,
};
use rsi_ai_protocol::{Message, MessageContent};

impl ContextFold {
    /// Last retained, entered native runtime snapshot, borrowed without copying text.
    pub fn runtime_context(&self) -> Option<&str> {
        self.turns.iter().rev().find_map(|turn| {
            let index = turn.runtime_context?;
            match turn.messages[index].content() {
                [MessageContent::Text { text }] => Some(text.as_str()),
                _ => Some(""),
            }
        })
    }
    /// Current canonical Step guidance, cleared at Step boundaries.
    pub fn step_prompt(&self) -> &rsi_agent_session_protocol::StepPrompt {
        &self.step_prompt
    }

    /// Last entered non-Tool context input from this producer in retained Turns.
    /// Claims or omitted inputs do not advance it. Replay and checkpoints retain
    /// the original Fact timestamp; removing its Turn forgets this derived value.
    pub fn previous_context_input_at_ms(&self, producer: &ContributionId) -> Option<u64> {
        self.turns.iter().rev().find_map(|turn| {
            turn.context_input_times
                .get(producer)
                .map(|time| time.get())
        })
    }

    pub(super) fn claim_input(&mut self, turn: &TurnId, fact: &SessionFact) -> Result<()> {
        let turn = self.turn_mut(turn)?;
        turn.claimed_bytes = turn
            .claimed_bytes
            .checked_add(fact.encoded_len())
            .ok_or(ContextError::Capacity)?;
        turn.claimed.push(fact.clone());
        Ok(())
    }
    /// Borrows this Session's exact unprepared input Facts for a Turn.
    pub fn claimed_inputs(&self, turn: &TurnId) -> Result<&[SessionFact]> {
        let index = self
            .turn_index
            .get(&self.turn_key(turn))
            .copied()
            .ok_or_else(|| {
                ContextError::Invalid("input capture references an unknown Turn".into())
            })?;
        Ok(&self.turns[self.relative_index(index)?].claimed)
    }

    pub(super) fn enter_prepared_inputs(
        &mut self,
        turn: &TurnId,
        decision: &StepInputDecision,
        seq: u64,
    ) -> Result<()> {
        let local_execution = matches!(
            self.header.coordinates().location(),
            rsi_agent_session_protocol::ExecutionLocation::Local
        );
        let pending = self.turn_mut(turn)?;
        pending.claimed_bytes = 0;
        let claimed = std::mem::take(&mut pending.claimed);
        let mut offset = 0;
        for input in decision.messages().unwrap_or_default() {
            let (message, origin) = match input {
                PreparedInput::Claimed { claim } => {
                    let fact = claimed_fact(&claimed, *claim, seq)?;
                    (original(fact, *claim, local_execution)?, Some(fact))
                }
                PreparedInput::Replacement { claim, message } => {
                    let fact = claimed_fact(&claimed, *claim, seq)?;
                    let result = prepared(message, seq, offset, local_execution)?;
                    offset += message.content().len();
                    (result, Some(fact))
                }
                PreparedInput::Added { message } => {
                    let result = prepared(message, seq, offset, local_execution)?;
                    offset += message.content().len();
                    (result, None)
                }
            };
            if let Some(message) = message {
                self.push_turn_message(turn, message)?;
                if let Some(fact) = origin {
                    if matches!(
                        fact.body(),
                        SessionFactBody::InputMessageClaimed {
                            source: InputMessageSource::RuntimeContext,
                            ..
                        }
                    ) {
                        let turn = self.turn_mut(turn)?;
                        turn.runtime_context = Some(turn.messages.len() - 1);
                    }
                    if let SessionFactBody::InputMessageClaimed {
                        source:
                            InputMessageSource::PluginContext {
                                contribution_id,
                                tool: None,
                            },
                        ..
                    } = fact.body()
                    {
                        self.turn_mut(turn)?.context_input_times.insert(
                            contribution_id.clone(),
                            std::num::NonZeroU64::new(fact.timestamp_ms())
                                .expect("validated Fact timestamp"),
                        );
                    }
                    let key = self.turn_key(turn);
                    let index = self.turn_mut(turn)?.messages.len() - 1;
                    let changed = self.semantic.as_mut().map_or_else(Vec::new, |semantic| {
                        semantic.record_input(&key, fact.body(), index)
                    });
                    for affected in changed {
                        self.reaccount_turn(&affected)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn claimed_fact(facts: &[SessionFact], claim: InputClaimRef, seq: u64) -> Result<&SessionFact> {
    let fact = facts
        .binary_search_by_key(&claim.fact_seq(seq), SessionFact::seq)
        .ok()
        .map(|index| &facts[index])
        .filter(|fact| (claim.ordinal() as usize) < fact.claimed_input_count())
        .ok_or_else(|| {
            ContextError::Invalid("prepared input is outside the current claimed set".into())
        })?;
    Ok(fact)
}

fn original(
    fact: &SessionFact,
    claim: InputClaimRef,
    local_execution: bool,
) -> Result<Option<Message>> {
    let message = match fact.body() {
        SessionFactBody::TurnAccepted { text, .. } => {
            Message::user_text(text).map_err(|error| ContextError::Invalid(error.to_string()))?
        }
        SessionFactBody::InputMessageClaimed {
            source, content, ..
        } => input_message(source, content, fact.seq(), local_execution)?,
        SessionFactBody::ToolContextsClaimed { messages, .. } => {
            let input = &messages[claim.ordinal() as usize];
            Message::user(
                input
                    .message
                    .text
                    .iter()
                    .map(|text| MessageContent::Text { text: text.clone() })
                    .collect(),
            )
            .map_err(|error| ContextError::Invalid(error.to_string()))?
        }
        _ => {
            return Err(ContextError::Invalid(
                "input coordinate is not a claimed Fact".into(),
            ));
        }
    };
    Ok(Some(message))
}

fn prepared(
    message: &PreparedInputMessage,
    seq: u64,
    offset: usize,
    local_execution: bool,
) -> Result<Option<Message>> {
    // Journal identity remains even when a rewrite empties a message. Native
    // Language requests omit content-less messages instead of rejecting a hook.
    let mut content = input_content(message.content(), seq, offset, local_execution)?;
    content.retain(|part| !matches!(part, MessageContent::Text { text } if text.is_empty()));
    if content.is_empty() {
        return Ok(None);
    }
    match message.role() {
        PreparedInputRole::User => Message::user(content),
        PreparedInputRole::Developer => Message::developer_text(
            content
                .iter()
                .filter_map(|part| match part {
                    MessageContent::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
    .map(Some)
    .map_err(|error| ContextError::Invalid(error.to_string()))
}
