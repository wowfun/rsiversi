//! Bounded helper frame grammar; decoding a header precedes allocating its body.

/// Fixed wire header length.
pub const HEADER_BYTES: usize = 32;
/// Maximum data or ordinary message fragment.
pub const MAXIMUM_FRAGMENT_BYTES: usize = 16 * 1024;
/// Maximum complete reserved control message.
pub const MAXIMUM_CONTROL_BYTES: usize = 1024;
/// Maximum assembled ordinary request or reply.
pub const MAXIMUM_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
/// Maximum outstanding frame credits for one stream.
pub const STREAM_WINDOW_FRAMES: u32 = 4;
const MAGIC: [u8; 4] = *b"RSI\x01";

/// Closed wire failure before a frame becomes an accepted operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FrameError {
    /// Truncated, unrecognized or inconsistent wire data.
    #[error("invalid SSH helper frame")]
    Invalid,
    /// Frame belongs to a different connection.
    #[error("SSH helper connection epoch differs")]
    Epoch,
    /// A frame or assembled message exceeds its fixed bound.
    #[error("SSH helper frame capacity exceeded")]
    Capacity,
}
/// Frame grammar result.
pub type Result<T> = std::result::Result<T, FrameError>;

/// Wire role, independent of JSON helper operation names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameKind {
    /// Ordinary fragmented request.
    Request = 1,
    /// Ordinary fragmented reply.
    Reply = 2,
    /// Finite cleanup, cancel or resize request on reserved capacity.
    ControlRequest = 3,
    /// Reply to a reserved control request.
    ControlReply = 4,
    /// One ordered stream chunk, consuming one credit even for terminal EOF.
    Data = 5,
    /// A sequenced grant of one through four frame credits.
    Credit = 6,
    /// Fresh connection heartbeat; identity is its monotonic serial.
    Heartbeat = 7,
    /// Exact heartbeat acknowledgement.
    HeartbeatAck = 8,
    /// Connection shutdown marker, with zero identity.
    Close = 9,
}
impl TryFrom<u8> for FrameKind {
    type Error = FrameError;
    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Reply),
            3 => Ok(Self::ControlRequest),
            4 => Ok(Self::ControlReply),
            5 => Ok(Self::Data),
            6 => Ok(Self::Credit),
            7 => Ok(Self::Heartbeat),
            8 => Ok(Self::HeartbeatAck),
            9 => Ok(Self::Close),
            _ => Err(FrameError::Invalid),
        }
    }
}

