use crate::{
    BoundedWriter, MAXIMUM_STORAGE_VALUE_BYTES, MAXIMUM_STORAGE_VALUE_DEPTH, Result, StorageError,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::value::RawValue;
use std::{fmt, sync::Arc};

/// Validated, bounded compact JSON. Cloning shares immutable encoded bytes.
#[derive(Clone)]
pub struct EncodedValue(Arc<Box<RawValue>>);

impl fmt::Debug for EncodedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncodedValue")
            .field("bytes", &self.encoded_len())
            .finish_non_exhaustive()
    }
}

impl EncodedValue {
    /// Encodes a trusted typed value without an intermediate JSON tree.
    pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Self> {
        let mut writer = BoundedWriter::new(Vec::new(), MAXIMUM_STORAGE_VALUE_BYTES);
        serde_json::to_writer(&mut writer, value).map_err(invalid)?;
        Self::from_bytes(writer.into_inner())
    }

    /// Admits durable bytes after their owning reader has bounded materialization.
    /// Callers map a rejection to corruption when these bytes came from storage.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_STORAGE_VALUE_BYTES {
            return Err(invalid("stored value exceeds its encoded byte bound"));
        }
        let text = String::from_utf8(bytes).map_err(invalid)?;
        compact_depth(&text)?;
        // RawValue validates syntax without constructing a Value tree. Retaining
        // its Box inside Arc avoids copying the complete body into a new Arc<str>.
        let raw = RawValue::from_string(text).map_err(invalid)?;
        Ok(Self(Arc::new(raw)))
    }

    /// Copies an already parsed borrowed JSON value only after admitting its
    /// byte/depth bounds. Document readers can borrow their bounded input buffer
    /// without allocating an oversized per-record value or parsing syntax twice.
    pub fn copy_raw(raw: &RawValue) -> Result<Self> {
        if raw.get().len() > MAXIMUM_STORAGE_VALUE_BYTES {
            return Err(invalid("stored value exceeds its encoded byte bound"));
        }
        compact_depth(raw.get())?;
        Ok(Self(Arc::new(raw.to_owned())))
    }

    /// Decodes once at the owning business-schema boundary.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(self.0.get()).map_err(|error| StorageError::Corrupt(error.to_string()))
    }

    /// Borrows the exact retained compact bytes.
    pub fn as_bytes(&self) -> &[u8] {
        self.0.get().as_bytes()
    }

    /// Exact encoded byte count, including JSON string framing.
    pub fn encoded_len(&self) -> usize {
        self.0.get().len()
    }
}

impl Serialize for EncodedValue {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}

fn invalid(error: impl fmt::Display) -> StorageError {
    StorageError::InvalidInput(error.to_string())
}

// Syntax is checked by RawValue. This scan only enforces compact framing and
// the family depth budget, including a scalar child of the deepest container.
fn compact_depth(text: &str) -> Result<()> {
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        if byte.is_ascii_whitespace() {
            return Err(invalid("stored value must use compact JSON framing"));
        }
        if depth == MAXIMUM_STORAGE_VALUE_DEPTH && !matches!(byte, b']' | b'}') {
            return Err(invalid("stored value exceeds its JSON depth bound"));
        }
        match byte {
            b'"' => quoted = true,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::{Value, json};

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Record {
        text: String,
        payload: Value,
    }

    #[test]
    fn typed_encoding_keeps_exact_order_and_clones_share_bytes() {
        let value = Record {
            text: "中 🦀 \"\\[]{} ".into(),
            payload: json!({"z": 1, "a": null}),
        };
        let encoded = EncodedValue::encode(&value).unwrap();
        assert_eq!(encoded.as_bytes(), serde_json::to_vec(&value).unwrap());
        assert_eq!(encoded.decode::<Record>().unwrap(), value);
        assert_eq!(
            encoded.as_bytes().as_ptr(),
            encoded.clone().as_bytes().as_ptr()
        );
        assert_eq!(serde_json::to_vec(&encoded).unwrap(), encoded.as_bytes());
        assert!(
            EncodedValue::encode(&json!({"unknown": true}))
                .unwrap()
                .decode::<Record>()
                .is_err()
        );
    }

    #[test]
    fn borrowed_document_values_keep_their_exact_compact_lexemes() {
        let text = r#"{"z":"\u4e2d","a":1e+09}"#;
        let raw: &RawValue = serde_json::from_str(text).unwrap();
        let value = EncodedValue::copy_raw(raw).unwrap();
        assert_eq!(value.as_bytes(), text.as_bytes());
        assert_eq!(value.decode::<Value>().unwrap()["z"], "中");
        let pretty: &RawValue = serde_json::from_str("[true, false]").unwrap();
        assert!(EncodedValue::copy_raw(pretty).is_err());
    }

    #[test]
    fn depth_and_framing_admission_matches_the_existing_value_boundary() {
        for leaf in [
            "null",
            "\"{\\\"x\\\":[]}\"",
            "[]",
            "{}",
            "123456789012345678901234567890",
        ] {
            for depth in [0, 62, 63, 64, 65] {
                let text = format!("{}{leaf}{}", "[".repeat(depth), "]".repeat(depth));
                let value: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(
                    EncodedValue::from_bytes(text.into_bytes()).is_ok(),
                    crate::validate_value(&value).is_ok()
                );
            }
        }
        for invalid in [
            "",
            " null",
            "null ",
            "[true, false]",
            "[",
            "{]",
            "NaN",
            "\"\\x\"",
            "truefalse",
        ] {
            assert!(
                EncodedValue::from_bytes(invalid.as_bytes().to_vec()).is_err(),
                "{invalid:?}"
            );
        }
        assert!(EncodedValue::from_bytes(vec![0xff]).is_err());
        assert!(EncodedValue::encode(&"x".repeat(MAXIMUM_STORAGE_VALUE_BYTES)).is_err());
    }
}
