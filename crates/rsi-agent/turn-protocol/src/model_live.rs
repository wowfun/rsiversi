//! Independent, bounded live model observation.
use crate::{EffectId, ObservedModel, Result, SessionId, TurnId};
use futures_util::Stream;
use rsi_agent_session_protocol::{ModelAttemptPrefix, ModelEventPurpose, ModelPrefixDecoder};
use serde::{Deserialize, Serialize};
use std::{fmt, pin::Pin};

/// Terminal status of a transient attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelLiveStatus {
    /// Prefix may still grow.
    Streaming,
    /// Exact durable settlement sequence.
    Committed {
        /// Sequence after successful Store commit.
        fact_seq: u64,
    },
    /// Owner disappeared without a committed settlement.
    Abandoned,
}
/// Stable complete prefix at one revision, independent of durable observation cursors.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelLiveSnapshot {
    /// Exact Session.
    pub session_id: SessionId,
    /// Exact Turn.
    pub turn_id: TurnId,
    /// Exact attempt.
    pub effect_id: EffectId,
    /// Projection scope.
    pub purpose: ModelEventPurpose,
    /// Monotonic revision within this effect.
    pub revision: u64,
    /// Original events and timing.
    pub prefix: ModelAttemptPrefix,
    /// Durability or abandonment status.
    pub status: ModelLiveStatus,
}
impl ModelLiveSnapshot {
    /// Conservative encoded payload credit, including metadata.
    pub fn encoded_len(&self) -> usize {
        self.prefix.encoded_len() + 4096
    }
}
/// Coalesced model stream; slow readers resynchronize to a complete immutable prefix.
pub type ModelLiveStream = Pin<Box<dyn Stream<Item = Result<ObservedModel>> + Send + 'static>>;
/// Kernel admission retained throughout accumulation, validation and settlement.
pub trait ModelAttemptLease: fmt::Debug + Send {
    /// Activates the clock after the exact `ModelStarted` has been committed.
    fn started(&mut self) -> Result<()>;
    /// Monotonic elapsed time derived from the Kernel's injected clock.
    fn elapsed_ms(&self) -> u64;
    /// Publishes the latest prefix, without affecting durable cursors.
    fn publish(&mut self, prefix: ModelAttemptPrefix) -> Result<()>;
    /// Verifies a sealed accepted settlement against the resident durable watermark.
    /// Ensure the settlement sequence is durable before calling, for example by
    /// awaiting [`TurnExecution::flush`](crate::TurnExecution::flush). This method
    /// neither flushes nor waits; an accepted but undurable settlement is rejected
    /// without committing the lease.
    fn committed(&mut self, receipt: &crate::PublishedFacts) -> Result<()>;
}

