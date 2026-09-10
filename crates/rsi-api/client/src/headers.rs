use crate::{FiniteEncoding, invalid};
use rsi_api_protocol::{ApiError, EndpointId, HostEpoch, Result};

fn one<'a>(headers: &'a http::HeaderMap, name: &str) -> Result<Option<&'a str>> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .map(|value| value.to_str().map_err(|_| invalid()))
        .transpose()?;
    if values.next().is_some() {
        return Err(invalid());
    }
    Ok(value)
}
/// Checks the exact content type and rejects an encoded/decompressed representation.
pub fn response_content_type(headers: &http::HeaderMap, expected: &str) -> Result<()> {
    if one(headers, "content-type")? != Some(expected)
        || one(headers, "content-encoding")?.is_some()
    {
        return Err(invalid());
    }
    Ok(())
}
/// Checks wire/deployment identity and the pinned generation before exposing a body.
pub fn response_identity(
    headers: &http::HeaderMap,
    endpoint: &EndpointId,
    expected_epoch: Option<&HostEpoch>,
) -> Result<HostEpoch> {
    if one(headers, "x-rsi-wire-version")? != Some("1")
        || one(headers, "x-rsi-endpoint-id")? != Some(endpoint.as_str())
    {
        return Err(invalid());
    }
    let epoch = HostEpoch::parse(one(headers, "x-rsi-host-epoch")?.ok_or_else(invalid)?)
        .map_err(|_| invalid())?;
    if expected_epoch.is_some_and(|expected| expected != &epoch) {
        return Err(ApiError::ShuttingDown);
    }
    Ok(epoch)
}
/// Reads a single declared byte length; missing length requires bounded EOF decoding.
pub fn response_content_length(headers: &http::HeaderMap) -> Result<Option<usize>> {
    one(headers, "content-length")?
        .map(|value| value.parse::<usize>().map_err(|_| invalid()))
        .transpose()
}
/// Validated representation metadata, independent of the platform response handle.
#[derive(Clone, Copy, Debug)]
pub struct FiniteResponseHead {
    /// Exact representation accepted for this HTTP status.
    pub encoding: FiniteEncoding,
    /// Declared wire bytes, including binary framing when present.
    pub length: Option<usize>,
    /// Whether the JSON body carries a closed domain failure.
    pub domain_error: bool,
}
/// Accepts successful finite data or a typed domain error, rejecting contradictory headers.
pub fn finite_response_head(status: u16, headers: &http::HeaderMap) -> Result<FiniteResponseHead> {
    let encoding = match (status, one(headers, "content-type")?) {
        (200, Some("application/json")) | (422, Some("application/vnd.rsi.domain-error+json")) => {
            FiniteEncoding::Json
        }
        (200, Some("application/vnd.rsi.binary")) => FiniteEncoding::Binary,
        _ => return Err(invalid()),
    };
    if one(headers, "content-encoding")?.is_some() {
        return Err(invalid());
    }
    Ok(FiniteResponseHead {
        encoding,
        length: response_content_length(headers)?,
        domain_error: status == 422,
    })
}
/// Checks that a decoded common failure agrees with its HTTP status.
pub fn validate_error_status(status: u16, error: &ApiError) -> Result<()> {
    let expected = match error {
        ApiError::Unauthorized => 401,
        ApiError::Capacity => 429,
        ApiError::ShuttingDown => 409,
        ApiError::Unavailable => 404,
        ApiError::Invalid(_) => 400,
        ApiError::Backend(_) | ApiError::OutcomeUnknown => 500,
        ApiError::Domain(_) => return Err(invalid()),
    };
    if status != expected {
        return Err(invalid());
    }
    Ok(())
}
