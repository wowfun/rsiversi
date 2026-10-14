//! Canonical bounded evidence for one model attempt, independent of transport framing.

use crate::{Result, SessionError};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rsi_ai_protocol::{ContentDelta, LanguageAssembler, LanguageAssemblyError, LanguageEvent};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Complete encoded settlement body ceiling, including identities and terminal reserve.
pub const MAXIMUM_MODEL_ATTEMPT_BYTES: usize = 32 * 1024 * 1024;
/// Bytes kept available for terminal evidence after accepting a prefix.
pub const MODEL_ATTEMPT_TERMINAL_RESERVE: usize = 64 * 1024;
/// Maximum encoded block, including its JSON string framing and separator.
pub const MODEL_ATTEMPT_CHUNK_BYTES: usize = 256 * 1024;
const RAW_CHUNK_BYTES: usize = (192 * 1024 - 4) / 4 * 3;
const STREAM_BYTES: usize = MAXIMUM_MODEL_ATTEMPT_BYTES - 4096;

fn invalid(reason: impl Into<String>) -> SessionError {
    SessionError::Invalid(format!("model attempt: {}", reason.into()))
}

#[derive(Clone, Debug, PartialEq)]
struct Chunk(Arc<str>);
impl Serialize for Chunk {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for Chunk {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        if text.len() + 3 > MODEL_ATTEMPT_CHUNK_BYTES {
            return Err(serde::de::Error::custom(
                "model attempt block exceeds 256 KiB",
            ));
        }
        Ok(Self(Arc::from(text)))
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamData {
    v: u8,
    retain_context: bool,
    chunks: Vec<Chunk>,
}

/// Validated complete attempt. Clones share immutable encoded blocks.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAttemptLog {
    data: Arc<StreamData>,
    terminal: Arc<LanguageEvent>,
}
impl Serialize for ModelAttemptLog {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.data.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for ModelAttemptLog {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::validate_data(StreamData::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
impl ModelAttemptLog {
    fn validate_data(data: StreamData) -> Result<Self> {
        if data.v != 1 || data.chunks.is_empty() {
            return Err(invalid("unsupported version or empty evidence"));
        }
        let mut bytes = 64_usize;
        for (index, chunk) in data.chunks.iter().enumerate() {
            bytes = bytes
                .checked_add(chunk.0.len() + 3)
                .ok_or_else(|| invalid("size overflow"))?;
            if bytes > STREAM_BYTES {
                return Err(invalid("encoded evidence exceeds 32 MiB"));
            }
            let decoded = STANDARD
                .decode(chunk.0.as_bytes())
                .map_err(|_| invalid("invalid base64"))?;
            if decoded.is_empty()
                || decoded.len() > RAW_CHUNK_BYTES
                || STANDARD.encode(&decoded) != chunk.0.as_ref()
                || (index + 1 < data.chunks.len() && decoded.len() != RAW_CHUNK_BYTES)
            {
                return Err(invalid("noncanonical block"));
            }
        }
        let mut assembler = LanguageAssembler::new();
        let mut terminal = None;
        for event in Events::new(&data.chunks) {
            let (_, event) = event?;
            assembler
                .push(&event)
                .map_err(|error| invalid(error.to_string()))?;
            if is_terminal(&event) {
                terminal = Some(event);
            }
        }
        let terminal = terminal.ok_or_else(|| invalid("missing terminal event"))?;
        match assembler.finish() {
            Err(LanguageAssemblyError::Protocol(error)) => return Err(invalid(error.to_string())),
            Ok(_)
            | Err(
                LanguageAssemblyError::Provider { .. } | LanguageAssemblyError::Incomplete { .. },
            ) => {}
        }
        Ok(Self {
            data: Arc::new(data),
            terminal: Arc::new(terminal),
        })
    }

    /// Iterates the lossless normalized events without expanding the whole log.
    pub fn events(&self) -> impl Iterator<Item = Result<(u64, LanguageEvent)>> + '_ {
        Events::new(&self.data.chunks)
    }

    /// Resumes without rescanning previously delivered original events.
    pub fn events_after<'a>(
        &'a self,
        cursor: &'a mut ModelEventCursor,
    ) -> impl Iterator<Item = Result<(u64, LanguageEvent)>> + 'a {
        events_after(&self.data.chunks, cursor)
    }

