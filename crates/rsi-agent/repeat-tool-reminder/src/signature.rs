use serde::{
    Serialize,
    ser::{SerializeMap as _, SerializeSeq as _},
};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

struct Ordered<'a>(&'a Value);
impl Serialize for Ordered<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(values) => {
                let mut fields = values.iter().collect::<Vec<_>>();
                fields.sort_unstable_by(|left, right| left.0.cmp(right.0));
                let mut output = serializer.serialize_map(Some(fields.len()))?;
                for (key, value) in fields {
                    output.serialize_entry(key, &Ordered(value))?;
                }
                output.end()
            }
            Value::Array(values) => {
                let mut output = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    output.serialize_element(&Ordered(value))?;
                }
                output.end()
            }
            value => value.serialize(serializer),
        }
    }
}
struct Writer(Sha256);
impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn signature(name: &str, arguments: &Value) -> super::ContributionResult<String> {
    let mut writer = Writer(Sha256::new());
    writer.0.update(b"rsi.repeat-tool-reminder/v1\0");
    serde_json::to_writer(&mut writer, &(name, Ordered(arguments))).map_err(super::invalid)?;
    Ok(hex::encode(writer.0.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn full_deep_arguments_ignore_only_object_order_and_preserve_number_precision() {
        assert_eq!(
            signature("read", &json!({"a":{"x":1,"y":2},"b":3})).unwrap(),
            signature("read", &json!({"b":3,"a":{"y":2,"x":1}})).unwrap()
        );
        assert_ne!(
            signature("read", &json!([1, 2])).unwrap(),
            signature("read", &json!([2, 1])).unwrap()
        );
        assert_ne!(
            signature("read", &json!({})).unwrap(),
            signature("read_more", &json!({})).unwrap()
        );
        let prefix = "x".repeat(100_000);
        assert_ne!(
            signature("read", &json!(format!("{prefix}a"))).unwrap(),
            signature("read", &json!(format!("{prefix}b"))).unwrap()
        );
        let first: Value = serde_json::from_str("123456789012345678901234567890").unwrap();
        let next: Value = serde_json::from_str("123456789012345678901234567891").unwrap();
        assert_ne!(
            signature("read", &first).unwrap(),
            signature("read", &next).unwrap()
        );
    }
}
