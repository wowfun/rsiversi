//! Bounded semantic model blocks shared by durable and live renderers.
use crate::{FactField, FieldWindow};
use rsi_ai_protocol::{ContentDelta, ContentStart, LanguageEvent};
use std::collections::BTreeMap;

/// One original model content block with a bounded leading display window.
#[derive(Clone, Debug)]
pub struct ModelBlockPreview {
    /// Original provider-neutral content index.
    pub index: u16,
    /// Stable source field within the single settlement Fact.
    pub field: FactField,
    /// Bounded safe UTF-8 source window.
    pub window: FieldWindow,
    /// Original first-block offset in milliseconds.
    pub started_ms: u64,
    /// Last known closing offset, including interrupted terminal closure.
    pub ended_ms: Option<u64>,
}
/// Finite retained preview independent of fragment count.
#[derive(Clone, Debug)]
pub struct ModelPreviewState {
    blocks: BTreeMap<u16, ModelBlockPreview>,
    maximum_block: usize,
    remaining: usize,
}
impl ModelPreviewState {
    /// Limits each block and the aggregate retained text independently.
    pub fn new(maximum_block: usize, maximum_total: usize) -> Self {
        Self {
            blocks: BTreeMap::new(),
            maximum_block: maximum_block.min(crate::MAXIMUM_WINDOW_BYTES),
            remaining: maximum_total,
        }
    }
    /// Reduces one validated normalized event without retaining original fragments.
    pub fn push(&mut self, elapsed: u64, event: &LanguageEvent) {
        match event {
            LanguageEvent::ContentStarted { index, content } => {
                let Ok(index) = u16::try_from(*index) else {
                    return;
                };
                let field = match content {
                    ContentStart::Text => FactField::ModelText { index },
                    ContentStart::Reasoning => FactField::ModelReasoning { index },
                    ContentStart::ToolCall { .. } => return,
                };
                self.blocks
                    .entry(index)
                    .or_insert_with(|| ModelBlockPreview {
                        index,
                        field,
                        window: FieldWindow {
                            text: String::new(),
                            start: 0,
                            end: 0,
                            more: false,
                        },
                        started_ms: elapsed,
                        ended_ms: None,
                    });
            }
            LanguageEvent::ContentDelta {
                index,
                delta: ContentDelta::Text(text) | ContentDelta::Reasoning(text),
            } => {
                let Ok(index) = u16::try_from(*index) else {
                    return;
                };
                if let Some(block) = self.blocks.get_mut(&index)
                    && !block.window.more
                {
                    let take = self
                        .remaining
                        .min(self.maximum_block.saturating_sub(block.window.text.len()))
                        .min(text.len());
                    let take = text.floor_char_boundary(take);
                    block.window.text.push_str(&text[..take]);
                    block.window.end += take;
                    self.remaining -= take;
                    block.window.more = take < text.len();
                }
            }
            LanguageEvent::ContentFinished { index } => {
                if let Ok(index) = u16::try_from(*index)
                    && let Some(block) = self.blocks.get_mut(&index)
                {
                    block.ended_ms = Some(elapsed);
                }
            }
            LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. } => {
                for block in self.blocks.values_mut() {
                    block.ended_ms.get_or_insert(elapsed);
                }
            }
            _ => {}
        }
    }
    /// Ordered content blocks, with no whole-attempt text duplication.
    pub fn blocks(&self) -> impl Iterator<Item = &ModelBlockPreview> {
        self.blocks.values()
    }
}
/// Projects one durable compact attempt with the same reducer as live output.
pub fn model_previews(
    attempt: &rsi_agent_session_protocol::ModelAttemptLog,
    maximum_block: usize,
    maximum_total: usize,
) -> ModelPreviewState {
    let mut state = ModelPreviewState::new(maximum_block, maximum_total);
    for (elapsed, event) in attempt.events().filter_map(Result::ok) {
        state.push(elapsed, &event);
    }
    state
}

