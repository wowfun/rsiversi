use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{MAX_BINARY_CHUNK_BYTES, MAX_CONTROL_FRAME_BYTES, MediaDescriptor, validation};

const MAGIC: &[u8; 4] = b"RAI0";
const WIRE_VERSION: u8 = 0;
const KIND_CONTROL: u8 = 1;
const KIND_BLOB_CHUNK: u8 = 2;
const FLAG_FINAL: u16 = 1;
const HEADER_BYTES: usize = 20;

/// One normalized rsi-ai payload carried inside an rsi-meta DATA frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireFrame {
    Control {
        call_id: String,
        payload: Vec<u8>,
    },
    BlobChunk {
        call_id: String,
        blob_id: String,
        sequence: u32,
        final_chunk: bool,
        bytes: Vec<u8>,
    },
}

/// Encodes one bounded frame without base64 expansion.
pub fn encode_wire_frame(frame: &WireFrame) -> Result<Vec<u8>, WireError> {
    let (kind, flags, call_id, blob_id, sequence, payload, maximum) = match frame {
        WireFrame::Control { call_id, payload } => (
            KIND_CONTROL,
            0,
            call_id.as_str(),
            "",
            0,
            payload.as_slice(),
            MAX_CONTROL_FRAME_BYTES,
        ),
        WireFrame::BlobChunk {
            call_id,
            blob_id,
            sequence,
            final_chunk,
            bytes,
        } => {
            if *sequence == 0 {
                return Err(WireError::new(
                    "wire.invalid_sequence",
                    "blob chunk sequence must begin at one",
                ));
            }
            validation::identifier("blob_id", blob_id)
                .map_err(|reason| WireError::new("wire.invalid_blob_id", reason))?;
            (
                KIND_BLOB_CHUNK,
                u16::from(*final_chunk),
                call_id.as_str(),
                blob_id.as_str(),
                *sequence,
                bytes.as_slice(),
                MAX_BINARY_CHUNK_BYTES,
            )
        }
    };
    validation::identifier("call_id", call_id)
        .map_err(|reason| WireError::new("wire.invalid_call_id", reason))?;
    if payload.len() > maximum {
        return Err(WireError::new(
            "wire.payload_too_large",
            format!("frame payload exceeds {maximum} bytes"),
        ));
    }

    let call_len = u16::try_from(call_id.len()).map_err(|_| {
        WireError::new("wire.invalid_call_id", "call identifier length exceeds u16")
    })?;
    let blob_len = u16::try_from(blob_id.len()).map_err(|_| {
        WireError::new("wire.invalid_blob_id", "blob identifier length exceeds u16")
    })?;
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| WireError::new("wire.payload_too_large", "payload length exceeds u32"))?;
    let capacity = HEADER_BYTES
        .checked_add(call_id.len())
        .and_then(|size| size.checked_add(blob_id.len()))
        .and_then(|size| size.checked_add(payload.len()))
        .ok_or_else(|| WireError::new("wire.payload_too_large", "frame length overflowed"))?;
    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(MAGIC);
    encoded.push(WIRE_VERSION);
    encoded.push(kind);
    encoded.extend_from_slice(&flags.to_be_bytes());
    encoded.extend_from_slice(&call_len.to_be_bytes());
    encoded.extend_from_slice(&blob_len.to_be_bytes());
    encoded.extend_from_slice(&sequence.to_be_bytes());
    encoded.extend_from_slice(&payload_len.to_be_bytes());
    encoded.extend_from_slice(call_id.as_bytes());
    encoded.extend_from_slice(blob_id.as_bytes());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

