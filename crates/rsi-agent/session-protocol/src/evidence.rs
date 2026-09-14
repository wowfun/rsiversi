//! Atomic optional request evidence, without another durable object store.

use crate::{Result, SessionError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum decoded evidence bytes per request and new inline bytes per Turn.
pub const MAXIMUM_REQUEST_EVIDENCE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum occurrences of one content kind in the inspection manifest.
pub const MAXIMUM_EVIDENCE_CONTENT_COUNT: usize = 131_072;

/// One of the three independently reusable request sections.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSection {
    /// Prepared route, generation and semantic generation settings.
    Configuration,
    /// Actual system and developer instructions in request order.
    System,
    /// Declared caller/hosted tools and tool selection policy.
    Tools,
}

/// Exact UTF-8 section or a direct reference to an earlier inline section.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidencePart {
    /// Complete source bytes in this intent.
    Inline {
        /// Complete decoded UTF-8 source bytes.
        text: String,
        /// Lowercase SHA-256 of those bytes.
        sha256: String,
    },
    /// Original inline bytes in this Session; references cannot be targets.
    Reference {
        /// Earlier original `ModelIntent` in the same Session.
        seq: u64,
        /// Same named section as this part's position.
        section: EvidenceSection,
        /// Digest of the original inline source.
        sha256: String,
        /// Decoded source length, charged even when referenced.
        bytes: u32,
    },
}
impl EvidencePart {
    /// Captures exact source bytes and their digest.
    pub fn inline(text: String) -> Self {
        let sha256 = hex::encode(Sha256::digest(text.as_bytes()));
        Self::Inline { text, sha256 }
    }
    /// Lowercase SHA-256 of decoded source bytes.
    pub fn sha256(&self) -> &str {
        match self {
            Self::Inline { sha256, .. } | Self::Reference { sha256, .. } => sha256,
        }
    }
    /// Decoded bytes, regardless of physical representation.
    pub fn bytes(&self) -> usize {
        match self {
            Self::Inline { text, .. } => text.len(),
            Self::Reference { bytes, .. } => *bytes as usize,
        }
    }
    fn validate(&self, expected: EvidenceSection, intent_seq: Option<u64>) -> Result<()> {
        if self.bytes() > MAXIMUM_REQUEST_EVIDENCE_BYTES
            || self.sha256().len() != 64
            || !self
                .sha256()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(SessionError::Invalid(
                "invalid evidence length or digest".into(),
            ));
        }
        match self {
            Self::Inline { text, sha256 }
                if hex::encode(Sha256::digest(text.as_bytes())) != *sha256 =>
            {
                return Err(SessionError::Invalid(
                    "evidence inline digest mismatch".into(),
                ));
            }
            Self::Reference { seq, section, .. }
                if *seq == 0
                    || *section != expected
                    || intent_seq.is_some_and(|current| *seq >= current) =>
            {
                return Err(SessionError::Invalid(
                    "evidence reference is not an earlier exact section".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

/// Content types represented in the actual request; no content body is repeated.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceContentKind {
    /// Ordinary visible text.
    Text,
    /// Provider reasoning text already present in conversation history.
    Reasoning,
    /// Historical model-requested tool arguments.
    ToolCall,
    /// Historical result envelopes; child content has separate counts.
    ToolResult,
    /// Declared image bytes, without their bodies or locators.
    Image,
    /// Declared audio bytes, without their bodies or locators.
    Audio,
}

/// Number and source byte length of one ordinary content kind.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceContentCount {
    /// Exact semantic category.
    pub kind: EvidenceContentKind,
    /// Positive number of actual occurrences.
    pub count: u32,
    /// UTF-8 or declared media bytes; envelope-only items contribute zero.
    pub bytes: u64,
}

/// A closed explanation when optional request inspection cannot be retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceUnavailable {
    /// Optional evidence exceeded its own or the Turn's remaining byte budget.
    Budget,
    /// A producer did not capture evidence for this intent.
    NotCaptured,
}

/// Complete optional inspection data published with one prepared model intent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "availability",
    rename_all = "snake_case",
    try_from = "EvidenceWire"
)]
pub enum RequestEvidence {
    /// Three exact sections and a bounded ordinary-content manifest.
    Available {
        /// Actual selected settings and prepared route facts.
        configuration: EvidencePart,
        /// System/developer messages in their original request order.
        system: EvidencePart,
        /// Caller tools, hosted tools and exact selection policy.
        tools: EvidencePart,
        /// Ordinary-content counts, without a second copy of their bodies.
        manifest: Vec<EvidenceContentCount>,
    },
    /// The entire package is absent for this explicit reason.
    Unavailable {
        /// Why this entire optional package was omitted.
        reason: EvidenceUnavailable,
    },
}
#[derive(Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceWire {
    Available {
        configuration: EvidencePart,
        system: EvidencePart,
        tools: EvidencePart,
        manifest: Vec<EvidenceContentCount>,
    },
    Unavailable {
        reason: EvidenceUnavailable,
    },
}
impl TryFrom<EvidenceWire> for RequestEvidence {
    type Error = SessionError;
    fn try_from(wire: EvidenceWire) -> Result<Self> {
        let evidence = match wire {
            EvidenceWire::Available {
                configuration,
                system,
                tools,
                manifest,
            } => Self::Available {
                configuration,
                system,
                tools,
                manifest,
            },
            EvidenceWire::Unavailable { reason } => Self::Unavailable { reason },
        };
        evidence.validate(None)?;
        Ok(evidence)
    }
}
impl RequestEvidence {
    /// Iterates the three source sections in deterministic order.
    pub fn parts(&self) -> Vec<(EvidenceSection, &EvidencePart)> {
        match self {
            Self::Available {
                configuration,
                system,
                tools,
                ..
            } => vec![
                (EvidenceSection::Configuration, configuration),
                (EvidenceSection::System, system),
                (EvidenceSection::Tools, tools),
            ],
            Self::Unavailable { .. } => vec![],
        }
    }
    /// Finds one section without resolving references.
    pub fn part(&self, section: EvidenceSection) -> Option<&EvidencePart> {
        self.parts()
            .into_iter()
            .find(|(kind, _)| *kind == section)
            .map(|(_, part)| part)
    }
    /// Mutable section access for metadata-only deduplication before publication.
    pub fn part_mut(&mut self, section: EvidenceSection) -> Option<&mut EvidencePart> {
        match self {
            Self::Available {
                configuration,
                system,
                tools,
                ..
            } => Some(match section {
                EvidenceSection::Configuration => configuration,
                EvidenceSection::System => system,
                EvidenceSection::Tools => tools,
            }),
            Self::Unavailable { .. } => None,
        }
    }
    /// Newly retained source bytes, excluding direct references.
    pub fn inline_bytes(&self) -> usize {
        self.parts()
            .iter()
            .map(|(_, part)| match part {
                EvidencePart::Inline { text, .. } => text.len(),
                EvidencePart::Reference { .. } => 0,
            })
            .sum()
    }
    /// Revalidates decoded section bounds, identity and a bounded content manifest.
    pub fn validate(&self, intent_seq: Option<u64>) -> Result<()> {
        let mut bytes = 0usize;
        for (section, part) in self.parts() {
            part.validate(section, intent_seq)?;
            bytes = bytes
                .checked_add(part.bytes())
                .ok_or_else(|| SessionError::Invalid("evidence bytes overflow".into()))?;
        }
        if bytes > MAXIMUM_REQUEST_EVIDENCE_BYTES {
            return Err(SessionError::Invalid(
                "evidence exceeds 16 MiB decoded bytes".into(),
            ));
        }
        if let Self::Available { manifest, .. } = self
            && (manifest.len() > 6
                || manifest.windows(2).any(|pair| pair[0].kind >= pair[1].kind)
                || manifest.iter().any(|item| {
                    item.count == 0
                        || item.count as usize > MAXIMUM_EVIDENCE_CONTENT_COUNT
                        || item.bytes > 512 * 1024 * 1024
                }))
        {
            return Err(SessionError::Invalid(
                "invalid request content manifest".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn available() -> RequestEvidence {
        RequestEvidence::Available {
            configuration: EvidencePart::inline("{}".into()),
            system: EvidencePart::inline("system".into()),
            tools: EvidencePart::inline("[]".into()),
            manifest: vec![],
        }
    }
    #[test]
    fn sections_decode_closed_and_references_bind_an_earlier_exact_section() {
        let evidence = available();
        assert_eq!(
            serde_json::from_value::<RequestEvidence>(serde_json::to_value(&evidence).unwrap())
                .unwrap(),
            evidence
        );
        let mut wire = serde_json::to_value(&evidence).unwrap();
        wire["configuration"]["sha256"] = serde_json::json!("0".repeat(64));
        assert!(serde_json::from_value::<RequestEvidence>(wire).is_err());
        let mut wire = serde_json::to_value(&evidence).unwrap();
        wire["secret"] = serde_json::json!("extra");
        assert!(serde_json::from_value::<RequestEvidence>(wire).is_err());
        let mut reference = evidence.clone();
        *reference.part_mut(EvidenceSection::System).unwrap() = EvidencePart::Reference {
            seq: 4,
            section: EvidenceSection::System,
            sha256: evidence
                .part(EvidenceSection::System)
                .unwrap()
                .sha256()
                .into(),
            bytes: 6,
        };
        assert!(reference.validate(Some(5)).is_ok());
        assert!(reference.validate(Some(4)).is_err());
        let EvidencePart::Reference { section, .. } =
            reference.part_mut(EvidenceSection::System).unwrap()
        else {
            unreachable!()
        };
        *section = EvidenceSection::Tools;
        assert!(reference.validate(Some(5)).is_err());
    }
    #[test]
    fn decoded_budget_counts_references_and_inline_bytes_separately() {
        let mut evidence = available();
        *evidence.part_mut(EvidenceSection::System).unwrap() = EvidencePart::Reference {
            seq: 1,
            section: EvidenceSection::System,
            sha256: "0".repeat(64),
            bytes: u32::try_from(MAXIMUM_REQUEST_EVIDENCE_BYTES).unwrap(),
        };
        assert_eq!(evidence.inline_bytes(), 4);
        assert!(evidence.validate(Some(2)).is_err());
        if let RequestEvidence::Available { manifest, .. } = &mut evidence {
            manifest.push(EvidenceContentCount {
                kind: EvidenceContentKind::Text,
                count: 0,
                bytes: 1,
            });
        }
        assert!(evidence.validate(Some(2)).is_err());
    }
}