    /// The exact final event, cached at validation without a second persisted copy.
    pub fn terminal(&self) -> &LanguageEvent {
        &self.terminal
    }

    /// Whether safe partial content enters subsequent model requests.
    pub fn retain_context(&self) -> bool {
        self.data.retain_context
    }

    /// Builds deterministic evidence from elapsed millisecond offsets.
    pub fn from_events(
        events: impl IntoIterator<Item = (u64, LanguageEvent)>,
        retain_context: bool,
    ) -> Result<Self> {
        let mut builder = ModelAttemptBuilder::new();
        for (elapsed, event) in events {
            builder.push(elapsed, &event)?;
        }
        builder.finish(retain_context)
    }
}

/// Local replay cursor bound by the consumer to one exact attempt.
#[derive(Clone, Debug, Default)]
pub struct ModelEventCursor {
    raw_offset: usize,
    elapsed: u64,
    count: u64,
}
impl ModelEventCursor {
    /// Original events already consumed.
    pub fn event_count(&self) -> u64 {
        self.count
    }
}
fn events_after<'a>(
    chunks: &'a [Chunk],
    cursor: &'a mut ModelEventCursor,
) -> impl Iterator<Item = Result<(u64, LanguageEvent)>> + 'a {
    let mut events = Events::new(chunks);
    events.chunks = chunks
        .get(cursor.raw_offset / RAW_CHUNK_BYTES..)
        .unwrap_or_default()
        .iter();
    events.raw_offset = cursor.raw_offset;
    events.elapsed = cursor.elapsed;
    events.initial_skip = cursor.raw_offset % RAW_CHUNK_BYTES;
    std::iter::from_fn(move || {
        let next = events.next()?;
        if next.is_ok() {
            cursor.raw_offset = events.raw_offset;
            cursor.elapsed = events.elapsed;
            cursor.count += 1;
        }
        Some(next)
    })
}

/// Immutable event-aligned live prefix. Encoded blocks are shared with the accumulator.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ModelAttemptPrefix {
    chunks: Vec<Chunk>,
    events: u64,
}
impl ModelAttemptPrefix {
    /// Raw compact bytes, including the current unsealed tail.
    pub fn raw_len(&self) -> usize {
        self.chunks
            .iter()
            .map(|chunk| {
                chunk.0.len() / 4 * 3
                    - chunk
                        .0
                        .bytes()
                        .rev()
                        .take_while(|byte| *byte == b'=')
                        .count()
            })
            .sum()
    }

