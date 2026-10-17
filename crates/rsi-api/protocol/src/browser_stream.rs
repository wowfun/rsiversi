//! Closed metadata for the loopback browser subscription transport.
use crate::{ApiError, DeviceId, EndpointId, HostEpoch, Result};
use serde::{Deserialize, Serialize};

/// Maximum response head and protocol metadata bytes.
pub const HEAD_BYTES: usize = 1024;
/// Maximum body bytes sent for one pull credit.
pub const CHUNK_BYTES: usize = 64 * 1024;

/// Non-secret fences carried in the selected WebSocket subprotocol.
#[derive(Debug)]
pub struct Preface {
    /// Device selected during connection negotiation.
    pub device: DeviceId,
    /// Negotiated running Host generation.
    pub epoch: HostEpoch,
    /// Exact request body length, admitted before receiving it.
    pub length: usize,
}
impl Preface {
    /// Encodes the one supported subprotocol without credentials.
    pub fn encode(&self) -> String {
        format!(
            "rsi-stream-v1.{}.{}.{}",
            self.device.as_str(),
            self.epoch.as_str(),
            self.length
        )
    }
    /// Validates the closed protocol and canonical length spelling.
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() > HEAD_BYTES {
            return Err(ApiError::Unauthorized);
        }
        let mut parts = value.split('.');
        if parts.next() != Some("rsi-stream-v1") {
            return Err(ApiError::Unauthorized);
        }
        let device = DeviceId::parse(parts.next().ok_or(ApiError::Unauthorized)?)
            .map_err(|_| ApiError::Unauthorized)?;
        let epoch = HostEpoch::parse(parts.next().ok_or(ApiError::Unauthorized)?)
            .map_err(|_| ApiError::Unauthorized)?;
        let raw = parts.next().ok_or(ApiError::Unauthorized)?;
        let length = raw.parse::<usize>().map_err(|_| ApiError::Unauthorized)?;
        if parts.next().is_some() || raw != length.to_string() {
            return Err(ApiError::Unauthorized);
        }
        Ok(Self {
            device,
            epoch,
            length,
        })
    }
}

/// Response metadata; existing API decoders validate identity and content.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    /// HTTP-equivalent status code.
    pub status: u16,
    /// Persisted endpoint identity.
    pub endpoint: EndpointId,
    /// Exact running generation.
    pub epoch: HostEpoch,
    /// Existing API response media type.
    pub content_type: String,
    /// Finite body length; absent for subscriptions.
    pub content_length: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_preface_identities_are_unauthorized() {
        let valid = Preface {
            device: DeviceId::from_bytes([1; 16]),
            epoch: HostEpoch::from_bytes([2; 16]),
            length: 0,
        };
        assert!(Preface::parse(&valid.encode()).is_ok());
        for input in [
            format!("rsi-stream-v1.invalid.{}.0", valid.epoch.as_str()),
            format!("rsi-stream-v1.{}.invalid.0", valid.device.as_str()),
        ] {
            assert!(matches!(
                Preface::parse(&input),
                Err(ApiError::Unauthorized)
            ));
        }
    }
}
