use rsi_credentials_protocol::{CredentialRef, CredentialSource};
use serde_json::json;

#[test]
fn credential_addresses_and_sources_revalidate_during_deserialization() {
    serde_json::from_value::<CredentialRef>(json!({
        "owner": "rsi.ai.openai",
        "slot": "not a slot"
    }))
    .expect_err("invalid slot must not enter the typed contract");

    serde_json::from_value::<CredentialSource>(json!({
        "kind": "environment",
        "variable": "9INVALID"
    }))
    .expect_err("invalid environment provenance must not enter durable facts");
}

#[test]
fn file_status_is_bounded_and_historical_keyring_provenance_remains_readable() {
    use rsi_credentials_protocol::{CredentialStatus, CredentialStoreFailure};
    for (wire, expected) in [
        ("file", CredentialSource::File),
        ("keyring", CredentialSource::Keyring),
    ] {
        assert_eq!(
            serde_json::from_value::<CredentialSource>(json!({"kind":wire})).unwrap(),
            expected
        );
    }
    let value = json!({"availability":{"kind":"unavailable","reason":"permissions"},"editable":false,"store_path":"/host/credentials/credentials.json"});
    let status: CredentialStatus = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(status).unwrap(), value);
    for path in ["x".repeat(2049), "bad\npath".into(), String::new()] {
        let mut bad = value.clone();
        bad["store_path"] = json!(path);
        assert!(serde_json::from_value::<CredentialStatus>(bad).is_err());
    }
    let mut bad = value;
    bad["editable"] = json!(true);
    assert!(serde_json::from_value::<CredentialStatus>(bad).is_err());
    assert_eq!(
        serde_json::to_value(CredentialStoreFailure::LockTimeout).unwrap(),
        json!("lock_timeout")
    );
}
