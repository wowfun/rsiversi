//! Bounded disposable extension views at one exact Session cut.

use crate::{ContributionId, DomainStateValue, Result, SessionError, SessionId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Maximum distinct producer outcomes in one projection snapshot.
pub const MAXIMUM_SESSION_PROJECTIONS: usize = 64;
/// Maximum canonical JSON bytes in one producer's complete value.
pub const MAXIMUM_PROJECTION_VALUE_BYTES: usize = 64 * 1024;
/// Maximum complete encoded snapshot, including identities and failure entries.
pub const MAXIMUM_SESSION_PROJECTION_BYTES: usize = 5 * 1024 * 1024;

/// Exact captured draft revision or simultaneous durable watermarks.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectionCursor {
    /// An unpublished draft's actual initial values.
    Draft {
        /// Lease-local mutation revision.
        revision: u64,
    },
    /// A consistent durable Store read cut.
    Durable {
        /// Inclusive committed Fact watermark.
        fact_seq: u64,
        /// Inclusive committed control watermark.
        control_seq: u64,
    },
}
impl ProjectionCursor {
    /// Tests monotonic advancement, including the one-way publication boundary.
    pub const fn can_follow(self, previous: Self) -> bool {
        match (self, previous) {
            (Self::Draft { revision }, Self::Draft { revision: prior }) => revision >= prior,
            (Self::Durable { .. }, Self::Draft { .. }) => true,
            (Self::Draft { .. }, Self::Durable { .. }) => false,
            (
                Self::Durable {
                    fact_seq,
                    control_seq,
                },
                Self::Durable {
                    fact_seq: prior_fact,
                    control_seq: prior_control,
                },
            ) => fact_seq >= prior_fact && control_seq >= prior_control,
        }
    }
}

/// Validated opaque complete JSON view; shared values are immutable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProjectionValue(DomainStateValue);
impl ProjectionValue {
    /// Validates JSON structure and canonical size at the producer boundary.
    pub fn new(value: serde_json::Value) -> Result<Self> {
        Self::checked(DomainStateValue::new(value)?)
    }
    /// Encodes linked typed output through a bounded writer before JSON allocation.
    pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Self> {
        Self::checked(DomainStateValue::encode(value)?)
    }
    fn checked(value: DomainStateValue) -> Result<Self> {
        if value.encoded_len() > MAXIMUM_PROJECTION_VALUE_BYTES {
            return Err(SessionError::TooLarge {
                kind: "projection value",
                maximum: MAXIMUM_PROJECTION_VALUE_BYTES,
                actual: value.encoded_len(),
            });
        }
        Ok(Self(value))
    }
    /// Borrows the complete value, without granting mutation authority.
    pub fn value(&self) -> &serde_json::Value {
        self.0.value()
    }
}
impl<'de> Deserialize<'de> for ProjectionValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::checked(DomainStateValue::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Content {
    Value { value: ProjectionValue },
    Failed { message: String },
}

/// A single producer's complete value or isolated failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionEntry {
    producer: ContributionId,
    content: Content,
}
impl ProjectionEntry {
    /// Attributes a validated view to the exact registered producer.
    pub const fn value(producer: ContributionId, value: ProjectionValue) -> Self {
        Self {
            producer,
            content: Content::Value { value },
        }
    }
    /// Validates one bounded diagnostic without discarding other producers' views.
    pub fn failed(producer: ContributionId, message: impl Into<String>) -> Result<Self> {
        let message = message.into();
        crate::validate_safe_diagnostic("projection failure", &message)?;
        Ok(Self {
            producer,
            content: Content::Failed { message },
        })
    }
    /// Returns stable registered identity.
    pub const fn producer(&self) -> &ContributionId {
        &self.producer
    }
    /// Borrows successful output; failure is available independently.
    pub const fn view(&self) -> Option<&ProjectionValue> {
        match &self.content {
            Content::Value { value } => Some(value),
            Content::Failed { .. } => None,
        }
    }
    /// Borrows a failed producer's bounded diagnostic.
    pub fn failure(&self) -> Option<&str> {
        match &self.content {
            Content::Failed { message } => Some(message),
            Content::Value { .. } => None,
        }
    }
}
impl<'de> Deserialize<'de> for ProjectionEntry {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            producer: ContributionId,
            content: Content,
        }
        let wire = Wire::deserialize(deserializer)?;
        match wire.content {
            Content::Value { value } => Ok(Self::value(wire.producer, value)),
            Content::Failed { message } => {
                Self::failed(wire.producer, message).map_err(serde::de::Error::custom)
            }
        }
    }
}

/// Complete disposable extension state bound to one Header and generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionProjectionSnapshot {
    session_id: SessionId,
    header_sha256: String,
    generation_sha256: String,
    cursor: ProjectionCursor,
    entries: Vec<ProjectionEntry>,
}
impl SessionProjectionSnapshot {
    /// Validates unique producer identity and the complete encoded envelope.
    pub fn new(
        session_id: SessionId,
        header_sha256: impl Into<String>,
        generation_sha256: impl Into<String>,
        cursor: ProjectionCursor,
        entries: Vec<ProjectionEntry>,
    ) -> Result<Self> {
        let header_sha256 = header_sha256.into();
        let generation_sha256 = generation_sha256.into();
        crate::validate_sha256("projection Header", &header_sha256)?;
        crate::validate_sha256("projection generation", &generation_sha256)?;
        let mut identities = BTreeSet::new();
        if entries.len() > MAXIMUM_SESSION_PROJECTIONS
            || entries
                .iter()
                .any(|entry| !identities.insert(entry.producer()))
        {
            return Err(SessionError::Invalid(
                "projection producers exceed their count or identity bound".into(),
            ));
        }
        let snapshot = Self {
            session_id,
            header_sha256,
            generation_sha256,
            cursor,
            entries,
        };
        let bytes = snapshot.encoded_len()?;
        if bytes > MAXIMUM_SESSION_PROJECTION_BYTES {
            return Err(SessionError::TooLarge {
                kind: "projection snapshot",
                maximum: MAXIMUM_SESSION_PROJECTION_BYTES,
                actual: bytes,
            });
        }
        Ok(snapshot)
    }
    /// Returns the selected Session identity.
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }
    /// Returns the exact Header fingerprint at capture.
    pub fn header_sha256(&self) -> &str {
        &self.header_sha256
    }
    /// Returns the immutable composition's source digest.
    pub fn generation_sha256(&self) -> &str {
        &self.generation_sha256
    }
    /// Returns the shared cut reflected by every entry.
    pub const fn cursor(&self) -> ProjectionCursor {
        self.cursor
    }
    /// Borrows complete producer outcomes in frozen catalog order.
    pub fn entries(&self) -> &[ProjectionEntry] {
        &self.entries
    }
    /// Counts canonical bytes for retained snapshot admission without encoding a second copy.
    pub fn encoded_len(&self) -> Result<usize> {
        crate::compact_json_len(self)
    }
}
impl<'de> Deserialize<'de> for SessionProjectionSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            session_id: SessionId,
            header_sha256: String,
            generation_sha256: String,
            cursor: ProjectionCursor,
            entries: Vec<ProjectionEntry>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.session_id,
            wire.header_sha256,
            wire.generation_sha256,
            wire.cursor,
            wire.entries,
        )
        .map_err(serde::de::Error::custom)
    }
}
