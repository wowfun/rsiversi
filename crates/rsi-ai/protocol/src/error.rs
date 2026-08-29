use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::MAX_ERROR_SUMMARY_BYTES;

/// Produces a non-empty, safe, UTF-8-boundary-preserving provider error summary.
#[must_use]
pub fn sanitize_error_summary(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(MAX_ERROR_SUMMARY_BYTES));
    for character in value.chars() {
        let character = if matches!(character, '\0' | '\u{7f}') {
            '\u{fffd}'
        } else {
            character
        };
        if output.len().saturating_add(character.len_utf8()) > MAX_ERROR_SUMMARY_BYTES {
            break;
        }
        output.push(character);
    }
    if output.is_empty() {
        output.push_str("provider error");
    }
    output
}

/// Provider-neutral category of an AI operation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The caller supplied a request outside the provider-neutral contract.
    InvalidRequest,
    /// The selected provider does not implement the requested capability or setting.
    Unsupported,
    /// The provider rejected or could not resolve the configured credential.
    Authentication,
    /// The credential is valid but lacks authority for the requested operation.
    Permission,
    /// The selected provider resource does not exist.
    NotFound,
    /// The provider temporarily refused work because of a rate limit.
    RateLimited,
    /// The account has exhausted a billing or usage quota.
    Quota,
    /// The provider rejected the request because its model context limit was exceeded.
    ContextLimit,
    /// A finite operation deadline elapsed.
    Timeout,
    /// The transport failed without a valid provider response.
    Transport,
    /// The provider reported an internal or unavailable-server failure.
    Server,
    /// Provider output could not satisfy the normalized semantic contract.
    OutputValidation,
    /// Provider or wire traffic violated its declared protocol.
    Protocol,
    /// The caller or owner cancelled the operation.
    Cancelled,
    /// Failure occurred after dispatch may have crossed the external-effect seam.
    DispatchUncertain,
    /// A provider-managed deferred operation no longer exists.
    RemoteExpired,
    /// Media resolution, validation, or durable artifact handling failed.
    Artifact,
}

impl ErrorKind {
    /// Stable provider-neutral code used across library and plugin boundaries.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "provider.invalid_request",
            Self::Unsupported => "provider.unsupported",
            Self::Authentication => "provider.authentication",
            Self::Permission => "provider.permission",
            Self::NotFound => "provider.not_found",
            Self::RateLimited => "provider.rate_limited",
            Self::Quota => "provider.quota",
            Self::ContextLimit => "provider.context_limit",
            Self::Timeout => "provider.timeout",
            Self::Transport => "provider.transport",
            Self::Server => "provider.server",
            Self::OutputValidation => "provider.output_validation",
            Self::Protocol => "provider.protocol",
            Self::Cancelled => "provider.cancelled",
            Self::DispatchUncertain => "provider.dispatch_uncertain",
            Self::RemoteExpired => "provider.remote_expired",
            Self::Artifact => "provider.artifact",
        }
    }

    /// Decodes a stable code emitted by [`Self::code`].
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "provider.invalid_request" => Self::InvalidRequest,
            "provider.unsupported" => Self::Unsupported,
            "provider.authentication" => Self::Authentication,
            "provider.permission" => Self::Permission,
            "provider.not_found" => Self::NotFound,
            "provider.rate_limited" => Self::RateLimited,
            "provider.quota" => Self::Quota,
            "provider.context_limit" => Self::ContextLimit,
            "provider.timeout" => Self::Timeout,
            "provider.transport" => Self::Transport,
            "provider.server" => Self::Server,
            "provider.output_validation" => Self::OutputValidation,
            "provider.protocol" => Self::Protocol,
            "provider.cancelled" => Self::Cancelled,
            "provider.dispatch_uncertain" => Self::DispatchUncertain,
            "provider.remote_expired" => Self::RemoteExpired,
            "provider.artifact" => Self::Artifact,
            _ => return None,
        })
    }
}

/// Operation phase in which a failure became observable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorPhase {
    /// Request validation and provider preparation before external I/O.
    Prepare,
    /// Establishing the provider transport connection.
    Connect,
    /// Sending request headers or body bytes.
    Send,
    /// Waiting for the first semantic provider event.
    FirstEvent,
    /// Reading or translating an active response stream.
    Stream,
    /// Validating and assembling normalized output.
    Assemble,
    /// Submitting a provider-managed background response.
    DeferredSubmit,
    /// Polling or resuming a provider-managed response.
    DeferredPoll,
    /// Cancelling a provider-managed response.
    DeferredCancel,
}

/// What is known about whether an external request crossed its effect seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    /// No provider attempt was created.
    NotStarted,
    /// An attempt began locally but the request was proven not to be dispatched.
    NotDispatched,
    /// The request crossed the provider effect seam.
    Dispatched,
    /// The failure leaves dispatch unknowable and callers must not retry blindly.
    Unknown,
}

/// Safe, serializable provider failure facts. Policy decides retryability.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize)]
#[error("{safe_summary}")]
#[serde(deny_unknown_fields)]
pub struct AiError {
    kind: ErrorKind,
    phase: ErrorPhase,
    dispatch_status: DispatchStatus,
    status: Option<u16>,
    provider_code: Option<String>,
    retry_after_ms: Option<u64>,
    request_id: Option<String>,
    safe_summary: String,
}

