use rsi_settings_protocol::*;
use serde_json::json;

fn metadata() -> SettingsMetadata {
    SettingsMetadata {
        schema: json!({"type":"object"}),
        applies: SettingsApply::NewSession,
        description: "Captured in a new Session".into(),
        sensitive_fields: vec![vec!["credential_ref".into()]],
    }
}
#[test]
fn descriptive_metadata_is_bounded_and_never_a_validator() {
    let valid = metadata();
    valid.validate().unwrap();
    assert_eq!(
        serde_json::from_value::<SettingsMetadata>(serde_json::to_value(&valid).unwrap()).unwrap(),
        valid
    );
    for schema in [
        json!(null),
        json!([]),
        json!("code"),
        json!({"description":"x".repeat(MAXIMUM_SETTINGS_METADATA_BYTES)}),
    ] {
        let mut bad = metadata();
        bad.schema = schema;
        assert!(bad.validate().is_err());
    }
    let mut bad = metadata();
    bad.sensitive_fields = vec![vec!["x".into(); 33]];
    assert!(bad.validate().is_err());
    bad.sensitive_fields = vec![vec!["x\n".into()]];
    assert!(bad.validate().is_err());
    let mut wire = serde_json::to_value(valid).unwrap();
    wire["applies"] = json!("eval");
    assert!(serde_json::from_value::<SettingsMetadata>(wire).is_err());
}
#[test]
fn discovery_checks_cursor_progress_count_binding_and_default_bounds() {
    let page = SettingsPage {
        namespaces: vec!["b".into(), "c".into()],
        next: Some("c".into()),
    };
    page.validate(Some("a"), 2).unwrap();
    for (after, limit) in [
        (Some("b"), 2),
        (None, 1),
        (None, 0),
        (None, MAXIMUM_SETTINGS_PAGE + 1),
    ] {
        assert!(page.validate(after, limit).is_err());
    }
    for names in [
        vec!["c".into(), "b".into()],
        vec!["b".into(), "b".into()],
        vec![],
    ] {
        assert!(
            SettingsPage {
                namespaces: names,
                next: Some("c".into())
            }
            .validate(None, 2)
            .is_err()
        );
    }
    let mut description = SettingsDescription {
        namespace: "ui".into(),
        version: SettingsVersion {
            scope_id: SettingsScopeId::parse("a".repeat(32)).unwrap(),
            revision: 0,
        },
        defaults: json!({}),
        writable: false,
        metadata: metadata(),
    };
    description.validate("ui").unwrap();
    assert!(description.validate("foreign").is_err());
    description.defaults = json!("x".repeat(MAXIMUM_SETTINGS_SECTION_BYTES));
    assert!(description.validate("ui").is_err());
}
