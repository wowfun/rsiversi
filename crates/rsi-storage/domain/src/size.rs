use crate::StorageError;

/// Exact compact JSON size of a record object, initially `{}`.
///
/// Entry sizes come from [`encoded_entry_bytes`]. Replacements and removals must
/// supply the cached size of the actual previous entry. Projections do not
/// validate domain limits or reserve capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordObjectSize {
    records: usize,
    bytes: usize,
}

impl Default for RecordObjectSize {
    fn default() -> Self {
        Self {
            records: 0,
            bytes: 2,
        }
    }
}

impl RecordObjectSize {
    /// Number of records in the object.
    pub const fn records(self) -> usize {
        self.records
    }

    /// Encoded bytes, including braces and separators.
    pub const fn bytes(self) -> usize {
        self.bytes
    }

    /// Projects insertion (`previous = None`) or replacement of one entry.
    pub fn with_entry(self, previous: Option<usize>, next: usize) -> Result<Self, StorageError> {
        let (records, bytes) = if let Some(previous) = previous {
            if self.records == 0 {
                return Err(size_error());
            }
            (self.records, self.bytes.checked_sub(previous))
        } else {
            (
                self.records.checked_add(1).ok_or_else(size_error)?,
                self.bytes.checked_add(usize::from(self.records != 0)),
            )
        };
        Ok(Self {
            records,
            bytes: bytes
                .and_then(|bytes| bytes.checked_add(next))
                .ok_or_else(size_error)?,
        })
    }

    /// Projects removal of one existing entry.
    pub fn without_entry(self, previous: usize) -> Result<Self, StorageError> {
        let records = self.records.checked_sub(1).ok_or_else(size_error)?;
        Ok(Self {
            records,
            bytes: self
                .bytes
                .checked_sub(previous)
                .and_then(|bytes| bytes.checked_sub(usize::from(records != 0)))
                .ok_or_else(size_error)?,
        })
    }
}

/// Measures one compact JSON object's entry, excluding braces and commas.
///
/// `value_bytes` is the exact compact JSON length of the validated value.
pub fn encoded_entry_bytes(key: &str, value_bytes: usize) -> Result<usize, StorageError> {
    let key_bytes = serde_json::to_vec(key)
        .map_err(|error| StorageError::InvalidInput(error.to_string()))?
        .len();
    key_bytes
        .checked_add(1)
        .and_then(|bytes| bytes.checked_add(value_bytes))
        .ok_or_else(size_error)
}

fn size_error() -> StorageError {
    StorageError::InvalidInput("record object byte count overflowed or underflowed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    #[test]
    fn projections_match_json_through_insert_replace_and_delete() {
        let mut rows = BTreeMap::<String, Value>::new();
        let mut size = RecordObjectSize::default();
        let entry = |key: &str, value: &Value| {
            encoded_entry_bytes(key, serde_json::to_vec(value).unwrap().len()).unwrap()
        };
        let assert_exact = |size: RecordObjectSize, rows: &BTreeMap<String, Value>| {
            assert_eq!(size.records(), rows.len());
            assert_eq!(size.bytes(), serde_json::to_vec(rows).unwrap().len());
        };
        assert_exact(size, &rows);
        for (key, value) in [
            ("quote\"\\\n", json!({"nested": [true, null, "界"]})),
            ("界", json!(12345)),
            ("", json!([])),
            ("quote\"\\\n", json!("short")),
            ("界", json!({"longer": "\n\t\\\""})),
        ] {
            let previous = rows.get(key).map(|value| entry(key, value));
            size = size.with_entry(previous, entry(key, &value)).unwrap();
            rows.insert(key.into(), value);
            assert_exact(size, &rows);
        }
        for key in ["界", "", "quote\"\\\n"] {
            let value = rows.remove(key).unwrap();
            size = size.without_entry(entry(key, &value)).unwrap();
            assert_exact(size, &rows);
        }
        assert_eq!(size, RecordObjectSize::default());
    }

    #[test]
    fn arithmetic_failures_do_not_produce_a_projection() {
        let empty = RecordObjectSize::default();
        assert!(empty.with_entry(None, usize::MAX).is_err());
        assert!(empty.with_entry(Some(4), 4).is_err());
        assert!(empty.without_entry(4).is_err());
        let one = empty.with_entry(None, 4).unwrap();
        assert!(one.with_entry(Some(usize::MAX), 4).is_err());
        assert!(one.without_entry(usize::MAX).is_err());
        assert!(encoded_entry_bytes("key", usize::MAX).is_err());
    }
}
