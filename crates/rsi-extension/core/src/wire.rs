use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Largest encoded frame, excluding its newline delimiter.
pub const MAXIMUM_FRAME_BYTES: usize = 1024 * 1024;
/// Maximum active requests in each direction, including cancelled requests.
pub const MAXIMUM_PENDING_REQUESTS: usize = 64;
pub(crate) const MAXIMUM_ID: u64 = (1_u64 << 53) - 1;

/// One exact process-generation frame.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    /// Opaque identity selected by the process owner.
    pub generation: String,
    /// Correlation and operation data.
    pub message: Message,
}
/// Direction-local IDs correlate requests and replies independently.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    /// Invoke a bounded method under the recipient's own authority rules.
    Request {
        /// Monotonic direction-local identity.
        id: u64,
        /// Protocol method, not a capability by itself.
        method: String,
        /// Validated by the owning family adapter.
        payload: Value,
    },
    /// Settle a request exactly once, including cancellation.
    Reply {
        /// The peer's request identity.
        id: u64,
        /// Discriminated response, including a successful JSON null.
        result: Response,
    },
    /// Request cancellation; the recipient must still return a reply.
    Cancel {
        /// The sender's request identity.
        id: u64,
    },
}
/// A reply cannot ambiguously contain both success and failure.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "status",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Response {
    /// Successful family-owned payload.
    Ok(Value),
    /// Bounded peer diagnostic.
    Error(String),
}
impl Frame {
    /// Checks shape and logical bounds after bounded decoding.
    pub fn validate(&self) -> Result<()> {
        if self.generation.is_empty()
            || self.generation.len() > 128
            || !self
                .generation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(Error::Protocol);
        }
        let id = match &self.message {
            Message::Request { id, method, .. } => {
                if method.is_empty()
                    || method.len() > 128
                    || !method
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
                {
                    return Err(Error::Protocol);
                }
                *id
            }
            Message::Reply { id, result } => {
                if matches!(result, Response::Error(s) if s.is_empty() || s.len() > 4096) {
                    return Err(Error::Protocol);
                }
                *id
            }
            Message::Cancel { id } => *id,
        };
        if id == 0 || id > MAXIMUM_ID {
            return Err(Error::Protocol);
        }
        Ok(())
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec(self).map_err(|_| Error::Protocol)?;
        if bytes.len() > MAXIMUM_FRAME_BYTES {
            return Err(Error::Capacity);
        }
        bytes.push(b'\n');
        Ok(bytes)
    }
}

pub(crate) struct Decoder(Vec<u8>);
impl Decoder {
    pub fn new() -> Self {
        Self(Vec::new())
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Frame>> {
        let mut frames = Vec::new();
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            let complete = part.last() == Some(&b'\n');
            let body = if complete {
                &part[..part.len() - 1]
            } else {
                part
            };
            if self.0.len().saturating_add(body.len()) > MAXIMUM_FRAME_BYTES {
                return Err(Error::Capacity);
            }
            self.0.extend_from_slice(body);
            if complete {
                let frame: Frame = serde_json::from_slice(&self.0).map_err(|_| Error::Protocol)?;
                frame.validate()?;
                frames.push(frame);
                self.0.clear();
            }
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_utf8_and_batched_frames_are_lossless() {
        let frame = Frame {
            generation: "g1".into(),
            message: Message::Request {
                id: 1,
                method: "test".into(),
                payload: serde_json::json!({"text":"审阅"}),
            },
        };
        let bytes = frame.encode().unwrap();
        let mut decoder = Decoder::new();
        let mut count = 0;
        for byte in bytes.repeat(2) {
            count += decoder.push(&[byte]).unwrap().len();
        }
        assert_eq!(count, 2);
    }
    #[test]
    fn rejects_oversize_before_parsing_and_ambiguous_replies() {
        let mut decoder = Decoder::new();
        assert!(
            decoder
                .push(&vec![b' '; MAXIMUM_FRAME_BYTES])
                .unwrap()
                .is_empty()
        );
        assert!(matches!(decoder.push(b"x"), Err(Error::Capacity)));
        let reply = Frame {
            generation: "g".into(),
            message: Message::Reply {
                id: 1,
                result: Response::Error(String::new()),
            },
        };
        assert_eq!(reply.validate(), Err(Error::Protocol));
        assert!(
            Decoder::new()
                .push(
                    br#"{"generation":"g","extra":1,"message":{"kind":"cancel","id":1}}
"#
                )
                .is_err()
        );
    }
}