/// Validated header; fields cannot change after the owner admits its body length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    epoch: u64,
    kind: FrameKind,
    identity: u64,
    sequence: u32,
    final_fragment: bool,
    body_bytes: u32,
}
impl FrameHeader {
    /// Decodes exactly one header, rejecting foreign epochs before body allocation.
    pub fn decode(bytes: &[u8], expected_epoch: u64) -> Result<Self> {
        let bytes: &[u8; HEADER_BYTES] = bytes.try_into().map_err(|_| FrameError::Invalid)?;
        if bytes[..4] != MAGIC || bytes[5] > 1 || bytes[6..8] != [0; 2] {
            return Err(FrameError::Invalid);
        }
        let epoch = u64::from_be_bytes(bytes[8..16].try_into().map_err(|_| FrameError::Invalid)?);
        if epoch == 0 || epoch != expected_epoch {
            return Err(FrameError::Epoch);
        }
        let header = Self {
            epoch,
            kind: bytes[4].try_into()?,
            final_fragment: bytes[5] == 1,
            identity: u64::from_be_bytes(
                bytes[16..24].try_into().map_err(|_| FrameError::Invalid)?,
            ),
            body_bytes: u32::from_be_bytes(
                bytes[24..28].try_into().map_err(|_| FrameError::Invalid)?,
            ),
            sequence: u32::from_be_bytes(
                bytes[28..32].try_into().map_err(|_| FrameError::Invalid)?,
            ),
        };
        header.validate()?;
        Ok(header)
    }
    fn validate(self) -> Result<()> {
        if self.epoch == 0 || (self.identity == 0) != (self.kind == FrameKind::Close) {
            return Err(FrameError::Invalid);
        }
        let bytes = self.body_len();
        if bytes > MAXIMUM_FRAGMENT_BYTES {
            return Err(FrameError::Capacity);
        }
        match self.kind {
            FrameKind::Request | FrameKind::Reply => {
                if bytes == 0 || u64::from(self.sequence) >= MAXIMUM_MESSAGE_BYTES as u64 {
                    return Err(FrameError::Invalid);
                }
            }
            FrameKind::ControlRequest | FrameKind::ControlReply => {
                if bytes > MAXIMUM_CONTROL_BYTES {
                    return Err(FrameError::Capacity);
                }
                if bytes == 0 || self.sequence != 0 || !self.final_fragment {
                    return Err(FrameError::Invalid);
                }
            }
            FrameKind::Data => {
                if bytes == 0 && !self.final_fragment {
                    return Err(FrameError::Invalid);
                }
            }
            FrameKind::Credit => {
                if bytes != 4 || self.sequence == 0 || !self.final_fragment {
                    return Err(FrameError::Invalid);
                }
            }
            FrameKind::Heartbeat | FrameKind::HeartbeatAck | FrameKind::Close => {
                if bytes != 0 || self.sequence != 0 || !self.final_fragment {
                    return Err(FrameError::Invalid);
                }
            }
        }
        Ok(())
    }
    /// Returns the already bounded number of body bytes to read.
    pub const fn body_len(self) -> usize {
        self.body_bytes as usize
    }
    /// Returns the exact connection epoch.
    pub const fn epoch(self) -> u64 {
        self.epoch
    }
    /// Returns the closed frame kind.
    pub const fn kind(self) -> FrameKind {
        self.kind
    }
    /// Returns the request, stream or heartbeat identity according to its kind.
    pub const fn identity(self) -> u64 {
        self.identity
    }
    /// Returns the ordered fragment or credit-grant sequence.
    pub const fn sequence(self) -> u32 {
        self.sequence
    }
    /// Returns message completion or data-stream EOF, according to its kind.
    pub const fn is_final(self) -> bool {
        self.final_fragment
    }
    /// Completes decoding without allowing a different or partial body length.
    pub fn with_body(self, body: Vec<u8>) -> Result<Frame> {
        if body.len() != self.body_len() {
            return Err(FrameError::Invalid);
        }
        if self.kind == FrameKind::Credit {
            let credits = u32::from_be_bytes(
                body.as_slice()
                    .try_into()
                    .map_err(|_| FrameError::Invalid)?,
            );
            if !(1..=STREAM_WINDOW_FRAMES).contains(&credits) {
                return Err(FrameError::Invalid);
            }
        }
        Ok(Frame { header: self, body })
    }
    fn encode(self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0; HEADER_BYTES];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4] = self.kind as u8;
        bytes[5] = u8::from(self.final_fragment);
        bytes[8..16].copy_from_slice(&self.epoch.to_be_bytes());
        bytes[16..24].copy_from_slice(&self.identity.to_be_bytes());
        bytes[24..28].copy_from_slice(&self.body_bytes.to_be_bytes());
        bytes[28..32].copy_from_slice(&self.sequence.to_be_bytes());
        bytes
    }
}

