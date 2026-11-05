//! Built-in mechanical envelope; extension-specific event meaning stays with its adapter.
use crate::{DomainStateValue, ReferenceCollector, SessionId};
use serde::{Deserialize, Serialize};

/// Built-in domain identity, version one. Its reference meaning survives plugin removal.
pub const EXTENSION_JOURNAL_DOMAIN: &str = "rsi.extension-journal";

/// One immutable contiguous event segment in Agent CAS.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionJournal {
    /// Exact artifact cohort hash, not a mutable package version.
    pub artifact_sha256: String,
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
        crate::validate_sha256("extension artifact", &self.artifact_sha256)
            .map_err(|error| error.to_string())?;
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
                || segment.byte_len > 256 * 1024
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
            if bytes > 64 * 1024 * 1024 {
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
            artifact_sha256: "a".repeat(64),
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
}
