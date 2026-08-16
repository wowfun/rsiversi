use serde_json::Value;

use crate::{HostError, Result};

const MAX_STATE_KEY_CHARACTERS: usize = 255;
const MAX_STATE_VALUE_BYTES: usize = 1024 * 1024;

pub(super) fn validate_state_key(payload: &Value) -> Result<&str> {
    payload
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty() && key.chars().count() <= MAX_STATE_KEY_CHARACTERS)
        .ok_or_else(|| {
            HostError::InvalidEnvelope(format!(
                "state.cas key must contain 1 to {MAX_STATE_KEY_CHARACTERS} characters"
            ))
        })
}

pub(super) fn validate_state_value(value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?.len();
    if bytes > MAX_STATE_VALUE_BYTES {
        return Err(HostError::InvalidEnvelope(format!(
            "state.cas value exceeds {MAX_STATE_VALUE_BYTES} encoded bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn state_keys_and_values_are_bounded_before_persistence() {
        assert_eq!(
            validate_state_key(&json!({"key": "x".repeat(255)})).unwrap(),
            "x".repeat(255)
        );
        assert!(validate_state_key(&json!({"key": "x".repeat(256)})).is_err());
        assert!(validate_state_value(&json!("x".repeat(1024 * 1024))).is_err());
    }
}
