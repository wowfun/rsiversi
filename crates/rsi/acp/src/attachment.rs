use std::io;

/// Fixed preface for a local ACP attachment, before ordinary ACP NDJSON.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentHello {
    /// Live Host generation that accepted this stream.
    pub epoch: rsi_api_protocol::HostEpoch,
    /// ACP models, codecs and attachment framing implemented by the backend.
    pub protocol: rsi_api_protocol::ProtocolDescriptor,
}

/// Maximum JSON hello size, excluding its four-byte big-endian length.
pub const MAX_ATTACHMENT_HELLO: usize = 4096;

/// Descriptor generated from the ACP adapter and its wire dependencies.
pub fn attachment_protocol() -> rsi_api_protocol::ProtocolDescriptor {
    rsi_api_protocol::wire_contract!()
}

/// Encodes a bounded complete preface, including its network-order length.
///
/// # Errors
/// Returns an error when the hello cannot be serialized within the byte limit.
pub fn encode_attachment_hello(hello: &AttachmentHello) -> io::Result<Vec<u8>> {
    let bytes = rsi_api_protocol::ByteBudget::default()
        .encode(hello, MAX_ATTACHMENT_HELLO)
        .map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(4 + bytes.len());
    let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&bytes);
    Ok(frame)
}

/// Validates the prefix before either endpoint allocates its payload buffer.
///
/// # Errors
/// Returns an error for a zero or oversized length.
pub fn decode_attachment_length(header: [u8; 4]) -> io::Result<usize> {
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_ATTACHMENT_HELLO {
        return Err(io::Error::other("invalid ACP attachment hello length"));
    }
    Ok(length)
}

/// Decodes exactly one complete preface; trailing ACP bytes belong to its transport.
///
/// # Errors
/// Returns an error for incomplete, oversized, malformed or trailing input.
pub fn decode_attachment_hello(frame: &[u8]) -> io::Result<AttachmentHello> {
    let header = frame
        .get(..4)
        .ok_or_else(|| io::Error::other("incomplete ACP hello"))?;
    let length = decode_attachment_length(header.try_into().map_err(io::Error::other)?)?;
    if frame.len() != length + 4 {
        return Err(io::Error::other("ACP hello length mismatch"));
    }
    serde_json::from_slice(&frame[4..])
        .map_err(|_| io::Error::other("invalid ACP attachment hello"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_codec_bounds_prefix_and_owns_exact_frame() {
        for length in [
            0,
            u32::try_from(MAX_ATTACHMENT_HELLO).unwrap() + 1,
            u32::MAX,
        ] {
            assert!(decode_attachment_length(length.to_be_bytes()).is_err());
            assert!(decode_attachment_hello(&length.to_be_bytes()).is_err());
        }
        let hello = AttachmentHello {
            epoch: rsi_api_protocol::HostEpoch::generate().unwrap(),
            protocol: attachment_protocol(),
        };
        let mut frame = encode_attachment_hello(&hello).unwrap();
        assert_eq!(decode_attachment_hello(&frame).unwrap().epoch, hello.epoch);
        assert!(decode_attachment_hello(&frame[..frame.len() - 1]).is_err());
        frame.push(b'\n');
        assert!(decode_attachment_hello(&frame).is_err());
        let mut value = serde_json::to_value(hello).unwrap();
        value["unknown"] = true.into();
        let payload = serde_json::to_vec(&value).unwrap();
        let mut frame = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
        frame.extend_from_slice(&payload);
        assert!(decode_attachment_hello(&frame).is_err());
    }
}