    /// Encodes only bytes after a previously delivered raw offset in this attempt.
    ///
    /// # Panics
    /// The caller must supply an offset within this prefix's [`Self::raw_len`].
    pub fn pages_after(&self, offset: usize) -> impl Iterator<Item = String> + '_ {
        assert!(offset <= self.raw_len(), "raw suffix offset exceeds prefix");
        self.chunks
            .iter()
            .skip(offset / RAW_CHUNK_BYTES)
            .enumerate()
            .map(move |(index, chunk)| {
                let skip = if index == 0 {
                    offset % RAW_CHUNK_BYTES
                } else {
                    0
                };
                if skip == 0 {
                    chunk.0.to_string()
                } else {
                    let bytes = STANDARD
                        .decode(chunk.0.as_bytes())
                        .expect("validated compact block");
                    STANDARD.encode(&bytes[skip..])
                }
            })
            .filter(|page| !page.is_empty())
    }

    /// Number of nonempty pages required for a raw suffix.
    ///
    /// # Panics
    /// The caller must supply an offset within this prefix's [`Self::raw_len`].
    pub fn page_count_after(&self, offset: usize) -> usize {
        let length = self.raw_len();
        assert!(offset <= length, "raw suffix offset exceeds prefix");
        if offset == length {
            0
        } else {
            self.chunks.len() - offset / RAW_CHUNK_BYTES
        }
    }
    /// Validates a complete immutable prefix reconstructed from bounded transport pages.
    pub fn from_pages(pages: Vec<String>, expected_events: u64) -> Result<Self> {
        let mut total = 64_usize;
        let count = pages.len();
        for (index, page) in pages.iter().enumerate() {
            total = total
                .checked_add(page.len() + 3)
                .ok_or_else(|| invalid("prefix size overflow"))?;
            if total > STREAM_BYTES || page.len() > 192 * 1024 {
                return Err(invalid("prefix capacity exceeded"));
            }
            let decoded = STANDARD
                .decode(page)
                .map_err(|_| invalid("invalid prefix base64"))?;
            if decoded.is_empty()
                || decoded.len() > RAW_CHUNK_BYTES
                || STANDARD.encode(&decoded) != *page
                || (index + 1 < count && decoded.len() != RAW_CHUNK_BYTES)
            {
                return Err(invalid("noncanonical prefix page"));
            }
        }
        let prefix = Self {
            chunks: pages
                .into_iter()
                .map(|page| Chunk(Arc::from(page)))
                .collect(),
            events: expected_events,
        };
        let mut assembler = LanguageAssembler::new();
        let mut actual = 0;
        for item in prefix.events() {
            assembler
                .push(&item?.1)
                .map_err(|error| invalid(error.to_string()))?;
            actual += 1;
        }
        if actual != expected_events {
            return Err(invalid("prefix event count differs"));
        }
        Ok(prefix)
    }

    /// Original event count, used to suppress duplicate snapshot delivery.
    pub fn event_count(&self) -> u64 {
        self.events
    }
    /// Encoded storage charged to observers.
    pub fn encoded_len(&self) -> usize {
        self.chunks
            .iter()
            .map(|chunk| chunk.0.len() + 3)
            .sum::<usize>()
            + 64
    }
    /// Lossless normalized events in their original order.
    pub fn events(&self) -> impl Iterator<Item = Result<(u64, LanguageEvent)>> + '_ {
        Events::new(&self.chunks)
    }
    /// Resumes at a previously delivered event boundary in this same attempt.
    pub fn events_after<'a>(
        &'a self,
        cursor: &'a mut ModelEventCursor,
    ) -> impl Iterator<Item = Result<(u64, LanguageEvent)>> + 'a {
        events_after(&self.chunks, cursor)
    }
    /// Immutable compact pages; each leaves at least 64 KiB for protocol framing.
    pub fn pages(&self) -> impl Iterator<Item = &str> {
        self.chunks.iter().map(|chunk| chunk.0.as_ref())
    }
}

/// Incremental external-prefix validation. A failed decoder must be discarded.
#[derive(Debug, Default)]
pub struct ModelPrefixDecoder {
    builder: ModelAttemptBuilder,
    cursor: ModelEventCursor,
    assembler: LanguageAssembler,
}
impl ModelPrefixDecoder {
    /// Number of accepted raw bytes, for exact transport continuity checks.
    pub fn raw_len(&self) -> usize {
        self.builder.raw_bytes
    }

    /// Appends one canonical bounded wire page; grammar is checked at the revision boundary.
    pub fn append(&mut self, page: &str) -> Result<()> {
        if page.is_empty() {
            return Ok(());
        }
        if page.len() > 192 * 1024 {
            return Err(invalid("prefix page exceeds capacity"));
        }
        let bytes = STANDARD
            .decode(page)
            .map_err(|_| invalid("invalid prefix base64"))?;
        if bytes.len() > RAW_CHUNK_BYTES || STANDARD.encode(&bytes) != page {
            return Err(invalid("noncanonical prefix page"));
        }
        let total = self
            .builder
            .raw_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("prefix overflow"))?;
        if total.div_ceil(3) * 4 + total.div_ceil(RAW_CHUNK_BYTES) * 3 + 64 > STREAM_BYTES {
            return Err(invalid("prefix capacity exceeded"));
        }
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() {
            let take = remaining
                .len()
                .min(RAW_CHUNK_BYTES - self.builder.pending.len());
            self.builder.pending.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.builder.pending.len() == RAW_CHUNK_BYTES {
                self.builder.seal();
            }
        }
        self.builder.raw_bytes = total;
        Ok(())
    }

    /// Checks only the new event suffix, then shares the complete validated prefix.
    pub fn prefix(&mut self, expected_events: u64) -> Result<ModelAttemptPrefix> {
        let mut prefix = self.builder.prefix();
        for item in prefix.events_after(&mut self.cursor) {
            self.assembler
                .push(&item?.1)
                .map_err(|error| invalid(error.to_string()))?;
        }
        if self.cursor.event_count() != expected_events {
            return Err(invalid("prefix event count differs"));
        }
        prefix.events = expected_events;
        Ok(prefix)
    }
}

