use std::{
    io::{self, Read},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

use bytes::Bytes;
use futures_util::StreamExt as _;
use serde::de::DeserializeOwned;
use tokio::sync::{Semaphore, mpsc};

use crate::{ByteStream, TransportError};

const MAXIMUM_CONCURRENT_JSON_PROJECTIONS: usize = 32;
const MAXIMUM_RETAINED_STRING_FIELDS: usize = 32;
static JSON_PROJECTION_ADMISSION: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAXIMUM_CONCURRENT_JSON_PROJECTIONS)));

/// Body and selected-string bounds for one typed JSON control projection.
#[derive(Clone, Debug)]
pub struct JsonProjectionLimits {
    maximum_bytes: usize,
    retained_strings: Vec<RetainedStringLimit>,
}

#[derive(Clone, Debug)]
struct RetainedStringLimit {
    field: String,
    maximum_bytes: usize,
}

impl JsonProjectionLimits {
    /// Creates one projection with a nonzero total response-body bound.
    pub fn new(maximum_bytes: usize) -> Result<Self, TransportError> {
        if maximum_bytes == 0 {
            return Err(project_error("JSON projection byte bound must be nonzero"));
        }
        Ok(Self {
            maximum_bytes,
            retained_strings: Vec::new(),
        })
    }

    /// Bounds one selected top-level JSON string by its decoded UTF-8 bytes.
    pub fn with_top_level_string(
        mut self,
        field: impl Into<String>,
        maximum_bytes: usize,
    ) -> Result<Self, TransportError> {
        let field = field.into();
        if maximum_bytes == 0
            || maximum_bytes > self.maximum_bytes
            || field.is_empty()
            || field.len() > 255
            || !field.bytes().all(|byte| matches!(byte, 0x21..=0x7e))
        {
            return Err(project_error(
                "JSON projection retained-string bound is invalid",
            ));
        }
        if self.retained_strings.len() == MAXIMUM_RETAINED_STRING_FIELDS {
            return Err(project_error(
                "JSON projection has too many retained-string bounds",
            ));
        }
        if self
            .retained_strings
            .iter()
            .any(|current| current.field == field)
        {
            return Err(project_error(
                "JSON projection retained-string field is duplicated",
            ));
        }
        self.retained_strings.push(RetainedStringLimit {
            field,
            maximum_bytes,
        });
        Ok(self)
    }
}

/// Deserializes one small typed projection from a bounded JSON body stream.
///
/// Unknown fields are parsed and discarded by `serde_json` without retaining
/// their values. The returned type must itself bound every field it retains at
/// its owning semantic boundary.
pub async fn project_json_body<T>(
    mut body: ByteStream,
    limits: JsonProjectionLimits,
) -> Result<T, TransportError>
where
    T: DeserializeOwned + Send + 'static,
{
    let maximum_bytes = limits.maximum_bytes;
    let permit = Arc::clone(&JSON_PROJECTION_ADMISSION)
        .acquire_owned()
        .await
        .map_err(|_| project_error("JSON projection admission closed"))?;
    let (sender, receiver) = mpsc::channel(1);
    let retained_limit_exceeded = Arc::new(AtomicBool::new(false));
    let parser_limit_exceeded = Arc::clone(&retained_limit_exceeded);
    let mut parser = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let guard = RetainedStringGuard::new(limits.retained_strings);
        let reader = io::BufReader::with_capacity(
            8 * 1024,
            ChannelReader::new(receiver, guard, parser_limit_exceeded),
        );
        let mut deserializer = serde_json::Deserializer::from_reader(reader);
        let projection = T::deserialize(&mut deserializer).map_err(|_| {
            if retained_limit_exceeded.load(Ordering::Acquire) {
                project_limit_error("selected JSON string exceeds its decoded byte bound")
            } else {
                project_error("JSON response does not match its control projection")
            }
        })?;
        deserializer.end().map_err(|_| {
            if retained_limit_exceeded.load(Ordering::Acquire) {
                project_limit_error("selected JSON string exceeds its decoded byte bound")
            } else {
                project_error("JSON response has trailing or malformed data")
            }
        })?;
        Ok(projection)
    });

    let mut total_bytes = 0_usize;
    loop {
        let next = tokio::select! {
            result = &mut parser => {
                drop(sender);
                return result
                    .map_err(|error| project_error(format!("JSON projection task failed: {error}")))?;
            }
            next = body.next() => next,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                drop(sender);
                let _ignored = parser.await;
                return Err(error);
            }
        };
        total_bytes = total_bytes
            .checked_add(chunk.len())
            .ok_or_else(|| project_limit_error("JSON response length overflowed"))?;
        if total_bytes > maximum_bytes {
            drop(sender);
            let _ignored = parser.await;
            return Err(project_limit_error(
                "JSON response exceeds its total byte bound",
            ));
        }
        if sender.send(chunk).await.is_err() {
            break;
        }
    }
    drop(sender);
    parser
        .await
        .map_err(|error| project_error(format!("JSON projection task failed: {error}")))?
}

