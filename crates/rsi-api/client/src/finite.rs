use crate::{invalid, validate_json};
use rsi_api_protocol::{
    ApiMessage, ByteBudget, ByteReceiver, ByteReservation, Result, RetainedBytes,
};

/// Finite response representation selected by the validated HTTP content type.
#[derive(Clone, Copy, Debug)]
pub enum FiniteEncoding {
    /// One complete JSON value.
    Json,
    /// Two big-endian u64 lengths followed by exact JSON and binary payloads.
    Binary,
}

#[derive(Debug)]
enum Body {
    Prefix {
        bytes: [u8; 16],
        filled: usize,
        capacity: ByteReservation,
    },
    Payload {
        receiver: ByteReceiver,
        metadata: Option<usize>,
        exact: Option<usize>,
    },
    Failed,
}

/// Incremental finite-body decoder with admission preceding payload allocation.
#[derive(Debug)]
pub struct FiniteDecoder {
    body: Body,
    declared: Option<usize>,
    received: usize,
}
impl FiniteDecoder {
    /// Takes the operation's pre-acquired payload capacity and optional wire length.
    pub fn new(
        encoding: FiniteEncoding,
        mut capacity: ByteReservation,
        declared: Option<usize>,
    ) -> Result<Self> {
        let body = match encoding {
            FiniteEncoding::Json => {
                if let Some(length) = declared {
                    capacity.shrink(length)?;
                }
                Body::Payload {
                    receiver: capacity.receive(),
                    metadata: None,
                    exact: declared,
                }
            }
            FiniteEncoding::Binary => {
                if declared.is_some_and(|length| length < 16 || length - 16 > capacity.bytes()) {
                    return Err(invalid());
                }
                Body::Prefix {
                    bytes: [0; 16],
                    filled: 0,
                    capacity,
                }
            }
        };
        Ok(Self {
            body,
            declared,
            received: 0,
        })
    }

    /// Consumes a transport fragment, permanently poisoning state on failure.
    pub fn push(&mut self, chunk: &[u8]) -> Result<()> {
        let result = self.push_inner(chunk);
        if result.is_err() {
            self.body = Body::Failed;
        }
        result
    }
    fn push_inner(&mut self, mut chunk: &[u8]) -> Result<()> {
        self.received = self.received.checked_add(chunk.len()).ok_or_else(invalid)?;
        if self.declared.is_some_and(|length| self.received > length) {
            return Err(invalid());
        }
        if let Body::Prefix { bytes, filled, .. } = &mut self.body {
            let take = (16 - *filled).min(chunk.len());
            bytes[*filled..*filled + take].copy_from_slice(&chunk[..take]);
            *filled += take;
            chunk = &chunk[take..];
            if *filled != 16 {
                return Ok(());
            }
            let Body::Prefix {
                bytes,
                mut capacity,
                ..
            } = std::mem::replace(&mut self.body, Body::Failed)
            else {
                unreachable!()
            };
            let metadata = usize::try_from(u64::from_be_bytes(
                bytes[..8].try_into().expect("fixed prefix"),
            ))
            .map_err(|_| invalid())?;
            let binary = usize::try_from(u64::from_be_bytes(
                bytes[8..].try_into().expect("fixed prefix"),
            ))
            .map_err(|_| invalid())?;
            let total = metadata.checked_add(binary).ok_or_else(invalid)?;
            if self.declared.is_some_and(|length| length - 16 != total) {
                return Err(invalid());
            }
            capacity.shrink(total)?;
            self.body = Body::Payload {
                receiver: capacity.receive(),
                metadata: Some(metadata),
                exact: Some(total),
            };
        }
        match &mut self.body {
            Body::Payload { receiver, .. } => receiver.append(chunk),
            _ => Err(invalid()),
        }
    }

    /// Requires real body EOF, exact lengths and valid JSON before exposing any reply.
    pub fn finish(self) -> Result<ApiMessage> {
        self.complete(|receiver| Ok(receiver.finish()))
    }

    /// Validates EOF and transfers completed storage to a separate bounded owner.
    pub fn finish_into(self, destination: &ByteBudget) -> Result<ApiMessage> {
        self.complete(|receiver| receiver.finish_into(destination))
    }

    fn complete(
        self,
        finish: impl FnOnce(ByteReceiver) -> Result<RetainedBytes>,
    ) -> Result<ApiMessage> {
        if self.declared.is_some_and(|length| self.received != length) {
            return Err(invalid());
        }
        let Body::Payload {
            receiver,
            metadata,
            exact,
        } = self.body
        else {
            return Err(invalid());
        };
        let bytes = finish(receiver)?;
        if exact.is_some_and(|length| bytes.len() != length) {
            return Err(invalid());
        }
        let message = if let Some(metadata) = metadata {
            ApiMessage {
                json: bytes.slice(..metadata)?,
                binary: Some(bytes.slice(metadata..)?),
            }
        } else {
            ApiMessage {
                json: bytes,
                binary: None,
            }
        };
        validate_json(message.json.as_bytes())?;
        Ok(message)
    }
}
