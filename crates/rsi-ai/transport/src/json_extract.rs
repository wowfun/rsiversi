use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use crate::TransportError;

const MAX_JSON_NESTING: usize = 64;

/// Independent bounds for one streamed JSON extraction.
#[derive(Clone, Copy, Debug)]
pub struct JsonExtractionLimits {
    total: usize,
    envelope: usize,
    extracted: usize,
}

impl JsonExtractionLimits {
    /// Creates nonzero total, retained-envelope, and per-chunk/item bounds.
    pub fn new(
        total_bytes: usize,
        envelope_bytes: usize,
        extracted_bytes: usize,
    ) -> Result<Self, TransportError> {
        if total_bytes == 0
            || envelope_bytes == 0
            || extracted_bytes == 0
            || envelope_bytes > total_bytes
            || extracted_bytes > total_bytes
        {
            return Err(extract_error("JSON extraction limits are invalid"));
        }
        Ok(Self {
            total: total_bytes,
            envelope: envelope_bytes,
            extracted: extracted_bytes,
        })
    }
}

/// One value emitted while a selected JSON field is removed from its envelope.
#[derive(Eq, PartialEq)]
pub enum JsonExtractEvent {
    /// The selected string value has begun.
    TargetStarted,
    /// One decoded chunk from the selected JSON string.
    StringChunk(Vec<u8>),
    /// One complete raw object from the selected JSON array.
    ArrayItem(Vec<u8>),
}

/// Progress from scanning one response slice up to its next extracted event.
#[derive(Debug, Eq, PartialEq)]
pub struct JsonExtractProgress {
    /// Input bytes consumed from the supplied slice.
    pub consumed: usize,
    /// First extracted event encountered, when the slice contained one.
    pub event: Option<JsonExtractEvent>,
}

impl fmt::Debug for JsonExtractEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetStarted => formatter.write_str("TargetStarted"),
            Self::StringChunk(bytes) => formatter
                .debug_struct("StringChunk")
                .field("byte_len", &bytes.len())
                .finish(),
            Self::ArrayItem(bytes) => formatter
                .debug_struct("ArrayItem")
                .field("byte_len", &bytes.len())
                .finish(),
        }
    }
}

/// The validated JSON envelope after the selected content was normalized away.
#[derive(Eq, PartialEq)]
pub struct JsonExtraction {
    /// Complete JSON bytes with the selected string empty or array items `null`.
    pub envelope: Vec<u8>,
}