/// Decodes and validates one untrusted normalized DATA payload.
pub fn decode_wire_frame(bytes: &[u8]) -> Result<WireFrame, WireError> {
    if bytes.len() < HEADER_BYTES || &bytes[..4] != MAGIC {
        return Err(WireError::new(
            "wire.invalid_header",
            "frame is shorter than its header or has the wrong magic",
        ));
    }
    if bytes[4] != WIRE_VERSION {
        return Err(WireError::new(
            "wire.unsupported_version",
            format!("wire version {} is unsupported", bytes[4]),
        ));
    }
    let kind = bytes[5];
    let flags = u16::from_be_bytes([bytes[6], bytes[7]]);
    let call_len = usize::from(u16::from_be_bytes([bytes[8], bytes[9]]));
    let blob_len = usize::from(u16::from_be_bytes([bytes[10], bytes[11]]));
    let sequence = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let payload_len = usize::try_from(u32::from_be_bytes([
        bytes[16], bytes[17], bytes[18], bytes[19],
    ]))
    .map_err(|_| WireError::new("wire.payload_too_large", "payload length exceeds usize"))?;
    let expected = HEADER_BYTES
        .checked_add(call_len)
        .and_then(|size| size.checked_add(blob_len))
        .and_then(|size| size.checked_add(payload_len))
        .ok_or_else(|| WireError::new("wire.invalid_length", "frame length overflowed"))?;
    if expected != bytes.len() {
        return Err(WireError::new(
            "wire.invalid_length",
            "declared frame length does not match the received bytes",
        ));
    }
    let call_end = HEADER_BYTES + call_len;
    let blob_end = call_end + blob_len;
    let call_id = std::str::from_utf8(&bytes[HEADER_BYTES..call_end])
        .map_err(|_| WireError::new("wire.invalid_call_id", "call identifier is not UTF-8"))?
        .to_owned();
    let blob_id = std::str::from_utf8(&bytes[call_end..blob_end])
        .map_err(|_| WireError::new("wire.invalid_blob_id", "blob identifier is not UTF-8"))?
        .to_owned();
    validation::identifier("call_id", &call_id)
        .map_err(|reason| WireError::new("wire.invalid_call_id", reason))?;
    let payload = &bytes[blob_end..];

    match kind {
        KIND_CONTROL => {
            if flags != 0 || sequence != 0 || !blob_id.is_empty() {
                return Err(WireError::new(
                    "wire.invalid_control",
                    "control frame carries blob-only metadata",
                ));
            }
            if payload.len() > MAX_CONTROL_FRAME_BYTES {
                return Err(WireError::new(
                    "wire.payload_too_large",
                    format!("control payload exceeds {MAX_CONTROL_FRAME_BYTES} bytes"),
                ));
            }
            Ok(WireFrame::Control {
                call_id,
                payload: payload.to_vec(),
            })
        }
        KIND_BLOB_CHUNK => {
            if flags & !FLAG_FINAL != 0 || sequence == 0 {
                return Err(WireError::new(
                    "wire.invalid_sequence",
                    "blob frame has invalid flags or a zero sequence",
                ));
            }
            validation::identifier("blob_id", &blob_id)
                .map_err(|reason| WireError::new("wire.invalid_blob_id", reason))?;
            if payload.len() > MAX_BINARY_CHUNK_BYTES {
                return Err(WireError::new(
                    "wire.payload_too_large",
                    format!("blob chunk exceeds {MAX_BINARY_CHUNK_BYTES} bytes"),
                ));
            }
            Ok(WireFrame::BlobChunk {
                call_id,
                blob_id,
                sequence,
                final_chunk: flags & FLAG_FINAL != 0,
                bytes: payload.to_vec(),
            })
        }
        _ => Err(WireError::new(
            "wire.unknown_kind",
            format!("wire frame kind {kind} is unknown"),
        )),
    }
}

/// Bounded complete-body verifier used by direct complete helpers and tests.
#[derive(Debug)]
pub struct BlobAssembler {
    descriptor: MediaDescriptor,
    expected_sequence: u32,
    bytes: Vec<u8>,
    hasher: Sha256,
    final_seen: bool,
}

impl BlobAssembler {
    #[must_use]
    pub fn new(descriptor: MediaDescriptor) -> Self {
        Self {
            descriptor,
            expected_sequence: 1,
            bytes: Vec::new(),
            hasher: Sha256::new(),
            final_seen: false,
        }
    }

    pub const fn descriptor(&self) -> &MediaDescriptor {
        &self.descriptor
    }

    pub fn push(
        &mut self,
        sequence: u32,
        bytes: &[u8],
        final_chunk: bool,
    ) -> Result<(), WireError> {
        if self.final_seen {
            return Err(WireError::new(
                "blob.already_finished",
                "blob received a chunk after its final chunk",
            ));
        }
        if sequence != self.expected_sequence {
            return Err(WireError::new(
                "blob.sequence",
                format!(
                    "blob chunk sequence {sequence} does not match expected {}",
                    self.expected_sequence
                ),
            ));
        }
        if bytes.len() > MAX_BINARY_CHUNK_BYTES {
            return Err(WireError::new(
                "blob.chunk_too_large",
                format!("blob chunk exceeds {MAX_BINARY_CHUNK_BYTES} bytes"),
            ));
        }
        let projected = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| WireError::new("blob.length", "blob length overflowed"))?;
        if u64::try_from(projected).map_or(true, |length| length > self.descriptor.byte_len()) {
            return Err(WireError::new(
                "blob.length",
                "blob bytes exceed the declared length",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        self.hasher.update(bytes);
        self.expected_sequence = self.expected_sequence.saturating_add(1);
        self.final_seen = final_chunk;
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<u8>, WireError> {
        if !self.final_seen {
            return Err(WireError::new(
                "blob.incomplete",
                "blob ended without a final chunk",
            ));
        }
        if u64::try_from(self.bytes.len()).ok() != Some(self.descriptor.byte_len()) {
            return Err(WireError::new(
                "blob.length",
                "blob length does not match its descriptor",
            ));
        }
        let actual = self.hasher.finalize();
        let expected = decode_sha256(self.descriptor.sha256()).ok_or_else(|| {
            WireError::new("blob.digest", "descriptor digest is not valid SHA-256")
        })?;
        if actual.as_slice() != expected {
            return Err(WireError::new(
                "blob.digest",
                "blob digest does not match its descriptor",
            ));
        }
        Ok(self.bytes)
    }
}

fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut decoded = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Some(decoded)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

/// Rejection returned by the normalized frame or blob seam.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct WireError {
    code: &'static str,
    message: String,
}

impl WireError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}
