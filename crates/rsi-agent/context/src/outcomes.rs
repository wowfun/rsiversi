//! Provider-only completion of durably superseded or terminal Tool batches.
use crate::{ContextError, ContextFold, Result, rejected_tool_message, tool_message};
use rsi_agent_session_protocol::{CompactionSelection, EffectId, SessionFactBody, TurnId};
use rsi_ai_protocol::{Message, MessageContent, MessageRole};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::BTreeMap};

pub(super) type Batches = BTreeMap<usize, Batch>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Batch {
    pub source: EffectId,
    pub calls: BTreeMap<String, Call>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Call {
    pub effect: Option<EffectId>,
    pub started: bool,
    pub settled: bool,
    pub superseded: bool,
}

fn invalid() -> ContextError {
    ContextError::Invalid("invalid Tool outcome provenance in context".into())
}

pub(super) fn prepare_batch(
    batches: &Batches,
    index: usize,
    source: &EffectId,
    message: &Message,
) -> Result<Option<Batch>> {
    let mut calls = BTreeMap::new();
    for content in message.content() {
        if let MessageContent::ToolCall(call) = content
            && (calls.insert(call.id.clone(), Call::default()).is_some()
                || batches
                    .values()
                    .any(|batch| batch.calls.contains_key(&call.id)))
        {
            return Err(invalid());
        }
    }
    if batches.contains_key(&index) || batches.values().any(|batch| &batch.source == source) {
        return Err(invalid());
    }
    Ok((!calls.is_empty()).then(|| Batch {
        source: source.clone(),
        calls,
    }))
}

pub(super) fn validate(batches: &Batches, messages: &[Message]) -> Result<()> {
    let mut sources = std::collections::BTreeSet::new();
    let mut effects = std::collections::BTreeSet::new();
    let mut calls = std::collections::BTreeSet::new();
    for (index, batch) in batches {
        if !sources.insert(&batch.source) {
            return Err(invalid());
        }
        for (id, call) in &batch.calls {
            if !calls.insert(id) {
                return Err(invalid());
            }
            if let Some(effect) = &call.effect
                && (!effects.insert(effect) || (call.settled && !call.started))
            {
                return Err(invalid());
            }
        }
        let message = messages.get(*index).ok_or_else(invalid)?;
        let ids: Vec<_> = message
            .content()
            .iter()
            .filter_map(|c| match c {
                MessageContent::ToolCall(call) => Some(call.id.as_str()),
                _ => None,
            })
            .collect();
        if ids.is_empty()
            || ids.len() != batch.calls.len()
            || ids.iter().any(|id| !batch.calls.contains_key(*id))
            || batch.calls.values().any(|call| {
                (call.started && call.effect.is_none())
                    || (call.superseded && (call.effect.is_some() || call.settled))
            })
        {
            return Err(invalid());
        }
    }
    for (index, message) in messages.iter().enumerate() {
        if message
            .content()
            .iter()
            .any(|c| matches!(c, MessageContent::ToolCall(_)))
            && !batches.contains_key(&index)
        {
            return Err(invalid());
        }
    }
    // Live unfinished batches may be checkpointed, but raw result provenance and
    // adjacency must already be valid. Terminal viewing permits only the missing parts.
    validate_view(messages, batches, true)
}

enum ViewMessage<'a> {
    Retained(&'a Message),
    Missing {
        call_id: &'a str,
        text: &'static str,
    },
}

pub(super) fn normalize<'a>(
    messages: &'a [Message],
    batches: &Batches,
    terminal: bool,
) -> Result<Vec<Cow<'a, Message>>> {
    let mut result = Vec::with_capacity(messages.len());
    visit_view(messages, batches, terminal, |message| {
        result.push(match message {
            ViewMessage::Retained(message) => Cow::Borrowed(message),
            ViewMessage::Missing { call_id, text } => Cow::Owned(
                Message::tool_result(
                    call_id,
                    vec![MessageContent::Text { text: text.into() }],
                    true,
                )
                .map_err(|error| ContextError::Invalid(error.to_string()))?,
            ),
        });
        Ok(())
    })?;
    Ok(result)
}

/// Reuses retained byte totals and counts only newly synthesized outcomes.
pub(super) fn normalize_with_size<'a>(
    messages: &'a [Message],
    batches: &Batches,
    terminal: bool,
    retained_bytes: usize,
) -> Result<(Vec<Cow<'a, Message>>, usize)> {
    let messages = normalize(messages, batches, terminal)?;
    // Every raw message is retained exactly once; only synthesized outcomes add
    // bytes to the already maintained per-Turn total.
    let bytes = messages.iter().try_fold(retained_bytes, |bytes, message| {
        if matches!(message, Cow::Owned(_)) {
            bytes
                .checked_add(crate::encoded_message_bytes(message)?)
                .ok_or(ContextError::TooLarge)
        } else {
            Ok(bytes)
        }
    })?;
    Ok((messages, bytes))
}

