use crate::{Error, MAX_FRAME_BYTES};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// Exact correlation identity. Fractional and null request IDs are rejected.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Integral ID within the signed 64-bit range.
    Integer(i64),
    /// Peer-defined nonempty ID with at most 128 UTF-8 bytes.
    String(String),
}

/// Validated envelope, retaining params until method-specific admission.
pub enum Message {
    /// Peer call requiring one response with the same ID.
    Request {
        /// Exact peer correlation identity.
        id: RequestId,
        /// Bounded stable method name.
        method: String,
        /// Object requiring method-specific validation.
        params: Value,
    },
    /// Peer call without a response.
    Notification {
        /// Bounded stable method name.
        method: String,
        /// Object requiring method-specific validation.
        params: Value,
    },
    /// Correlated result or JSON-RPC error object.
    Response {
        /// Exact original request identity.
        id: RequestId,
        /// Successful value or complete error object.
        result: Result<Value, Value>,
    },
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(match self {
            Self::Request { .. } => "Request",
            Self::Notification { .. } => "Notification",
            Self::Response { .. } => "Response",
        })
        .finish_non_exhaustive()
    }
}

/// Decodes one finite JSON-RPC envelope with strict duplicate-key validation.
///
/// # Errors
/// Rejects bounds, duplicate keys, malformed JSON and ambiguous envelopes.
pub fn decode(bytes: &[u8]) -> Result<Message, Error> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::Limit);
    }
    let mut nodes = 0;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    Unique {
        depth: 0,
        nodes: &mut nodes,
    }
    .deserialize(&mut decoder)
    .map_err(|_| Error::Frame)?;
    decoder.end().map_err(|_| Error::Frame)?;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Error::Frame)?;
    let object = value.as_object().ok_or(Error::Frame)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(Error::Frame);
    }
    let id = object
        .get("id")
        .map(|id| {
            if let Some(id) = id.as_i64() {
                Ok(RequestId::Integer(id))
            } else {
                crate::text(Some(id), 128)
                    .map(|id| RequestId::String(id.into()))
                    .map_err(|_| Error::Frame)
            }
        })
        .transpose()?;
    if let Some(method) = object.get("method") {
        let method = crate::text(Some(method), 128)
            .map_err(|_| Error::Frame)?
            .to_owned();
        if object.contains_key("result") || object.contains_key("error") {
            return Err(Error::Frame);
        }
        let params = object
            .get("params")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !params.is_object() {
            return Err(Error::Frame);
        }
        return Ok(match id {
            Some(id) => Message::Request { id, method, params },
            None => Message::Notification { method, params },
        });
    }
    let id = id.ok_or(Error::Frame)?;
    if object.contains_key("params") {
        return Err(Error::Frame);
    }
    let result = match (object.get("result"), object.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error))
            if error.get("code").and_then(Value::as_i64).is_some()
                && error.get("message").and_then(Value::as_str).is_some() =>
        {
            Err(error.clone())
        }
        _ => return Err(Error::Frame),
    };
    Ok(Message::Response { id, result })
}

/// Incremental NDJSON framing. Input chunks are bounded independently of records.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    pending: Vec<u8>,
}
impl FrameDecoder {
    /// Accepts at most 64 KiB and emits complete records excluding LF.
    ///
    /// # Errors
    /// Rejects oversize records/chunks and empty lines; discard the decoder on error.
    pub fn push(&mut self, input: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        if input.len() > 64 * 1024 {
            return Err(Error::Limit);
        }
        let mut records = Vec::new();
        for segment in input.split_inclusive(|byte| *byte == b'\n') {
            let complete = segment.last() == Some(&b'\n');
            let payload = if complete {
                &segment[..segment.len() - 1]
            } else {
                segment
            };
            if self.pending.len() + payload.len() > MAX_FRAME_BYTES {
                return Err(Error::Limit);
            }
            self.pending.extend_from_slice(payload);
            if complete {
                if self.pending.is_empty() {
                    return Err(Error::Frame);
                }
                records.push(std::mem::take(&mut self.pending));
            }
        }
        Ok(records)
    }
    /// Confirms EOF occurred between records, never silently accepting a suffix.
    ///
    /// # Errors
    /// Returns an error when a final record lacked its LF delimiter.
    pub fn finish(&self) -> Result<(), Error> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(Error::Frame)
        }
    }
}

struct Unique<'a> {
    depth: usize,
    nodes: &'a mut usize,
}
impl<'de> DeserializeSeed<'de> for Unique<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        *self.nodes += 1;
        if self.depth > 32 || *self.nodes > 65536 {
            return Err(serde::de::Error::custom("JSON bound"));
        }
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Unique<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON with unique keys")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        while sequence
            .next_element_seed(Unique {
                depth: self.depth + 1,
                nodes: self.nodes,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut keys = std::collections::BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(serde::de::Error::custom("duplicate JSON key"));
            }
            map.next_value_seed(Unique {
                depth: self.depth + 1,
                nodes: self.nodes,
            })?;
        }
        Ok(())
    }
}
