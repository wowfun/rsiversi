use crate::{decode_error, invalid, validate_json};
use rsi_api_protocol::{
    ApiError, ApiMessage, ByteAccumulator, ByteBudget, MAXIMUM_API_BYTES, Result,
};

/// One decoded item or explicit terminal frame; the transport must still verify EOF.
#[derive(Debug)]
pub enum SseEvent {
    /// Raw JSON from a single admitted domain item.
    Item(ApiMessage),
    /// Explicit stream end, optionally carrying the preceding bounded error.
    End(Option<ApiError>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Item,
    Error,
    DomainError,
    End,
}
#[derive(Debug)]
enum Phase {
    Header,
    OpeningSeparator,
    DataPrefix(usize),
    Payload,
    Separator,
}

/// Queue-free incremental decoder for the exact API SSE framing contract.
#[derive(Debug)]
pub struct SseDecoder {
    budget: ByteBudget,
    retained: ByteBudget,
    maximum: usize,
    phase: Phase,
    kind: Option<Kind>,
    prefix: [u8; 32],
    prefix_len: usize,
    fixed: [u8; 128],
    fixed_len: usize,
    payload: Option<ByteAccumulator>,
    pending_error: Option<ApiError>,
    opening_allowed: bool,
    ended: bool,
    failed: bool,
}
impl SseDecoder {
    /// Binds partial and completed items to their explicit shared byte owners.
    pub fn new(budget: ByteBudget, retained: ByteBudget, maximum: usize) -> Result<Self> {
        if maximum > MAXIMUM_API_BYTES {
            return Err(invalid());
        }
        Ok(Self {
            budget,
            retained,
            maximum,
            phase: Phase::Header,
            kind: None,
            prefix: [0; 32],
            prefix_len: 0,
            fixed: [0; 128],
            fixed_len: 0,
            payload: None,
            pending_error: None,
            opening_allowed: true,
            ended: false,
            failed: false,
        })
    }
    /// Consumes at most one event, returning the consumed byte count without buffering a queue.
    pub fn push(&mut self, input: &[u8]) -> Result<(usize, Option<SseEvent>)> {
        let result = self.push_inner(input);
        if result.is_err() {
            self.failed = true;
            self.payload = None;
            self.pending_error = None;
        }
        result
    }
    fn push_inner(&mut self, input: &[u8]) -> Result<(usize, Option<SseEvent>)> {
        if self.failed || (self.ended && !input.is_empty()) {
            return Err(invalid());
        }
        let mut consumed = 0;
        while consumed < input.len() {
            let byte = input[consumed];
            match self.phase {
                Phase::Header => {
                    if self.prefix_len == self.prefix.len() {
                        return Err(invalid());
                    }
                    self.prefix[self.prefix_len] = byte;
                    self.prefix_len += 1;
                    if byte == b'\n' {
                        if &self.prefix[..self.prefix_len] == b": ready\n" && self.opening_allowed {
                            self.opening_allowed = false;
                            self.prefix_len = 0;
                            self.phase = Phase::OpeningSeparator;
                            consumed += 1;
                            continue;
                        }
                        self.opening_allowed = false;
                        let kind = match &self.prefix[..self.prefix_len] {
                            b"event: item\n" => Kind::Item,
                            b"event: error\n" => Kind::Error,
                            b"event: domain-error\n" => Kind::DomainError,
                            b"event: end\n" => Kind::End,
                            _ => return Err(invalid()),
                        };
                        if self.pending_error.is_some() && kind != Kind::End {
                            return Err(invalid());
                        }
                        self.kind = Some(kind);
                        self.prefix_len = 0;
                        self.phase = Phase::DataPrefix(0);
                    }
                }
                Phase::OpeningSeparator => {
                    if byte != b'\n' {
                        return Err(invalid());
                    }
                    self.phase = Phase::Header;
                }
                Phase::DataPrefix(index) => {
                    if byte != b"data: "[index] {
                        return Err(invalid());
                    }
                    if index == 5 {
                        if matches!(self.kind, Some(Kind::Item | Kind::DomainError)) {
                            self.payload = Some(ByteAccumulator::new(&self.budget, self.maximum)?);
                        }
                        self.phase = Phase::Payload;
                    } else {
                        self.phase = Phase::DataPrefix(index + 1);
                    }
                }
                Phase::Payload => {
                    let remaining = &input[consumed..];
                    let take = remaining
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .unwrap_or(remaining.len());
                    if let Some(payload) = &mut self.payload {
                        payload.append(&remaining[..take])?;
                    } else {
                        let maximum = if self.kind == Some(Kind::End) {
                            2
                        } else {
                            self.fixed.len()
                        };
                        if take > maximum - self.fixed_len {
                            return Err(invalid());
                        }
                        self.fixed[self.fixed_len..self.fixed_len + take]
                            .copy_from_slice(&remaining[..take]);
                        self.fixed_len += take;
                    }
                    consumed += take;
                    if take == remaining.len() {
                        break;
                    }
                    self.phase = Phase::Separator;
                }
                Phase::Separator => {
                    if byte != b'\n' {
                        return Err(invalid());
                    }
                    let event = self.complete()?;
                    consumed += 1;
                    if event.is_some() {
                        return Ok((consumed, event));
                    }
                    continue;
                }
            }
            consumed += 1;
        }
        Ok((consumed, None))
    }
    fn complete(&mut self) -> Result<Option<SseEvent>> {
        let kind = self.kind.take().ok_or_else(invalid)?;
        let event = match kind {
            Kind::Item | Kind::DomainError => {
                let bytes = self
                    .payload
                    .take()
                    .ok_or_else(invalid)?
                    .finish_into(&self.retained)?;
                validate_json(bytes.as_bytes())?;
                if kind == Kind::Item {
                    Some(SseEvent::Item(ApiMessage {
                        json: bytes,
                        binary: None,
                    }))
                } else {
                    self.pending_error = Some(ApiError::Domain(bytes));
                    None
                }
            }
            Kind::Error => {
                self.pending_error = Some(decode_error(&self.fixed[..self.fixed_len])?);
                None
            }
            Kind::End => {
                if &self.fixed[..self.fixed_len] != b"{}" {
                    return Err(invalid());
                }
                self.ended = true;
                Some(SseEvent::End(self.pending_error.take()))
            }
        };
        self.fixed_len = 0;
        self.phase = Phase::Header;
        Ok(event)
    }
    /// Confirms body EOF followed an explicit terminal frame without trailing bytes.
    pub fn finish(self) -> Result<()> {
        if self.failed || !self.ended || self.prefix_len != 0 {
            return Err(invalid());
        }
        Ok(())
    }
}
