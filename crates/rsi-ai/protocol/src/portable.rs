//! Bounded, accounted packet framing for Portable provider business protocols.

use rsi_api_protocol::{ByteBudget, ByteReceiver, RetainedBytes};
use std::fmt;

mod provider;
pub use provider::*;

/// Maximum payload bytes in one Meta Message fragment.
pub const MAXIMUM_FRAGMENT_BYTES: usize = 64 * 1024;
/// Maximum request plus prepared snapshot/provider metadata in one JSON packet.
pub const MAXIMUM_CONTROL_BYTES: usize = crate::MAX_REQUEST_BYTES + 256 * 1024;
/// Maximum raw credential or media body in one packet.
pub const MAXIMUM_BINARY_BYTES: usize = crate::MAX_BINARY_CHUNK_BYTES;
const HEADER_BYTES: usize = 9;

/// Logical packet content; binary bytes never become JSON fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// Closed semantic/control JSON.
    Json,
    /// Raw secret or Media bytes.
    Binary,
}
impl Kind {
    const fn tag(self) -> u8 {
        match self {
            Self::Json => 0,
            Self::Binary => 1,
        }
    }
    const fn maximum(self) -> usize {
        match self {
            Self::Json => MAXIMUM_CONTROL_BYTES,
            Self::Binary => MAXIMUM_BINARY_BYTES,
        }
    }
}

/// Failure contains no wire bytes or provider diagnostic text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FramingError {
    /// Invalid, truncated, noncontiguous or oversized packet.
    #[error("invalid Portable provider frame")]
    Invalid,
    /// Caller-owned receive budget cannot retain the declared packet.
    #[error("Portable provider receive capacity is exhausted")]
    Capacity,
    /// This decoder was already closed by invalid input.
    #[error("Portable provider decoder is closed")]
    Closed,
}
/// Packet framing result.
pub type Result<T> = std::result::Result<T, FramingError>;

/// Complete immutable packet; Debug exposes only kind and byte length.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Packet {
    /// Expected semantic role of its bytes.
    pub kind: Kind,
    /// Complete payload retaining its original receive reservation.
    pub bytes: RetainedBytes,
}

/// Borrowed, bounded outgoing fragmentation. Debug never exposes payload bytes.
pub struct Frames<'a> {
    kind: Kind,
    bytes: &'a [u8],
    offset: usize,
    done: bool,
}
impl fmt::Debug for Frames<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frames")
            .field("kind", &self.kind)
            .field("bytes", &self.bytes.len())
            .field("offset", &self.offset)
            .finish()
    }
}
/// Validates a complete packet before producing any outbound fragment.
pub fn frames(kind: Kind, bytes: &[u8]) -> Result<Frames<'_>> {
    if bytes.len() > kind.maximum() {
        return Err(FramingError::Invalid);
    }
    Ok(Frames {
        kind,
        bytes,
        offset: 0,
        done: false,
    })
}
impl Iterator for Frames<'_> {
    type Item = Vec<u8>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let end = (self.offset + MAXIMUM_FRAGMENT_BYTES).min(self.bytes.len());
        let mut frame = Vec::with_capacity(HEADER_BYTES + end - self.offset);
        frame.push(self.kind.tag());
        frame.extend_from_slice(
            &u32::try_from(self.bytes.len())
                .expect("bounded packet length")
                .to_le_bytes(),
        );
        frame.extend_from_slice(
            &u32::try_from(self.offset)
                .expect("bounded packet offset")
                .to_le_bytes(),
        );
        frame.extend_from_slice(&self.bytes[self.offset..end]);
        self.offset = end;
        self.done = end == self.bytes.len();
        Some(frame)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = if self.done {
            0
        } else {
            (self.bytes.len() - self.offset)
                .div_ceil(MAXIMUM_FRAGMENT_BYTES)
                .max(1)
        };
        (count, Some(count))
    }
}
impl ExactSizeIterator for Frames<'_> {}

struct Pending {
    kind: Kind,
    total: usize,
    received: usize,
    body: ByteReceiver,
}
enum State {
    Empty,
    Receiving(Pending),
    Closed,
}

/// Owns one unfinished receive packet; any failure permanently closes admission.
pub struct Decoder {
    budget: ByteBudget,
    maximum_control_bytes: usize,
    state: State,
}
impl fmt::Debug for Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.state {
            State::Empty => "empty",
            State::Receiving(_) => "receiving",
            State::Closed => "closed",
        };
        f.debug_struct("Decoder")
            .field("state", &state)
            .finish_non_exhaustive()
    }
}
impl Decoder {
    /// Uses one explicit shared retention budget; construction allocates no body.
    pub fn new(budget: ByteBudget) -> Self {
        Self {
            budget,
            maximum_control_bytes: MAXIMUM_CONTROL_BYTES,
            state: State::Empty,
        }
    }

    /// Narrows JSON admission for phases such as descriptions or prepared metadata.
    pub fn with_control_limit(budget: ByteBudget, maximum: usize) -> Result<Self> {
        if maximum > MAXIMUM_CONTROL_BYTES {
            return Err(FramingError::Invalid);
        }
        Ok(Self {
            budget,
            maximum_control_bytes: maximum,
            state: State::Empty,
        })
    }

    /// Accepts one complete Meta Message payload, reserving before body allocation.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<Packet>> {
        // Taking ownership first guarantees every failure releases partial bytes.
        let state = std::mem::replace(&mut self.state, State::Closed);
        if matches!(state, State::Closed) {
            return Err(FramingError::Closed);
        }
        if frame.len() < HEADER_BYTES || frame.len() > HEADER_BYTES + MAXIMUM_FRAGMENT_BYTES {
            return Err(FramingError::Invalid);
        }
        let kind = match frame[0] {
            0 => Kind::Json,
            1 => Kind::Binary,
            _ => return Err(FramingError::Invalid),
        };
        let total =
            u32::from_le_bytes(frame[1..5].try_into().map_err(|_| FramingError::Invalid)?) as usize;
        let offset =
            u32::from_le_bytes(frame[5..9].try_into().map_err(|_| FramingError::Invalid)?) as usize;
        let data = &frame[HEADER_BYTES..];
        if total > kind.maximum()
            || (kind == Kind::Json && total > self.maximum_control_bytes)
            || offset > total
            || data.len() > total - offset
            || (data.is_empty() && total != 0)
        {
            return Err(FramingError::Invalid);
        }
        let mut pending = match state {
            State::Empty => {
                if offset != 0 {
                    return Err(FramingError::Invalid);
                }
                let reservation = self
                    .budget
                    .reserve(total)
                    .map_err(|_| FramingError::Capacity)?;
                Pending {
                    kind,
                    total,
                    received: 0,
                    body: reservation.receive(),
                }
            }
            State::Receiving(pending) => pending,
            State::Closed => return Err(FramingError::Closed),
        };
        if kind != pending.kind || total != pending.total || offset != pending.received {
            return Err(FramingError::Invalid);
        }
        pending
            .body
            .append(data)
            .map_err(|_| FramingError::Invalid)?;
        pending.received += data.len();
        if pending.received == pending.total {
            self.state = State::Empty;
            Ok(Some(Packet {
                kind,
                bytes: pending.body.finish(),
            }))
        } else {
            self.state = State::Receiving(pending);
            Ok(None)
        }
    }

    /// Closes input and rejects an unfinished logical packet, releasing its bytes.
    pub fn finish(self) -> Result<()> {
        match self.state {
            State::Empty => Ok(()),
            State::Receiving(_) => Err(FramingError::Invalid),
            State::Closed => Err(FramingError::Closed),
        }
    }
}
