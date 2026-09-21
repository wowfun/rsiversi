use crate::{Error, Result};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::fmt;

pub(super) fn decode(bytes: &[u8]) -> Result<serde_json::Value> {
    let mut nodes = 0;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    Unique {
        depth: 0,
        nodes: &mut nodes,
    }
    .deserialize(&mut decoder)
    .map_err(|_| Error::Protocol)?;
    decoder.end().map_err(|_| Error::Protocol)?;
    serde_json::from_slice(bytes).map_err(|_| Error::Protocol)
}
struct Unique<'a> {
    depth: usize,
    nodes: &'a mut usize,
}
impl<'de> DeserializeSeed<'de> for Unique<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
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
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<(), A::Error> {
        while sequence
            .next_element_seed(Unique {
                depth: self.depth + 1,
                nodes: self.nodes,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ambiguous_nested_objects_and_excess_depth_before_normalization() {
        assert!(decode(br#"{"result":{"uri":"safe","uri":"other"}}"#).is_err());
        assert!(decode(format!("{}0{}", "[".repeat(34), "]".repeat(34)).as_bytes()).is_err());
        assert!(decode(br#"{"jsonrpc":"2.0","id":1,"result":null}"#).is_ok());
    }
}
