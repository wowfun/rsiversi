//! Window-owned classification, independent of block content revisions.
use super::Block;
use rsi_agent_session_protocol::{
    EffectId, ModelEventPurpose, SessionFact, SessionFactBody as Fact, TurnId, TurnOutcome,
};
use rsi_ai_protocol::{ContentStart, FinishReason, LanguageEvent, TokenUsage};
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
    pub terminal: Option<String>,
    pub timestamps: BTreeMap<String, u64>,
    pub elapsed_ms: Option<u64>,
    pub usage: Option<TokenUsage>,
    pub partial: bool,
    pub status: String,
    pub running: bool,
    pub foldable: bool,
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
    conversation: bool,
    tools: bool,
    finished: Option<(u64, bool)>,
    usage: Option<TokenUsage>,
}
#[derive(Clone, Debug, Default)]
struct State {
    accepted: bool,
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
        let id = fact.body().turn_id().clone();
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
                    model.conversation = *purpose == ModelEventPurpose::Conversation;
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
                        LanguageEvent::Usage { usage } => model.usage = Some(*usage),
                        _ => {}
                    }
                }
            }
            changed |= *model != previous;
        }
        match fact.body() {
            Fact::TurnAccepted { .. }
            | Fact::MessageTurnAccepted { .. }
            | Fact::ImageRequested { .. } => {
                changed |= !state.accepted;
                state.accepted = true;
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
    fn project(&self, id: &TurnId, blocks: &VecDeque<Block>) -> Turn {
        let partial = !self.accepted || self.lost_start;
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
            terminal: None,
            timestamps: self
                .links
                .iter()
                .filter_map(|(key, link)| link.timestamp_ms.map(|time| (key.clone(), time)))
                .collect(),
            elapsed_ms: (!partial)
                .then_some(self.started_ms.zip(self.ended_ms))
                .flatten()
                .and_then(|(start, end)| end.checked_sub(start)),
            usage: latest.and_then(|(_, model)| model.usage),
            partial,
            running: self.terminal.is_none(),
            foldable: !partial && (self.terminal.is_none() || (successful && !interrupted_model)),
            status: self
                .terminal
                .as_ref()
                .map_or("Running", |(_, _, label)| label)
                .into(),
        };
        for block in blocks {
            let Some(link) = self.links.get(&block.key) else {
                continue;
            };
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
                turn.answer.push(block.key.clone());
            } else if candidate {
                turn.candidate.push(block.key.clone());
            }
            let compaction = link
                .effect
                .as_ref()
                .and_then(|effect| self.models.get(effect))
                .is_some_and(|model| !model.conversation);
            if matches!(link.role, "tool" | "reasoning" | "metadata" | "assistant")
                && !answer
                && !compaction
            {
                turn.process.push(block.key.clone());
            }
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
                transcript.fact(&SessionFact::new(seq, 1, *body).unwrap());
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
        apply(&mut t, terminal(TurnOutcome::Completed));
        let key = rsi_conversation::BlockIdentity::Terminal {
            turn: &TurnId::new("turn").unwrap(),
        }
        .key();
        assert_eq!(turn(&t).terminal.as_deref(), Some(key.as_str()));
        assert!(turn(&t).blocks.contains(&key));
    }
    fn turn(transcript: &Transcript) -> &Turn {
        transcript.turns.view.entries.values().next().unwrap()
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
            Fact::InputMessageEntered {
                turn_id: TurnId::new("turn").unwrap(),
                step_id: StepId::new("step").unwrap(),
                source: InputMessageSource::Human {
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
    fn agent_and_continuation_inputs_remain_outside_folded_process() {
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
        let inputs = [
            (
                InputMessageSource::Agent {
                    message_id: MessageId::new("agent").unwrap(),
                    source_session_id: SessionId::new("sender").unwrap(),
                },
                "Agent input",
                "Message from sender",
            ),
            (
                InputMessageSource::Continuation {
                    message_id: continuation.message_id.clone(),
                    source: ContinuationSource {
                        domain: DomainIdentity::new("goal", 1).unwrap(),
                        owner: continuation.owner.clone(),
                        round: 1,
                        reserved_revision: DomainRevision::new(1),
                        provenance: ContinuationProvenance::Baseline {
                            snapshot_sha256: "a".repeat(64),
                        },
                        text_sha256: continuation.text_sha256(),
                    },
                },
                continuation.text.as_str(),
                "Goal continuation",
            ),
        ];
        let mut t = Transcript::default();
        apply(&mut t, accepted());
        for (source, text, title) in inputs {
            apply(
                &mut t,
                Fact::InputMessageEntered {
                    turn_id: TurnId::new("turn").unwrap(),
                    step_id: StepId::new("step").unwrap(),
                    source,
                    content: vec![AgentMessageContent::Text { text: text.into() }],
                },
            );
            let boundary = t.blocks.back().unwrap();
            assert_eq!(boundary.title, title);
            assert_eq!(boundary.role, "status");
        }
        apply(&mut t, text("model", "Answer"));
        apply(&mut t, finish("model", FinishReason::Stop));
        apply(&mut t, terminal(TurnOutcome::Completed));
        for block in t.blocks.iter().filter(|block| block.role == "status") {
            assert!(turn(&t).blocks.contains(&block.key));
            assert!(!turn(&t).process.contains(&block.key));
            assert!(!turn(&t).answer.contains(&block.key));
        }
    }
    #[test]
    fn recorded_clocks_duration_and_response_usage_survive_replay() {
        let mut transcript = ActualTranscript::default();
        transcript.fact(&SessionFact::new(1, 1_000, accepted()).unwrap());
        let mut response = complete("answer", "Hello");
        if let Fact::ModelAttemptSettled { attempt, .. } = &mut response {
            let mut events: Vec<_> = attempt.events().map(Result::unwrap).collect();
            events.insert(
                events.len() - 1,
                (
                    0,
                    LanguageEvent::Usage {
                        usage: TokenUsage::new(100, 20, Some(80), None, None).unwrap(),
                    },
                ),
            );
            *attempt =
                rsi_agent_session_protocol::ModelAttemptLog::from_events(events, true).unwrap();
        }
        let fact = SessionFact::new(2, 2_900, response).unwrap();
        transcript.fact(&fact);
        transcript.fact(&SessionFact::new(3, 3_000, terminal(TurnOutcome::Completed)).unwrap());
        let turn = &transcript.turns.view.entries[&TurnId::new("turn").unwrap()];
        assert_eq!(turn.elapsed_ms, Some(2_000));
        assert_eq!(turn.timestamps[&turn.answer[0]], 2_900);
        assert_eq!(turn.usage.unwrap().input_tokens(), 100);
        let previous = turn.clone();
        transcript.fact(&fact);
        assert_eq!(
            transcript.turns.view.entries[&TurnId::new("turn").unwrap()],
            previous
        );
        let mut partial = ActualTranscript::default();
        partial.fact(&fact);
        partial.fact(&SessionFact::new(3, 3_000, terminal(TurnOutcome::Completed)).unwrap());
        assert_eq!(
            partial.turns.view.entries[&TurnId::new("turn").unwrap()].elapsed_ms,
            None
        );
    }
}
