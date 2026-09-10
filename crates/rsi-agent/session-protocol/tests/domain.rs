use rsi_agent_session_protocol::{
    DomainIdentity, DomainRevision, DomainSnapshot, DomainStateValue, MAXIMUM_DOMAIN_STATE_BYTES,
};

#[test]
fn mixed_domain_receipts_bind_fact_bodies_but_not_allocated_positions() {
    use rsi_agent_session_protocol::{
        DomainFactSpan, DomainFactSpanBuilder, DomainMutationSource, DomainRequestId,
        DomainStateCommit, DomainStateUpdate, SessionFact, SessionFactBody, StepId, TurnId,
    };
    let turn = TurnId::new("mixed-turn").unwrap();
    let fact = |seq, timestamp, step: &str| {
        SessionFact::new(
            seq,
            timestamp,
            SessionFactBody::StepStarted {
                turn_id: turn.clone(),
                step_id: StepId::new(step).unwrap(),
            },
        )
        .unwrap()
    };
    let commit = DomainStateCommit::new(
        Some(DomainRequestId::new("mixed").unwrap()),
        DomainMutationSource::Turn {
            turn_id: turn.clone(),
        },
        vec![
            DomainStateUpdate::new(
                DomainRevision::new(1),
                DomainSnapshot::new(
                    DomainIdentity::new("a", 1).unwrap(),
                    DomainStateValue::new(true.into()).unwrap(),
                ),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let first = fact(2, 3, "a");
    let bound = commit.clone().with_facts([&first]).unwrap();
    let moved = commit.clone().with_facts([&fact(20, 99, "a")]).unwrap();
    assert_eq!(bound.request_sha256(), moved.request_sha256());
    assert_ne!(bound.fact_span(), moved.fact_span());
    assert_ne!(bound.request_sha256(), commit.request_sha256());
    assert_ne!(
        bound.request_sha256(),
        commit
            .with_facts([&fact(2, 3, "b")])
            .unwrap()
            .request_sha256()
    );
    let mut stream = DomainFactSpanBuilder::default();
    stream.push(&first).unwrap();
    assert_eq!(Some(&stream.finish().unwrap()), bound.fact_span());
    assert!(DomainFactSpan::from_facts([&first, &fact(4, 3, "b")]).is_err());
    assert!(DomainFactSpan::from_facts([&first; 513]).is_err());
    assert_eq!(
        serde_json::from_value::<DomainStateCommit>(serde_json::to_value(&bound).unwrap()).unwrap(),
        bound
    );
    let mut tampered = serde_json::to_value(&bound).unwrap();
    tampered["fact_span"]["bodies_sha256"] = "0".repeat(64).into();
    assert!(serde_json::from_value::<DomainStateCommit>(tampered).is_err());
    for span in [
        serde_json::json!({"first_seq":0,"count":1,"bodies_sha256":"0".repeat(64)}),
        serde_json::json!({"first_seq":1,"count":513,"bodies_sha256":"0".repeat(64)}),
        serde_json::json!({"first_seq":u64::MAX,"count":2,"bodies_sha256":"0".repeat(64)}),
    ] {
        assert!(serde_json::from_value::<DomainFactSpan>(span).is_err());
    }
}

#[test]
fn opaque_domain_states_validate_identity_revision_and_json_at_decode() {
    let null = DomainSnapshot::new(
        DomainIdentity::new("example.plan", 1).unwrap(),
        DomainStateValue::new(serde_json::Value::Null).unwrap(),
    );
    let encoded = serde_json::to_string(&null).unwrap();
    assert_eq!(
        encoded,
        r#"{"identity":{"id":"example.plan","version":1},"state":null}"#
    );
    assert_eq!(
        serde_json::from_str::<DomainSnapshot>(&encoded).unwrap(),
        null
    );
    assert!(
        serde_json::from_str::<DomainSnapshot>(r#"{"identity":{"id":"example.plan","version":1}}"#)
            .is_err()
    );
    for identity in [
        r#"{"id":"example.plan","version":0}"#,
        r#"{"id":"","version":1}"#,
        r#"{"id":"example.plan","version":1,"alias":2}"#,
    ] {
        assert!(serde_json::from_str::<DomainIdentity>(identity).is_err());
    }
    assert_eq!(
        DomainRevision::new(0).next().unwrap(),
        DomainRevision::new(1)
    );
    assert!(DomainRevision::new(u64::MAX).next().is_err());
    // Unknown versions remain readable as opaque history, with no codec fallback.
    assert!(DomainIdentity::new("example.plan", 912).is_ok());
}

#[test]
fn complete_domain_value_limits_apply_to_constructors_encoding_and_decode() {
    let fitting = "x".repeat(MAXIMUM_DOMAIN_STATE_BYTES - 2);
    let state = DomainStateValue::encode(&fitting).unwrap();
    assert_eq!(state.encoded_len(), MAXIMUM_DOMAIN_STATE_BYTES);
    let oversized = format!("{fitting}x");
    assert!(DomainStateValue::encode(&oversized).is_err());
    assert!(DomainStateValue::new(oversized.clone().into()).is_err());
    assert!(
        serde_json::from_str::<DomainStateValue>(&serde_json::to_string(&oversized).unwrap())
            .is_err()
    );
    let mut nested = serde_json::Value::Null;
    for _ in 0..66 {
        nested = serde_json::json!([nested]);
    }
    assert!(DomainStateValue::new(nested).is_err());
    assert!(DomainStateValue::new(serde_json::json!(vec![0; 65_537])).is_err());
}

#[test]
fn domain_commit_identity_binds_the_entire_distinct_domain_request() {
    use rsi_agent_session_protocol::{
        DomainMutationSource, DomainRequestId, DomainStateCommit, DomainStateUpdate, TurnId,
    };
    let update = DomainStateUpdate::new(
        DomainRevision::new(3),
        DomainSnapshot::new(
            DomainIdentity::new("example.plan", 1).unwrap(),
            DomainStateValue::new(serde_json::json!({"enabled": true})).unwrap(),
        ),
    )
    .unwrap();
    let source = DomainMutationSource::Turn {
        turn_id: TurnId::new("turn-a").unwrap(),
    };
    let request = DomainRequestId::new("request-a").unwrap();
    let commit = DomainStateCommit::new(Some(request), source, vec![update.clone()]).unwrap();
    assert_eq!(commit.updates()[0].revision(), DomainRevision::new(4));
    let mut encoded = serde_json::to_value(&commit).unwrap();
    assert_eq!(
        serde_json::from_value::<DomainStateCommit>(encoded.clone()).unwrap(),
        commit
    );
    encoded["updates"][0]["snapshot"]["state"]["enabled"] = false.into();
    assert!(serde_json::from_value::<DomainStateCommit>(encoded).is_err());
    assert!(
        DomainStateCommit::new(
            commit.request_id().cloned(),
            commit.source().clone(),
            vec![update.clone(), update]
        )
        .is_err()
    );
    assert!(
        DomainStateCommit::new(None, commit.source().clone(), commit.updates().to_vec()).is_err()
    );
    assert!(
        DomainStateCommit::new(
            commit.request_id().cloned(),
            DomainMutationSource::Baseline,
            commit.updates().to_vec()
        )
        .is_err()
    );
    let baseline_update = DomainStateUpdate::new(
        DomainRevision::new(0),
        commit.updates()[0].snapshot().clone(),
    )
    .unwrap();
    assert!(
        DomainStateCommit::new(
            None,
            DomainMutationSource::Baseline,
            vec![baseline_update.clone()]
        )
        .is_ok()
    );
    assert!(
        DomainStateCommit::new(
            commit.request_id().cloned(),
            commit.source().clone(),
            vec![baseline_update]
        )
        .is_err()
    );
}
