//! TUI-lifetime drafts with one active editor and at most one pending command.
//! Retention and revision policy: apps/external-drafts.md.

use rsi_acp_protocol::observation::ConversationId;
use std::collections::BTreeMap;

pub(super) const MAXIMUM_TEXT: usize = 128 * 1024;
const MAXIMUM_RECORDS: usize = 16;
const MAXIMUM_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Attempt {
    id: ConversationId,
    revision: u64,
    serial: u64,
}

#[derive(Default)]
struct Record {
    text: String,
    revision: u64,
    unknown: Option<u64>,
    pending: Option<Attempt>,
    used: u64,
}
#[derive(Default)]
pub(super) struct Drafts {
    records: BTreeMap<ConversationId, Record>,
    bound: Option<ConversationId>,
    clock: u64,
}
impl Drafts {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
    fn admit(&mut self, id: &ConversationId, bytes: usize, count: usize) {
        let mut total = self
            .records
            .values()
            .map(|record| record.text.len())
            .sum::<usize>()
            + bytes;
        let mut size = self.records.len() + count;
        if total <= MAXIMUM_BYTES && size <= MAXIMUM_RECORDS {
            return;
        }
        let mut candidates = self
            .records
            .iter()
            .filter(|(key, record)| {
                *key != id && self.bound.as_ref() != Some(*key) && record.pending.is_none()
            })
            .map(|(key, record)| (record.used, key.clone(), record.text.len()))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(used, _, _)| *used);
        let mut evicted = Vec::new();
        for (_, key, bytes) in candidates {
            if total <= MAXIMUM_BYTES && size <= MAXIMUM_RECORDS {
                break;
            }
            if size <= MAXIMUM_RECORDS && bytes == 0 {
                continue;
            }
            total -= bytes;
            size -= 1;
            evicted.push(key);
        }
        // The bounded editor and single pending command leave at most two
        // pinned records; both fit. Everything else is idle and evictable.
        assert!(total <= MAXIMUM_BYTES && size <= MAXIMUM_RECORDS);
        for key in evicted {
            self.records.remove(&key);
        }
    }
    pub(super) fn bind(&mut self, id: ConversationId) -> &str {
        self.unbind();
        if !self.records.contains_key(&id) {
            self.admit(&id, 0, 1);
        }
        let used = self.tick();
        self.bound = Some(id.clone());
        let record = self.records.entry(id).or_default();
        record.used = used;
        &record.text
    }
    pub(super) fn bound(&self) -> Option<&ConversationId> {
        self.bound.as_ref()
    }
    pub(super) fn unbind(&mut self) {
        if let Some(id) = self.bound.take() {
            let used = self.tick();
            self.records.get_mut(&id).expect("bound draft").used = used;
        }
    }
    pub(super) fn text(&self, id: &ConversationId) -> &str {
        self.records
            .get(id)
            .map_or("", |record| record.text.as_str())
    }
    pub(super) fn uncertain(&self, id: &ConversationId) -> bool {
        self.records
            .get(id)
            .is_some_and(|record| record.unknown == Some(record.revision))
    }
    pub(super) fn edit(&mut self, id: &ConversationId, text: &str) {
        let record = self.records.get(id).expect("bound draft");
        if record.text == text {
            return;
        }
        assert!(
            text.len() <= MAXIMUM_TEXT,
            "text admitted by external editor"
        );
        let extra = text.len().saturating_sub(record.text.len());
        self.admit(id, extra, 0);
        let used = self.tick();
        let record = self.records.get_mut(id).expect("retained active draft");
        text.clone_into(&mut record.text);
        record.revision += 1;
        record.unknown = None;
        record.used = used;
    }
    pub(super) fn begin(&mut self, id: &ConversationId) -> Option<Attempt> {
        if self.records.values().any(|record| record.pending.is_some()) {
            return None;
        }
        let serial = self.tick();
        let record = self.records.get_mut(id)?;
        if record.pending.is_some() || record.unknown == Some(record.revision) {
            return None;
        }
        let attempt = Attempt {
            id: id.clone(),
            revision: record.revision,
            serial,
        };
        record.pending = Some(attempt.clone());
        record.used = serial;
        Some(attempt)
    }
    pub(super) fn finish(&mut self, attempt: &Attempt, success: bool) {
        let used = self.tick();
        let Some(record) = self.records.get_mut(&attempt.id) else {
            return;
        };
        if record.pending.as_ref() != Some(attempt) {
            return;
        }
        record.pending = None;
        record.used = used;
        if record.revision == attempt.revision {
            if success {
                record.text.clear();
                record.revision += 1;
                record.unknown = None;
            } else {
                record.unknown = Some(attempt.revision);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(value: &str) -> ConversationId {
        ConversationId::new(value).unwrap()
    }
    #[test]
    fn byte_pressure_preserves_empty_idle_records() {
        let mut drafts = Drafts::default();
        let empty = id("empty");
        drafts.bind(empty.clone());
        for index in 0..9 {
            let other = id(&format!("full-{index}"));
            drafts.bind(other.clone());
            drafts.edit(&other, &"x".repeat(MAXIMUM_TEXT));
        }
        assert!(drafts.records.contains_key(&empty));
        assert!(!drafts.records.contains_key(&id("full-0")));
    }
    #[test]
    fn last_unbind_refreshes_idle_recency_before_admission() {
        let mut drafts = Drafts::default();
        let idle = id("idle");
        drafts.bind(idle.clone());
        let pending = drafts.begin(&idle).unwrap();
        for index in 0..14 {
            drafts.bind(id(&format!("other-{index}")));
        }
        let active = id("active");
        drafts.bind(active.clone());
        drafts.finish(&pending, true);
        drafts.unbind();
        assert!(drafts.records[&active].used > drafts.records[&idle].used);
        drafts.bind(id("new"));
        assert!(drafts.records.contains_key(&active));
        assert!(!drafts.records.contains_key(&id("other-0")));
    }
    #[test]
    fn uncertainty_and_completion_follow_identity_and_revision() {
        let mut drafts = Drafts::default();
        let a = id("a");
        let b = id("b");
        drafts.bind(a.clone());
        drafts.edit(&a, "same");
        let attempt = drafts.begin(&a).unwrap();
        drafts.finish(&attempt, false);
        drafts.bind(b.clone());
        drafts.edit(&b, "same");
        assert_eq!(drafts.bind(a.clone()), "same");
        drafts.edit(&a, "same");
        assert!(drafts.begin(&a).is_none());
        drafts.edit(&a, "different");
        let attempt = drafts.begin(&a).unwrap();
        drafts.edit(&a, "same");
        drafts.edit(&a, "different");
        drafts.finish(&attempt, true);
        assert_eq!(drafts.text(&a), "different");
        let attempt = drafts.begin(&a).unwrap();
        drafts.bind(b.clone());
        drafts.finish(&attempt, true);
        assert_eq!(drafts.text(&b), "same");
        assert_eq!(drafts.text(&a), "");
    }
    #[test]
    fn edits_with_spare_capacity_preserve_idle_uncertainty() {
        let mut drafts = Drafts::default();
        let idle = id("idle");
        drafts.bind(idle.clone());
        drafts.edit(&idle, "unconfirmed message");
        let attempt = drafts.begin(&idle).unwrap();
        drafts.finish(&attempt, false);
        let active = id("active");
        drafts.bind(active.clone());
        for text in ["a", "a longer edit", "", "final"] {
            drafts.edit(&active, text);
            assert_eq!(drafts.text(&idle), "unconfirmed message");
            assert!(drafts.uncertain(&idle));
        }
        assert_eq!(drafts.bind(idle.clone()), "unconfirmed message");
        assert!(drafts.begin(&idle).is_none());
    }
    #[test]
    fn idle_eviction_retains_active_and_single_pending_records() {
        let mut drafts = Drafts::default();
        let a = id("a");
        drafts.bind(a.clone());
        drafts.edit(&a, &"x".repeat(MAXIMUM_TEXT));
        let attempt = drafts.begin(&a).unwrap();
        for i in 0..20 {
            let other = id(&format!("record-{i}"));
            drafts.bind(other.clone());
            drafts.edit(&other, &"y".repeat(MAXIMUM_TEXT));
            assert!(drafts.begin(&other).is_none());
            assert!(drafts.records.len() <= MAXIMUM_RECORDS);
            assert!(drafts.records.values().map(|r| r.text.len()).sum::<usize>() <= MAXIMUM_BYTES);
            assert_eq!(drafts.text(&a).len(), MAXIMUM_TEXT);
        }
        drafts.finish(&attempt, false);
        assert!(drafts.uncertain(&a));
        for i in 0..MAXIMUM_RECORDS {
            drafts.bind(id(&format!("empty-{i}")));
        }
        assert_eq!(drafts.bind(a.clone()), "");
        assert!(!drafts.uncertain(&a));
    }
}