/// One bounded page from a stable live snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLivePage {
    /// Exact Session.
    pub session_id: SessionId,
    /// Exact Turn.
    pub turn_id: TurnId,
    /// Exact attempt.
    pub effect_id: EffectId,
    /// Projection scope.
    pub purpose: ModelEventPurpose,
    /// Immutable snapshot identity within the attempt.
    pub revision: u64,
    /// Zero-based page offset.
    pub index: usize,
    /// Exact raw prefix byte length before this revision's delta.
    pub offset: usize,
    /// Number of pages, including an empty status-only snapshot's single page.
    pub count: usize,
    /// Number of original normalized events in the snapshot.
    pub events: u64,
    /// Complete canonical payload size reserved before decoding.
    pub encoded_bytes: usize,
    /// Compact base64 suffix bytes; empty when this revision adds no bytes.
    pub data: String,
    /// Snapshot status.
    pub status: ModelLiveStatus,
}
impl ModelLiveSnapshot {
    /// Splits a frozen prefix, leaving framing headroom within 256 KiB.
    pub fn pages(&self) -> impl Iterator<Item = ModelLivePage> + '_ {
        self.pages_from(0)
    }
    fn pages_from(&self, offset: usize) -> impl Iterator<Item = ModelLivePage> + '_ {
        let count = self.prefix.page_count_after(offset).max(1);
        let mut pages = self.prefix.pages_after(offset);
        (0..count).map(move |index| ModelLivePage {
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone(),
            effect_id: self.effect_id.clone(),
            purpose: self.purpose,
            revision: self.revision,
            index,
            offset,
            count,
            events: self.prefix.event_count(),
            encoded_bytes: self.encoded_len(),
            data: pages.next().unwrap_or_default(),
            status: self.status.clone(),
        })
    }
}
/// Per-connection delta cursor; stores identity and byte count, never accumulated payload.
#[derive(Debug, Default)]
pub struct ModelLiveEncoder {
    last: Option<(SessionId, TurnId, EffectId, ModelEventPurpose, usize)>,
}
impl ModelLiveEncoder {
    /// Emits an initial prefix or only bytes appended since the last delivered revision.
    pub fn pages<'a>(
        &mut self,
        snapshot: &'a ModelLiveSnapshot,
    ) -> impl Iterator<Item = ModelLivePage> + 'a {
        let identity = (
            snapshot.session_id.clone(),
            snapshot.turn_id.clone(),
            snapshot.effect_id.clone(),
            snapshot.purpose,
        );
        let raw = snapshot.prefix.raw_len();
        let offset = self
            .last
            .as_ref()
            .filter(|(session, turn, effect, purpose, length)| {
                (&identity.0, &identity.1, &identity.2, identity.3)
                    == (session, turn, effect, *purpose)
                    && *length <= raw
            })
            .map_or(0, |last| last.4);
        self.last = Some((identity.0, identity.1, identity.2, identity.3, raw));
        snapshot.pages_from(offset)
    }
}
/// One bounded, ordered snapshot decode; incomplete EOF is a protocol failure.
#[derive(Debug, Default)]
pub struct ModelLiveDecoder {
    header: Option<ModelLivePage>,
    pages: usize,
    bytes: usize,
    prefix: ModelPrefixDecoder,
    last: Option<(
        SessionId,
        TurnId,
        EffectId,
        ModelEventPurpose,
        u64,
        ModelLiveStatus,
    )>,
}
impl ModelLiveDecoder {
    /// Receives one page without mixing effects or revisions.
    pub fn push(&mut self, mut page: ModelLivePage) -> Result<Option<ModelLiveSnapshot>> {
        let invalid = || crate::TurnError::Invalid("invalid live model snapshot pages".into());
        if page.revision == 0
            || page.count == 0
            || page.count > 256
            || page.index != self.pages
            || page.data.len() > 192 * 1024
            || page.encoded_bytes > 32 * 1024 * 1024 + 4096
            || matches!(page.status, ModelLiveStatus::Committed { fact_seq: 0 })
        {
            return Err(invalid());
        }
        if page.index == 0 {
            let same = self
                .last
                .as_ref()
                .is_some_and(|(session, turn, effect, purpose, _, _)| {
                    (session, turn, effect, *purpose)
                        == (
                            &page.session_id,
                            &page.turn_id,
                            &page.effect_id,
                            page.purpose,
                        )
                });
            if same
                && self.last.as_ref().is_some_and(|last| {
                    last.4 >= page.revision || last.5 != ModelLiveStatus::Streaming
                })
            {
                return Err(invalid());
            }
            let reset = !same || (page.events == 0 && page.status != ModelLiveStatus::Streaming);
            if reset {
                if page.offset != 0 {
                    return Err(invalid());
                }
                self.prefix = ModelPrefixDecoder::default();
            } else if page.offset != self.prefix.raw_len() {
                return Err(invalid());
            }
        }
        self.bytes = self
            .bytes
            .checked_add(page.data.len())
            .ok_or_else(invalid)?;
        if self.bytes > page.encoded_bytes {
            return Err(invalid());
        }
        let data = std::mem::take(&mut page.data);
        page.index = 0;
        if self.header.as_ref().is_some_and(|header| header != &page) {
            return Err(invalid());
        }
        let header = self.header.get_or_insert(page);
        if data.is_empty() && header.count != 1 {
            return Err(invalid());
        }
        self.prefix.append(&data).map_err(|_| invalid())?;
        self.pages += 1;
        if self.pages != header.count {
            return Ok(None);
        }
        let header = self.header.take().ok_or_else(invalid)?;
        self.pages = 0;
        self.bytes = 0;
        let prefix = self.prefix.prefix(header.events).map_err(|_| invalid())?;
        let snapshot = ModelLiveSnapshot {
            session_id: header.session_id,
            turn_id: header.turn_id,
            effect_id: header.effect_id,
            purpose: header.purpose,
            revision: header.revision,
            prefix,
            status: header.status,
        };
        if snapshot.encoded_len() != header.encoded_bytes {
            return Err(invalid());
        }
        self.last = Some((
            snapshot.session_id.clone(),
            snapshot.turn_id.clone(),
            snapshot.effect_id.clone(),
            snapshot.purpose,
            snapshot.revision,
            snapshot.status.clone(),
        ));
        Ok(Some(snapshot))
    }
    /// Verifies EOF did not cut a multi-page snapshot in half.
    pub fn finish(&self) -> Result<()> {
        if self.header.is_some() {
            Err(crate::TurnError::Invalid(
                "incomplete live model snapshot".into(),
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::ModelAttemptBuilder;
    use rsi_ai_protocol::{ContentDelta, ContentStart, LanguageEvent};
    fn snapshot() -> ModelLiveSnapshot {
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
        for i in 1..=10 {
            builder
                .push(
                    i,
                    &LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("界".repeat(30_000)),
                    },
                )
                .unwrap();
        }
        ModelLiveSnapshot {
            session_id: SessionId::new("session").unwrap(),
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("effect").unwrap(),
            purpose: ModelEventPurpose::Conversation,
            revision: 3,
            prefix: builder.prefix(),
            status: ModelLiveStatus::Streaming,
        }
    }
    #[test]
    fn delta_pages_are_linear_resumable_and_reject_rewrites() {
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
        let mut value = snapshot();
        let mut encoder = ModelLiveEncoder::default();
        let mut decoder = ModelLiveDecoder::default();
        let mut transferred = 0;
        for revision in 1..=160 {
            builder
                .push(
                    revision,
                    &LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("界🦀".repeat(1_000)),
                    },
                )
                .unwrap();
            value.revision = revision;
            value.prefix = builder.prefix();
            let mut restored = None;
            for page in encoder.pages(&value) {
                transferred += page.data.len();
                assert!(serde_json::to_vec(&page).unwrap().len() < 256 * 1024);
                restored = decoder.push(page).unwrap();
            }
            assert_eq!(restored.unwrap(), value);
        }
        assert!(transferred < value.encoded_len() + 160 * 4, "{transferred}");
        let mut reconnected = ModelLiveDecoder::default();
        let mut restored = None;
        for page in ModelLiveEncoder::default().pages(&value) {
            restored = reconnected.push(page).unwrap();
        }
        assert_eq!(restored.unwrap(), value);
        let mut rewrite = value.pages().next().unwrap();
        rewrite.revision += 1;
        assert!(decoder.push(rewrite).is_err());
        value.revision += 1;
        value.prefix = ModelAttemptPrefix::default();
        value.status = ModelLiveStatus::Committed { fact_seq: 42 };
        let page = encoder.pages(&value).next().unwrap();
        assert_eq!(reconnected.push(page).unwrap().unwrap(), value);
    }

    #[test]
    fn pages_are_bounded_stable_and_lossless() {
        let original = snapshot();
        let mut decoder = ModelLiveDecoder::default();
        let mut restored = None;
        for page in original.pages() {
            assert!(serde_json::to_vec(&page).unwrap().len() < 256 * 1024);
            restored = decoder.push(page).unwrap();
        }
        assert_eq!(restored.unwrap(), original);
        decoder.finish().unwrap();
    }
    #[test]
    fn rejects_gap_mixed_snapshot_false_count_and_incomplete_eof() {
        let original = snapshot();
        let mut pages = original.pages().collect::<Vec<_>>();
        assert!(pages.len() > 2);
        assert!(ModelLiveDecoder::default().push(pages[1].clone()).is_err());
        let mut decoder = ModelLiveDecoder::default();
        assert!(decoder.push(pages[0].clone()).unwrap().is_none());
        assert!(decoder.finish().is_err());
        pages[1].revision += 1;
        assert!(decoder.push(pages[1].clone()).is_err());
        let mut decoder = ModelLiveDecoder::default();
        let mut result = Ok(None);
        for mut page in original.pages() {
            page.events += 1;
            result = decoder.push(page);
            if result.is_err() {
                break;
            }
        }
        assert!(result.is_err());
    }
    #[test]
    fn observer_decode_capacity_follows_final_clone() {
        let value = snapshot();
        let pool = crate::ObservationRetention::default();
        let bytes = value.encoded_len();
        let reservation = pool.reserve_model_decode(bytes).unwrap();
        assert_eq!(pool.retained_bytes(), bytes);
        let observed = reservation.retain(value).unwrap();
        let clone = observed.clone();
        drop(observed);
        assert_eq!(pool.retained_bytes(), bytes);
        drop(clone);
        assert_eq!(pool.retained_bytes(), 0);
    }
}
