use rsi_agent_session_protocol::{MAXIMUM_RECORD_REFERENCES, ReferenceCollector, SessionId};
use serde_json::json;

#[test]
fn canonical_reference_sets_reject_invalid_scalars_duplicates_order_and_overflow() {
    let mut refs = ReferenceCollector::default();
    refs.session(&SessionId::new("child").unwrap()).unwrap();
    refs.cas(&"a".repeat(64), 17).unwrap();
    let wire = serde_json::to_value(&refs).unwrap();
    assert_eq!(
        serde_json::from_value::<ReferenceCollector>(wire.clone()).unwrap(),
        refs
    );
    let mut reversed = wire.as_array().unwrap().clone();
    reversed.reverse();
    assert!(serde_json::from_value::<ReferenceCollector>(json!(reversed)).is_err());
    assert!(serde_json::from_value::<ReferenceCollector>(json!([wire[0], wire[0]])).is_err());
    for reference in [
        json!({"sha256":"a".repeat(63),"byte_len":17}),
        json!({"sha256":"A".repeat(64),"byte_len":17}),
        json!({"sha256":"a".repeat(64),"byte_len":0}),
        json!({"sha256":"a".repeat(64),"byte_len":64*1024*1024+1}),
    ] {
        assert!(
            serde_json::from_value::<ReferenceCollector>(
                json!([{"kind":"cas","reference":reference}])
            )
            .is_err()
        );
    }
    let overflow = (0..=MAXIMUM_RECORD_REFERENCES)
        .map(|i| json!({"kind":"session","reference":format!("child-{i:04}")}))
        .collect::<Vec<_>>();
    assert!(serde_json::from_value::<ReferenceCollector>(json!(overflow)).is_err());
}

#[test]
fn merging_snapshots_keeps_the_aggregate_record_limit() {
    let mut record = ReferenceCollector::default();
    for index in 0..MAXIMUM_RECORD_REFERENCES {
        record
            .session(&SessionId::new(format!("child-{index}")).unwrap())
            .unwrap();
    }
    record.extend(&record.clone()).unwrap();
    let mut extra = ReferenceCollector::default();
    extra.session(&SessionId::new("overflow").unwrap()).unwrap();
    assert!(record.extend(&extra).is_err());
}
