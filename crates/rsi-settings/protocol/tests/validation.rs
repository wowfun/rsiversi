use rsi_settings_protocol::{
    MAXIMUM_SETTINGS_NAMESPACE_BYTES, MAXIMUM_SETTINGS_SECTION_BYTES, validate_namespace,
    validate_section,
};
use serde_json::json;

#[test]
fn namespace_validation_enforces_syntax_and_encoded_byte_bound() {
    assert!(validate_namespace("product.feature-1").is_ok());
    assert!(validate_namespace("").is_err());
    assert!(validate_namespace("contains/slash").is_err());
    assert!(validate_namespace(&"x".repeat(MAXIMUM_SETTINGS_NAMESPACE_BYTES + 1)).is_err());
}

#[test]
fn section_validation_measures_encoded_json_bytes() {
    assert_eq!(validate_section(&json!({"enabled": true})).unwrap(), 16);
    assert!(
        validate_section(&json!({
            "value": "x".repeat(MAXIMUM_SETTINGS_SECTION_BYTES)
        }))
        .is_err()
    );
}
