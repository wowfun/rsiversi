//! Immutable user-data references, independent of mechanical Store interfaces.
use crate::{
    Result, SessionError, SessionId, compact_json_len, validate_safe_text, validate_sha256,
};
use serde::{Deserialize, Serialize};

/// Maximum references in one input message.
pub const MAXIMUM_MESSAGE_REFERENCES: usize = 4;
/// Maximum exported text in one immutable reference.
pub const MAXIMUM_REFERENCE_TEXT_BYTES: usize = 1024 * 1024;
/// Maximum inline preview, charged to the input text budget.
pub const MAXIMUM_REFERENCE_PREVIEW_BYTES: usize = 8 * 1024;
/// Maximum encoded CAS envelope, including JSON escaping and provenance.
pub const MAXIMUM_REFERENCE_SNAPSHOT_BYTES: usize =
    6 * (MAXIMUM_REFERENCE_TEXT_BYTES + MAXIMUM_REFERENCE_PREVIEW_BYTES) + 16 * 1024;
/// Maximum Facts inspected by one capture.
pub const MAXIMUM_REFERENCE_SCAN_FACTS: usize = 1024;
/// Maximum aggregate encoded Facts materialized by one capture.
pub const MAXIMUM_REFERENCE_SCAN_BYTES: usize = 16 * 1024 * 1024;
/// Maximum explicit reference text read page.
pub const MAXIMUM_REFERENCE_PAGE_BYTES: usize = 64 * 1024;

/// Exact immutable Session binding. Fingerprints correlate; they grant no access.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceBinding {
    /// Selected Session.
    pub session_id: SessionId,
    /// Canonical immutable Header SHA-256.
    pub header_sha256: String,
}
impl ReferenceBinding {
    /// Validates the fingerprint; `SessionId` is already a validated typed identity.
    pub fn validate(&self) -> Result<()> {
        validate_sha256("reference Header", &self.header_sha256)
    }
}
/// Closed reasons why earlier material was omitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceOmission {
    /// The fixed Fact count stopped the backward scan.
    FactLimit,
    /// The next encoded Fact would exceed the byte scan budget.
    ScanBytes,
    /// Newer exported text filled the content budget.
    ContentBytes,
}
mod source;
pub use source::{
    ReferenceCapture, ReferenceContentKind, ReferenceMetadata, ReferenceRecord, ReferenceSelection,
    ReferenceSource, ReferenceSuffix,
};

/// Agent-owned content address; the mechanical Store adapts this to its CAS type.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSnapshotRef {
    /// Lowercase SHA-256 of the canonical envelope bytes.
    pub sha256: String,
    /// Exact encoded envelope length.
    pub byte_len: u64,
}
impl ReferenceSnapshotRef {
    /// Revalidates digest and the reference-specific CAS size ceiling.
    pub fn validate(&self) -> Result<()> {
        validate_sha256("reference snapshot", &self.sha256)?;
        if self.byte_len == 0 || self.byte_len > MAXIMUM_REFERENCE_SNAPSHOT_BYTES as u64 {
            return Err(SessionError::Invalid(
                "invalid reference snapshot length".into(),
            ));
        }
        Ok(())
    }
}
/// Captured user input. The preview is verified against CAS before admission.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenReference {
    /// Immutable content address.
    pub snapshot: ReferenceSnapshotRef,
    /// Source and original target provenance.
    pub metadata: ReferenceMetadata,
    /// Exact scalar-aligned prefix of exported text.
    pub preview: String,
}
impl FrozenReference {
    /// Checks finite input bounds without reading the Store.
    pub fn validate(&self) -> Result<()> {
        self.snapshot.validate()?;
        self.metadata.validate()?;
        validate_safe_text(
            "reference preview",
            &self.preview,
            MAXIMUM_REFERENCE_PREVIEW_BYTES,
            false,
        )?;
        if self.preview.len() > self.metadata.text_bytes {
            return Err(SessionError::Invalid(
                "reference preview exceeds content length".into(),
            ));
        }
        Ok(())
    }
}
/// Closed canonical CAS envelope; its owning reader verifies the digest first.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSnapshotEnvelope {
    /// Exact supported envelope version, two.
    pub version: u32,
    /// Source, target, horizon and retained interval.
    pub metadata: ReferenceMetadata,
    /// Exact inline prefix of text.
    pub preview: String,
    /// Bounded exported human and visible assistant text only.
    pub text: String,
}
impl ReferenceSnapshotEnvelope {
    /// Revalidates canonical payload bounds and exact preview binding.
    pub fn validate(&self) -> Result<()> {
        self.metadata.validate()?;
        validate_safe_text(
            "reference text",
            &self.text,
            MAXIMUM_REFERENCE_TEXT_BYTES,
            false,
        )?;
        if self.version != 2
            || self.text.len() != self.metadata.text_bytes
            || self.preview
                != self.text[..self
                    .text
                    .floor_char_boundary(self.text.len().min(MAXIMUM_REFERENCE_PREVIEW_BYTES))]
            || compact_json_len(self)? > MAXIMUM_REFERENCE_SNAPSHOT_BYTES
        {
            return Err(SessionError::Invalid(
                "invalid reference envelope or preview".into(),
            ));
        }
        Ok(())
    }
    /// Produces the immutable input descriptor after the Store installed its bytes.
    pub fn frozen(&self, snapshot: ReferenceSnapshotRef) -> Result<FrozenReference> {
        self.validate()?;
        let frozen = FrozenReference {
            snapshot,
            metadata: self.metadata.clone(),
            preview: self.preview.clone(),
        };
        frozen.validate()?;
        Ok(frozen)
    }
}
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    #[allow(clippy::trivially_copy_pass_by_ref, reason = "Serde with signature")]
    pub fn serialize<S: Serializer>(
        value: &u64,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<u64, D::Error> {
        let text = String::deserialize(deserializer)?;
        let value: u64 = text.parse().map_err(serde::de::Error::custom)?;
        if value.to_string() != text {
            return Err(serde::de::Error::custom("noncanonical decimal sequence"));
        }
        Ok(value)
    }
}