impl fmt::Debug for JsonExtraction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonExtraction")
            .field("envelope_bytes", &self.envelope.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExtractionMode {
    String,
    ObjectArray,
}

struct TargetSegment {
    value: String,
    index: Option<usize>,
}

enum JsonContext {
    Object {
        key: Option<String>,
        expects_key: bool,
    },
    Array {
        index: usize,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum StringEscape {
    #[default]
    None,
    Escaped,
    Unicode {
        value: u16,
        digits: u8,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EnvelopeString {
    #[default]
    Outside,
    Key {
        escaped: bool,
    },
    Value {
        escaped: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum TargetState {
    #[default]
    Searching,
    AwaitingColon,
    AwaitingValue,
    Extracted,
}

enum ExtractionState {
    Normal,
    String {
        escape: StringEscape,
        chunk: Vec<u8>,
    },
    Array {
        expects_item: bool,
    },
    Item {
        bytes: Vec<u8>,
        depth: usize,
        in_string: bool,
        escaped: bool,
    },
}

/// Incrementally extracts one large string field or one array of objects from
/// a bounded JSON response while retaining only a normalized envelope.
pub struct BoundedJsonExtractor {
    pointer: String,
    target: Vec<TargetSegment>,
    mode: ExtractionMode,
    limits: JsonExtractionLimits,
    total_bytes: usize,
    envelope: Vec<u8>,
    contexts: Vec<JsonContext>,
    envelope_string: EnvelopeString,
    key: Vec<u8>,
    target_state: TargetState,
    state: ExtractionState,
}

impl fmt::Debug for BoundedJsonExtractor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedJsonExtractor")
            .field("pointer", &self.pointer)
            .field("mode", &self.mode)
            .field("limits", &self.limits)
            .field("total_bytes", &self.total_bytes)
            .field("envelope_bytes", &self.envelope.len())
            .field("context_depth", &self.contexts.len())
            .field("target_state", &self.target_state)
            .finish_non_exhaustive()
    }
}

impl BoundedJsonExtractor {
    /// Selects one JSON string field and emits its decoded ASCII bytes in
    /// bounded chunks.
    pub fn string(
        pointer: impl Into<String>,
        limits: JsonExtractionLimits,
    ) -> Result<Self, TransportError> {
        Self::new(pointer.into(), ExtractionMode::String, limits)
    }

    /// Selects one JSON array whose elements must each be objects and emits one
    /// bounded raw JSON object at a time.
    pub fn object_array(
        pointer: impl Into<String>,
        limits: JsonExtractionLimits,
    ) -> Result<Self, TransportError> {
        Self::new(pointer.into(), ExtractionMode::ObjectArray, limits)
    }

    fn new(
        pointer: String,
        mode: ExtractionMode,
        limits: JsonExtractionLimits,
    ) -> Result<Self, TransportError> {
        let target = parse_pointer(&pointer)?;
        if target.is_empty() {
            return Err(extract_error("JSON extraction pointer must select a field"));
        }
        Ok(Self {
            pointer,
            target,
            mode,
            limits,
            total_bytes: 0,
            envelope: Vec::new(),
            contexts: Vec::new(),
            envelope_string: EnvelopeString::Outside,
            key: Vec::new(),
            target_state: TargetState::Searching,
            state: ExtractionState::Normal,
        })
    }

    /// Pushes one response byte and returns at most one extracted event.
    pub fn push(&mut self, byte: u8) -> Result<Option<JsonExtractEvent>, TransportError> {
        self.total_bytes = self
            .total_bytes
            .checked_add(1)
            .ok_or_else(|| extract_limit_error("JSON response length overflowed"))?;
        if self.total_bytes > self.limits.total {
            return Err(extract_limit_error(
                "JSON response exceeds its total byte bound",
            ));
        }

        let state = std::mem::replace(&mut self.state, ExtractionState::Normal);
        let (state, event) = match state {
            ExtractionState::Normal => {
                self.state = ExtractionState::Normal;
                return self.push_normal(byte);
            }
            ExtractionState::String { escape, chunk } => self.push_string(byte, escape, chunk)?,
            ExtractionState::Array { expects_item } => self.push_array(byte, expects_item)?,
            ExtractionState::Item {
                bytes,
                depth,
                in_string,
                escaped,
            } => self.push_item(byte, bytes, depth, in_string, escaped)?,
        };
        self.state = state;
        Ok(event)
    }

    /// Scans a response slice until its first event or the end of the slice.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<JsonExtractProgress, TransportError> {
        for (index, byte) in bytes.iter().copied().enumerate() {
            if let Some(event) = self.push(byte)? {
                return Ok(JsonExtractProgress {
                    consumed: index + 1,
                    event: Some(event),
                });
            }
        }
        Ok(JsonExtractProgress {
            consumed: bytes.len(),
            event: None,
        })
    }

    /// Finishes structural validation and returns the bounded normalized envelope.
    pub fn finish(self) -> Result<JsonExtraction, TransportError> {
        if !matches!(self.state, ExtractionState::Normal)
            || self.envelope_string != EnvelopeString::Outside
            || !self.contexts.is_empty()
            || self.target_state != TargetState::Extracted
        {
            return Err(extract_error(
                "JSON response ended before the selected field completed",
            ));
        }
        let value: Value = serde_json::from_slice(&self.envelope)
            .map_err(|_| extract_error("JSON response is malformed"))?;
        let selected = value
            .pointer(&self.pointer)
            .ok_or_else(|| extract_error("normalized JSON response lost the selected field"))?;
        let normalized = match self.mode {
            ExtractionMode::String => selected.as_str().is_some_and(str::is_empty),
            ExtractionMode::ObjectArray => selected
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_null)),
        };
        if !normalized {
            return Err(extract_error(
                "normalized JSON response has the wrong selected field type",
            ));
        }
        Ok(JsonExtraction {
            envelope: self.envelope,
        })
    }

    fn push_normal(&mut self, byte: u8) -> Result<Option<JsonExtractEvent>, TransportError> {
        self.push_envelope(byte)?;
        if self.target_state == TargetState::Searching
            && matches!(self.contexts.last(), Some(JsonContext::Array { .. }))
            && self.at_target()
        {
            self.target_state = TargetState::AwaitingValue;
        }
        if self.target_state == TargetState::AwaitingColon {
            if byte.is_ascii_whitespace() {
                return Ok(None);
            }
            if byte != b':' {
                return Err(extract_error("selected JSON field has no value separator"));
            }
            self.target_state = TargetState::AwaitingValue;
            return Ok(None);
        }
        if self.target_state == TargetState::AwaitingValue {
            if byte.is_ascii_whitespace() {
                return Ok(None);
            }
            self.target_state = TargetState::Extracted;
            return match (self.mode, byte) {
                (ExtractionMode::String, b'"') => {
                    self.state = ExtractionState::String {
                        escape: StringEscape::None,
                        chunk: Vec::with_capacity(self.limits.extracted),
                    };
                    Ok(Some(JsonExtractEvent::TargetStarted))
                }
                (ExtractionMode::ObjectArray, b'[') => {
                    self.state = ExtractionState::Array { expects_item: true };
                    Ok(None)
                }
                (ExtractionMode::String, _) => {
                    Err(extract_error("selected JSON field is not a string"))
                }
                (ExtractionMode::ObjectArray, _) => {
                    Err(extract_error("selected JSON field is not an array"))
                }
            };
        }
        if self.push_envelope_string(byte)? {
            return Ok(None);
        }

        self.push_structure(byte)?;
        Ok(None)
    }

    fn push_structure(&mut self, byte: u8) -> Result<(), TransportError> {
        match byte {
            b'"' => {
                let is_key = matches!(
                    self.contexts.last(),
                    Some(JsonContext::Object {
                        expects_key: true,
                        ..
                    })
                );
                self.key.clear();
                self.envelope_string = if is_key {
                    EnvelopeString::Key { escaped: false }
                } else {
                    EnvelopeString::Value { escaped: false }
                };
            }
            b'{' => self.contexts.push(JsonContext::Object {
                key: None,
                expects_key: true,
            }),
            b'[' => self.contexts.push(JsonContext::Array { index: 0 }),
            b'}' | b']' => {
                self.contexts
                    .pop()
                    .ok_or_else(|| extract_error("JSON response has unbalanced nesting"))?;
            }
            b',' => match self.contexts.last_mut() {
                Some(JsonContext::Object { key, expects_key }) => {
                    *key = None;
                    *expects_key = true;
                }
                Some(JsonContext::Array { index }) => {
                    *index = index
                        .checked_add(1)
                        .ok_or_else(|| extract_error("JSON array index overflowed"))?;
                }
                None => {}
            },
            _ => {}
        }
        if self.contexts.len() > MAX_JSON_NESTING {
            return Err(extract_limit_error(
                "JSON response nesting exceeds its bound",
            ));
        }
        Ok(())
    }

    fn push_envelope_string(&mut self, byte: u8) -> Result<bool, TransportError> {
        let state = std::mem::take(&mut self.envelope_string);
        let (next, finished_key) = match state {
            EnvelopeString::Outside => return Ok(false),
            EnvelopeString::Key { escaped: false } if byte == b'\\' => {
                self.key.push(byte);
                (EnvelopeString::Key { escaped: true }, false)
            }
            EnvelopeString::Key { escaped: false } if byte == b'"' => {
                (EnvelopeString::Outside, true)
            }
            EnvelopeString::Key { .. } => {
                self.key.push(byte);
                (EnvelopeString::Key { escaped: false }, false)
            }
            EnvelopeString::Value { escaped: false } if byte == b'\\' => {
                (EnvelopeString::Value { escaped: true }, false)
            }
            EnvelopeString::Value { escaped: false } if byte == b'"' => {
                (EnvelopeString::Outside, false)
            }
            EnvelopeString::Value { .. } => (EnvelopeString::Value { escaped: false }, false),
        };
        self.envelope_string = next;
        if finished_key {
            self.finish_key()?;
        }
        Ok(true)
    }

    fn finish_key(&mut self) -> Result<(), TransportError> {
        let mut quoted = Vec::with_capacity(self.key.len().saturating_add(2));
        quoted.push(b'"');
        quoted.append(&mut self.key);
        quoted.push(b'"');
        let key = serde_json::from_slice::<String>(&quoted)
            .map_err(|_| extract_error("JSON response contains a malformed object key"))?;
        let Some(JsonContext::Object {
            key: current,
            expects_key,
        }) = self.contexts.last_mut()
        else {
            return Err(extract_error(
                "JSON response has an object key outside an object",
            ));
        };
        *current = Some(key);
        *expects_key = false;
        if self.at_target() {
            if self.target_state == TargetState::Extracted {
                return Err(extract_error("selected JSON field occurs more than once"));
            }
            self.target_state = TargetState::AwaitingColon;
        }
        Ok(())
    }

    fn at_target(&self) -> bool {
        self.contexts.len() == self.target.len()
            && self
                .contexts
                .iter()
                .zip(&self.target)
                .all(|(context, target)| match context {
                    JsonContext::Object { key, .. } => key.as_deref() == Some(&target.value),
                    JsonContext::Array { index } => target.index == Some(*index),
                })
    }

    fn push_string(
        &mut self,
        byte: u8,
        escape: StringEscape,
        mut chunk: Vec<u8>,
    ) -> Result<(ExtractionState, Option<JsonExtractEvent>), TransportError> {
        let decoded = match escape {
            StringEscape::None => match byte {
                b'"' => {
                    self.push_envelope(byte)?;
                    let event = (!chunk.is_empty())
                        .then(|| JsonExtractEvent::StringChunk(std::mem::take(&mut chunk)));
                    return Ok((ExtractionState::Normal, event));
                }
                b'\\' => {
                    return Ok((
                        ExtractionState::String {
                            escape: StringEscape::Escaped,
                            chunk,
                        },
                        None,
                    ));
                }
                byte if byte.is_ascii() && byte >= 0x20 => byte,
                _ => return Err(extract_error("selected JSON string is not valid ASCII")),
            },
            StringEscape::Escaped => match byte {
                b'"' | b'\\' | b'/' => byte,
                b'b' => 0x08,
                b'f' => 0x0c,
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'u' => {
                    return Ok((
                        ExtractionState::String {
                            escape: StringEscape::Unicode {
                                value: 0,
                                digits: 0,
                            },
                            chunk,
                        },
                        None,
                    ));
                }
                _ => return Err(extract_error("selected JSON string has an invalid escape")),
            },
            StringEscape::Unicode { value, digits } => {
                let digit = hex_digit(byte)
                    .ok_or_else(|| extract_error("selected JSON string has invalid Unicode"))?;
                let value = (value << 4) | digit;
                if digits != 3 {
                    return Ok((
                        ExtractionState::String {
                            escape: StringEscape::Unicode {
                                value,
                                digits: digits + 1,
                            },
                            chunk,
                        },
                        None,
                    ));
                }
                u8::try_from(value)
                    .ok()
                    .filter(u8::is_ascii)
                    .ok_or_else(|| extract_error("selected JSON string is not valid ASCII"))?
            }
        };
        chunk.push(decoded);
        if chunk.len() == self.limits.extracted {
            return Ok((
                ExtractionState::String {
                    escape: StringEscape::None,
                    chunk: Vec::with_capacity(self.limits.extracted),
                },
                Some(JsonExtractEvent::StringChunk(chunk)),
            ));
        }
        Ok((
            ExtractionState::String {
                escape: StringEscape::None,
                chunk,
            },
            None,
        ))
    }

    fn push_array(
        &mut self,
        byte: u8,
        expects_item: bool,
    ) -> Result<(ExtractionState, Option<JsonExtractEvent>), TransportError> {
        if byte.is_ascii_whitespace() {
            self.push_envelope(byte)?;
            return Ok((ExtractionState::Array { expects_item }, None));
        }
        if expects_item {
            if byte == b']' {
                self.push_envelope(byte)?;
                return Ok((ExtractionState::Normal, None));
            }
            if byte != b'{' {
                return Err(extract_error(
                    "selected JSON array contains a non-object item",
                ));
            }
            self.push_envelope_bytes(b"null")?;
            return Ok((
                ExtractionState::Item {
                    bytes: vec![byte],
                    depth: 1,
                    in_string: false,
                    escaped: false,
                },
                None,
            ));
        }
        match byte {
            b',' => {
                self.push_envelope(byte)?;
                Ok((ExtractionState::Array { expects_item: true }, None))
            }
            b']' => {
                self.push_envelope(byte)?;
                Ok((ExtractionState::Normal, None))
            }
            _ => Err(extract_error("selected JSON array is malformed")),
        }
    }

    fn push_item(
        &self,
        byte: u8,
        mut bytes: Vec<u8>,
        mut depth: usize,
        mut in_string: bool,
        mut escaped: bool,
    ) -> Result<(ExtractionState, Option<JsonExtractEvent>), TransportError> {
        bytes.push(byte);
        if bytes.len() > self.limits.extracted {
            return Err(extract_limit_error(
                "selected JSON array item exceeds its byte bound",
            ));
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            } else if byte < 0x20 {
                return Err(extract_error("selected JSON array item is malformed"));
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => {
                    depth = depth
                        .checked_add(1)
                        .ok_or_else(|| extract_error("JSON item nesting overflowed"))?;
                    if self.contexts.len().saturating_add(depth) > MAX_JSON_NESTING {
                        return Err(extract_limit_error(
                            "JSON response nesting exceeds its bound",
                        ));
                    }
                }
                b'}' | b']' => {
                    depth = depth
                        .checked_sub(1)
                        .ok_or_else(|| extract_error("selected JSON array item is malformed"))?;
                    if depth == 0 {
                        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
                        serde::de::IgnoredAny::deserialize(&mut deserializer)
                            .and_then(|_| deserializer.end())
                            .map_err(|_| extract_error("selected JSON array item is malformed"))?;
                        return Ok((
                            ExtractionState::Array {
                                expects_item: false,
                            },
                            Some(JsonExtractEvent::ArrayItem(bytes)),
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok((
            ExtractionState::Item {
                bytes,
                depth,
                in_string,
                escaped,
            },
            None,
        ))
    }

    fn push_envelope(&mut self, byte: u8) -> Result<(), TransportError> {
        self.envelope.push(byte);
        if self.envelope.len() > self.limits.envelope {
            return Err(extract_limit_error(
                "normalized JSON envelope exceeds its byte bound",
            ));
        }
        Ok(())
    }

    fn push_envelope_bytes(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        for byte in bytes {
            self.push_envelope(*byte)?;
        }
        Ok(())
    }
}

fn parse_pointer(pointer: &str) -> Result<Vec<TargetSegment>, TransportError> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    if !pointer.starts_with('/') {
        return Err(extract_error("JSON extraction pointer is invalid"));
    }
    pointer[1..]
        .split('/')
        .map(|raw| {
            let mut value = String::with_capacity(raw.len());
            let mut chars = raw.chars();
            while let Some(character) = chars.next() {
                if character != '~' {
                    value.push(character);
                    continue;
                }
                value.push(match chars.next() {
                    Some('0') => '~',
                    Some('1') => '/',
                    _ => return Err(extract_error("JSON extraction pointer is invalid")),
                });
            }
            let index = value.parse().ok();
            Ok(TargetSegment { value, index })
        })
        .collect()
}

const fn hex_digit(byte: u8) -> Option<u16> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u16),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u16),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u16),
        _ => None,
    }
}

fn extract_error(message: impl Into<String>) -> TransportError {
    TransportError::new("json.extract", message)
}

fn extract_limit_error(message: impl Into<String>) -> TransportError {
    TransportError::new("json.extract_limit", message)
}
