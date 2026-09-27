//! Window-owned classification, independent of block content revisions.
use super::Block;
use rsi_agent_session_protocol::{
    EffectId, ModelEventPurpose, SessionFact, SessionFactBody as Fact, TurnId, TurnOutcome,
};
use rsi_ai_protocol::{ContentStart, FinishReason, LanguageEvent};
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
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Model {
    seq: u64,
    conversation: bool,
    tools: bool,
    finished: Option<(u64, bool)>,
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
    pub fn observe(&mut self, fact: &SessionFact, blocks: &VecDeque<Block>) {
        let id = fact.body().turn_id().clone();
        let state = self.states.entry(id.clone()).or_default();
        let mut changed = false;
        let effect = match fact.body() {
            Fact::ModelIntent { effect_id, .. }
            | Fact::ModelStarted { effect_id, .. }
            | Fact::ModelEvent { effect_id, .. } => Some(effect_id.clone()),
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
            if let Fact::ModelEvent { purpose, event, .. } = fact.body() {
                model.conversation = *purpose == ModelEventPurpose::Conversation;
                match event {
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
            changed |= *model != previous;
        }
        match fact.body() {
            Fact::TurnAccepted { .. }
            | Fact::MessageTurnAccepted { .. }
            | Fact::ImageRequested { .. } => {
                changed |= !state.accepted;
                state.accepted = true;
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
                    TurnOutcome::BudgetExceeded { .. } => "Budget exceeded",
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
            if matches!(link.role, "tool" | "reasoning" | "metadata" | "assistant") && !answer {
                turn.process.push(block.key.clone());
            }
        }
        turn
    }
}

#[cfg(test)]
mod tests {
    use super::super::Transcript;
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
    fn event(effect: &str, event: LanguageEvent, purpose: ModelEventPurpose) -> Fact {
        Fact::ModelEvent {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new(effect).unwrap(),
            event,
            purpose,
        }
    }
    fn text(effect: &str, text: &str) -> Fact {
        event(
            effect,
            LanguageEvent::ContentDelta {
                index: 0,
                delta: rsi_ai_protocol::ContentDelta::Text(text.into()),
            },
            ModelEventPurpose::Conversation,
        )
    }
    fn finish(effect: &str, reason: FinishReason) -> Fact {
        event(
            effect,
            LanguageEvent::Finished {
                reason,
                replay: None,
            },
            ModelEventPurpose::Conversation,
        )
    }
    fn terminal(outcome: TurnOutcome) -> Fact {
        Fact::TurnTerminal {
            turn_id: TurnId::new("turn").unwrap(),
            outcome,
            result: None,
        }
    }
    fn apply(transcript: &mut Transcript, body: Fact) {
        transcript.fact(&SessionFact::new(transcript.seq + 1, 1, body).unwrap());
    }
    fn turn(transcript: &Transcript) -> &Turn {
        transcript.turns.view.entries.values().next().unwrap()
    }
    #[test]
    fn new_turns_and_backfill_only_reclassify_affected_entries() {
        let mut t = Transcript {
            seq: 100,
            ..Transcript::default()
        };
        for i in 0..32 {
            let mut fact = text(&format!("effect-{i}"), "text");
            if let Fact::ModelEvent { turn_id, .. } = &mut fact {
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
        let mut backfill = text("earlier", "earlier text");
        if let Fact::ModelEvent { turn_id, .. } = &mut backfill {
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
        let block = t.blocks[0].revision.clone();
        let key = t.blocks[0].key.clone();
        assert_eq!(turn(&t).candidate, vec![key.clone()]);
        apply(&mut t, finish("first", FinishReason::ToolCalls));
        assert!(turn(&t).candidate.is_empty());
        assert!(std::sync::Arc::ptr_eq(&block, &t.blocks[0].revision));
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
        assert!(std::sync::Arc::ptr_eq(&block, &t.blocks[0].revision));
        let revision = t.turns.view.revision;
        t.turns.retain(&t.blocks, None);
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
            t.turns.retain(&t.blocks, None);
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
        let first = t.blocks[0].key.clone();
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
            TurnOutcome::BudgetExceeded {
                dimension: rsi_agent_session_protocol::BudgetDimension::ProviderAttempts,
                consumed: 1,
                limit: 1,
            },
        ] {
            let mut t = Transcript::default();
            apply(&mut t, accepted());
            apply(&mut t, text("model", "Retained response"));
            apply(&mut t, finish("model", FinishReason::Stop));
            let key = t.blocks[0].key.clone();
            apply(&mut t, terminal(outcome));
            assert!(!turn(&t).partial);
            assert!(!turn(&t).foldable);
            assert!(!turn(&t).running);
            assert!(turn(&t).answer.is_empty());
            assert!(turn(&t).process.contains(&key));
            assert_eq!(t.blocks[0].text, "Retained response");
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
            assert_eq!(t.blocks[0].text, "Retained partial response");
            assert!(turn(&t).process.contains(&t.blocks[0].key));
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
}
