use serde::Serialize;
use std::io::Write;

/// Maximum retained raw source bytes in one field window.
pub const MAXIMUM_WINDOW_BYTES: usize = 256 * 1024;

/// Invalid caller bounds or a failed linked serializer.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    /// A window must fit at least one UTF-8 scalar and remain within its ceiling.
    #[error("field window limit must be between 4 and 262144 bytes")]
    InvalidLimit,
    /// Serialization failed independently of the intentional window stop.
    #[error("field serialization failed: {0}")]
    Serialization(String),
}

/// Raw UTF-8 window; source offsets precede renderer filtering or formatting.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FieldWindow {
    /// Complete code points only.
    pub text: String,
    /// Actual first source byte after aligning the requested start.
    pub start: usize,
    /// Exclusive source byte following the returned text.
    pub end: usize,
    /// The source has more bytes after this window.
    pub more: bool,
}
fn limit(maximum: usize) -> Result<(), WindowError> {
    if !(4..=MAXIMUM_WINDOW_BYTES).contains(&maximum) {
        return Err(WindowError::InvalidLimit);
    }
    Ok(())
}
impl FieldWindow {
    /// Borrows and copies only the selected scalar-aligned text range.
    ///
    /// # Errors
    /// Rejects limits outside 4 through [`MAXIMUM_WINDOW_BYTES`].
    pub fn text(text: &str, start: usize, maximum: usize) -> Result<Self, WindowError> {
        limit(maximum)?;
        let mut start = start.min(text.len());
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let mut end = start + maximum.min(text.len() - start);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(Self {
            text: text[start..end].into(),
            start,
            end,
            more: end < text.len(),
        })
    }
    /// Serializes through a bounded prefix-skipping writer and stops after the window.
    ///
    /// # Errors
    /// Rejects invalid limits and propagates serializer errors unrelated to the window stop.
    pub fn json<T: Serialize + ?Sized>(
        value: &T,
        start: usize,
        maximum: usize,
    ) -> Result<Self, WindowError> {
        limit(maximum)?;
        let mut writer = Slice {
            bytes: Vec::new(),
            start,
            position: 0,
            capacity: maximum + 8,
            complete: false,
        };
        if let Err(error) = serde_json::to_writer_pretty(&mut writer, value)
            && !writer.complete
        {
            return Err(WindowError::Serialization(error.to_string()));
        }
        let skip = writer
            .bytes
            .iter()
            .take_while(|byte| **byte & 0xc0 == 0x80)
            .count();
        let raw = &writer.bytes[skip..];
        let text = match std::str::from_utf8(raw) {
            Ok(text) => text,
            Err(error) => std::str::from_utf8(&raw[..error.valid_up_to()])
                .map_err(|error| WindowError::Serialization(error.to_string()))?,
        };
        let mut result = Self::text(text, 0, maximum)?;
        result.start = start.min(writer.position) + skip;
        result.end += result.start;
        result.more |= writer.complete;
        Ok(result)
    }
}
struct Slice {
    bytes: Vec<u8>,
    start: usize,
    position: usize,
    capacity: usize,
    complete: bool,
}
impl Write for Slice {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let skip = self.start.saturating_sub(self.position).min(bytes.len());
        let count = (bytes.len() - skip).min(self.capacity - self.bytes.len());
        let required = self.bytes.len() + count;
        if required > self.bytes.capacity() {
            let capacity = required
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.capacity);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(&bytes[skip..skip + count]);
        self.position = self
            .position
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("serialized field offset overflow"))?;
        if self.bytes.len() == self.capacity {
            self.complete = true;
            return Err(std::io::Error::other("field window complete"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
