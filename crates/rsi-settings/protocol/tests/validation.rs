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
    let exact = json!("\\".repeat((MAXIMUM_SETTINGS_SECTION_BYTES - 2) / 2));
    assert_eq!(
        validate_section(&exact).unwrap(),
        MAXIMUM_SETTINGS_SECTION_BYTES
    );
    assert!(validate_section(&json!("\\".repeat(MAXIMUM_SETTINGS_SECTION_BYTES / 2))).is_err());
    assert!(
        validate_section(&json!({
            "value": "x".repeat(MAXIMUM_SETTINGS_SECTION_BYTES)
        }))
        .is_err()
    );
}

#[test]
fn projected_versions_validate_identity_and_preserve_exact_revision_numbers() {
    use rsi_settings_protocol::{SettingsScopeId, SettingsVersion};
    for value in [
        String::new(),
        "a".repeat(31),
        "a".repeat(33),
        "A".repeat(32),
        "g".repeat(32),
    ] {
        assert!(serde_json::from_value::<SettingsScopeId>(serde_json::json!(value)).is_err());
    }
    let version = SettingsVersion {
        scope_id: SettingsScopeId::parse("0123456789abcdef".repeat(2)).unwrap(),
        revision: u64::MAX,
    };
    let encoded = serde_json::to_vec(&version).unwrap();
    assert_eq!(
        serde_json::from_slice::<SettingsVersion>(&encoded).unwrap(),
        version
    );
}