struct ChannelReader {
    receiver: mpsc::Receiver<Bytes>,
    current: Bytes,
    offset: usize,
    retained_strings: RetainedStringGuard,
    retained_limit_exceeded: Arc<AtomicBool>,
}

impl ChannelReader {
    fn new(
        receiver: mpsc::Receiver<Bytes>,
        retained_strings: RetainedStringGuard,
        retained_limit_exceeded: Arc<AtomicBool>,
    ) -> Self {
        Self {
            receiver,
            current: Bytes::new(),
            offset: 0,
            retained_strings,
            retained_limit_exceeded,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        while self.offset == self.current.len() {
            let Some(chunk) = self.receiver.blocking_recv() else {
                return Ok(0);
            };
            self.current = chunk;
            self.offset = 0;
        }
        let count = output.len().min(self.current.len() - self.offset);
        let source = &self.current[self.offset..self.offset + count];
        for byte in source.iter().copied() {
            if let Err(error) = self.retained_strings.push(byte) {
                if error == ProjectionScanError::Limit {
                    self.retained_limit_exceeded.store(true, Ordering::Release);
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "JSON projection retained-string guard rejected input",
                ));
            }
        }
        output[..count].copy_from_slice(source);
        self.offset += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_reader_fills_the_available_output_span() {
        let (sender, receiver) = mpsc::channel(1);
        sender
            .blocking_send(Bytes::from_static(b"abcdefgh"))
            .unwrap();
        drop(sender);
        let exceeded = Arc::new(AtomicBool::new(false));
        let mut reader =
            ChannelReader::new(receiver, RetainedStringGuard::new(Vec::new()), exceeded);
        let mut output = [0_u8; 8];

        assert_eq!(reader.read(&mut output).unwrap(), output.len());
        assert_eq!(&output, b"abcdefgh");
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProjectionScanError {
    Invalid,
    Limit,
}

struct RetainedStringGuard {
    limits: Vec<RetainedStringLimit>,
    maximum_field_bytes: usize,
    depth: usize,
    root_object: bool,
    expects_root_key: bool,
    pending_limit: Option<usize>,
    awaiting_selected_value: bool,
    string: Option<GuardString>,
}

struct GuardString {
    role: GuardStringRole,
    escape: GuardEscape,
}

enum GuardStringRole {
    Key { bytes: Vec<u8>, possible: bool },
    Selected { maximum: usize, decoded: usize },
    Other,
}

#[derive(Clone, Copy, Default)]
enum GuardEscape {
    #[default]
    None,
    Escaped,
    Unicode {
        value: u16,
        digits: u8,
    },
    LowBackslash,
    LowU,
    LowUnicode {
        value: u16,
        digits: u8,
    },
}

impl RetainedStringGuard {
    fn new(limits: Vec<RetainedStringLimit>) -> Self {
        let maximum_field_bytes = limits
            .iter()
            .map(|limit| limit.field.len())
            .max()
            .unwrap_or(0);
        Self {
            limits,
            maximum_field_bytes,
            depth: 0,
            root_object: false,
            expects_root_key: false,
            pending_limit: None,
            awaiting_selected_value: false,
            string: None,
        }
    }

    fn push(&mut self, byte: u8) -> Result<(), ProjectionScanError> {
        if let Some(mut string) = self.string.take() {
            if !self.push_string_byte(&mut string, byte)? {
                self.string = Some(string);
            }
            return Ok(());
        }

        if self.awaiting_selected_value {
            if byte.is_ascii_whitespace() {
                return Ok(());
            }
            self.awaiting_selected_value = false;
            let selected = self.pending_limit.take();
            if byte == b'"' {
                if let Some(index) = selected {
                    self.string = Some(GuardString {
                        role: GuardStringRole::Selected {
                            maximum: self.limits[index].maximum_bytes,
                            decoded: 0,
                        },
                        escape: GuardEscape::None,
                    });
                } else {
                    self.string = Some(GuardString {
                        role: GuardStringRole::Other,
                        escape: GuardEscape::None,
                    });
                }
                return Ok(());
            }
        }

        match byte {
            b'"' => {
                let role = if self.root_object && self.depth == 1 && self.expects_root_key {
                    self.expects_root_key = false;
                    GuardStringRole::Key {
                        bytes: Vec::with_capacity(self.maximum_field_bytes),
                        possible: true,
                    }
                } else {
                    GuardStringRole::Other
                };
                self.string = Some(GuardString {
                    role,
                    escape: GuardEscape::None,
                });
            }
            b'{' | b'[' => {
                self.depth = self
                    .depth
                    .checked_add(1)
                    .ok_or(ProjectionScanError::Invalid)?;
                if self.depth == 1 && byte == b'{' {
                    self.root_object = true;
                    self.expects_root_key = true;
                }
            }
            b'}' | b']' => {
                self.depth = self
                    .depth
                    .checked_sub(1)
                    .ok_or(ProjectionScanError::Invalid)?;
            }
            b':' if self.root_object && self.depth == 1 && self.pending_limit.is_some() => {
                self.awaiting_selected_value = true;
            }
            b',' if self.root_object && self.depth == 1 => {
                self.expects_root_key = true;
                self.pending_limit = None;
                self.awaiting_selected_value = false;
            }
            _ => {}
        }
        Ok(())
    }

    fn push_string_byte(
        &mut self,
        string: &mut GuardString,
        byte: u8,
    ) -> Result<bool, ProjectionScanError> {
        match string.escape {
            GuardEscape::None => match byte {
                b'"' => {
                    self.finish_string(&string.role);
                    return Ok(true);
                }
                b'\\' => string.escape = GuardEscape::Escaped,
                byte if byte >= 0x20 => self.push_decoded(&mut string.role, Some(byte), 1)?,
                _ => return Err(ProjectionScanError::Invalid),
            },
            GuardEscape::Escaped => match byte {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                    self.push_decoded(&mut string.role, Some(decoded_escape(byte)), 1)?;
                    string.escape = GuardEscape::None;
                }
                b'u' => {
                    string.escape = GuardEscape::Unicode {
                        value: 0,
                        digits: 0,
                    }
                }
                _ => return Err(ProjectionScanError::Invalid),
            },
            GuardEscape::Unicode { value, digits } => {
                let value = (value << 4) | hex_digit(byte).ok_or(ProjectionScanError::Invalid)?;
                if digits != 3 {
                    string.escape = GuardEscape::Unicode {
                        value,
                        digits: digits + 1,
                    };
                } else if (0xd800..=0xdbff).contains(&value) {
                    string.escape = GuardEscape::LowBackslash;
                } else if (0xdc00..=0xdfff).contains(&value) {
                    return Err(ProjectionScanError::Invalid);
                } else {
                    let character =
                        char::from_u32(u32::from(value)).ok_or(ProjectionScanError::Invalid)?;
                    self.push_decoded(
                        &mut string.role,
                        character.is_ascii().then_some(character as u8),
                        character.len_utf8(),
                    )?;
                    string.escape = GuardEscape::None;
                }
            }
            GuardEscape::LowBackslash => {
                if byte != b'\\' {
                    return Err(ProjectionScanError::Invalid);
                }
                string.escape = GuardEscape::LowU;
            }
            GuardEscape::LowU => {
                if byte != b'u' {
                    return Err(ProjectionScanError::Invalid);
                }
                string.escape = GuardEscape::LowUnicode {
                    value: 0,
                    digits: 0,
                };
            }
            GuardEscape::LowUnicode { value, digits } => {
                let value = (value << 4) | hex_digit(byte).ok_or(ProjectionScanError::Invalid)?;
                if digits == 3 {
                    if !(0xdc00..=0xdfff).contains(&value) {
                        return Err(ProjectionScanError::Invalid);
                    }
                    self.push_decoded(&mut string.role, None, 4)?;
                    string.escape = GuardEscape::None;
                } else {
                    string.escape = GuardEscape::LowUnicode {
                        value,
                        digits: digits + 1,
                    };
                }
            }
        }
        Ok(false)
    }

    fn push_decoded(
        &self,
        role: &mut GuardStringRole,
        ascii: Option<u8>,
        decoded_bytes: usize,
    ) -> Result<(), ProjectionScanError> {
        match role {
            GuardStringRole::Key { bytes, possible } => {
                let Some(byte) = ascii else {
                    *possible = false;
                    return Ok(());
                };
                if bytes.len() == self.maximum_field_bytes {
                    *possible = false;
                } else if *possible {
                    bytes.push(byte);
                }
            }
            GuardStringRole::Selected { maximum, decoded } => {
                *decoded = decoded
                    .checked_add(decoded_bytes)
                    .ok_or(ProjectionScanError::Limit)?;
                if *decoded > *maximum {
                    return Err(ProjectionScanError::Limit);
                }
            }
            GuardStringRole::Other => {}
        }
        Ok(())
    }

    fn finish_string(&mut self, role: &GuardStringRole) {
        let GuardStringRole::Key { bytes, possible } = role else {
            return;
        };
        self.pending_limit = (*possible)
            .then(|| {
                self.limits
                    .iter()
                    .position(|limit| limit.field.as_bytes() == bytes)
            })
            .flatten();
    }
}

const fn decoded_escape(byte: u8) -> u8 {
    match byte {
        b'b' => 0x08,
        b'f' => 0x0c,
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        byte => byte,
    }
}

const fn hex_digit(byte: u8) -> Option<u16> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u16),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u16),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u16),
        _ => None,
    }
}

fn project_error(message: impl Into<String>) -> TransportError {
    TransportError::new("json.project", message)
}

fn project_limit_error(message: impl Into<String>) -> TransportError {
    TransportError::new("json.project_limit", message)
}
