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
    InvalidRequest,
    Unsupported,
    Authentication,
    Permission,
    NotFound,
    RateLimited,
    Quota,
    Timeout,
    Transport,
    Server,
    OutputValidation,
    Protocol,
    Cancelled,
    DispatchUncertain,
    RemoteExpired,
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
    Prepare,
    Connect,
    Send,
    FirstEvent,
    Stream,
    Assemble,
    DeferredSubmit,
    DeferredPoll,
    DeferredCancel,
    Realtime,
}

/// What is known about whether an external request crossed its effect seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    NotStarted,
    NotDispatched,
    Dispatched,
    Unknown,
}

/// Safe, serializable provider failure facts. Policy decides retryability.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
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

impl AiError {
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

    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    pub fn provider_code(&self) -> Option<&str> {
        self.provider_code.as_deref()
    }

    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }

    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

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

    /// Attaches an HTTP status observed at the provider seam.
    #[must_use]
    pub const fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
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
