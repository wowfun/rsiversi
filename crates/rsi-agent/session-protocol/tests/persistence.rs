use rsi_agent_session_protocol::{
    AgentControlRecord, DomainStateValue, ModelAttemptLog, SESSION_FORMAT_VERSION, SessionFact,
    SessionHeader,
};
use rsi_persistence_testkit::structural;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeSet, path::Path};

fn current() -> Value {
    json!({
        "header": structural::<SessionHeader>(),
        "fact": structural::<SessionFact>(),
        "control": structural::<AgentControlRecord>(),
        "compact_attempt": structural::<ModelAttemptLog>(),
        // Compact chunks contain exact JSON for non-delta Language events.
        "compact_event": structural::<rsi_ai_protocol::LanguageEvent>(),
        "opaque_domain_value": structural::<DomainStateValue>(),
    })
}

#[test]
#[ignore = "requires an explicitly new RSI_SCHEMA_CAPTURE directory"]
fn capture_current_schema() {
    let root = std::env::var_os("RSI_SCHEMA_CAPTURE").expect("explicit new schema destination");
    let root = Path::new(&root);
    std::fs::create_dir(root).expect("capture never replaces retained evidence");
    let schema = serde_json::to_vec_pretty(&current()).unwrap();
    std::fs::write(root.join("schema.json"), &schema).unwrap();
    std::fs::write(
        root.join("baseline.json"),
        serde_json::to_vec_pretty(&json!({
            "format": SESSION_FORMAT_VERSION,
            "schema_sha256": hex::encode(Sha256::digest(&schema)),
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn retained_serialization_shapes_and_order() {
    let expected = current();
    let mut covered = BTreeSet::new();
    for entry in
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/persistence"))
            .unwrap()
    {
        let root = entry.unwrap().path();
        let baseline: Value =
            serde_json::from_slice(&std::fs::read(root.join("baseline.json")).unwrap()).unwrap();
        let stored = std::fs::read(root.join("schema.json")).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&stored)),
            baseline["schema_sha256"]
        );
        if baseline["format"] == SESSION_FORMAT_VERSION {
            let shapes: Value = serde_json::from_slice(&stored).unwrap();
            for (name, shape) in shapes.as_object().unwrap() {
                covered.insert(name.clone());
                let actual = expected
                    .get(name)
                    .expect("retained format root was removed");
                assert_eq!(
                    serde_json::to_string_pretty(shape).unwrap(),
                    serde_json::to_string_pretty(actual).unwrap(),
                    "same-version serialization shape or field order changed for {name}; retained evidence must not be refreshed"
                );
            }
        }
    }
    assert_eq!(
        covered,
        expected.as_object().unwrap().keys().cloned().collect(),
        "current session format needs retained evidence for every root"
    );
}
