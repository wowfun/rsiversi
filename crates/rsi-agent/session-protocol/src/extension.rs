//! Built-in mechanical envelope; extension-specific event meaning stays with its adapter.
use crate::{DomainStateValue, ReferenceCollector, SessionId};
use serde::{Deserialize, Serialize};

/// Built-in domain identity. Its reference meaning survives plugin removal.
pub const EXTENSION_JOURNAL_DOMAIN: &str = "rsi.extension-journal";
/// Current mechanical envelope, independent of executable code selection.
pub const EXTENSION_JOURNAL_VERSION: u32 = 2;
/// Maximum encoded bytes across the journal's immutable CAS segments.
pub const MAXIMUM_EXTENSION_JOURNAL_BYTES: usize = 64 * 1024 * 1024;
/// Packing target for ordinary events; a larger event occupies one segment.
pub const EXTENSION_SEGMENT_TARGET_BYTES: usize = 256 * 1024;

/// One immutable contiguous event segment in Agent CAS.
#[derive(schemars::JsonSchema, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionSegment {
    /// First zero-based event position.
    pub first: u64,
    /// Nonzero event count.
    pub count: u64,
    /// Lowercase content hash of the complete encoded segment.
    pub sha256: String,
    /// Exact encoded byte length.
    pub byte_len: u64,
}

/// Typed bounded journal roots and recovery metadata; publication uses ordinary domains.
#[derive(schemars::JsonSchema, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionJournal {
    /// Immutable contiguous CAS roots, in event order.
    pub segments: Vec<ExtensionSegment>,
    /// Explicit Session edges used by the extension data.
    pub sessions: Vec<SessionId>,
    /// Recovery metadata without implicit Agent references.
    pub checkpoint: DomainStateValue,
}

impl ExtensionJournal {
    /// Checks mechanical bounds before canonical admission or reference maintenance.
    pub fn validate(&self) -> Result<(), String> {
        if self.segments.len() > 256
            || self.sessions.len() > 64
            || self.checkpoint.encoded_len() > 64 * 1024
        {
            return Err("extension journal capacity exceeded".into());
        }
        let mut cursor = 0_u64;
        let mut bytes = 0_u64;
        let mut output = ReferenceCollector::default();
        for segment in &self.segments {
            if segment.first != cursor
                || segment.count == 0
                || segment.count > 4096
                || (segment.byte_len > EXTENSION_SEGMENT_TARGET_BYTES as u64 && segment.count != 1)
            {
                return Err("extension journal segment gap, overlap or empty range".into());
            }
            cursor = cursor
                .checked_add(segment.count)
                .ok_or("extension cursor overflow")?;
            if cursor > 100_000 {
                return Err("extension event capacity exceeded".into());
            }
            bytes = bytes
                .checked_add(segment.byte_len)
                .ok_or("extension byte overflow")?;
            if bytes > MAXIMUM_EXTENSION_JOURNAL_BYTES as u64 {
                return Err("extension journal byte capacity exceeded".into());
            }
            output.cas(&segment.sha256, segment.byte_len)?;
        }
        let unique = self
            .sessions
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if unique.len() != self.sessions.len() {
            return Err("duplicate extension Session reference".into());
        }
        Ok(())
    }

    /// Visits every typed root after validation, even when execution code is unavailable.
    pub fn references(&self, output: &mut ReferenceCollector) -> Result<(), String> {
        self.validate()?;
        for segment in &self.segments {
            output.cas(&segment.sha256, segment.byte_len)?;
        }
        for session in &self.sessions {
            output.session(session)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn journal() -> ExtensionJournal {
        ExtensionJournal {
            segments: vec![ExtensionSegment {
                first: 0,
                count: 2,
                sha256: "b".repeat(64),
                byte_len: 20,
            }],
            sessions: vec![SessionId::new("source").unwrap()],
            checkpoint: DomainStateValue::new(serde_json::json!({"intent":null})).unwrap(),
        }
    }
    #[test]
    fn references_are_explicit_and_ranges_are_contiguous() {
        let mut value = journal();
        let mut references = ReferenceCollector::default();
        value.references(&mut references).unwrap();
        assert_eq!(references.iter().count(), 2);
        value.segments.push(ExtensionSegment {
            first: 3,
            ..value.segments[0].clone()
        });
        assert!(value.validate().is_err());
        value.segments[1].first = 2;
        value.validate().unwrap();
        value.segments[1].count = u64::MAX;
        assert!(value.validate().is_err());
    }
    #[test]
    fn recovery_metadata_cannot_exhaust_the_domain_budget() {
        let mut value = journal();
        value.checkpoint = DomainStateValue::new(serde_json::json!("x".repeat(64 * 1024))).unwrap();
        assert!(value.validate().is_err());
    }

    #[test]
    fn large_events_are_standalone_within_the_aggregate_byte_bound() {
        let mut value = journal();
        value.segments[0].count = 1;
        value.segments[0].byte_len = MAXIMUM_EXTENSION_JOURNAL_BYTES as u64;
        value.validate().unwrap();
        value.segments[0].count = 2;
        assert!(value.validate().is_err());
        value.segments[0].count = 1;
        value.segments[0].byte_len += 1;
        assert!(value.validate().is_err());
    }
}
