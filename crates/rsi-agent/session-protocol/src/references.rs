//! Bounded canonical references, independent of plugin execution.

use crate::SessionId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Maximum distinct durable references in one canonical record.
pub const MAXIMUM_RECORD_REFERENCES: usize = 1024;

/// A typed durable edge; arbitrary strings and Media addresses are not Agent-CAS roots.
#[derive(
    schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize,
)]
#[serde(
    tag = "kind",
    content = "reference",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DurableReference {
    /// Another Session retained by this record.
    Session(SessionId),
    /// An exact immutable Agent-CAS object.
    Cas {
        /// Lowercase SHA-256 of the complete object.
        sha256: String,
        /// Exact nonzero encoded byte length.
        byte_len: u64,
    },
}

/// Bounded deduplicating output owned by the canonical record reader.
#[derive(schemars::JsonSchema, Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ReferenceCollector(BTreeSet<DurableReference>);
impl ReferenceCollector {
    /// Adds one typed Session edge, rejecting overflow before allocation.
    pub fn session(&mut self, session: &SessionId) -> Result<(), String> {
        self.insert(DurableReference::Session(session.clone()))
    }
    /// Adds one typed CAS edge after validating its durable scalar bounds.
    pub fn cas(&mut self, sha256: &str, byte_len: u64) -> Result<(), String> {
        crate::validate_sha256("durable CAS reference", sha256).map_err(|e| e.to_string())?;
        if byte_len == 0 || byte_len > 64 * 1024 * 1024 {
            return Err("durable CAS reference length exceeds its bound".into());
        }
        self.insert(DurableReference::Cas {
            sha256: sha256.into(),
            byte_len,
        })
    }
    fn insert(&mut self, reference: DurableReference) -> Result<(), String> {
        if !self.0.contains(&reference) && self.0.len() == MAXIMUM_RECORD_REFERENCES {
            return Err("canonical record exceeds its durable reference limit".into());
        }
        self.0.insert(reference);
        Ok(())
    }
    /// Iterates exact sorted edges without materializing a second collection.
    pub fn iter(&self) -> impl Iterator<Item = &DurableReference> {
        self.0.iter()
    }
    /// Combines already validated canonical references, preserving the record bound.
    pub fn extend(&mut self, other: &Self) -> Result<(), String> {
        for reference in other.iter() {
            self.insert(reference.clone())?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ReferenceCollector {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CanonicalReferences;
        impl<'de> serde::de::Visitor<'de> for CanonicalReferences {
            type Value = ReferenceCollector;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded sorted unique set of durable references")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut output = ReferenceCollector::default();
                while let Some(value) = sequence.next_element::<DurableReference>()? {
                    if output.0.len() == MAXIMUM_RECORD_REFERENCES
                        || output.0.last().is_some_and(|previous| previous >= &value)
                    {
                        return Err(serde::de::Error::custom(
                            "canonical references require a bounded sorted unique set",
                        ));
                    }
                    if let DurableReference::Cas { sha256, byte_len } = &value {
                        crate::validate_sha256("durable CAS reference", sha256)
                            .map_err(serde::de::Error::custom)?;
                        if *byte_len == 0 || *byte_len > 64 * 1024 * 1024 {
                            return Err(serde::de::Error::custom(
                                "durable CAS reference length exceeds its bound",
                            ));
                        }
                    }
                    output.0.insert(value);
                }
                Ok(output)
            }
        }
        deserializer.deserialize_seq(CanonicalReferences)
    }
}