/// Exact recorded input coordinate; its reader separately checks Session authority.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceReadRequest {
    /// Current Session or the direct parent whose actual inherited interval contains this Fact.
    pub recorded_session_id: SessionId,
    /// Positive durable `InputMessageEntered` Fact, serialized as an exact decimal string.
    #[serde(with = "decimal")]
    pub fact_seq: u64,
    /// Reference's index in that Fact's content array.
    pub content_index: usize,
    /// Raw exported UTF-8 byte cursor, aligned by the reader.
    #[serde(default)]
    pub offset: usize,
    /// Maximum page bytes, at most 64 KiB.
    pub maximum: usize,
}
impl ReferenceReadRequest {
    /// Resolves the sole permitted recorded Header binding without Store I/O.
    pub fn recorded_binding(&self, header: &crate::SessionHeader) -> Result<ReferenceBinding> {
        self.validate()?;
        if &self.recorded_session_id == header.session_id() {
            return Ok(ReferenceBinding {
                session_id: self.recorded_session_id.clone(),
                header_sha256: header.fingerprint()?,
            });
        }
        let origin = header
            .fork_origin()
            .filter(|origin| {
                origin.parent_session_id == self.recorded_session_id
                    && self.fact_seq > origin.resolved_after_seq
                    && self.fact_seq <= origin.resolved_terminal_seq
            })
            .ok_or_else(|| {
                SessionError::Invalid(
                    "reference is outside the actual inherited parent interval".into(),
                )
            })?;
        Ok(ReferenceBinding {
            session_id: self.recorded_session_id.clone(),
            header_sha256: origin.parent_header_fingerprint.clone(),
        })
    }
    /// Validates request bounds before Store work.
    pub fn validate(&self) -> Result<()> {
        if self.fact_seq == 0
            || self.fact_seq == u64::MAX
            || self.content_index >= crate::MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS
        {
            return Err(SessionError::Invalid(
                "invalid reference read coordinates or page".into(),
            ));
        }
        validate_reference_page_bounds(self.offset, self.maximum)
    }
}
/// Validates draft and recorded-reference page bounds at their common wire boundary.
pub fn validate_reference_page_bounds(offset: usize, maximum: usize) -> Result<()> {
    if offset > MAXIMUM_REFERENCE_TEXT_BYTES
        || !(4..=MAXIMUM_REFERENCE_PAGE_BYTES).contains(&maximum)
    {
        return Err(SessionError::Invalid(
            "invalid reference page bounds".into(),
        ));
    }
    Ok(())
}
/// Finite exact text window from immutable reference contents.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceTextPage {
    /// Exact recorded read request, absent for an unsubmitted draft preview.
    pub recorded: Option<ReferenceReadRequest>,
    /// Original immutable descriptor, including provenance and omission state.
    pub reference: FrozenReference,
    /// Actual scalar-aligned byte start.
    pub offset: usize,
    /// Exact next byte cursor.
    pub next_offset: usize,
    /// Exported text in this window.
    pub text: String,
    /// Whether text remains after `next_offset`.
    pub has_more: bool,
}
impl ReferenceTextPage {
    /// Verifies the bounded text and all echoed cursors.
    pub fn validate(&self) -> Result<()> {
        self.reference.validate()?;
        if let Some(request) = &self.recorded {
            request.validate()?;
            if request.recorded_session_id != self.reference.metadata.target.session_id
                || self.text.len() > request.maximum
            {
                return Err(SessionError::Invalid(
                    "reference page request mismatch".into(),
                ));
            }
        }
        if self.text.len() > MAXIMUM_REFERENCE_PAGE_BYTES
            || self.offset > self.next_offset
            || self.next_offset - self.offset != self.text.len()
            || self.next_offset > self.reference.metadata.text_bytes
            || self.has_more != (self.next_offset < self.reference.metadata.text_bytes)
        {
            return Err(SessionError::Invalid("invalid reference page".into()));
        }
        Ok(())
    }
}
