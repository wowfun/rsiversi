//! Shared negotiated connection ownership and bounded API response decoding.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod connection;
mod finite;
mod headers;
mod response;
mod sse;
pub use connection::{ClientConnection, ClientResponseCapacity, ConnectionTransport};
pub use finite::{FiniteDecoder, FiniteEncoding};
pub use headers::{
    FiniteResponseHead, finite_response_head, response_content_length, response_content_type,
    response_identity, validate_error_status,
};
pub use response::{ResponseBytes, decode_event_stream, decode_response};
pub use sse::{SseDecoder, SseEvent};

use rsi_api_protocol::{ApiError, Result};
use serde::Deserialize;

fn invalid() -> ApiError {
    ApiError::Invalid("malformed or incomplete API response".into())
}
fn validate_json(bytes: &[u8]) -> Result<()> {
    serde_json::from_slice::<serde::de::IgnoredAny>(bytes).map_err(|_| invalid())?;
    Ok(())
}

/// Decodes a closed common-error envelope without reflecting remote diagnostics.
pub fn decode_error(bytes: &[u8]) -> Result<ApiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Envelope {
        code: Code,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Code {
        OutcomeUnknown,
        Unauthorized,
        Capacity,
        GenerationRetired,
        Unavailable,
        Invalid,
        Backend,
    }
    if bytes.len() > 128 {
        return Err(invalid());
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    Ok(match envelope.code {
        Code::OutcomeUnknown => ApiError::OutcomeUnknown,
        Code::Unauthorized => ApiError::Unauthorized,
        Code::Capacity => ApiError::Capacity,
        Code::GenerationRetired => ApiError::ShuttingDown,
        Code::Unavailable => ApiError::Unavailable,
        Code::Invalid => ApiError::Invalid("remote API rejected the request".into()),
        Code::Backend => ApiError::Backend("remote API failed".into()),
    })
}
