use serde_json::Value;
use std::fmt;

use crate::{MAX_EXTENSION_BYTES, MAX_ID_BYTES, MAX_JSON_DEPTH, MAX_JSON_NODES};

/// Validates the shared provider-neutral identifier syntax and byte bound.
pub fn identifier(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.bytes().all(|byte| matches!(byte, 0x21..=0x7e))
    {
        return Err(format!(
            "{field} must contain 1..={MAX_ID_BYTES} non-whitespace printable ASCII bytes"
        ));
    }
    Ok(())
}

pub(crate) fn tool_name(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        return Err(format!(
            "{field} must start with an ASCII alphanumeric and contain only ASCII alphanumerics, '.', '_', or '-'"
        ));
    }
    Ok(())
}

pub(crate) fn extension_size(field: &str, value: &serde_json::Value) -> Result<(), String> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| format!("{field} could not be encoded: {error}"))?;
    if encoded.len() > MAX_EXTENSION_BYTES {
        return Err(format!(
            "{field} exceeds the {MAX_EXTENSION_BYTES}-byte encoded limit"
        ));
    }
    Ok(())
}

pub(crate) fn encoded_len(value: &(impl serde::Serialize + ?Sized)) -> Result<usize, String> {
    struct Counter(usize);

    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("encoded JSON length overflowed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|error| error.to_string())?;
    Ok(counter.0)
}

pub(crate) fn safe_text(
    field: &str,
    value: &str,
    maximum_bytes: usize,
    allow_empty: bool,
) -> Result<(), String> {
    if (!allow_empty && value.is_empty())
        || value.len() > maximum_bytes
        || value
            .chars()
            .any(|character| character == '\0' || character == '\u{7f}')
    {
        return Err(format!(
            "{field} must contain {}..={maximum_bytes} safe UTF-8 bytes",
            usize::from(!allow_empty)
        ));
    }
    Ok(())
}

/// Why an arbitrary JSON value exceeds the provider-neutral structure bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonStructureError {
    TooDeep,
    TooManyNodes,
}

impl fmt::Display for JsonStructureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooDeep => write!(formatter, "JSON nesting exceeds {MAX_JSON_DEPTH}"),
            Self::TooManyNodes => {
                write!(formatter, "JSON node count exceeds {MAX_JSON_NODES}")
            }
        }
    }
}

impl std::error::Error for JsonStructureError {}

/// Enforces the shared nesting-depth and node-count limits on arbitrary JSON.
pub fn validate_json_structure(value: &Value) -> Result<(), JsonStructureError> {
    let mut nodes = 0;
    validate_json_at(value, 0, &mut nodes)
}

fn validate_json_at(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), JsonStructureError> {
    if depth > MAX_JSON_DEPTH {
        return Err(JsonStructureError::TooDeep);
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_JSON_NODES {
        return Err(JsonStructureError::TooManyNodes);
    }
    match value {
        Value::Array(values) => {
            for value in values {
                validate_json_at(value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_json_at(value, depth + 1, nodes)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

pub(crate) fn canonical_json(mut value: Value) -> Result<Value, JsonStructureError> {
    validate_json_structure(&value)?;
    sort_json_keys(&mut value);
    Ok(value)
}

fn sort_json_keys(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                sort_json_keys(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                sort_json_keys(value);
            }
            values.sort_keys();
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}
