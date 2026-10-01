use rsi_configuration_api::ssh::*;
use serde_json::json;
#[test]
fn target_inputs_reject_unknown_fields_stale_epochs_and_ambiguous_revisions() {
    let epoch = serde_json::from_value(json!("1".repeat(32))).unwrap();
    let other = serde_json::from_value(json!("2".repeat(32))).unwrap();
    let value = json!({"host_epoch":epoch,"expected":"0","candidate":{"target":"a".repeat(32),"name":"target","endpoint":{"host":"127.0.0.1","port":22,"user":"fixture"}}});
    let valid: PutCandidate = serde_json::from_value(value.clone()).unwrap();
    valid.validate(&epoch).unwrap();
    assert!(valid.validate(&other).is_err());
    for revision in ["00", "01", "+1", "18446744073709551616"] {
        let mut bad = valid.clone();
        bad.expected = revision.into();
        assert!(bad.validate(&epoch).is_err());
    }
    let mut bad = value.clone();
    bad["candidate"]["endpoint"]["ProxyCommand"] = "touch unexpected".into();
    assert!(serde_json::from_value::<PutCandidate>(bad).is_err());
    let mut bad = value;
    bad["trusted"] = true.into();
    assert!(serde_json::from_value::<PutCandidate>(bad).is_err());
    let connection = json!({"selection":{"host_epoch":epoch,"target":"a".repeat(32),"revision":"1"},"expected_connection_epoch":"1"});
    serde_json::from_value::<ConnectionRequest>(connection.clone())
        .unwrap()
        .validate(&epoch)
        .unwrap();
    let mut stale = connection.clone();
    stale["expected_connection_epoch"] = "0".into();
    assert!(
        serde_json::from_value::<ConnectionRequest>(stale)
            .unwrap()
            .validate(&epoch)
            .is_err()
    );
    for path in ["relative", "/a/../b", "C:\\target", "/a\0"] {
        let request: ResolveDirectory =
            serde_json::from_value(json!({"connection":connection,"path":path})).unwrap();
        assert!(request.validate(&epoch).is_err());
    }
}
#[test]
fn redacted_catalog_rejects_impossible_connection_state_and_duplicate_targets() {
    let epoch = serde_json::from_value(json!("1".repeat(32))).unwrap();
    let target = json!({"candidate":{"target":"a".repeat(32),"name":"target","endpoint":{"host":"host","port":22,"user":"fixture"}},"revision":"1","fingerprint":null,"connection_epoch":null,"connected":false,"unavailable_programs":[],"permissions":{"use_target":false,"manage":false}});
    let value = json!({"host_epoch":epoch,"targets":[target.clone()]});
    serde_json::from_value::<Catalog>(value.clone())
        .unwrap()
        .validate(&epoch)
        .unwrap();
    let mut invalid = value.clone();
    invalid["targets"][0]["connected"] = true.into();
    assert!(
        serde_json::from_value::<Catalog>(invalid)
            .unwrap()
            .validate(&epoch)
            .is_err()
    );
    let mut invalid = value;
    invalid["targets"].as_array_mut().unwrap().push(target);
    assert!(
        serde_json::from_value::<Catalog>(invalid)
            .unwrap()
            .validate(&epoch)
            .is_err()
    );
}
