//! Finite export stream shared by local and remote clients.
use crate::{Result, SessionError};
use rsi_agent_session_protocol::SessionId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, pin::Pin};

/// Maximum source bytes in one export chunk.
pub const CHUNK_BYTES: usize = 64 * 1024;
/// Finite, backpressured artifact stream. Dropping it cancels further work.
pub type ExportStream = Pin<Box<dyn futures_util::Stream<Item = Result<ExportEvent>> + Send>>;

/// Artifact encoding.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Readable Markdown.
    #[default]
    Markdown,
    /// Structured JSON.
    Json,
}
impl ExportFormat {
    /// Conventional file suffix.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }
}
/// Exact selectable artifact sections.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExportInclude {
    /// Redacted session metadata.
    Header,
    /// Caller-facing conversation records.
    Messages,
    /// Readable reasoning, also enabling messages.
    Reasoning,
    /// Recorded model input inspection evidence.
    ProviderInputEvidence,
    /// Approximate semantic input for the last completed conversation call.
    LastProviderRequest,
    /// Normalized persisted output of that call.
    LastProviderResponse,
}
/// Options have no filesystem or execution authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportOptions {
    /// Requested encoding.
    pub format: ExportFormat,
    /// Exact section set; reasoning requires messages.
    pub include: BTreeSet<ExportInclude>,
}
impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Markdown,
            include: BTreeSet::from([ExportInclude::Messages]),
        }
    }
}
impl ExportOptions {
    /// Rejects noncanonical empty or unexpanded option sets.
    pub fn validate(&self) -> Result<()> {
        if self.include.is_empty()
            || (self.has(ExportInclude::Reasoning) && !self.has(ExportInclude::Messages))
        {
            return Err(invalid("invalid export include set"));
        }
        Ok(())
    }
    /// Whether a section is selected.
    pub fn has(&self, section: ExportInclude) -> bool {
        self.include.contains(&section)
    }
    /// Parses the user-facing include vocabulary and expands reasoning.
    pub fn select(&mut self, value: &str) -> Result<()> {
        let mut include = BTreeSet::new();
        for token in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            include.insert(match token {
                "header" | "h" => ExportInclude::Header,
                "messages" | "m" => ExportInclude::Messages,
                "reasoning" | "r" => ExportInclude::Reasoning,
                "provider-input-evidence" | "pie" => ExportInclude::ProviderInputEvidence,
                "last-provider-request" | "lpr" => ExportInclude::LastProviderRequest,
                "last-provider-response" => ExportInclude::LastProviderResponse,
                _ => return Err(invalid("unknown export include value")),
            });
        }
        if include.contains(&ExportInclude::Reasoning) {
            include.insert(ExportInclude::Messages);
        }
        let options = Self {
            format: self.format,
            include,
        };
        options.validate()?;
        *self = options;
        Ok(())
    }
}
/// Export framing. Decimal strings preserve identities through JavaScript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExportEvent {
    /// Exactly one opening item, before any bytes.
    Start {
        /// Authorized source.
        session_id: SessionId,
        /// Frozen Header identity.
        header_sha256: String,
        /// Inclusive durable source cut, in canonical decimal.
        through_seq: String,
        /// Canonical accepted options.
        options: ExportOptions,
        /// Safe filename suggestion.
        filename: String,
    },
    /// One nonempty bounded UTF-8 fragment.
    Chunk {
        /// Byte offset in canonical decimal.
        offset: String,
        /// Artifact bytes as UTF-8.
        text: String,
    },
    /// Exactly one terminal item; transport EOF must follow.
    Complete {
        /// Total UTF-8 bytes in canonical decimal.
        bytes: String,
        /// SHA-256 of concatenated chunks.
        sha256: String,
    },
}
/// Checks the complete stream while retaining only its hash and counters.
#[derive(Debug, Default)]
pub struct ExportVerifier {
    started: bool,
    complete: bool,
    offset: u64,
    hash: Sha256,
}
impl ExportVerifier {
    /// Validates one event, optionally enforcing the initiating handle and options.
    pub fn accept(
        &mut self,
        event: &ExportEvent,
        expected: Option<(&SessionId, &str, &ExportOptions)>,
    ) -> Result<()> {
        if self.complete {
            return Err(invalid("export item follows completion"));
        }
        match event {
            ExportEvent::Start {
                session_id,
                header_sha256,
                through_seq,
                options,
                filename,
            } => {
                options.validate()?;
                decimal(through_seq)?;
                if self.started
                    || !digest(header_sha256)
                    || filename.is_empty()
                    || matches!(filename.as_str(), "." | "..")
                    || filename.len() > 256
                    || filename.contains(['/', '\\', '\r', '\n', '\0'])
                    || expected.is_some_and(|(id, header, selected)| {
                        id != session_id || header != header_sha256 || selected != options
                    })
                {
                    return Err(invalid("export start does not match its request"));
                }
                self.started = true;
            }
            ExportEvent::Chunk { offset, text } => {
                if !self.started
                    || text.is_empty()
                    || text.len() > CHUNK_BYTES
                    || decimal(offset)? != self.offset
                {
                    return Err(invalid("invalid export chunk or offset"));
                }
                self.offset = self
                    .offset
                    .checked_add(text.len() as u64)
                    .ok_or_else(|| invalid("export length overflow"))?;
                self.hash.update(text.as_bytes());
            }
            ExportEvent::Complete { bytes, sha256 } => {
                if !self.started
                    || decimal(bytes)? != self.offset
                    || *sha256 != hex::encode(self.hash.clone().finalize())
                {
                    return Err(invalid("export completion length or digest mismatch"));
                }
                self.complete = true;
            }
        }
        Ok(())
    }
    /// Rejects a truncated stream.
    pub fn finish(&self) -> Result<u64> {
        if !self.complete {
            return Err(invalid("export ended without verified completion"));
        }
        Ok(self.offset)
    }
}
fn decimal(text: &str) -> Result<u64> {
    text.parse::<u64>()
        .ok()
        .filter(|n| n.to_string() == text)
        .ok_or_else(|| invalid("invalid export cursor"))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn invalid(message: &str) -> SessionError {
    SessionError::Invalid(message.into())
}

/// Stable filename with a safe readable prefix and collision-resistant identity.
pub fn default_filename(id: &SessionId, format: ExportFormat) -> String {
    let prefix: String = id
        .as_str()
        .chars()
        .take(48)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let hash = hex::encode(Sha256::digest(id.as_str().as_bytes()));
    format!(
        "rsi-session-{prefix}-{}.{}",
        &hash[..16],
        format.extension()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn start() -> ExportEvent {
        ExportEvent::Start {
            session_id: SessionId::new("session").unwrap(),
            header_sha256: "a".repeat(64),
            through_seq: "0".into(),
            options: ExportOptions::default(),
            filename: "session.md".into(),
        }
    }
    #[test]
    fn exact_selection_and_reasoning_aliases() {
        let mut options = ExportOptions::default();
        for (names, expected) in [
            ("header,h", BTreeSet::from([ExportInclude::Header])),
            ("messages,m", BTreeSet::from([ExportInclude::Messages])),
            (
                "reasoning,r",
                BTreeSet::from([ExportInclude::Messages, ExportInclude::Reasoning]),
            ),
            (
                "provider-input-evidence,pie",
                BTreeSet::from([ExportInclude::ProviderInputEvidence]),
            ),
            (
                "last-provider-request,lpr",
                BTreeSet::from([ExportInclude::LastProviderRequest]),
            ),
            (
                "last-provider-response",
                BTreeSet::from([ExportInclude::LastProviderResponse]),
            ),
        ] {
            options.select(names).unwrap();
            assert_eq!(options.include, expected);
        }
        for invalid in ["", ",", "all", "lpresp", "message"] {
            assert!(options.select(invalid).is_err());
        }
    }
    #[test]
    fn filename_suggestions_cannot_be_dot_components() {
        for name in [".", ".."] {
            let mut event = start();
            let ExportEvent::Start { filename, .. } = &mut event else {
                unreachable!()
            };
            *filename = name.into();
            assert!(
                ExportVerifier::default().accept(&event, None).is_err(),
                "{name}"
            );
        }
    }
    #[test]
    fn framing_rejects_gaps_oversize_corruption_and_truncation() {
        for invalid in [
            start(),
            ExportEvent::Chunk {
                offset: "1".into(),
                text: "x".into(),
            },
            ExportEvent::Chunk {
                offset: "00".into(),
                text: "x".into(),
            },
            ExportEvent::Chunk {
                offset: "0".into(),
                text: String::new(),
            },
            ExportEvent::Chunk {
                offset: "0".into(),
                text: "x".repeat(CHUNK_BYTES + 1),
            },
            ExportEvent::Complete {
                bytes: "0".into(),
                sha256: "a".repeat(64),
            },
        ] {
            let mut verifier = ExportVerifier::default();
            verifier.accept(&start(), None).unwrap();
            assert!(verifier.accept(&invalid, None).is_err());
            assert!(verifier.finish().is_err());
        }
        let mut verifier = ExportVerifier::default();
        verifier.accept(&start(), None).unwrap();
        verifier
            .accept(
                &ExportEvent::Chunk {
                    offset: "0".into(),
                    text: "🦀".into(),
                },
                None,
            )
            .unwrap();
        assert!(verifier.finish().is_err());
        verifier
            .accept(
                &ExportEvent::Complete {
                    bytes: "4".into(),
                    sha256: hex::encode(Sha256::digest("🦀".as_bytes())),
                },
                None,
            )
            .unwrap();
        assert_eq!(verifier.finish().unwrap(), 4);
        assert!(verifier.accept(&start(), None).is_err());
    }
}