impl<'de> Deserialize<'de> for AiError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireError {
            kind: ErrorKind,
            phase: ErrorPhase,
            dispatch_status: DispatchStatus,
            status: Option<u16>,
            provider_code: Option<String>,
            retry_after_ms: Option<u64>,
            request_id: Option<String>,
            safe_summary: String,
        }

        let wire = WireError::deserialize(deserializer)?;
        let error = Self {
            kind: wire.kind,
            phase: wire.phase,
            dispatch_status: wire.dispatch_status,
            status: wire.status,
            provider_code: wire.provider_code,
            retry_after_ms: wire.retry_after_ms,
            request_id: wire.request_id,
            safe_summary: wire.safe_summary,
        };
        error
            .validate()
            .map(|()| error)
            .map_err(serde::de::Error::custom)
    }
}

impl AiError {
    pub(crate) fn deferred_unsupported() -> Self {
        Self {
            kind: ErrorKind::Unsupported,
            phase: ErrorPhase::Prepare,
            dispatch_status: DispatchStatus::NotStarted,
            status: None,
            provider_code: None,
            retry_after_ms: None,
            request_id: None,
            safe_summary: "provider does not support deferred language responses".into(),
        }
    }

    /// Creates bounded error facts without retaining a raw provider body.
    pub fn new(
        kind: ErrorKind,
        phase: ErrorPhase,
        dispatch_status: DispatchStatus,
        safe_summary: impl Into<String>,
    ) -> Result<Self, ErrorFactsError> {
        let safe_summary = safe_summary.into();
        validate_safe_text("safe_summary", &safe_summary, MAX_ERROR_SUMMARY_BYTES)?;
        Ok(Self {
            kind,
            phase,
            dispatch_status,
            status: None,
            provider_code: None,
            retry_after_ms: None,
            request_id: None,
            safe_summary,
        })
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub const fn phase(&self) -> ErrorPhase {
        self.phase
    }

    pub const fn dispatch_status(&self) -> DispatchStatus {
        self.dispatch_status
    }

    /// Reclassifies an already-validated error without rebuilding or dropping its facts.
    #[must_use]
    pub const fn with_kind(mut self, kind: ErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Returns the validated HTTP status, when the provider exposed one.
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    /// Returns the bounded provider-specific error code, when available.
    pub fn provider_code(&self) -> Option<&str> {
        self.provider_code.as_deref()
    }

    /// Returns the provider-requested delay in milliseconds, without applying retry policy.
    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }

    /// Returns the provider request identifier retained for safe diagnostics.
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Returns the bounded summary safe to persist or display.
    pub fn safe_summary(&self) -> &str {
        &self.safe_summary
    }

    /// Revalidates deserialized error facts at an untrusted boundary.
    pub fn validate(&self) -> Result<(), ErrorFactsError> {
        validate_safe_text("safe_summary", &self.safe_summary, MAX_ERROR_SUMMARY_BYTES)?;
        if self
            .status
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(ErrorFactsError("status must be an HTTP status".to_owned()));
        }
        if let Some(code) = &self.provider_code {
            validate_ascii_token("provider_code", code)?;
        }
        if let Some(request_id) = &self.request_id {
            validate_ascii_token("request_id", request_id)?;
        }
        Ok(())
    }

    /// Attaches a validated HTTP status observed at the provider seam.
    pub fn with_status(mut self, status: u16) -> Result<Self, ErrorFactsError> {
        if !(100..=599).contains(&status) {
            return Err(ErrorFactsError("status must be an HTTP status".to_owned()));
        }
        self.status = Some(status);
        Ok(self)
    }

    /// Attaches a provider-requested retry delay.
    #[must_use]
    pub const fn with_retry_after_ms(mut self, retry_after_ms: u64) -> Self {
        self.retry_after_ms = Some(retry_after_ms);
        self
    }

    /// Attaches a bounded provider code.
    pub fn with_provider_code(mut self, code: impl Into<String>) -> Result<Self, ErrorFactsError> {
        let code = code.into();
        validate_ascii_token("provider_code", &code)?;
        self.provider_code = Some(code);
        Ok(self)
    }

    /// Attaches a bounded request identifier safe for diagnostics.
    pub fn with_request_id(
        mut self,
        request_id: impl Into<String>,
    ) -> Result<Self, ErrorFactsError> {
        let request_id = request_id.into();
        validate_ascii_token("request_id", &request_id)?;
        self.request_id = Some(request_id);
        Ok(self)
    }
}

/// Invalid facts supplied while constructing a safe error.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{0}")]
pub struct ErrorFactsError(String);

fn validate_ascii_token(field: &str, value: &str) -> Result<(), ErrorFactsError> {
    crate::validate_identifier(field, value).map_err(ErrorFactsError)
}

fn validate_safe_text(field: &str, value: &str, maximum: usize) -> Result<(), ErrorFactsError> {
    if value.is_empty()
        || value.len() > maximum
        || value
            .chars()
            .any(|character| character == '\0' || character == '\u{7f}')
    {
        return Err(ErrorFactsError(format!(
            "{field} must contain 1..={maximum} safe UTF-8 bytes"
        )));
    }
    Ok(())
}