/// Complete bounded frame, without any target-operation or allocation authority.
#[derive(Eq, PartialEq)]
pub struct Frame {
    header: FrameHeader,
    body: Vec<u8>,
}
impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("header", &self.header)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}
impl Frame {
    /// Constructs one frame at the sending boundary, using the same wire invariants.
    pub fn new(
        epoch: u64,
        kind: FrameKind,
        identity: u64,
        sequence: u32,
        final_fragment: bool,
        body: Vec<u8>,
    ) -> Result<Self> {
        let header = FrameHeader {
            epoch,
            kind,
            identity,
            sequence,
            final_fragment,
            body_bytes: u32::try_from(body.len()).map_err(|_| FrameError::Capacity)?,
        };
        header.validate()?;
        header.with_body(body)
    }
    /// Returns validated routing and length metadata.
    pub const fn header(&self) -> FrameHeader {
        self.header
    }
    /// Borrows the complete raw body, which is not implicitly trusted JSON.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Consumes the frame into its exact body without another byte allocation.
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
    /// Encodes one header only after confirming the sending connection epoch.
    pub fn encode_header(&self, expected_epoch: u64) -> Result<[u8; HEADER_BYTES]> {
        if self.header.epoch != expected_epoch {
            return Err(FrameError::Epoch);
        }
        Ok(self.header.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_headers_reject_before_body_allocation_and_old_epochs_never_encode() {
        let frame = Frame::new(9, FrameKind::Request, 12, 0, true, b"{}".to_vec()).unwrap();
        let encoded = frame.encode_header(9).unwrap();
        for length in 0..HEADER_BYTES {
            assert!(FrameHeader::decode(&encoded[..length], 9).is_err());
        }
        assert_eq!(FrameHeader::decode(&encoded, 10), Err(FrameError::Epoch));
        assert_eq!(frame.encode_header(10), Err(FrameError::Epoch));
        for (offset, value) in [(0, 0), (3, 2), (4, 255), (5, 2), (6, 1), (7, 1)] {
            let mut corrupt = encoded;
            corrupt[offset] = value;
            assert!(FrameHeader::decode(&corrupt, 9).is_err());
        }
        let mut oversized = encoded;
        oversized[24..28].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            FrameHeader::decode(&oversized, 9),
            Err(FrameError::Capacity)
        );
        let header = FrameHeader::decode(&encoded, 9).unwrap();
        assert_eq!(header.with_body(vec![0]), Err(FrameError::Invalid));
        assert_eq!(header.with_body(b"{}".to_vec()).unwrap(), frame);
    }
    #[test]
    fn reserved_control_and_stream_credit_have_independent_closed_bounds() {
        assert!(
            Frame::new(
                1,
                FrameKind::Data,
                1,
                0,
                false,
                vec![0; MAXIMUM_FRAGMENT_BYTES]
            )
            .is_ok()
        );
        assert_eq!(
            Frame::new(1, FrameKind::Data, 1, 0, false, vec![]),
            Err(FrameError::Invalid)
        );
        assert!(Frame::new(1, FrameKind::Data, 1, 1, true, vec![]).is_ok());
        assert_eq!(
            Frame::new(
                1,
                FrameKind::ControlRequest,
                1,
                0,
                true,
                vec![0; MAXIMUM_CONTROL_BYTES + 1]
            ),
            Err(FrameError::Capacity)
        );
        assert_eq!(
            Frame::new(1, FrameKind::ControlReply, 1, 1, true, vec![0]),
            Err(FrameError::Invalid)
        );
        for credits in 0..=STREAM_WINDOW_FRAMES + 1 {
            let result = Frame::new(
                1,
                FrameKind::Credit,
                2,
                1,
                true,
                credits.to_be_bytes().to_vec(),
            );
            assert_eq!(
                result.is_ok(),
                (1..=STREAM_WINDOW_FRAMES).contains(&credits)
            );
        }
        assert!(Frame::new(1, FrameKind::Heartbeat, 1, 0, true, vec![]).is_ok());
        assert_eq!(
            Frame::new(1, FrameKind::HeartbeatAck, 1, 0, true, vec![0]),
            Err(FrameError::Invalid)
        );
        assert!(Frame::new(1, FrameKind::Close, 0, 0, true, vec![]).is_ok());
        assert_eq!(
            Frame::new(1, FrameKind::Close, 1, 0, true, vec![]),
            Err(FrameError::Invalid)
        );
    }
}

#[cfg(test)]
mod debug_redaction_tests {
    use super::*;
    #[test]
    fn debug_omits_frame_payload_bytes() {
        let frame = Frame::new(1, FrameKind::Request, 2, 0, true, b"secret".to_vec()).unwrap();
        let debug = format!("{frame:?}");
        assert!(debug.contains("body_bytes: 6"));
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("115, 101, 99"));
    }
}
