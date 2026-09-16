//! Finite retrieval inputs and durable external-source values; no network code.
use rsi_agent_session_protocol::{DomainIdentity, DomainSnapshot, DomainStateValue};
use rsi_credentials_protocol::CredentialRef;
use serde::{Deserialize, Serialize};

/// Maximum encoded URL bytes.
pub const MAX_URL: usize = 2 * 1024;
/// Maximum compressed HTTP body bytes.
pub const MAX_WIRE: usize = 4 * 1024 * 1024;
/// Maximum decoded body bytes.
pub const MAX_DECODED: usize = 8 * 1024 * 1024;
/// Maximum extracted UTF-8 text bytes across sources.
pub const MAX_TEXT: usize = 256 * 1024;
/// Owner Settings namespace.
pub const SETTINGS_NAMESPACE: &str = "rsi.retrieval";
/// Exact pre-seal Domain identity, codec version one.
pub const CONFIG_DOMAIN: &str = "rsi.retrieval.config";

/// Independent default-off flags captured before Tool catalog sealing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Admit the public HTTP/S fetch Tool.
    pub web_fetch: bool,
    /// Admit the Exa search Tool; credential storage is independent.
    pub web_search: bool,
}
impl RetrievalConfig {
    /// Validates a decoded Domain using the common owner codec interface.
    ///
    /// # Errors
    /// Typed boolean flags have no invalid combination; validation always succeeds.
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }
    /// Captures the exact immutable configuration for fresh composition.
    #[expect(
        clippy::missing_panics_doc,
        reason = "Only fixed validated constants and infallible JSON flag serialization are unwrapped."
    )]
    pub fn snapshot(&self) -> DomainSnapshot {
        DomainSnapshot::new(
            DomainIdentity::new(CONFIG_DOMAIN, 1).expect("static retrieval Domain"),
            DomainStateValue::new(serde_json::to_value(self).expect("retrieval flags"))
                .expect("bounded retrieval flags"),
        )
    }
}
/// Fixed owner-scoped Exa secret; never a model-provider credential.
#[expect(
    clippy::missing_panics_doc,
    reason = "Only fixed validated constants and infallible JSON flag serialization are unwrapped."
)]
pub fn exa_credential() -> CredentialRef {
    CredentialRef::new("rsi.retrieval", "exa").expect("static Exa credential")
}
/// Stable, redacted operation failures suitable for `ToolResults` and configuration UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalError {
    #[error("Web retrieval is disabled in current settings")]
    Disabled,
    #[error("Web retrieval settings could not be read or decoded")]
    Configuration,
    #[error("Web retrieval worker failed")]
    WorkerFailed,
    #[error("Invalid retrieval arguments")]
    InvalidInput,
    #[error("URL is outside the public HTTP/S boundary")]
    BlockedUrl,
    #[error("Public destination resolution failed")]
    Resolution,
    #[error("Web request failed")]
    Network,
    #[error("Web server returned an unsuccessful status")]
    HttpStatus,
    #[error("Web redirect exceeded the same-origin or five-hop limit")]
    Redirect,
    #[error("Web response exceeded its wire or decoded byte limit")]
    Capacity,
    #[error("Web response media type, encoding or charset is unsupported")]
    UnsupportedContent,
    #[error("Web response could not be decoded")]
    Decode,
    #[error("Exa credential is unavailable; save it separately in Settings")]
    MissingCredential,
    #[error("Exa returned an invalid response")]
    Protocol,
    #[error("Web retrieval is busy; no request was started")]
    Busy,
    #[error("Web retrieval timed out; the started request was not replayed")]
    Timeout,
    #[error("Web retrieval was cancelled; the started request was not replayed")]
    Cancelled,
}
/// Public source operation, with distinct evidence semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalOperation {
    Fetch,
    Search,
}
/// Immutable attributed source. Text is always external data, never authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievedSource {
    pub url: String,
    pub title: String,
    pub text: String,
    pub published_at: Option<String>,
    pub truncated: bool,
}
/// Durable `ToolResult` value; renderers never refetch these sources.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalResult {
    pub version: u8,
    pub operation: RetrievalOperation,
    /// Original fetch URL or search query, preserving the request's attribution.
    pub request: String,
    pub sources: Vec<RetrievedSource>,
    /// Provider entries omitted because they have no safe URL or useful highlight.
    pub omitted: u32,
    pub truncated: bool,
}
impl RetrievalResult {
    /// Validates historical external data before projection.
    /// Validates bounded recorded source data.
    ///
    /// # Errors
    /// Rejects invalid versions, URLs, source counts, field sizes and inconsistent operations.
    pub fn validate(&self) -> Result<(), RetrievalError> {
        if self.version != 1
            || self.request.is_empty()
            || self.request.len() > 8192
            || self.sources.len() > 10
            || self.omitted > 1024
            || (self.operation == RetrievalOperation::Fetch
                && (self.sources.len() != 1 || self.omitted != 0 || !safe_url(&self.request)))
        {
            return Err(RetrievalError::Protocol);
        }
        let mut bytes = 0usize;
        for source in &self.sources {
            bytes += source.text.len();
            if !safe_url(&source.url)
                || source.title.len() > 1024
                || source
                    .published_at
                    .as_ref()
                    .is_some_and(|date| date.len() > 128)
                || bytes > MAX_TEXT
                || (source.truncated && !self.truncated)
            {
                return Err(RetrievalError::Protocol);
            }
        }
        Ok(())
    }
}
/// Syntactic link validation only. The network owner additionally checks DNS.
pub fn safe_url(value: &str) -> bool {
    if value.len() > MAX_URL {
        return false;
    }
    url::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_source_boundaries_are_not_html_or_javascript_links() {
        assert!(!safe_url("javascript:alert(1)"));
        assert!(!safe_url("https://user:pass@example.com"));
        assert!(safe_url("https://example.com/a#b"));
        assert!(
            serde_json::from_str::<RetrievalConfig>(
                r#"{"web_fetch":true,"web_search":false,"proxy":"x"}"#
            )
            .is_err()
        );
        assert_eq!(
            RetrievalConfig::default().snapshot().identity().id(),
            CONFIG_DOMAIN
        );
    }
}
