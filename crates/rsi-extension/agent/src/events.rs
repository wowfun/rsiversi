//! Canonical event bytes; peer arrays never retain an aggregate decoded value tree.
use crate::{Error, encoding::BoundedBytes, error};
use rsi_agent_session_protocol::MAXIMUM_EXTENSION_JOURNAL_BYTES;
use serde::{
    Deserialize, Deserializer,
    de::{SeqAccess, Visitor},
};
use serde_json::Value;
use tokio::sync::OwnedSemaphorePermit;

const MAXIMUM_EVENTS: usize = 4096;

#[derive(Debug, Default)]
pub(crate) struct Events {
    pub(crate) arrays: Vec<Vec<u8>>,
    pub(crate) bytes: usize,
    pub(crate) credits: Vec<OwnedSemaphorePermit>,
}
impl Events {
    pub(crate) fn from_values(values: Vec<Value>) -> Result<Self, Error> {
        let mut events = Self::default();
        for value in values {
            events.push(&value)?;
        }
        Ok(events)
    }
    fn push(&mut self, value: &Value) -> Result<(), Error> {
        if self.arrays.len() == MAXIMUM_EVENTS {
            return Err(error("operation event count exceeded"));
        }
        // Individually framed arrays cost one additional byte per event compared
        // with a single packed array. Packing later preserves transport-page independence.
        let mut output = BoundedBytes {
            bytes: Vec::new(),
            maximum: MAXIMUM_EXTENSION_JOURNAL_BYTES + MAXIMUM_EVENTS - self.bytes,
        };
        serde_json::to_writer(&mut output, std::slice::from_ref(value)).map_err(error)?;
        self.bytes += output.bytes.len();
        self.arrays.push(output.bytes);
        Ok(())
    }
    pub(crate) fn append(&mut self, mut page: Self) -> Result<(), Error> {
        if self.arrays.len() + page.arrays.len() > MAXIMUM_EVENTS
            || self.bytes + page.bytes > MAXIMUM_EXTENSION_JOURNAL_BYTES + MAXIMUM_EVENTS
        {
            return Err(error("operation event capacity exceeded"));
        }
        self.bytes += page.bytes;
        self.arrays.append(&mut page.arrays);
        self.credits.append(&mut page.credits);
        Ok(())
    }
}
impl<'de> Deserialize<'de> for Events {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EventArray;
        impl<'de> Visitor<'de> for EventArray {
            type Value = Events;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded array of extension events")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Events, A::Error> {
                let mut events = Events::default();
                while let Some(value) = sequence.next_element::<Value>()? {
                    events.push(&value).map_err(serde::de::Error::custom)?;
                }
                Ok(events)
            }
        }
        deserializer.deserialize_seq(EventArray)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrays_are_closed_bounded_and_canonical_without_retaining_decoded_events() {
        for wire in ["null", "{}", "[1] trailing", "[1,]"] {
            assert!(serde_json::from_str::<Events>(wire).is_err(), "{wire}");
        }
        assert!(serde_json::from_value::<Events>(serde_json::json!(vec![0; 4097])).is_err());
        assert!(
            serde_json::from_str::<Events>("[]")
                .unwrap()
                .arrays
                .is_empty()
        );
        let wire = r#"[ { "z": [true, null], "a": -0 }, { "b": 1e2, "a":"\u754c" } ]"#;
        let decoded: Events = serde_json::from_str(wire).unwrap();
        let expected = Events::from_values(serde_json::from_str(wire).unwrap()).unwrap();
        assert_eq!(decoded.arrays, expected.arrays);
        assert_eq!(decoded.bytes, expected.bytes);
    }
}