/// An admitted attempt's compact accumulator. Capacity refusal leaves the accepted prefix intact.
#[derive(Debug, Default)]
pub struct ModelAttemptBuilder {
    chunks: Vec<Chunk>,
    pending: Vec<u8>,
    raw_bytes: usize,
    elapsed_ms: u64,
    terminal: bool,
    events: u64,
}
impl ModelAttemptBuilder {
    /// Starts an empty admitted attempt.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends exactly one original event and its clock offset.
    pub fn push(&mut self, elapsed_ms: u64, event: &LanguageEvent) -> Result<()> {
        if self.terminal || elapsed_ms < self.elapsed_ms {
            return Err(invalid("event follows terminal or clock moved backwards"));
        }
        event
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        let mut header = Vec::with_capacity(32);
        put_varint(elapsed_ms - self.elapsed_ms, &mut header);
        let value: std::borrow::Cow<'_, [u8]> = match event {
            LanguageEvent::ContentDelta { index, delta } => {
                let (tag, value) = match delta {
                    ContentDelta::Text(text) => (1, text),
                    ContentDelta::Reasoning(text) => (2, text),
                    ContentDelta::ToolArguments(text) => (3, text),
                };
                header.push(tag);
                put_varint(u64::from(*index), &mut header);
                std::borrow::Cow::Borrowed(value.as_bytes())
            }
            event => {
                header.push(0);
                std::borrow::Cow::Owned(
                    serde_json::to_vec(event).map_err(|error| invalid(error.to_string()))?,
                )
            }
        };
        put_varint(value.len() as u64, &mut header);
        let raw_bytes = self
            .raw_bytes
            .checked_add(header.len())
            .and_then(|bytes| bytes.checked_add(value.len()))
            .ok_or_else(|| invalid("size overflow"))?;
        let encoded = raw_bytes.div_ceil(3) * 4 + raw_bytes.div_ceil(RAW_CHUNK_BYTES) * 3 + 64;
        let maximum = STREAM_BYTES
            - if is_terminal(event) {
                0
            } else {
                MODEL_ATTEMPT_TERMINAL_RESERVE
            };
        if encoded > maximum {
            return Err(SessionError::TooLarge {
                kind: "model_output.capacity",
                maximum,
                actual: encoded,
            });
        }
        for mut remaining in [header.as_slice(), value.as_ref()] {
            while !remaining.is_empty() {
                let take = remaining.len().min(RAW_CHUNK_BYTES - self.pending.len());
                self.pending.extend_from_slice(&remaining[..take]);
                remaining = &remaining[take..];
                if self.pending.len() == RAW_CHUNK_BYTES {
                    self.seal();
                }
            }
        }
        self.raw_bytes = raw_bytes;
        self.elapsed_ms = elapsed_ms;
        self.terminal = is_terminal(event);
        self.events += 1;
        Ok(())
    }

    /// Takes a stable prefix while sharing every sealed block.
    pub fn prefix(&self) -> ModelAttemptPrefix {
        let mut chunks = self.chunks.clone();
        if !self.pending.is_empty() {
            chunks.push(Chunk(Arc::from(STANDARD.encode(&self.pending))));
        }
        ModelAttemptPrefix {
            chunks,
            events: self.events,
        }
    }

    fn seal(&mut self) {
        self.chunks
            .push(Chunk(Arc::from(STANDARD.encode(&self.pending))));
        self.pending.clear();
    }

    /// Seals and validates the complete grammar before durable publication.
    pub fn finish(mut self, retain_context: bool) -> Result<ModelAttemptLog> {
        if !self.pending.is_empty() {
            self.seal();
        }
        ModelAttemptLog::validate_data(StreamData {
            v: 1,
            retain_context,
            chunks: self.chunks,
        })
    }
}

fn is_terminal(event: &LanguageEvent) -> bool {
    matches!(
        event,
        LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
    )
}
fn put_varint(mut value: u64, output: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        output.push(byte | if value == 0 { 0 } else { 0x80 });
        if value == 0 {
            break;
        }
    }
}

