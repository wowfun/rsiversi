//! Optional output storage, opaque locators and generic streaming capture.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{fmt, sync::Arc};

/// Maximum complete bytes in one object.
pub const MAXIMUM_SPILL_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum EOF publication wait and total added wait for Agent text retention.
pub const PUBLICATION_WAIT: std::time::Duration = std::time::Duration::from_millis(250);
/// Default raw retrieval window.
pub const DEFAULT_SPILL_READ_BYTES: usize = 16 * 1024;
/// Maximum raw retrieval window or capture chunk.
pub const MAXIMUM_SPILL_READ_BYTES: usize = 64 * 1024;

/// A caller-owned storage namespace, independent of Session scheduling.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct SpillScope(#[schemars(length(equal = 64), regex(pattern = "^[0-9a-f]{64}$"))] String);

impl SpillScope {
    /// Hashes a trusted owner's namespace and identity without embedding paths.
    pub fn new(namespace: &str, identity: &str) -> Self {
        let mut digest = Sha256::new();
        digest.update((namespace.len() as u64).to_le_bytes());
        digest.update(namespace.as_bytes());
        digest.update(identity.as_bytes());
        Self(hex::encode(digest.finalize()))
    }
    /// Admits a serialized scope digest.
    pub fn parse(value: String) -> Result<Self> {
        if !is_hex(&value, 64) {
            return Err(SpillError::Invalid("invalid spill scope".into()));
        }
        Ok(Self(value))
    }
    /// Borrows the canonical digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for SpillScope {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Opaque completed object identity; never an arbitrary filesystem path.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct SpillLocator(
    #[schemars(length(equal = 97), regex(pattern = "^[0-9a-f]{64}-[0-9a-f]{32}$"))] String,
);
impl SpillLocator {
    /// Constructs a locator from a validated scope and backend-generated entropy.
    pub fn new(scope: &SpillScope, object: [u8; 16]) -> Self {
        Self(format!("{}-{}", scope.as_str(), hex::encode(object)))
    }
    /// Admits a serialized locator before filesystem lookup.
    pub fn parse(value: String) -> Result<Self> {
        if value.len() != 97
            || !value.is_ascii()
            || value.as_bytes()[64] != b'-'
            || !is_hex(&value[..64], 64)
            || !is_hex(&value[65..], 32)
        {
            return Err(SpillError::Invalid("invalid spill locator".into()));
        }
        Ok(Self(value))
    }
    /// Borrows the complete model-facing identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Borrows the already validated scope digest.
    pub fn scope_name(&self) -> &str {
        &self.0[..64]
    }
    /// Borrows the already validated object name.
    pub fn object_name(&self) -> &str {
        &self.0[65..]
    }
}
impl fmt::Display for SpillLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for SpillLocator {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A completed object reference, separate from durable Media references.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, try_from = "Reference")]
pub struct SpillRef {
    /// Opaque retrieval identity.
    pub locator: SpillLocator,
    /// Exact raw byte length.
    #[schemars(range(max = 67_108_864))]
    pub bytes: u64,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Reference {
    locator: SpillLocator,
    #[schemars(range(max = 67_108_864))]
    bytes: u64,
}
impl TryFrom<Reference> for SpillRef {
    type Error = SpillError;
    fn try_from(value: Reference) -> Result<Self> {
        if value.bytes > MAXIMUM_SPILL_BYTES {
            return Err(SpillError::Invalid(
                "spill byte count exceeds 64 MiB".into(),
            ));
        }
        Ok(Self {
            locator: value.locator,
            bytes: value.bytes,
        })
    }
}
/// One raw page; adapters transport bytes separately from JSON metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpillPage {
    /// Requested locator.
    pub locator: SpillLocator,
    /// Actual first byte offset.
    pub offset: u64,
    /// First byte after this page.
    pub next_offset: u64,
    /// Complete object length.
    pub total_bytes: u64,
    /// Exact bounded raw bytes.
    pub bytes: bytes::Bytes,
}

/// Validates a retrieval window before allocation or I/O.
pub fn validate_read_limit(limit: usize) -> Result<()> {
    if !(1..=MAXIMUM_SPILL_READ_BYTES).contains(&limit) {
        return Err(SpillError::Invalid(
            "read window must be 1..=65536 bytes".into(),
        ));
    }
    Ok(())
}
impl SpillPage {
    /// Checks an external provider's page against the admitted request.
    pub fn validate_for(&self, locator: &SpillLocator, offset: u64, limit: usize) -> Result<()> {
        validate_read_limit(limit)?;
        if &self.locator != locator
            || self.offset != offset
            || self.bytes.len() > limit
            || self.total_bytes > MAXIMUM_SPILL_BYTES
            || offset.checked_add(self.bytes.len() as u64) != Some(self.next_offset)
            || self.next_offset > self.total_bytes
            || (self.bytes.is_empty() && self.next_offset < self.total_bytes)
        {
            return Err(SpillError::Invalid("invalid completed-output page".into()));
        }
        Ok(())
    }
}

/// Output cache failure; it cannot change a producer's execution result.
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum SpillError {
    /// Malformed external retrieval input or configuration.
    #[error("invalid spill input: {0}")]
    Invalid(String),
    /// Missing, expired, fenced or failed object.
    #[error("spill unavailable: {0}")]
    Unavailable(String),
    /// An API adapter preserves transport failure classification.
    #[error(transparent)]
    Api(rsi_api_protocol::ApiError),
    /// Bounded provider admission is occupied.
    #[error("spill capacity is occupied")]
    Capacity,
    /// Provider retirement has closed admission.
    #[error("spill store is closed")]
    Closed,
}
/// Spill operation result.
pub type Result<T> = std::result::Result<T, SpillError>;

