use crate::wire::{self, Port};
use rsi_api_protocol::{
    ApiError, ByteBudget, ByteReservation, Result,
    portable::{ErrorCode, Header},
};

pub(crate) async fn send(
    port: &mut impl Port,
    scratch: &ByteBudget,
    error: ApiError,
    maximum: usize,
) -> Result<()> {
    let (code, bytes) = match error {
        ApiError::OutcomeUnknown => (ErrorCode::OutcomeUnknown, None),
        ApiError::Unauthorized => (ErrorCode::Unauthorized, None),
        ApiError::Domain(bytes) if bytes.len() <= maximum => (ErrorCode::Domain, Some(bytes)),
        ApiError::Backend(_) | ApiError::Domain(_) => (ErrorCode::Backend, None),
        ApiError::Invalid(_) => (ErrorCode::Invalid, None),
        ApiError::Capacity => (ErrorCode::Capacity, None),
        ApiError::ShuttingDown => (ErrorCode::ShuttingDown, None),
        ApiError::Unavailable => (ErrorCode::Unavailable, None),
    };
    wire::send_header(
        port,
        scratch,
        &Header::Error {
            code,
            domain: bytes.as_ref().map(rsi_api_protocol::RetainedBytes::len),
        },
    )
    .await?;
    if let Some(bytes) = bytes {
        wire::send_payload(port, scratch, bytes.as_bytes(), &[]).await?;
    }
    Ok(())
}
pub(crate) async fn receive(
    port: &mut impl Port,
    code: ErrorCode,
    domain: Option<usize>,
    reservation: ByteReservation,
    retained: &ByteBudget,
    maximum: usize,
) -> Result<ApiError> {
    if code == ErrorCode::Domain {
        let length = domain
            .filter(|length| *length <= maximum)
            .ok_or_else(wire::invalid)?;
        let bytes = wire::retained_payload(port, reservation, length, retained).await?;
        serde_json::from_slice::<serde::de::IgnoredAny>(bytes.as_bytes())
            .map_err(|_| wire::invalid())?;
        return Ok(ApiError::Domain(bytes));
    }
    if domain.is_some() {
        return Err(wire::invalid());
    }
    Ok(match code {
        ErrorCode::OutcomeUnknown => ApiError::OutcomeUnknown,
        ErrorCode::Unauthorized => ApiError::Unauthorized,
        ErrorCode::Backend => ApiError::Backend("Portable API provider failed".into()),
        ErrorCode::Invalid => ApiError::Invalid("Portable API request was rejected".into()),
        ErrorCode::Capacity => ApiError::Capacity,
        ErrorCode::ShuttingDown => ApiError::ShuttingDown,
        ErrorCode::Unavailable => ApiError::Unavailable,
        ErrorCode::Domain => unreachable!("domain handled above"),
    })
}
