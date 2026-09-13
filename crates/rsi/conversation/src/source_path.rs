use crate::{FactField, FieldValue, SourceRef};
use rsi_agent_session_protocol::{SessionFact, SessionFactBody};
use serde::{Deserialize, Serialize};

/// Bounded literal-key path below one exact Tool result value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ToolValuePath(Vec<String>);

/// Invalid finite Tool-value selector.
#[derive(Debug, thiserror::Error)]
#[error("Tool value path requires 1..=8 components, at most 64 bytes each and 256 bytes total")]
pub struct SourcePathError;

impl ToolValuePath {
    /// Validates an owned path before source admission.
    ///
    /// # Errors
    /// Rejects component or aggregate bounds.
    pub fn new(components: Vec<String>) -> Result<Self, SourcePathError> {
        if components.is_empty()
            || components.len() > 8
            || components.iter().any(|key| key.len() > 64)
            || components.iter().map(String::len).sum::<usize>() > 256
        {
            return Err(SourcePathError);
        }
        Ok(Self(components))
    }
}
impl<'de> Deserialize<'de> for ToolValuePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Vec::<String>::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Borrows only a variant-matched exact Fact's selected Tool value subfield.
pub fn select_tool_value_path<'a>(
    fact: &'a SessionFact,
    source: SourceRef,
    path: &ToolValuePath,
) -> Option<FieldValue<'a>> {
    if fact.seq() != source.seq || source.field != FactField::ToolValue {
        return None;
    }
    let SessionFactBody::ToolResult { result, .. } = fact.body() else {
        return None;
    };
    let mut value = &result.value;
    for key in &path.0 {
        value = match value {
            serde_json::Value::Object(object) => object.get(key)?,
            serde_json::Value::Array(array) => {
                let index = key.parse::<usize>().ok()?;
                if index.to_string() != *key {
                    return None;
                }
                array.get(index)?
            }
            _ => return None,
        };
    }
    Some(match value {
        serde_json::Value::String(text) => FieldValue::Text(text.as_str().into()),
        _ => FieldValue::Json(value),
    })
}
