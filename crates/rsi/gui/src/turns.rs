//! Window-owned classification, independent of block content revisions.
use super::Block;
use rsi_agent_session_protocol::{
    EffectId, ModelEventPurpose, SessionFact, SessionFactBody as Fact, TurnId, TurnOutcome,
};
use rsi_ai_protocol::{ContentStart, FinishReason, LanguageEvent, ModelRef, TokenUsage};
use rsi_conversation::RetryState;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct View {
    pub revision: u64,
    pub entries: BTreeMap<TurnId, Turn>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Turn {
    pub id: TurnId,
    pub blocks: Vec<String>,
    pub process: Vec<String>,
    pub candidate: Vec<String>,
    pub answer: Vec<String>,
    /// Positions in `blocks` of the last settled conversational response with visible prose.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub closing_prose: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_message: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub retries: Vec<RetryRow>,
    pub terminal: Option<String>,
    /// Latest received turn boundary represented in this window, used to acknowledge reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading_sequence: Option<String>,
    pub timestamps: BTreeMap<String, u64>,
    pub elapsed_ms: Option<u64>,
    pub usage: Option<TokenUsage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub usage_routes: Vec<ModelRef>,
    pub partial: bool,
    pub status: String,
    pub running: bool,
    pub foldable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RetryRow {
    block: usize,
    number: u8,
    maximum: Option<u8>,
    delay_ms: u64,
    reason: &'static str,
    explanation: Box<str>,
    state: RetryState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Link {
    effect: Option<EffectId>,
    role: &'static str,
    image: bool,
    timestamp_ms: Option<u64>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Model {
    seq: u64,
    started: bool,
    conversation: bool,
    tools: bool,
    finished: Option<(u64, bool)>,
}
#[derive(Clone, Debug, Default)]
struct State {
    accepted: Option<u64>,
    first: Option<String>,
    lost_start: bool,
    links: BTreeMap<String, Link>,
    models: BTreeMap<EffectId, Model>,
    tool_seq: u64,
    terminal: Option<(u64, bool, String)>,
    started_ms: Option<u64>,
    ended_ms: Option<u64>,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Index {
    states: BTreeMap<TurnId, State>,
    pub view: View,
    pub cache_revision: std::sync::Arc<()>,
    window: Vec<String>,
    dirty: BTreeSet<TurnId>,
    #[cfg(test)]
    rebuilds: usize,
}
impl Index {
    pub fn live(
        &mut self,
        turn: &TurnId,
        effect: &EffectId,
        purpose: ModelEventPurpose,
        keys: &[String],
        blocks: &VecDeque<Block>,
        after: u64,
    ) {
        let state = self.states.entry(turn.clone()).or_default();
        let model = state.models.entry(effect.clone()).or_default();
        let previous = *model;
        if model.seq == 0 {
            model.seq = after.saturating_add(1);
        }
        model.conversation = purpose == ModelEventPurpose::Conversation;
        let mut changed = *model != previous;
        for block in blocks.iter().filter(|block| keys.contains(&block.key)) {
            let link = Link {
                effect: Some(effect.clone()),
                role: block.role,
                image: false,
                timestamp_ms: state
                    .links
                    .get(&block.key)
                    .and_then(|link| link.timestamp_ms),
            };
            if state.links.get(&block.key) != Some(&link) {
                state.links.insert(block.key.clone(), link);
                changed = true;
            }
        }
        if changed {
            self.dirty.insert(turn.clone());
        }
        self.retain(blocks, Some(turn));
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one Fact updates its model and Turn projection together"
    )]
    pub fn observe(&mut self, fact: &SessionFact, blocks: &VecDeque<Block>) {
        let Some(id) = fact.body().turn_id().cloned() else {
            return;
        };
        let state = self.states.entry(id.clone()).or_default();
        let mut changed = false;
        let effect = match fact.body() {
            Fact::ModelIntent { effect_id, .. }
            | Fact::ModelStarted { effect_id, .. }
            | Fact::ModelAttemptSettled { effect_id, .. } => Some(effect_id.clone()),
            _ => None,
        };
        if let Some(effect) = &effect {
            let model = state.models.entry(effect.clone()).or_default();
            let previous = *model;
            model.seq = if model.seq == 0 {
                fact.seq()
            } else {
                model.seq.min(fact.seq())
            };
            if let Fact::ModelAttemptSettled {
                purpose, attempt, ..
            } = fact.body()
            {
                for (_, event) in attempt.events().filter_map(Result::ok) {
                    match &event {
                        LanguageEvent::ContentStarted {
                            content: ContentStart::ToolCall { .. },
                            ..
                        } => model.tools = true,
                        LanguageEvent::Finished { reason, .. } => {
                            model.tools |= *reason == FinishReason::ToolCalls;
                            model.finished = Some((
                                fact.seq(),
                                matches!(reason, FinishReason::Stop | FinishReason::ToolCalls),
                            ));
                        }
                        LanguageEvent::Failed { .. } => model.finished = Some((fact.seq(), false)),
                        _ => {}
                    }
                }
                model.conversation = *purpose == ModelEventPurpose::Conversation;
            }
            changed |= *model != previous;
            if matches!(fact.body(), Fact::ModelStarted { .. }) && !model.started {
                model.started = true;
                changed |= self
                    .view
                    .entries
                    .get(&id)
                    .is_some_and(|turn| !turn.retries.is_empty());
            }
        }
        match fact.body() {
            Fact::TurnAccepted { .. }
            | Fact::MessageTurnAccepted { .. }
            | Fact::ImageRequested { .. } => {
                changed |= state.accepted != Some(fact.seq());
                state.accepted = Some(fact.seq());
                state.started_ms = Some(fact.timestamp_ms());
            }
            Fact::ToolIntent { .. }
            | Fact::ToolStarted { .. }
            | Fact::ToolResult { .. }
            | Fact::ToolRejected { .. } => {
                changed |= state.tool_seq < fact.seq();
                state.tool_seq = state.tool_seq.max(fact.seq());
            }
            Fact::TurnTerminal { outcome, .. } => {
                let label = match outcome {
                    TurnOutcome::Completed => "Completed",
                    TurnOutcome::Blocked => "Blocked",
                    TurnOutcome::Cancelled => "Cancelled",
                    TurnOutcome::Failed { .. } => "Failed",
                    TurnOutcome::PartialFailed { .. } => "Partially failed",
                    TurnOutcome::Interrupted { .. } => "Interrupted",
                    TurnOutcome::MaxTokens => "Output limit",
                };
                if state
                    .terminal
                    .as_ref()
                    .is_none_or(|(seq, _, _)| *seq < fact.seq())
                {
                    changed = true;
                    state.terminal = Some((
                        fact.seq(),
                        matches!(outcome, TurnOutcome::Completed),
                        label.into(),
                    ));
                    state.ended_ms = Some(fact.timestamp_ms());
                }
            }
            _ => {}
        }
        for block in blocks {
            if block.sources.contains_sequence(fact.seq())
                || block.first_seq == fact.seq()
                || block.tool_start_seq == Some(fact.seq())
            {
                state.first.get_or_insert_with(|| block.key.clone());
                let link = Link {
                    effect: effect.clone(),
                    role: block.role,
                    image: matches!(fact.body(), Fact::ImageOutput { .. }),
                    timestamp_ms: if block.first_seq == fact.seq() {
                        Some(fact.timestamp_ms())
                    } else {
                        state
                            .links
                            .get(&block.key)
                            .and_then(|link| link.timestamp_ms)
                    },
                };
                if state.links.get(&block.key) != Some(&link) {
                    state.links.insert(block.key.clone(), link);
                    changed = true;
                }
            }
        }
        if changed {
            self.dirty.insert(id.clone());
        }
        self.retain(blocks, Some(&id));
    }
    pub fn retain(&mut self, blocks: &VecDeque<Block>, pending: Option<&TurnId>) {
        let window_changed = !self
            .window
            .iter()
            .map(String::as_str)
            .eq(blocks.iter().map(|block| block.key.as_str()));
        if window_changed {
            let keys: BTreeSet<_> = blocks.iter().map(|block| block.key.as_str()).collect();
            for (id, state) in &mut self.states {
                if state
                    .first
                    .as_ref()
                    .is_some_and(|key| !keys.contains(key.as_str()))
                {
                    state.lost_start = true;
                }
                state.links.retain(|key, _| keys.contains(key.as_str()));
                // New blocks in another Turn do not invalidate this Turn's view.
                let unchanged = self.view.entries.get(id).is_some_and(|view| {
                    view.blocks.iter().map(String::as_str).eq(blocks
                        .iter()
                        .filter(|block| state.links.contains_key(&block.key))
                        .map(|block| block.key.as_str()))
                });
                if !unchanged {
                    self.dirty.insert(id.clone());
                }
            }
            self.window = blocks.iter().map(|block| block.key.clone()).collect();
        }
        self.states
            .retain(|id, state| !state.links.is_empty() || Some(id) == pending);
        let previous_len = self.view.entries.len();
        self.view
            .entries
            .retain(|id, _| self.states.contains_key(id));
        let mut changed = previous_len != self.view.entries.len();
        for id in std::mem::take(&mut self.dirty) {
            let Some(state) = self.states.get_mut(&id) else {
                continue;
            };
            state.models.retain(|effect, _| {
                state
                    .links
                    .values()
                    .any(|link| link.effect.as_ref() == Some(effect))
            });
            if state.links.is_empty() {
                changed |= self.view.entries.remove(&id).is_some();
                continue;
            }
            #[cfg(test)]
            {
                self.rebuilds += 1;
            }
            let turn = state.project(&id, blocks);
            if self.view.entries.get(&id) != Some(&turn) {
                self.view.entries.insert(id, turn);
                changed = true;
            }
        }
        if changed {
            self.cache_revision = std::sync::Arc::new(());
            self.view.revision = self
                .view
                .revision
                .checked_add(1)
                .expect("Turn presentation revision exhausted");
        }
    }
}

impl State {
    fn closing_effect(&self, blocks: &VecDeque<Block>) -> Option<&EffectId> {
        self.terminal.as_ref()?;
        blocks.iter().rev().find_map(|block| {
            let link = self.links.get(&block.key)?;
            let effect = link.effect.as_ref()?;
            (link.role == "assistant"
                && !link.image
                && self
                    .models
                    .get(effect)
                    .is_some_and(|model| model.conversation)
                && !block.text.trim().is_empty())
            .then_some(effect)
        })
    }
    fn project_retry<'a>(
        &self,
        block: &'a Block,
        link: &Link,
        turn: &mut Turn,
        reservations: &mut BTreeMap<&'a EffectId, usize>,
    ) {
        let Some((retry, maximum)) = block
            .request
            .as_ref()
            .and_then(rsi_conversation::RequestPresentation::scheduled_retry)
        else {
            return;
        };
        let decision = &retry.decision;
        let state = RetryState::observed(
            self.models
                .get(&decision.effect_id)
                .is_some_and(|model| model.started),
            self.terminal.is_some(),
        );
        let previous = link
            .effect
            .as_ref()
            .and_then(|effect| reservations.remove(effect));
        let index = previous.unwrap_or(turn.retries.len());
        let row = RetryRow {
            block: previous.map_or(turn.blocks.len(), |i: usize| turn.retries[i].block),
            number: decision.number,
            maximum,
            delay_ms: decision.delay_ms,
            reason: retry.reason.code(),
            explanation: retry.explanation.clone(),
            state,
        };
        if let Some(i) = previous {
            turn.retries[i] = row;
        } else {
            turn.retries.push(row);
        }
        reservations.insert(&decision.effect_id, index);
    }
    fn project(&self, id: &TurnId, blocks: &VecDeque<Block>) -> Turn {
        let partial = self.accepted.is_none() || self.lost_start;
        let latest = self
            .models
            .iter()
            .filter(|(_, model)| model.conversation)
            .max_by_key(|(_, model)| model.seq);
        let successful = self.terminal.as_ref().is_some_and(|(_, ok, _)| *ok);
        let interrupted_model =
            latest.is_some_and(|(_, model)| model.finished.is_some_and(|(_, ok)| !ok));
        let mut turn = Turn {
            id: id.clone(),
            blocks: vec![],
            process: vec![],
            candidate: vec![],
            answer: vec![],
            closing_prose: vec![],
            feedback_message: None,
            retries: vec![],
            terminal: None,
            reading_sequence: None,
            timestamps: BTreeMap::new(),
            elapsed_ms: (!partial)
                .then_some(self.started_ms.zip(self.ended_ms))
                .flatten()
                .and_then(|(start, end)| end.checked_sub(start)),
            usage: None,
            usage_routes: vec![],
            partial,
            running: self.terminal.is_none(),
            foldable: !partial && (self.terminal.is_none() || (successful && !interrupted_model)),
            status: self
                .terminal
                .as_ref()
                .map_or("Running", |(_, _, label)| label)
                .into(),
        };
        let closing = self.closing_effect(blocks);
        turn.feedback_message = closing
            .and_then(|effect| self.models.get(effect)?.finished)
            .filter(|(_, ok)| *ok)
            .map(|(seq, _)| seq.to_string());
        let mut reservations = BTreeMap::new();
        let mut measure_usage = !partial && self.terminal.is_some();
        let mut routes_known = true;
        for block in blocks {
            let Some(link) = self.links.get(&block.key) else {
                continue;
            };
            if measure_usage
                && let Some(request) = block.request.as_ref().filter(|r| !r.is_compaction())
            {
                turn.usage = request
                    .reported_usage()
                    .and_then(|reading| match turn.usage {
                        Some(previous) => rsi_conversation::sum_token_usage(previous, reading).ok(),
                        None => Some(reading),
                    });
                measure_usage = turn.usage.is_some();
                if routes_known && measure_usage {
                    if let Some(route) = request.model_route() {
                        if !turn.usage_routes.contains(route) {
                            turn.usage_routes.push(route.clone());
                        }
                    } else {
                        routes_known = false;
                    }
                }
            }
            if let Some(time) = link.timestamp_ms {
                turn.timestamps.insert(block.key.clone(), time);
            }
            self.project_retry(block, link, &mut turn, &mut reservations);
            if closing.is_some()
                && link.effect.as_ref() == closing
                && link.role == "assistant"
                && !link.image
            {
                turn.closing_prose.push(turn.blocks.len());
            }
            turn.blocks.push(block.key.clone());
            if self
                .terminal
                .as_ref()
                .is_some_and(|(seq, _, _)| *seq == block.first_seq)
            {
                turn.terminal = Some(block.key.clone());
            }
            let candidate = link.role == "assistant"
                && (link.image
                    || latest.is_some_and(|(effect, model)| {
                        Some(effect) == link.effect.as_ref()
                            && !model.tools
                            && self.tool_seq < model.finished.map_or(model.seq, |(seq, _)| seq)
                    }));
            let answer = candidate
                && successful
                && !partial
                && (link.image
                    || latest.is_some_and(|(_, model)| {
                        !model.tools
                            && model
                                .finished
                                .is_some_and(|(seq, ok)| ok && seq > self.tool_seq)
                    }));
            if answer {
                if link.image {
                    turn.feedback_message = Some(block.first_seq.to_string());
                }
                turn.answer.push(block.key.clone());
            } else if candidate {
                turn.candidate.push(block.key.clone());
            }
            let compaction = link
                .effect
                .as_ref()
                .and_then(|effect| self.models.get(effect))
                .is_some_and(|model| !model.conversation);
            if matches!(
                link.role,
                "tool" | "reasoning" | "metadata" | "assistant" | "step-context"
            ) && !answer
                && !compaction
            {
                turn.process.push(block.key.clone());
            }
        }
        let boundary = if turn.terminal.is_some() {
            self.terminal.as_ref().map(|(seq, _, _)| *seq)
        } else {
            self.accepted.filter(|_| !self.lost_start)
        };
        turn.reading_sequence = boundary.map(|seq| seq.to_string());
        if !routes_known || turn.usage.is_none() {
            turn.usage_routes = vec![];
        }
        turn
    }
}

#[cfg(test)]
mod tests {
    use super::super::Transcript as ActualTranscript;
    use super::*;
    use rsi_agent_session_protocol::{
        ActivationId, AgentMessageContent, InputMessageSource, MessageId, StepId,
    };
    fn accepted() -> Fact {
        Fact::MessageTurnAccepted {
            turn_id: TurnId::new("turn").unwrap(),
            activation_id: ActivationId::new("activation").unwrap(),
            message_ids: vec![MessageId::new("input").unwrap()],
            model: None,
            reasoning_effort: None,
            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
            require_approval: false,
        }
    }
    enum Input {
        Fact(Box<Fact>),
        Event {
            effect: String,
            event: LanguageEvent,
            purpose: ModelEventPurpose,
        },
    }
    impl From<Fact> for Input {
        fn from(fact: Fact) -> Self {
            Self::Fact(Box::new(fact))
        }
    }
    #[derive(Default)]
    struct Transcript {
        inner: ActualTranscript,
        streaming: Option<(
            String,
            ModelEventPurpose,
            rsi_agent_session_protocol::ModelAttemptBuilder,
            u64,
        )>,
    }
    impl std::ops::Deref for Transcript {
        type Target = ActualTranscript;
        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl std::ops::DerefMut for Transcript {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }
    fn event(effect: &str, event: LanguageEvent, purpose: ModelEventPurpose) -> Input {
        Input::Event {
            effect: effect.into(),
            event,
            purpose,
        }
    }
    fn text(effect: &str, text: &str) -> Input {
        event(
            effect,
            LanguageEvent::ContentDelta {
                index: 0,
                delta: rsi_ai_protocol::ContentDelta::Text(text.into()),
            },
            ModelEventPurpose::Conversation,
        )
    }
    fn finish(effect: &str, reason: FinishReason) -> Input {
        event(
            effect,
            LanguageEvent::Finished {
                reason,
                replay: None,
            },
            ModelEventPurpose::Conversation,
        )
    }
    fn complete(effect: &str, text: &str) -> Fact {
        Fact::ModelAttemptSettled {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new(effect).unwrap(),
            purpose: ModelEventPurpose::Conversation,
            attempt: rsi_agent_session_protocol::ModelAttemptLog::from_events(
                [
                    LanguageEvent::ContentStarted {
                        index: 0,
                        content: ContentStart::Text,
                    },
                    LanguageEvent::ContentDelta {
                        index: 0,
                        delta: rsi_ai_protocol::ContentDelta::Text(text.into()),
                    },
                    LanguageEvent::ContentFinished { index: 0 },
                    LanguageEvent::Finished {
                        reason: FinishReason::Stop,
                        replay: None,
                    },
                ]
                .into_iter()
                .map(|event| (0, event)),
                true,
            )
            .unwrap(),
        }
    }
    fn terminal(outcome: TurnOutcome) -> Fact {
        Fact::TurnTerminal {
            turn_id: TurnId::new("turn").unwrap(),
            outcome,
            result: None,
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "fixture mirrors one complete streaming attempt lifecycle"
    )]
    fn apply(transcript: &mut Transcript, input: impl Into<Input>) {
        let input = input.into();
        if let Some((old, _, _, _)) = &transcript.streaming {
            let change = match &input {
                Input::Fact(_) => true,
                Input::Event { effect, .. } => effect != old,
            };
            if change {
                let old = old.clone();
                apply(transcript, finish(&old, FinishReason::MaxTokens));
            }
        }
        match input {
            Input::Fact(body) => {
                let seq = transcript.seq + 1;
                let fact = SessionFact::new(seq, 1, *body).unwrap();
                transcript.fact(&fact);
                if let Fact::InputMessageClaimed {
                    turn_id,
                    step_id,
                    source,
                    content,
                } = fact.body()
                {
                    let claim = rsi_agent_session_protocol::InputClaimRef::new(seq, 0).unwrap();
                    let decision = rsi_agent_session_protocol::StepInputDecision::enter(vec![
                        rsi_agent_session_protocol::PreparedInput::Claimed { claim },
                    ])
                    .unwrap();
                    let prepared = SessionFact::new(
                        seq + 1,
                        1,
                        Fact::StepInputsPrepared {
                            turn_id: turn_id.clone(),
                            step_id: step_id.clone(),
                            decision,
                        },
                    )
                    .unwrap();
                    transcript.entered(
                        &prepared,
                        rsi_conversation::EnteredInput {
                            index: 0,
                            seq,
                            offset: 0,
                            source: Some(source),
                            content: rsi_conversation::EnteredContent::Blocks(content),
                        },
                    );
                    transcript.fact(&prepared);
                }
            }
            Input::Event {
                effect,
                event,
                purpose,
            } => {
                let turn = TurnId::new("turn").unwrap();
                let effect_id = EffectId::new(&effect).unwrap();
                if transcript.streaming.is_none() {
                    let seq = transcript.seq + 1;
                    transcript.fact(
                        &SessionFact::new(
                            seq,
                            1,
                            Fact::ModelStarted {
                                turn_id: turn.clone(),
                                effect_id: effect_id.clone(),
                            },
                        )
                        .unwrap(),
                    );
                    let mut builder = rsi_agent_session_protocol::ModelAttemptBuilder::new();
                    builder
                        .push(
                            0,
                            &LanguageEvent::ContentStarted {
                                index: 0,
                                content: ContentStart::Text,
                            },
                        )
                        .unwrap();
                    transcript.streaming = Some((effect.clone(), purpose, builder, 0));
                }
                let (_, _, builder, revision) = transcript.streaming.as_mut().unwrap();
                *revision += 1;
                let terminal = matches!(
                    &event,
                    LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
                );
                if terminal {
                    builder
                        .push(*revision, &LanguageEvent::ContentFinished { index: 0 })
                        .unwrap();
                    if matches!(
                        &event,
                        LanguageEvent::Finished {
                            reason: FinishReason::ToolCalls,
                            ..
                        }
                    ) {
                        builder
                            .push(
                                *revision,
                                &LanguageEvent::ContentStarted {
                                    index: 1,
                                    content: ContentStart::ToolCall {
                                        id: "call".into(),
                                        name: "tool".into(),
                                        kind: rsi_ai_protocol::ToolCallKind::Function,
                                    },
                                },
                            )
                            .unwrap();
                        builder
                            .push(
                                *revision,
                                &LanguageEvent::ContentDelta {
                                    index: 1,
                                    delta: rsi_ai_protocol::ContentDelta::ToolArguments(
                                        "{}".into(),
                                    ),
                                },
                            )
                            .unwrap();
                        builder
                            .push(*revision, &LanguageEvent::ContentFinished { index: 1 })
                            .unwrap();
                    }
                }
                builder.push(*revision, &event).unwrap();
                if terminal {
                    let (_, purpose, builder, _) = transcript.streaming.take().unwrap();
                    let seq = transcript.seq + 1;
                    transcript.fact(
                        &SessionFact::new(
                            seq,
                            1,
                            Fact::ModelAttemptSettled {
                                turn_id: turn,
                                effect_id,
                                purpose,
                                attempt: builder.finish(true).unwrap(),
                            },
                        )
                        .unwrap(),
                    );
                } else {
                    let snapshot = rsi_agent_turn_protocol::ModelLiveSnapshot {
                        session_id: rsi_agent_session_protocol::SessionId::new("session").unwrap(),
                        turn_id: turn,
                        effect_id,
                        purpose,
                        revision: *revision,
                        prefix: builder.prefix(),
                        status: rsi_agent_turn_protocol::ModelLiveStatus::Streaming,
                    };
                    transcript.model(&snapshot);
                }
            }
        }
    }
    #[test]
    fn terminal_identity_is_explicit_without_removing_chronological_evidence() {
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        let start = t.seq.to_string();
        apply(&mut t, text("current", "Visible response"));
        assert_eq!(turn(&t).reading_sequence.as_ref(), Some(&start));
        apply(&mut t, terminal(TurnOutcome::Completed));
        let key = rsi_conversation::BlockIdentity::Terminal {
            turn: &TurnId::new("turn").unwrap(),
        }
        .key();
        assert_eq!(turn(&t).terminal.as_deref(), Some(key.as_str()));
        assert_eq!(turn(&t).reading_sequence, Some(t.seq.to_string()));
        assert!(turn(&t).blocks.contains(&key));
    }
    fn turn(transcript: &Transcript) -> &Turn {
        transcript.turns.view.entries.values().next().unwrap()
    }
    #[test]
    fn retry_chain_matches_reserved_effects_and_survives_reverse_history() {
        use rsi_agent_session_protocol::{ModelAttemptBuilder, ScheduledModelRetry};
        use rsi_ai_protocol::{AiError, DispatchStatus, ErrorKind, ErrorPhase};

        let failed = |source: &str, next: &str, number| {
            let mut attempt = ModelAttemptBuilder::new();
            attempt
                .push(
                    0,
                    &LanguageEvent::Failed {
                        error: AiError::new(
                            ErrorKind::RateLimited,
                            ErrorPhase::Stream,
                            DispatchStatus::Dispatched,
                            "rate limited",
                        )
                        .unwrap(),
                        replay: None,
                    },
                )
                .unwrap();
            Fact::ModelAttemptSettled {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new(source).unwrap(),
                purpose: ModelEventPurpose::Conversation,
                attempt: attempt
                    .finish_with_retry(
                        false,
                        Some(ScheduledModelRetry {
                            effect_id: EffectId::new(next).unwrap(),
                            number,
                            delay_ms: 6000,
                        }),
                    )
                    .unwrap(),
            }
        };
        let started = |effect| Fact::ModelStarted {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new(effect).unwrap(),
        };
        let facts: Vec<_> = [
            accepted(),
            failed("original", "first", 1),
            started("unrelated"),
            started("first"),
            failed("first", "second", 2),
            terminal(TurnOutcome::Cancelled),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, body)| SessionFact::new(i as u64 + 1, 1, body).unwrap())
        .collect();
        let mut live = Transcript::default();
        for (i, fact) in facts.iter().enumerate() {
            live.fact(fact);
            if i == 0 {
                continue;
            }
            let [row] = turn(&live).retries.as_slice() else {
                panic!("expected one retry chain");
            };
            assert_eq!(row.number, if i < 4 { 1 } else { 2 });
            assert_eq!(
                row.state,
                match i {
                    3 => RetryState::Started,
                    5 => RetryState::Cancelled,
                    _ => RetryState::Scheduled,
                }
            );
            assert_eq!(
                turn(&live).blocks[row.block],
                r#"["request","turn","original"]"#
            );
        }
        let mut history = Transcript::default();
        for fact in facts.iter().rev() {
            history.fact(fact);
        }
        assert_eq!(turn(&history).retries, turn(&live).retries);
    }
    #[test]
    fn new_turns_and_backfill_only_reclassify_affected_entries() {
        let mut t = Transcript {
            inner: ActualTranscript {
                seq: 100,
                ..ActualTranscript::default()
            },
            ..Transcript::default()
        };
        for i in 0..32 {
            let mut fact = complete(&format!("effect-{i}"), "text");
            if let Fact::ModelAttemptSettled { turn_id, .. } = &mut fact {
                *turn_id = TurnId::new(format!("turn-{i}")).unwrap();
            }
            let before = t.turns.rebuilds;
            apply(&mut t, fact);
            assert_eq!(t.turns.rebuilds - before, 1);
        }
        assert_eq!(t.turns.view.entries.len(), 32);
        // Backfill adds an earlier block to one Turn without rewriting its peers.
        let before = t.turns.rebuilds;
        let old = t.turns.view.entries.clone();
        let mut backfill = complete("earlier", "earlier text");
        if let Fact::ModelAttemptSettled { turn_id, .. } = &mut backfill {
            *turn_id = TurnId::new("turn-20").unwrap();
        }
        t.fact(&SessionFact::new(1, 1, backfill).unwrap());
        assert_eq!(t.turns.rebuilds - before, 1);
        for (id, entry) in old {
            if id.as_str() != "turn-20" {
                assert_eq!(&entry, &t.turns.view.entries[&id]);
            }
        }
    }
    #[test]
    fn streaming_deltas_reuse_classification_at_the_window_limit() {
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        for i in 0..128 {
            apply(&mut t, text(&format!("effect-{i}"), "text"));
        }
        assert_eq!(t.blocks.len(), 128);
        let before = t.turns.rebuilds;
        let view = t.turns.view.clone();
        for _ in 0..200 {
            apply(&mut t, text("effect-127", " delta"));
        }
        assert_eq!(t.turns.view, view);
        assert_eq!(
            t.turns.rebuilds - before,
            0,
            "text does not reclassify Turns"
        );
        apply(&mut t, finish("effect-127", FinishReason::ToolCalls));
        assert!(turn(&t).candidate.is_empty());
        assert_eq!(t.turns.rebuilds - before, 1);
    }
    #[test]
    fn closing_prose_survives_a_tool_only_follow_up_and_waits_for_settlement() {
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        apply(&mut t, text("first", "Earlier reply"));
        apply(&mut t, finish("first", FinishReason::ToolCalls));
        apply(&mut t, text("second", "Reading the workspace"));
        apply(&mut t, finish("second", FinishReason::ToolCalls));
        apply(&mut t, text("third", " "));
        apply(&mut t, finish("third", FinishReason::ToolCalls));
        assert!(turn(&t).closing_prose.is_empty());
        assert!(turn(&t).feedback_message.is_none());
        let seq = t
            .turns
            .states
            .values()
            .next()
            .unwrap()
            .models
            .get(&EffectId::new("second").unwrap())
            .unwrap()
            .finished
            .unwrap()
            .0
            .to_string();
        apply(&mut t, terminal(TurnOutcome::Cancelled));
        assert_eq!(turn(&t).feedback_message.as_ref(), Some(&seq));
        let view = turn(&t);
        assert!(view.answer.is_empty());
        let [position] = view.closing_prose[..] else {
            panic!("one closing prose block")
        };
        assert_eq!(
            t.blocks
                .iter()
                .find(|block| block.key == view.blocks[position])
                .unwrap()
                .text,
            "Reading the workspace"
        );
    }
    #[test]
    fn tool_reclassification_and_completion_do_not_change_block_identity() {
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        apply(&mut t, text("first", "Let me check"));
        let block = t
            .blocks
            .iter()
            .find(|block| block.role == "assistant")
            .unwrap()
            .revision
            .clone();
        let key = t
            .blocks
            .iter()
            .find(|block| block.role == "assistant")
            .unwrap()
            .key
            .clone();
        assert_eq!(turn(&t).candidate, vec![key.clone()]);
        apply(&mut t, finish("first", FinishReason::ToolCalls));
        assert!(turn(&t).candidate.is_empty());
        assert!(!std::sync::Arc::ptr_eq(
            &block,
            &t.blocks
                .iter()
                .find(|block| block.role == "assistant")
                .unwrap()
                .revision
        ));
        let block = t
            .blocks
            .iter()
            .find(|block| block.role == "assistant")
            .unwrap()
            .revision
            .clone();
        apply(
            &mut t,
            Fact::InputMessageClaimed {
                turn_id: TurnId::new("turn").unwrap(),
                step_id: StepId::new("step").unwrap(),
                source: InputMessageSource::Human {
                    attribution: None,
                    message_id: MessageId::new("steer").unwrap(),
                },
                content: vec![AgentMessageContent::Text {
                    text: "focus here".into(),
                }],
            },
        );
        let steering = t
            .blocks
            .iter()
            .find(|b| b.role == "user")
            .unwrap()
            .key
            .clone();
        apply(&mut t, text("second", "Answer"));
        apply(&mut t, finish("second", FinishReason::Stop));
        apply(&mut t, terminal(TurnOutcome::Completed));
        assert_eq!(turn(&t).answer.len(), 1);
        assert!(!turn(&t).process.contains(&steering));
        assert!(turn(&t).process.contains(&key));
        assert!(std::sync::Arc::ptr_eq(
            &block,
            &t.blocks
                .iter()
                .find(|block| block.role == "assistant")
                .unwrap()
                .revision
        ));
        let revision = t.turns.view.revision;
        t.inner.turns.retain(&t.inner.blocks, None);
        assert_eq!(revision, t.turns.view.revision);
    }
    #[test]
    fn partial_and_unsuccessful_windows_do_not_invent_answers_and_eviction_releases_index() {
        for complete in [true, false] {
            let mut t = Transcript::default();
            apply(&mut t, text("first", "Partial"));
            assert_eq!(turn(&t).reading_sequence, None);
            apply(&mut t, finish("first", FinishReason::Stop));
            apply(
                &mut t,
                terminal(if complete {
                    TurnOutcome::Completed
                } else {
                    TurnOutcome::Cancelled
                }),
            );
            assert!(turn(&t).partial);
            assert!(!turn(&t).foldable);
            assert!(turn(&t).answer.is_empty());
            t.blocks.clear();
            t.inner.turns.retain(&t.inner.blocks, None);
            assert!(t.turns.view.entries.is_empty());
            assert!(t.turns.states.is_empty());
        }
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        apply(
            &mut t,
            event(
                "compact",
                LanguageEvent::ContentDelta {
                    index: 0,
                    delta: rsi_ai_protocol::ContentDelta::Text("summary".into()),
                },
                ModelEventPurpose::ContextCompaction,
            ),
        );
        apply(&mut t, terminal(TurnOutcome::Completed));
        assert!(turn(&t).answer.is_empty());
        assert!(turn(&t).process.is_empty());
        for i in 0..140 {
            apply(&mut t, text(&format!("effect-{i}"), "text"));
        }
        assert!(t.blocks.len() <= 128);
        assert!(turn(&t).partial);
        assert!(
            t.turns
                .states
                .values()
                .all(|state| state.links.len() <= 128 && state.models.len() <= 128)
        );
    }

    #[test]
    fn only_the_last_successful_response_before_terminal_can_be_an_answer() {
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        apply(&mut t, text("first", "Intermediate"));
        apply(&mut t, finish("first", FinishReason::Stop));
        let first = t
            .blocks
            .iter()
            .find(|block| block.role == "assistant")
            .unwrap()
            .key
            .clone();
        apply(&mut t, text("second", "Final response"));
        let second = t.blocks.back().unwrap().key.clone();
        apply(&mut t, finish("second", FinishReason::Stop));
        apply(&mut t, terminal(TurnOutcome::Completed));
        assert_eq!(turn(&t).answer, vec![second]);
        assert!(turn(&t).process.contains(&first));

        let mut t = Transcript::default();
        apply(&mut t, accepted());
        apply(&mut t, text("model", "Before a later Tool"));
        apply(&mut t, finish("model", FinishReason::Stop));
        apply(
            &mut t,
            Fact::ToolStarted {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("tool").unwrap(),
                identity: rsi_tools_protocol::ToolResultIdentity::new(
                    "owner",
                    "invocation",
                    "call",
                    "a".repeat(64),
                )
                .unwrap(),
            },
        );
        apply(&mut t, terminal(TurnOutcome::Completed));
        assert!(turn(&t).answer.is_empty());
        assert!(turn(&t).candidate.is_empty());

        let mut t = Transcript::default();
        apply(&mut t, accepted());
        apply(&mut t, terminal(TurnOutcome::Completed));
        assert!(turn(&t).answer.is_empty());
        assert_eq!(turn(&t).status, "Completed");
    }

    fn media() -> rsi_media_protocol::MediaRef {
        serde_json::from_value(serde_json::json!({"id":"d".repeat(64), "mime":"image/png", "bytes":7, "width":1, "height":1})).unwrap()
    }

    #[test]
    fn unsuccessful_complete_windows_keep_content_without_successful_answers() {
        for outcome in [
            TurnOutcome::Cancelled,
            TurnOutcome::Failed {
                code: "fixture".into(),
                message: "provider failed".into(),
            },
            TurnOutcome::PartialFailed {
                media: vec![media()],
                code: "fixture".into(),
                message: "later output failed".into(),
            },
            TurnOutcome::Interrupted {
                effect: None,
                reason: "interrupted".into(),
            },
            TurnOutcome::MaxTokens,
        ] {
            let mut t = Transcript::default();
            apply(&mut t, accepted());
            apply(&mut t, text("model", "Retained response"));
            apply(&mut t, finish("model", FinishReason::Stop));
            let key = t
                .blocks
                .iter()
                .find(|block| block.role == "assistant")
                .unwrap()
                .key
                .clone();
            apply(&mut t, terminal(outcome));
            assert!(!turn(&t).partial);
            assert!(!turn(&t).foldable);
            assert!(!turn(&t).running);
            assert!(turn(&t).answer.is_empty());
            assert!(turn(&t).process.contains(&key));
            assert_eq!(
                t.blocks
                    .iter()
                    .find(|block| block.role == "assistant")
                    .unwrap()
                    .text,
                "Retained response"
            );
        }
    }

    #[test]
    fn interrupted_model_finish_never_becomes_a_successful_answer() {
        for reason in [
            FinishReason::MaxTokens,
            FinishReason::ContentFilter,
            FinishReason::Cancelled,
        ] {
            let mut t = Transcript::default();
            apply(&mut t, accepted());
            apply(&mut t, text("model", "Retained partial response"));
            apply(&mut t, finish("model", reason.clone()));
            apply(&mut t, terminal(TurnOutcome::Completed));
            assert!(
                turn(&t).answer.is_empty(),
                "{reason:?} is not a successful model response"
            );
            assert!(
                !turn(&t).foldable,
                "partial process remains visible: {reason:?}"
            );
            assert_eq!(
                t.blocks
                    .iter()
                    .find(|block| block.role == "assistant")
                    .unwrap()
                    .text,
                "Retained partial response"
            );
            assert!(
                turn(&t).process.contains(
                    &t.blocks
                        .iter()
                        .find(|block| block.role == "assistant")
                        .unwrap()
                        .key
                )
            );
        }
    }

    #[test]
    fn direct_generated_images_are_answers_only_after_success() {
        for successful in [true, false] {
            let mut t = Transcript::default();
            apply(
                &mut t,
                Fact::ImageRequested {
                    turn_id: TurnId::new("turn").unwrap(),
                    model: rsi_ai_protocol::ModelRef::new("fixture", "image").unwrap(),
                    request: rsi_ai_protocol::ImageRequest::new("draw", 1).unwrap(),
                },
            );
            apply(
                &mut t,
                Fact::ImageOutput {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: EffectId::new("image").unwrap(),
                    index: 0,
                    media: media(),
                },
            );
            let key = t.blocks.back().unwrap().key.clone();
            assert_eq!(turn(&t).candidate, vec![key.clone()]);
            apply(
                &mut t,
                terminal(if successful {
                    TurnOutcome::Completed
                } else {
                    TurnOutcome::Cancelled
                }),
            );
            assert_eq!(turn(&t).answer.contains(&key), successful);
            assert!(t.blocks.iter().any(|block| block.key == key));
        }
    }

    #[test]
    fn attributed_inputs_distinguish_step_context_from_turn_triggers() {
        use rsi_agent_session_protocol::{
            ContinuationInput, ContinuationProvenance, ContinuationSource, DomainIdentity,
            DomainRequestId, DomainRevision, SessionId,
        };
        let continuation = ContinuationInput {
            owner: DomainRequestId::new("goal").unwrap(),
            round: 1,
            message_id: MessageId::new("continuation").unwrap(),
            text: "continue".into(),
        };
        let automatic = |domain| InputMessageSource::Continuation {
            message_id: continuation.message_id.clone(),
            source: ContinuationSource {
                domain: DomainIdentity::new(domain, 1).unwrap(),
                owner: continuation.owner.clone(),
                round: 1,
                reserved_revision: DomainRevision::new(1),
                provenance: ContinuationProvenance::Baseline {
                    snapshot_sha256: "a".repeat(64),
                },
                text_sha256: continuation.text_sha256(),
            },
        };
        let change = rsi_agent_session_protocol::CommandMessageSource {
            command: rsi_agent_session_protocol::ContributionId::new("rsi.schedule.delete")
                .unwrap(),
            request_id: DomainRequestId::new("delete").unwrap(),
            invocation_sha256: "f".repeat(64),
            ordinal: 0,
            attribution: None,
        };
        let inputs = [
            (
                InputMessageSource::Command {
                    message_id: change.message_id().unwrap(),
                    source: change,
                },
                "Recorded change",
                "Automation task",
                "step-context",
            ),
            (
                InputMessageSource::RuntimeContext,
                "Workspace location",
                "Runtime context",
                "context",
            ),
            (
                InputMessageSource::Agent {
                    message_id: MessageId::new("agent").unwrap(),
                    source_session_id: SessionId::new("sender").unwrap(),
                },
                "Agent input",
                "Message from sender",
                "trigger-agent",
            ),
            (
                automatic("rsi.goal"),
                continuation.text.as_str(),
                "Goal continuation",
                "trigger-goal",
            ),
            (
                automatic("rsi.schedule"),
                continuation.text.as_str(),
                "Automation task",
                "trigger-schedule",
            ),
            (
                automatic("example"),
                continuation.text.as_str(),
                "Automatic continuation",
                "trigger-request",
            ),
        ];
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        for (source, text, title, role) in inputs {
            apply(
                &mut t,
                Fact::InputMessageClaimed {
                    turn_id: TurnId::new("turn").unwrap(),
                    step_id: StepId::new("step").unwrap(),
                    source,
                    content: vec![AgentMessageContent::Text { text: text.into() }],
                },
            );
            let boundary = t.blocks.back().unwrap();
            assert_eq!(boundary.title, title);
            assert_eq!(boundary.role, role);
            let wire = serde_json::to_value(boundary).unwrap();
            if let Some(kind) = role.strip_prefix("trigger-") {
                assert_eq!(wire["role"], "status");
                assert_eq!(wire["trigger"], kind);
            } else {
                assert!(wire.get("trigger").is_none());
                assert_eq!(wire["role"], "context");
            }
        }
        apply(&mut t, text("model", "Answer"));
        apply(&mut t, finish("model", FinishReason::Stop));
        apply(&mut t, terminal(TurnOutcome::Completed));
        for block in t.blocks.iter().filter(|block| {
            block.role.starts_with("trigger-") || matches!(block.role, "context" | "step-context")
        }) {
            assert!(turn(&t).blocks.contains(&block.key));
            assert_eq!(
                turn(&t).process.contains(&block.key),
                block.role == "step-context"
            );
            assert!(!turn(&t).answer.contains(&block.key));
        }
    }
    fn response_with_usage(effect: &str, usage: TokenUsage) -> Fact {
        let mut response = complete(effect, "Hello");
        if let Fact::ModelAttemptSettled { attempt, .. } = &mut response {
            let mut events: Vec<_> = attempt.events().map(Result::unwrap).collect();
            events.insert(events.len() - 1, (0, LanguageEvent::Usage { usage }));
            *attempt =
                rsi_agent_session_protocol::ModelAttemptLog::from_events(events, true).unwrap();
        }
        response
    }

    #[test]
    fn recorded_clocks_and_complete_turn_usage_survive_replay() {
        let mut transcript = ActualTranscript::default();
        transcript.fact(&SessionFact::new(1, 1_000, accepted()).unwrap());
        let first = response_with_usage("step", TokenUsage::new(3, 1, None, None, None).unwrap());
        transcript.fact(&SessionFact::new(2, 2_000, first).unwrap());
        let response = response_with_usage(
            "answer",
            TokenUsage::new(100, 20, Some(80), None, None).unwrap(),
        );
        let fact = SessionFact::new(3, 2_900, response).unwrap();
        transcript.fact(&fact);
        assert!(
            transcript.turns.view.entries[&TurnId::new("turn").unwrap()]
                .usage
                .is_none()
        );
        let end = SessionFact::new(4, 3_000, terminal(TurnOutcome::Completed)).unwrap();
        transcript.fact(&end);
        let turn = &transcript.turns.view.entries[&TurnId::new("turn").unwrap()];
        assert_eq!(turn.elapsed_ms, Some(2_000));
        assert_eq!(turn.timestamps[&turn.answer[0]], 2_900);
        assert_eq!(
            turn.usage,
            Some(TokenUsage::new(103, 21, None, None, None).unwrap())
        );
        let previous = turn.clone();
        transcript.fact(&fact);
        assert_eq!(
            transcript.turns.view.entries[&TurnId::new("turn").unwrap()],
            previous
        );
        let mut partial = ActualTranscript::default();
        partial.fact(&fact);
        partial.fact(&end);
        let turn = &partial.turns.view.entries[&TurnId::new("turn").unwrap()];
        assert!(turn.elapsed_ms.is_none() && turn.usage.is_none());
    }

    #[test]
    fn turn_usage_counts_only_conversation_attempts_and_requires_every_reading() {
        let reading = TokenUsage::new(3, 1, Some(0), None, None).unwrap();
        let mut summary = complete("summary", "checkpoint");
        let Fact::ModelAttemptSettled { purpose, .. } = &mut summary else {
            unreachable!()
        };
        *purpose = ModelEventPurpose::ContextCompaction;
        for (prior, expected) in [
            (summary, Some(reading)),
            (complete("retry", "unmetered"), None),
            (
                response_with_usage(
                    "large",
                    TokenUsage::new(u64::MAX, 0, None, None, None).unwrap(),
                ),
                None,
            ),
        ] {
            let mut t = Transcript::default();
            apply(&mut t, accepted());
            apply(&mut t, prior);
            apply(&mut t, response_with_usage("answer", reading));
            apply(&mut t, terminal(TurnOutcome::Completed));
            assert_eq!(turn(&t).usage, expected);
        }
    }

    #[test]
    fn turn_usage_lists_unique_prepared_routes_in_step_order_after_backfill() {
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        let reading = TokenUsage::new(3, 1, None, None, None).unwrap();
        let mut inputs = Vec::new();
        for (i, (effect, deployment)) in [("a", "work"), ("b", "work"), ("c", "backup")]
            .iter()
            .enumerate()
        {
            let seq = 2 + 2 * i as u64;
            inputs.push(
                SessionFact::new(
                    seq,
                    seq,
                    Fact::ModelIntent {
                        turn_id: TurnId::new("turn").unwrap(),
                        effect_id: EffectId::new(*effect).unwrap(),
                        snapshot: rsi_ai_protocol::PreparedCallSnapshot {
                            call_id: effect.to_string(),
                            deployment_id: deployment.to_string(),
                            provider_family: "test".into(),
                            capability: rsi_ai_protocol::AiCapability::Language,
                            model: "model".into(),
                            protocol: "chat".into(),
                            transport: "http".into(),
                            endpoint_fingerprint: "local".into(),
                            config_generation: 1,
                            credential_source: None,
                            retry_policy: Default::default(),
                            request_sha256: "0".repeat(64),
                            language_settings: None,
                        },
                        purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
                        price_quote: None,
                        evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                            reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
                        },
                    },
                )
                .unwrap(),
            );
            t.fact(
                &SessionFact::new(seq + 1, seq + 1, response_with_usage(effect, reading)).unwrap(),
            );
        }
        apply(&mut t, terminal(TurnOutcome::Completed));
        for intent in inputs.iter().rev() {
            assert!(turn(&t).usage_routes.is_empty());
            t.fact(intent);
        }
        assert_eq!(turn(&t).usage.unwrap().input_tokens(), 9);
        assert_eq!(
            turn(&t)
                .usage_routes
                .iter()
                .map(ModelRef::deployment)
                .collect::<Vec<_>>(),
            ["work", "backup"]
        );
    }
}