/// One nonblocking raw capture, shared by its producer and retained output view.
#[async_trait]
pub trait SpillCapture: fmt::Debug + Send + Sync {
    /// Enqueues a bounded chunk; false means full capture has been abandoned.
    fn push(&self, bytes: &[u8]) -> bool;
    /// Waits for bounded queue admission, suitable for already retained text.
    async fn write(&self, bytes: &[u8]) -> bool;
    /// Publishes only after every accepted byte has been written.
    async fn finish(&self) -> Option<SpillRef>;
    /// Returns a completed publication, if one won before abandonment.
    fn reference(&self) -> Option<SpillRef>;
    /// Prevents an unfinished capture from publishing later.
    fn abandon(&self);
}

/// A producer's scope-bound capture factory; contains no Session API.
pub trait CaptureSink: fmt::Debug + Send + Sync {
    /// Starts an independently bounded object, or declines best-effort capture.
    fn capture(&self) -> Option<Arc<dyn SpillCapture>>;
}

/// Read-only output authority suitable for authenticated API adapters.
#[async_trait]
pub trait SpillReader: fmt::Debug + Send + Sync {
    /// Reads at most 64 KiB after validating the requested window.
    async fn read(&self, locator: &SpillLocator, offset: u64, limit: usize) -> Result<SpillPage>;
}

/// Storage and scope cleanup, independent of result-retention policy.
#[async_trait]
pub trait SpillStore: SpillReader {
    /// Returns a capture sink tied to this exact namespace.
    fn scope(&self, scope: SpillScope) -> Arc<dyn CaptureSink>;
    /// Returns the provider's process-generation namespace for non-Session work.
    fn generation_scope(&self) -> SpillScope;
    /// Fences scope capture and publication before canonical Session retirement.
    fn fence(&self, scope: &SpillScope);
    /// Removes this exact fenced scope's generated objects; idempotent and retryable.
    async fn cleanup(&self, scope: &SpillScope) -> Result<()>;
    /// Observes whether a fenced scope has no remaining generated output.
    async fn is_clean(&self, scope: &SpillScope) -> Result<bool>;
    /// Saves supplied UTF-8 verbatim, without changing preview or canonical values.
    async fn save_text(&self, scope: SpillScope, text: &str) -> Result<SpillRef> {
        if text.len() as u64 > MAXIMUM_SPILL_BYTES {
            return Err(SpillError::Unavailable(
                "text exceeds the complete-object bound".into(),
            ));
        }
        let capture = self
            .scope(scope)
            .capture()
            .ok_or_else(|| SpillError::Unavailable("capture was declined".into()))?;
        for chunk in text.as_bytes().chunks(MAXIMUM_SPILL_READ_BYTES) {
            if !capture.write(chunk).await {
                return Err(SpillError::Unavailable("capture write failed".into()));
            }
        }
        capture
            .finish()
            .await
            .ok_or_else(|| SpillError::Unavailable("capture did not publish".into()))
    }
}

/// Local write and cleanup authority.
#[derive(Debug)]
pub struct SpillStoreContract;
impl LocalContract for SpillStoreContract {
    const KEY: &'static str = "rsi.spill.store";
    type Service = dyn SpillStore;
}
/// Local read-only authority.
#[derive(Debug)]
pub struct SpillReaderContract;
impl LocalContract for SpillReaderContract {
    const KEY: &'static str = "rsi.spill.reader";
    type Service = dyn SpillReader;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_locators_and_lengths_cannot_escape_the_generated_namespace() {
        let scope = SpillScope::new("session", "fixture");
        let locator = SpillLocator::new(&scope, [0xab; 16]);
        assert_eq!(SpillLocator::parse(locator.to_string()).unwrap(), locator);
        for invalid in ["../output".to_owned(), "f".repeat(97), "中".repeat(33)] {
            assert!(SpillLocator::parse(invalid).is_err());
        }
        assert_ne!(scope, SpillScope::new("generation", "fixture"));
        assert!(
            serde_json::from_value::<SpillRef>(
                serde_json::json!({"locator":locator,"bytes":MAXIMUM_SPILL_BYTES + 1})
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod output_page_tests {
    use super::*;
    #[test]
    fn remote_output_rejects_unbounded_or_nonadvancing_pages() {
        let id = SpillLocator::new(&SpillScope::new("fixture", "page"), [0; 16]);
        let mut page = SpillPage {
            locator: id.clone(),
            offset: 0,
            next_offset: 1,
            total_bytes: 1,
            bytes: vec![b'x'].into(),
        };
        page.validate_for(&id, 0, 1).unwrap();
        page.total_bytes = u64::MAX;
        assert!(page.validate_for(&id, 0, 1).is_err());
        page.total_bytes = 1;
        page.bytes.clear();
        page.next_offset = 0;
        assert!(page.validate_for(&id, 0, 1).is_err());
        page.total_bytes = 0;
        page.validate_for(&id, 0, 1).unwrap();
    }
}

/// Renders the same retrieval instruction for retained and later-pruned output.
pub fn retrieval_notice(reference: &SpillRef) -> String {
    format!(
        "[Full output: {} bytes. Read with output_read: {{\"id\":\"{}\",\"offset\":0,\"limit\":65536}}]",
        reference.bytes, reference.locator
    )
}