struct Events<'a> {
    chunks: std::slice::Iter<'a, Chunk>,
    decoded: Vec<u8>,
    offset: usize,
    elapsed: u64,
    failed: bool,
    raw_offset: usize,
    initial_skip: usize,
}
impl<'a> Events<'a> {
    fn new(chunks: &'a [Chunk]) -> Self {
        Self {
            chunks: chunks.iter(),
            decoded: Vec::new(),
            offset: 0,
            elapsed: 0,
            failed: false,
            raw_offset: 0,
            initial_skip: 0,
        }
    }
    fn available(&mut self) -> Result<bool> {
        if self.offset == self.decoded.len() {
            let Some(chunk) = self.chunks.next() else {
                return Ok(false);
            };
            self.decoded = STANDARD
                .decode(chunk.0.as_bytes())
                .map_err(|_| invalid("invalid base64"))?;
            self.offset = std::mem::take(&mut self.initial_skip);
            if self.offset == self.decoded.len() {
                return Ok(false);
            }
        }
        if self.offset > self.decoded.len() {
            return Err(invalid("invalid block offset"));
        }
        Ok(true)
    }
    fn byte(&mut self) -> Result<Option<u8>> {
        if !self.available()? {
            return Ok(None);
        }
        let byte = self
            .decoded
            .get(self.offset)
            .copied()
            .ok_or_else(|| invalid("empty block"))?;
        self.offset += 1;
        self.raw_offset += 1;
        Ok(Some(byte))
    }
    fn required_byte(&mut self) -> Result<u8> {
        self.byte()?.ok_or_else(|| invalid("truncated record"))
    }
    fn varint(&mut self, first: Option<u8>) -> Result<u64> {
        let mut byte = match first {
            Some(byte) => byte,
            None => self.required_byte()?,
        };
        let mut value = 0_u64;
        for shift in (0..70).step_by(7) {
            if shift == 63 && byte > 1 {
                return Err(invalid("integer overflow"));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                if shift != 0 && byte == 0 {
                    return Err(invalid("noncanonical integer"));
                }
                return Ok(value);
            }
            byte = self.required_byte()?;
        }
        Err(invalid("invalid integer"))
    }
    fn record(&mut self, first: u8) -> Result<(u64, LanguageEvent)> {
        let delta = self.varint(Some(first))?;
        self.elapsed = self
            .elapsed
            .checked_add(delta)
            .ok_or_else(|| invalid("clock overflow"))?;
        let tag = self.required_byte()?;
        let index = if tag == 0 {
            0
        } else {
            u32::try_from(self.varint(None)?).map_err(|_| invalid("index overflow"))?
        };
        let length = usize::try_from(self.varint(None)?).map_err(|_| invalid("length overflow"))?;
        if length > STREAM_BYTES {
            return Err(invalid("record too large"));
        }
        let mut data = Vec::with_capacity(length);
        while data.len() < length {
            if !self.available()? {
                return Err(invalid("truncated record"));
            }
            let count = (length - data.len()).min(self.decoded.len() - self.offset);
            data.extend_from_slice(&self.decoded[self.offset..self.offset + count]);
            self.offset += count;
            self.raw_offset += count;
        }
        let event = if tag == 0 {
            let event: LanguageEvent =
                serde_json::from_slice(&data).map_err(|error| invalid(error.to_string()))?;
            if matches!(event, LanguageEvent::ContentDelta { .. })
                || serde_json::to_vec(&event).map_err(|error| invalid(error.to_string()))? != data
            {
                return Err(invalid("noncanonical event"));
            }
            event
        } else {
            let text = String::from_utf8(data).map_err(|_| invalid("invalid UTF-8"))?;
            let delta = match tag {
                1 => ContentDelta::Text(text),
                2 => ContentDelta::Reasoning(text),
                3 => ContentDelta::ToolArguments(text),
                _ => return Err(invalid("unknown event tag")),
            };
            LanguageEvent::ContentDelta { index, delta }
        };
        Ok((self.elapsed, event))
    }
}
impl Iterator for Events<'_> {
    type Item = Result<(u64, LanguageEvent)>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let result = match self.byte() {
            Ok(None) => return None,
            Ok(Some(first)) => self.record(first),
            Err(error) => Err(error),
        };
        self.failed = result.is_err();
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_ai_protocol::{ContentStart, FinishReason};

    #[test]
    fn suffix_page_boundaries_are_checked_before_indexing() {
        let mut builder = ModelAttemptBuilder::new();
        builder
            .push(
                0,
                &LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            )
            .unwrap();
        let prefix = builder.prefix();
        assert_eq!(prefix.page_count_after(0), 1);
        assert_eq!(prefix.page_count_after(prefix.raw_len()), 0);
        assert_eq!(prefix.pages_after(prefix.raw_len()).count(), 0);
        assert!(
            std::panic::catch_unwind(|| prefix.page_count_after(prefix.raw_len() + 1)).is_err()
        );
        assert!(
            std::panic::catch_unwind(|| prefix.pages_after(prefix.raw_len() + 1).count()).is_err()
        );
    }

    #[test]
    fn durable_decoder_rejects_oversized_final_chunk_and_invalid_index() {
        let log = ModelAttemptLog::from_events(
            [
                (
                    0,
                    LanguageEvent::ContentStarted {
                        index: 0,
                        content: ContentStart::Text,
                    },
                ),
                (
                    1,
                    LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("x".repeat(RAW_CHUNK_BYTES)),
                    },
                ),
                (2, LanguageEvent::ContentFinished { index: 0 }),
                (
                    3,
                    LanguageEvent::Finished {
                        reason: FinishReason::Stop,
                        replay: None,
                    },
                ),
            ],
            true,
        )
        .unwrap();
        let bytes: Vec<_> = log
            .data
            .chunks
            .iter()
            .flat_map(|chunk| STANDARD.decode(chunk.0.as_bytes()).unwrap())
            .collect();
        assert!(bytes.len() > RAW_CHUNK_BYTES);
        let mut wire = serde_json::to_value(&log).unwrap();
        wire["chunks"] = serde_json::json!([STANDARD.encode(bytes)]);
        assert!(serde_json::from_value::<ModelAttemptLog>(wire).is_err());

        let mut bytes = Vec::new();
        for event in [
            LanguageEvent::ContentStarted {
                index: 65_536,
                content: ContentStart::Text,
            },
            LanguageEvent::Finished {
                reason: FinishReason::MaxTokens,
                replay: None,
            },
        ] {
            let payload = serde_json::to_vec(&event).unwrap();
            bytes.extend([0, 0]);
            put_varint(payload.len() as u64, &mut bytes);
            bytes.extend(payload);
        }
        assert!(
            serde_json::from_value::<ModelAttemptLog>(serde_json::json!({
                "v": 1, "retain_context": true, "chunks": [STANDARD.encode(bytes)]
            }))
            .is_err()
        );
    }

    #[test]
    fn payload_crossing_multiple_chunks_preserves_unicode_and_cursor() {
        let mut builder = ModelAttemptBuilder::new();
        builder
            .push(
                0,
                &LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            )
            .unwrap();
        let mut cursor = ModelEventCursor::default();
        assert_eq!(builder.prefix().events_after(&mut cursor).count(), 1);
        let event = LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("界🦀".repeat(RAW_CHUNK_BYTES * 3 / 7 + 1)),
        };
        builder.push(127, &event).unwrap();
        let prefix = builder.prefix();
        assert!(prefix.chunks.len() > 3);
        assert_eq!(
            prefix
                .events_after(&mut cursor)
                .collect::<Result<Vec<_>>>()
                .unwrap(),
            vec![(127, event)]
        );
        assert_eq!(prefix.events_after(&mut cursor).count(), 0);
        builder
            .push(128, &LanguageEvent::ContentFinished { index: 0 })
            .unwrap();
        builder
            .push(
                16_384,
                &LanguageEvent::Finished {
                    reason: FinishReason::Stop,
                    replay: None,
                },
            )
            .unwrap();
        let log = builder.finish(true).unwrap();
        let decoded: ModelAttemptLog =
            serde_json::from_slice(&serde_json::to_vec(&log).unwrap()).unwrap();
        assert_eq!(decoded, log);
        assert_eq!(
            decoded
                .events_after(&mut cursor)
                .collect::<Result<Vec<_>>>()
                .unwrap()
                .iter()
                .map(|(at, _)| *at)
                .collect::<Vec<_>>(),
            vec![128, 16_384]
        );
        assert_eq!(cursor.event_count(), 4);
    }

    #[test]
    fn prefix_cursor_resumes_across_tail_growth_and_chunk_sealing() {
        let mut builder = ModelAttemptBuilder::new();
        builder
            .push(
                0,
                &LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            )
            .unwrap();
        let mut cursor = ModelEventCursor::default();
        assert_eq!(builder.prefix().events_after(&mut cursor).count(), 1);
        for batch in 0..50 {
            for offset in 0..1000 {
                builder
                    .push(
                        batch * 1000 + offset,
                        &LanguageEvent::ContentDelta {
                            index: 0,
                            delta: ContentDelta::Text("界🙂".into()),
                        },
                    )
                    .unwrap();
            }
            let prefix = builder.prefix();
            let values = prefix
                .events_after(&mut cursor)
                .collect::<Result<Vec<_>>>()
                .unwrap();
            assert_eq!(values.len(), 1000);
            assert_eq!(values[0].0, batch * 1000);
            assert_eq!(values[999].0, batch * 1000 + 999);
            assert_eq!(prefix.events_after(&mut cursor).count(), 0);
        }
        builder
            .push(50_000, &LanguageEvent::ContentFinished { index: 0 })
            .unwrap();
        builder
            .push(
                50_001,
                &LanguageEvent::Finished {
                    reason: FinishReason::Stop,
                    replay: None,
                },
            )
            .unwrap();
        let log = builder.finish(true).unwrap();
        assert_eq!(
            log.events_after(&mut cursor)
                .collect::<Result<Vec<_>>>()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(cursor.event_count(), 50_003);
    }

    #[test]
    fn incomplete_terminals_keep_open_text_and_tool_arguments_as_evidence() {
        for reason in [FinishReason::MaxTokens, FinishReason::Cancelled] {
            let mut builder = ModelAttemptBuilder::new();
            builder
                .push(
                    0,
                    &LanguageEvent::ContentStarted {
                        index: 0,
                        content: ContentStart::Text,
                    },
                )
                .unwrap();
            builder
                .push(
                    1,
                    &LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("partial".into()),
                    },
                )
                .unwrap();
            builder
                .push(
                    2,
                    &LanguageEvent::Finished {
                        reason,
                        replay: None,
                    },
                )
                .unwrap();
            let log = builder.finish(true).unwrap();
            assert_eq!(log.events().count(), 3);
        }
    }

    #[test]
    fn million_fragments_roundtrip_with_unicode_and_exact_clock_offsets() {
        let mut builder = ModelAttemptBuilder::new();
        builder
            .push(
                0,
                &LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            )
            .unwrap();
        for ordinal in 0..1_048_577 {
            builder
                .push(
                    ordinal,
                    &LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("界".into()),
                    },
                )
                .unwrap();
        }
        builder
            .push(1_048_577, &LanguageEvent::ContentFinished { index: 0 })
            .unwrap();
        builder
            .push(
                1_048_578,
                &LanguageEvent::Finished {
                    reason: FinishReason::Stop,
                    replay: None,
                },
            )
            .unwrap();
        let log = builder.finish(true).unwrap();
        let bytes = serde_json::to_vec(&log).unwrap();
        assert!(bytes.len() < 12 * 1024 * 1024);
        let decoded: ModelAttemptLog = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, log);
        let mut events = decoded.events();
        assert_eq!(events.next().unwrap().unwrap().0, 0);
        for ordinal in 0..1_048_577 {
            assert_eq!(
                events.next().unwrap().unwrap(),
                (
                    ordinal,
                    LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("界".into())
                    }
                )
            );
        }
        assert_eq!(events.count(), 2);
    }

    #[test]
    fn capacity_refusal_keeps_prefix_and_terminal_reserve() {
        let mut builder = ModelAttemptBuilder::new();
        builder
            .push(
                0,
                &LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            )
            .unwrap();
        let delta = LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("x".repeat(128 * 1024)),
        };
        let mut accepted = 0;
        while builder.push(1, &delta).is_ok() {
            accepted += 1;
        }
        let error = rsi_ai_protocol::AiError::new(
            rsi_ai_protocol::ErrorKind::OutputValidation,
            rsi_ai_protocol::ErrorPhase::Assemble,
            rsi_ai_protocol::DispatchStatus::Dispatched,
            "model_output.capacity",
        )
        .unwrap();
        builder
            .push(
                2,
                &LanguageEvent::Failed {
                    error,
                    replay: None,
                },
            )
            .unwrap();
        let log = builder.finish(true).unwrap();
        assert_eq!(log.events().count(), accepted + 2);
        assert!(serde_json::to_vec(&log).unwrap().len() < MAXIMUM_MODEL_ATTEMPT_BYTES);
    }
}