/// One admitted projection and one bounded candidate awaiting its durable start.
/// The original prefixes are never retained.
#[derive(Clone, Debug, Default)]
pub struct LiveModelPreview {
    current: Option<LivePreview>,
    pending: Option<LivePreview>,
    expected: Option<rsi_agent_session_protocol::EffectId>,
    fact_seq: u64,
}
#[derive(Clone, Debug)]
struct LivePreview {
    turn: rsi_agent_session_protocol::TurnId,
    effect: rsi_agent_session_protocol::EffectId,
    purpose: rsi_agent_session_protocol::ModelEventPurpose,
    revision: u64,
    cursor: rsi_agent_session_protocol::ModelEventCursor,
    state: ModelPreviewState,
}
impl LiveModelPreview {
    /// Admits only the newest durable started attempt; history backfills are inert.
    /// Returns whether an already received candidate became visible.
    pub fn started(&mut self, seq: u64, effect: &rsi_agent_session_protocol::EffectId) -> bool {
        if seq > self.fact_seq {
            self.fact_seq = seq;
            self.expected = Some(effect.clone());
            if self
                .pending
                .as_ref()
                .is_some_and(|pending| &pending.effect == effect)
            {
                self.current = self.pending.take();
                return true;
            }
        }
        false
    }
    /// Incrementally applies unseen original events, including after slow-reader resynchronization.
    ///
    /// # Errors
    /// Returns a decoding error if the retained event cursor cannot read the snapshot.
    pub fn update(
        &mut self,
        snapshot: &rsi_agent_turn_protocol::ModelLiveSnapshot,
        maximum_block: usize,
        maximum_total: usize,
    ) -> rsi_agent_session_protocol::Result<bool> {
        use rsi_agent_turn_protocol::ModelLiveStatus;
        let admitted = self.expected.as_ref() == Some(&snapshot.effect_id);
        let slot = if admitted {
            &mut self.current
        } else {
            &mut self.pending
        };
        if matches!(snapshot.status, ModelLiveStatus::Abandoned) {
            let removed = slot
                .as_ref()
                .is_some_and(|value| value.effect == snapshot.effect_id);
            if removed {
                *slot = None;
            }
            return Ok(admitted && removed);
        }
        if matches!(snapshot.status, ModelLiveStatus::Committed { .. }) {
            if self
                .pending
                .as_ref()
                .is_some_and(|value| value.effect == snapshot.effect_id)
            {
                self.pending = None;
            }
            return Ok(false);
        }
        if slot
            .as_ref()
            .is_none_or(|current| current.effect != snapshot.effect_id)
        {
            *slot = Some(LivePreview {
                turn: snapshot.turn_id.clone(),
                effect: snapshot.effect_id.clone(),
                purpose: snapshot.purpose,
                revision: 0,
                cursor: rsi_agent_session_protocol::ModelEventCursor::default(),
                state: ModelPreviewState::new(maximum_block, maximum_total),
            });
        }
        let Some(current) = slot.as_mut() else {
            return Ok(false);
        };
        if snapshot.revision <= current.revision {
            return Ok(false);
        }
        for event in snapshot.prefix.events_after(&mut current.cursor) {
            let (elapsed, event) = event?;
            current.state.push(elapsed, &event);
        }
        current.revision = snapshot.revision;
        Ok(admitted)
    }
    /// Removes only the matching transient projection once its durable Fact is available.
    pub fn settle(&mut self, seq: u64, effect: &rsi_agent_session_protocol::EffectId) {
        if seq > self.fact_seq {
            self.fact_seq = seq;
            self.expected = None;
        }
        if self
            .current
            .as_ref()
            .is_some_and(|current| &current.effect == effect)
        {
            self.current = None;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| &pending.effect == effect)
        {
            self.pending = None;
        }
    }
    /// Current transient content, with no durable provenance.
    pub fn view(
        &self,
    ) -> Option<(
        &rsi_agent_session_protocol::TurnId,
        &rsi_agent_session_protocol::EffectId,
        rsi_agent_session_protocol::ModelEventPurpose,
        &ModelPreviewState,
    )> {
        self.current.as_ref().map(|current| {
            (
                &current.turn,
                &current.effect,
                current.purpose,
                &current.state,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        EffectId, ModelAttemptBuilder, ModelEventPurpose, SessionId, TurnId,
    };
    use rsi_agent_turn_protocol::{ModelLiveSnapshot, ModelLiveStatus};

    fn snapshot(effect: &str, text: &str) -> ModelLiveSnapshot {
        let mut builder = ModelAttemptBuilder::new();
        builder
            .push(
                0,
                &LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            )
            .unwrap();
        builder
            .push(
                1,
                &LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text(text.into()),
                },
            )
            .unwrap();
        ModelLiveSnapshot {
            session_id: SessionId::new("session").unwrap(),
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new(effect).unwrap(),
            purpose: ModelEventPurpose::Conversation,
            revision: 1,
            prefix: builder.prefix(),
            status: ModelLiveStatus::Streaming,
        }
    }

    #[test]
    fn prefix_before_started_is_visible_without_another_model_delta() {
        let a = snapshot("a", "Waiting for native fixture release.");
        let mut live = LiveModelPreview::default();
        assert!(!live.update(&a, 1024, 2048).unwrap());
        assert!(live.view().is_none());
        live.started(2, &a.effect_id);
        assert_eq!(
            live.view().unwrap().3.blocks().next().unwrap().window.text,
            "Waiting for native fixture release."
        );
    }

    #[test]
    fn pending_preview_is_bounded_replaced_and_retired_before_admission() {
        let a = snapshot("a", "obsolete");
        let mut b = snapshot("b", &"界🦀".repeat(200));
        let mut live = LiveModelPreview::default();
        live.update(&a, 10, 10).unwrap();
        live.update(&b, 10, 10).unwrap();
        assert!(!live.started(1, &a.effect_id));
        assert!(live.view().is_none());
        assert!(live.started(2, &b.effect_id));
        assert_eq!(
            live.view().unwrap().3.blocks().next().unwrap().window.text,
            "界🦀界"
        );
        live.settle(3, &b.effect_id);
        for status in [
            ModelLiveStatus::Abandoned,
            ModelLiveStatus::Committed { fact_seq: 6 },
        ] {
            let mut candidate = LiveModelPreview::default();
            b.status = ModelLiveStatus::Streaming;
            candidate.update(&b, 10, 10).unwrap();
            b.status = status;
            candidate.update(&b, 10, 10).unwrap();
            assert!(!candidate.started(2, &b.effect_id));
            assert!(candidate.view().is_none());
        }
    }

    #[test]
    fn durable_admission_prevents_cross_plane_duplicates_and_history_rewinds() {
        let a = snapshot("a", "界🦀");
        let b = snapshot("b", "second");
        let mut live = LiveModelPreview::default();
        assert!(!live.update(&a, 1024, 2048).unwrap());
        assert!(live.started(2, &a.effect_id));
        assert!(!live.update(&a, 1024, 2048).unwrap());
        live.settle(3, &a.effect_id);
        live.started(5, &b.effect_id);
        assert!(live.update(&b, 1024, 2048).unwrap());
        live.started(2, &a.effect_id);
        live.settle(3, &a.effect_id);
        assert!(!live.update(&a, 1024, 2048).unwrap());
        assert_eq!(
            live.view().unwrap().3.blocks().next().unwrap().window.text,
            "second"
        );
        live.settle(6, &b.effect_id);
        assert!(!live.update(&b, 1024, 2048).unwrap());
        assert!(live.view().is_none());
    }

    #[test]
    fn slow_reader_resync_bounds_unicode_and_abandonment_clears_transient_output() {
        let mut value = snapshot("a", &"界🦀".repeat(200));
        let mut live = LiveModelPreview::default();
        live.started(2, &value.effect_id);
        assert!(live.update(&value, 10, 10).unwrap());
        let block = live.view().unwrap().3.blocks().next().unwrap();
        assert_eq!(block.window.text, "界🦀界");
        assert!(block.window.more);
        value.status = ModelLiveStatus::Abandoned;
        assert!(live.update(&value, 10, 10).unwrap());
        assert!(live.view().is_none());
    }
}