/// Enforces the provider-view outcome gate without allocating derived messages.
pub(super) fn validate_view(messages: &[Message], batches: &Batches, terminal: bool) -> Result<()> {
    visit_view(messages, batches, terminal, |_| Ok(()))
}

fn visit_view<'a>(
    messages: &'a [Message],
    batches: &Batches,
    terminal: bool,
    mut emit: impl FnMut(ViewMessage<'a>) -> Result<()>,
) -> Result<()> {
    let mut index = 0;
    while let Some(message) = messages.get(index) {
        if message.role() == MessageRole::Tool {
            return Err(invalid());
        }
        let batch = batches.get(&index);
        emit(ViewMessage::Retained(message))?;
        index += 1;
        for call in message.content().iter().filter_map(|c| match c {
            MessageContent::ToolCall(call) => Some(call),
            _ => None,
        }) {
            let state = batch
                .and_then(|batch| batch.calls.get(&call.id))
                .ok_or_else(invalid)?;
            if let Some(message) = messages.get(index).filter(|m| matches!(m.content(), [MessageContent::ToolResult { call_id, .. }] if call_id == &call.id)) {
                if !state.settled { return Err(invalid()); }
                emit(ViewMessage::Retained(message))?;
                index += 1;
                continue;
            }
            if state.settled {
                return Err(invalid());
            }
            let text = if state.superseded {
                "This tool call was not executed because newer input superseded it."
            } else if !terminal {
                return Err(ContextError::Invalid(
                    "unfinished live Tool batch in provider context".into(),
                ));
            } else if state.started {
                "No result was durably recorded for this tool call. Its outcome is unknown. Do not assume it had no effects or retry it blindly."
            } else {
                "This tool call was not executed; the turn ended before it was started."
            };
            emit(ViewMessage::Missing {
                call_id: &call.id,
                text,
            })?;
        }
    }
    Ok(())
}

pub(super) fn retained(
    batches: &Batches,
    turn: &TurnId,
    selections: &[CompactionSelection],
) -> Batches {
    batches
        .iter()
        .filter_map(|(index, batch)| {
            crate::compaction::retained_index(selections, turn, *index)
                .map(|index| (index, batch.clone()))
        })
        .collect()
}

impl ContextFold {
    fn settle_tool_call(
        &mut self,
        turn_id: &TurnId,
        call_id: &str,
        effect: Option<&EffectId>,
        message: Message,
    ) -> Result<()> {
        let turn = self.turn_mut(turn_id)?;
        let (index, call) = turn
            .batches
            .iter()
            .find_map(|(index, batch)| batch.calls.get(call_id).map(|call| (*index, call)))
            .ok_or_else(invalid)?;
        if call.settled
            || call.superseded
            || call
                .effect
                .as_ref()
                .is_some_and(|expected| effect != Some(expected) || !call.started)
        {
            return Err(invalid());
        }
        let batch = &turn.batches[&index];
        let settled = batch.calls.values().filter(|call| call.settled).count();
        let mut past_call = false;
        for content in turn.messages[index].content() {
            if let MessageContent::ToolCall(call) = content {
                if past_call && batch.calls[&call.id].settled {
                    return Err(invalid());
                }
                past_call |= call.id == call_id;
            }
        }
        if turn.messages.len() != index + 1 + settled {
            return Err(invalid());
        }
        self.push_turn_message_with(turn_id, message, |turn| {
            // The validated live Turn cannot be evicted by message admission.
            turn.batches
                .get_mut(&index)
                .expect("validated batch")
                .calls
                .get_mut(call_id)
                .expect("validated call")
                .settled = true;
        })
    }

