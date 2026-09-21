use super::{
    Deserialize, MAXIMUM_REFERENCE_SCAN_BYTES, MAXIMUM_REFERENCE_SCAN_FACTS,
    MAXIMUM_REFERENCE_TEXT_BYTES, ReferenceBinding, ReferenceOmission, Result, Serialize,
    SessionError, decimal, validate_sha256,
};
use std::fmt;

/// Reference provenance keeps native durability and product observations distinct.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReferenceSource {
    /// One native immutable Header.
    Native {
        /// Source Session and Header digest.
        binding: ReferenceBinding,
    },
    /// A product-owned observed record set; not native Agent Facts.
    Observed {
        /// Stable source owner namespace, not an authorization credential.
        owner: String,
        /// Exact source identity within that owner.
        id: String,
        /// Immutable observation/replay epoch.
        #[serde(with = "decimal")]
        epoch: u64,
    },
}
impl ReferenceSource {
    /// Validates external or durable source identity without invoking its owner.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Native { binding } => binding.validate(),
            Self::Observed { owner, id, epoch } => {
                for value in [owner, id] {
                    if value.is_empty()
                        || value.len() > 256
                        || value.chars().any(|c| c.is_control() || c.is_whitespace())
                    {
                        return Err(SessionError::Invalid(
                            "invalid observed reference identity".into(),
                        ));
                    }
                }
                if *epoch == 0 {
                    return Err(SessionError::Invalid(
                        "invalid observed reference epoch".into(),
                    ));
                }
                Ok(())
            }
        }
    }
    /// Native Header binding, absent for product observations.
    pub const fn native(&self) -> Option<&ReferenceBinding> {
        match self {
            Self::Native { binding } => Some(binding),
            Self::Observed { .. } => None,
        }
    }
}
impl fmt::Display for ReferenceSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native { binding } => write!(f, "{}", binding.session_id),
            Self::Observed { owner, id, epoch } => {
                write!(f, "{owner}/{id} (observed epoch {epoch})")
            }
        }
    }
}
/// Closed, explicit text export classes. Reasoning and provider inputs are absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceContentKind {
    /// Direct human text.
    Human,
    /// Visible assistant conversation text.
    Assistant,
    /// Explicit model-facing Tool text evidence, never arbitrary raw JSON.
    ToolEvidence,
}
/// One source record and its exact text-content item.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRecord {
    /// Positive source-local record sequence.
    #[serde(with = "decimal")]
    pub sequence: u64,
    /// Explicit export class.
    pub kind: ReferenceContentKind,
    /// Exact content item inside the source record.
    pub content_index: usize,
}
impl ReferenceRecord {
    /// Bounds the coordinate before any source owner read.
    pub fn validate(&self) -> Result<()> {
        if self.sequence == 0 || self.content_index >= 1024 {
            return Err(SessionError::Invalid(
                "invalid selected reference record".into(),
            ));
        }
        Ok(())
    }
}
/// Exact selected range and original evidence, independent of the latest suffix.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSelection {
    /// Record and original text field to reread.
    pub record: ReferenceRecord,
    /// Source cutoff observed when the original was selected.
    #[serde(with = "decimal")]
    pub through_seq: u64,
    /// Inclusive scalar-aligned UTF-8 byte start in the original text field.
    pub start: usize,
    /// Exclusive scalar-aligned UTF-8 byte end in the original text field.
    pub end: usize,
    /// Digest of the complete original field, not just the selected fragment.
    pub text_sha256: String,
    /// Encoded original bytes admitted by its source owner before materialization.
    pub scanned_bytes: usize,
}
impl ReferenceSelection {
    /// Validates finite ranges; the source owner separately checks text boundaries.
    pub fn validate(&self) -> Result<()> {
        self.record.validate()?;
        validate_sha256("selected reference original", &self.text_sha256)?;
        if self.record.sequence > self.through_seq
            || self.start >= self.end
            || self.end > MAXIMUM_REFERENCE_TEXT_BYTES
            || self.scanned_bytes == 0
            || self.scanned_bytes > MAXIMUM_REFERENCE_SCAN_BYTES
        {
            return Err(SessionError::Invalid(
                "invalid selected reference range".into(),
            ));
        }
        Ok(())
    }
}
/// A capture records either a suffix interval or one exact selected original.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReferenceCapture {
    /// Existing finite native suffix capture.
    Suffix {
        /// Native scan coordinates, digest and explicit omissions.
        interval: ReferenceSuffix,
    },
    /// One precise source range; it may precede the latest 1,024 native Facts.
    Selected {
        /// Original identity, cutoff, digest and selection.
        selection: ReferenceSelection,
    },
}
/// Source interval of a bounded native suffix.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSuffix {
    /// Source Fact cutoff.
    #[serde(with = "decimal")]
    pub through_seq: u64,
    /// Canonical prefix digest at the cutoff.
    pub fact_prefix_sha256: String,
    /// Exclusive first scanned coordinate.
    #[serde(with = "decimal")]
    pub scanned_after_seq: u64,
    /// Exclusive first retained coordinate.
    #[serde(with = "decimal")]
    pub retained_after_seq: u64,
    /// Last retained coordinate.
    #[serde(with = "decimal")]
    pub retained_through_seq: u64,
    /// Exact admitted canonical original bytes.
    pub scanned_bytes: usize,
    /// Unique bounded omission reasons.
    pub omissions: Vec<ReferenceOmission>,
}
impl ReferenceSuffix {
    fn validate(&self) -> Result<()> {
        validate_sha256("reference Fact prefix", &self.fact_prefix_sha256)?;
        if self.through_seq == 0
            || self.scanned_after_seq >= self.through_seq
            || self.through_seq - self.scanned_after_seq > MAXIMUM_REFERENCE_SCAN_FACTS as u64
            || self.retained_after_seq < self.scanned_after_seq
            || self.retained_after_seq >= self.retained_through_seq
            || self.retained_through_seq > self.through_seq
            || self.scanned_bytes == 0
            || self.scanned_bytes > MAXIMUM_REFERENCE_SCAN_BYTES
            || self.omissions.len() > 3
            || self
                .omissions
                .iter()
                .enumerate()
                .any(|(i, reason)| self.omissions[..i].contains(reason))
        {
            return Err(SessionError::Invalid(
                "invalid frozen reference interval or limits".into(),
            ));
        }
        Ok(())
    }
}
/// Frozen source identity, target binding and capture provenance.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceMetadata {
    /// Native or observed source identity.
    pub source: ReferenceSource,
    /// Original receiving Session's immutable Header binding.
    pub target: ReferenceBinding,
    /// Exact suffix or selected range frozen by its source owner.
    pub capture: ReferenceCapture,
    /// Exact exported text bytes.
    pub text_bytes: usize,
}
impl ReferenceMetadata {
    /// Validates source/capture correlation and all durable bounds.
    pub fn validate(&self) -> Result<()> {
        self.source.validate()?;
        self.target.validate()?;
        if self.text_bytes == 0 || self.text_bytes > MAXIMUM_REFERENCE_TEXT_BYTES {
            return Err(SessionError::Invalid(
                "invalid frozen reference text length".into(),
            ));
        }
        match &self.capture {
            ReferenceCapture::Suffix { interval } => {
                if self.source.native().is_none() {
                    return Err(SessionError::Invalid(
                        "observations cannot claim a native Fact suffix".into(),
                    ));
                }
                interval.validate()
            }
            ReferenceCapture::Selected { selection } => {
                selection.validate()?;
                if self.text_bytes != selection.end - selection.start {
                    return Err(SessionError::Invalid(
                        "selected reference length mismatch".into(),
                    ));
                }
                Ok(())
            }
        }
    }
    /// Source cutoff, retaining the source's own sequence semantics.
    pub const fn through_seq(&self) -> u64 {
        match &self.capture {
            ReferenceCapture::Suffix { interval } => interval.through_seq,
            ReferenceCapture::Selected { selection } => selection.through_seq,
        }
    }
    /// Inclusive retained record interval.
    pub const fn retained_interval(&self) -> (u64, u64) {
        match &self.capture {
            ReferenceCapture::Suffix { interval } => (
                interval.retained_after_seq + 1,
                interval.retained_through_seq,
            ),
            ReferenceCapture::Selected { selection } => {
                (selection.record.sequence, selection.record.sequence)
            }
        }
    }
    /// Explicit omissions apply only to suffix capture.
    pub fn omissions(&self) -> &[ReferenceOmission] {
        match &self.capture {
            ReferenceCapture::Suffix { interval } => &interval.omissions,
            ReferenceCapture::Selected { .. } => &[],
        }
    }
}