    pub(super) fn apply_tool_outcome(&mut self, body: &SessionFactBody) -> Result<()> {
        self.require_live_turn(body.turn_id())?;
        match body {
            SessionFactBody::ToolCallsSuperseded {
                turn_id,
                source_model_effect_id,
            } => {
                let turn = self.turn_mut(turn_id)?;
                let batch = turn
                    .batches
                    .values_mut()
                    .find(|batch| &batch.source == source_model_effect_id)
                    .ok_or_else(|| {
                        ContextError::Invalid(
                            "Tool supersession has no completed model batch".into(),
                        )
                    })?;
                let mut count = 0;
                for call in batch
                    .calls
                    .values_mut()
                    .filter(|call| call.effect.is_none() && !call.settled && !call.superseded)
                {
                    call.superseded = true;
                    count += 1;
                }
                if count == 0 {
                    return Err(ContextError::Invalid(
                        "Tool supersession has no remaining calls".into(),
                    ));
                }
            }
            SessionFactBody::ToolIntent {
                turn_id,
                source_model_effect_id,
                effect_id,
                identity,
                ..
            } => {
                let call = self
                    .turn_mut(turn_id)?
                    .batches
                    .values_mut()
                    .find(|batch| &batch.source == source_model_effect_id)
                    .and_then(|batch| batch.calls.get_mut(identity.call_id()))
                    .ok_or_else(|| ContextError::Invalid("Tool intent has no model call".into()))?;
                if call.effect.is_some() || call.settled || call.superseded {
                    return Err(ContextError::Invalid(
                        "Tool call was already consumed".into(),
                    ));
                }
                call.effect = Some(effect_id.clone());
            }
            SessionFactBody::ToolStarted {
                turn_id,
                effect_id,
                identity,
            } => {
                let call = self
                    .turn_mut(turn_id)?
                    .batches
                    .values_mut()
                    .filter_map(|batch| batch.calls.get_mut(identity.call_id()))
                    .find(|call| call.effect.as_ref() == Some(effect_id))
                    .ok_or_else(|| ContextError::Invalid("Tool start has no intent".into()))?;
                if call.started || call.settled || call.superseded {
                    return Err(ContextError::Invalid("Tool call cannot start again".into()));
                }
                call.started = true;
            }
            SessionFactBody::ToolRejected {
                turn_id,
                identity,
                rejection,
                ..
            } => {
                let message = rejected_tool_message(identity.call_id(), rejection)?;
                self.settle_tool_call(turn_id, identity.call_id(), None, message)?;
            }
            SessionFactBody::ToolResult {
                turn_id,
                identity,
                result,
                effect_id,
                ..
            } => {
                let message = tool_message(identity.call_id(), result)?;
                self.settle_tool_call(turn_id, identity.call_id(), Some(effect_id), message)?;
            }
            _ => unreachable!("caller selected a Tool outcome Fact"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId,
    };
    use rsi_ai_protocol::{
        ContentDelta, ContentStart, FinishReason, LanguageEvent, ModelRef, ToolCall, ToolCallKind,
    };

    fn fold() -> (ContextFold, TurnId) {
        let header = SessionHeader::new(
            SessionId::new("session").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "",
                ModelRef::new("test", "model").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let mut fold = ContextFold::with_limits(header, crate::ContextLimits::default()).unwrap();
        let turn = TurnId::new("turn").unwrap();
        fold.insert_turn(&turn, Message::user_text("work").unwrap())
            .unwrap();
        (fold, turn)
    }

    fn full(fold: &mut ContextFold, turn: &TurnId) {
        while fold.retained_messages < crate::MAXIMUM_CONTEXT_MESSAGES {
            fold.push_turn_message(turn, Message::user_text("more").unwrap())
                .unwrap();
        }
    }

    #[test]
    fn full_context_does_not_settle_without_retaining_the_result() {
        let (mut fold, turn) = fold();
        while fold.retained_messages < crate::MAXIMUM_CONTEXT_MESSAGES - 1 {
            fold.push_turn_message(&turn, Message::user_text("more").unwrap())
                .unwrap();
        }
        let index = fold.retained_messages;
        let message = Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: "call".into(),
            name: "read".into(),
            arguments: "{}".into(),
            kind: ToolCallKind::Function,
        })])
        .unwrap();
        let batch = prepare_batch(
            &Batches::new(),
            index,
            &EffectId::new("model").unwrap(),
            &message,
        )
        .unwrap()
        .unwrap();
        fold.push_turn_message(&turn, message).unwrap();
        fold.turn_mut(&turn).unwrap().batches.insert(index, batch);
        let result = Message::tool_result(
            "call",
            vec![MessageContent::Text {
                text: "done".into(),
            }],
            false,
        )
        .unwrap();
        assert_eq!(
            fold.settle_tool_call(&turn, "call", None, result),
            Err(ContextError::TooLarge)
        );
        let turn = fold.turn_mut(&turn).unwrap();
        assert!(!turn.batches[&index].calls["call"].settled);
        validate(&turn.batches, &turn.messages).unwrap();
    }

    #[test]
    fn full_context_does_not_register_an_unretained_model_batch() {
        let (mut fold, turn) = fold();
        full(&mut fold, &turn);
        let effect = EffectId::new("model").unwrap();
        let mut assembler = rsi_ai_protocol::LanguageAssembler::new();
        for event in [
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::ToolCall {
                    id: "call".into(),
                    name: "read".into(),
                    kind: ToolCallKind::Function,
                },
            },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::ToolArguments("{}".into()),
            },
            LanguageEvent::ContentFinished { index: 0 },
        ] {
            assembler.push(&event).unwrap();
        }
        fold.assemblers.insert(
            effect.clone(),
            crate::ActiveAssembler {
                turn_id: turn.clone(),
                model: ModelRef::new("test", "model").unwrap(),
                eligible_summary: false,
                purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
                assembler,
            },
        );
        let body = SessionFactBody::ModelEvent {
            turn_id: turn.clone(),
            effect_id: effect,
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            event: LanguageEvent::Finished {
                reason: FinishReason::ToolCalls,
                replay: None,
            },
        };
        assert_eq!(fold.apply_body(&body, 1), Err(ContextError::TooLarge));
        assert!(!fold.checkpointable_prefix);
        let turn = fold.turn_mut(&turn).unwrap();
        assert!(turn.batches.is_empty());
        validate(&turn.batches, &turn.messages).unwrap();
    }

    #[test]
    fn checkpoint_rejects_call_ids_reused_across_batches() {
        let message = Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: "call".into(),
            name: "read".into(),
            arguments: "{}".into(),
            kind: ToolCallKind::Function,
        })])
        .unwrap();
        let mut batches = Batches::new();
        for index in 0..2 {
            batches.insert(
                index,
                Batch {
                    source: EffectId::new(format!("model-{index}")).unwrap(),
                    calls: BTreeMap::from([("call".into(), Call::default())]),
                },
            );
        }
        assert!(validate(&batches, &[message.clone(), message]).is_err());
    }
    #[test]
    fn checkpoint_rejects_missing_settled_results_and_orphan_results() {
        let message = Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: "call".into(),
            name: "read".into(),
            arguments: "{}".into(),
            kind: ToolCallKind::Function,
        })])
        .unwrap();
        let mut batch = prepare_batch(
            &Batches::new(),
            0,
            &EffectId::new("model").unwrap(),
            &message,
        )
        .unwrap()
        .unwrap();
        batch.calls.get_mut("call").unwrap().settled = true;
        assert!(validate(&BTreeMap::from([(0, batch)]), &[message]).is_err());
        let orphan = Message::tool_result(
            "call",
            vec![MessageContent::Text {
                text: "orphan".into(),
            }],
            false,
        )
        .unwrap();
        assert!(validate(&Batches::new(), &[orphan]).is_err());
    }
    #[test]
    fn restore_rejects_an_integrity_bound_checkpoint_with_a_missing_settled_result() {
        let (mut fold, turn) = fold();
        fold.through_seq = 1;
        let message = Message::assistant(vec![MessageContent::ToolCall(ToolCall {
            id: "call".into(),
            name: "read".into(),
            arguments: "{}".into(),
            kind: ToolCallKind::Function,
        })])
        .unwrap();
        let mut batch = prepare_batch(
            &Batches::new(),
            1,
            &EffectId::new("model").unwrap(),
            &message,
        )
        .unwrap()
        .unwrap();
        batch.calls.get_mut("call").unwrap().settled = true;
        fold.push_turn_message(&turn, message).unwrap();
        fold.turn_mut(&turn).unwrap().batches.insert(1, batch);
        let bytes = fold.checkpoint_bytes().unwrap();
        assert!(
            matches!(ContextFold::from_checkpoint(fold.header.clone(), crate::ContextLimits::default(), &bytes), Err(ContextError::Invalid(reason)) if reason.contains("outcome provenance"))
        );
    }
    #[test]
    fn settlement_rejects_reversed_results_but_retains_success_after_an_unsettled_sibling() {
        let (mut fold, turn_id) = fold();
        let message = Message::assistant(
            ["a", "b"]
                .into_iter()
                .map(|id| {
                    MessageContent::ToolCall(ToolCall {
                        id: id.into(),
                        name: "read".into(),
                        arguments: "{}".into(),
                        kind: ToolCallKind::Function,
                    })
                })
                .collect(),
        )
        .unwrap();
        let batch = prepare_batch(
            &Batches::new(),
            1,
            &EffectId::new("model").unwrap(),
            &message,
        )
        .unwrap()
        .unwrap();
        fold.push_turn_message(&turn_id, message).unwrap();
        fold.turn_mut(&turn_id).unwrap().batches.insert(1, batch);
        let result = |id| {
            Message::tool_result(
                id,
                vec![MessageContent::Text {
                    text: "done".into(),
                }],
                false,
            )
            .unwrap()
        };
        fold.settle_tool_call(&turn_id, "b", None, result("b"))
            .unwrap();
        assert!(
            fold.settle_tool_call(&turn_id, "a", None, result("a"))
                .is_err()
        );
        let turn = fold.turn_mut(&turn_id).unwrap();
        assert!(!turn.batches[&1].calls["a"].settled);
        validate(&turn.batches, &turn.messages).unwrap();
        let view = normalize(&turn.messages, &turn.batches, true).unwrap();
        assert_eq!(view.len(), 4, "terminal view synthesizes a, retains b");
    }
}
