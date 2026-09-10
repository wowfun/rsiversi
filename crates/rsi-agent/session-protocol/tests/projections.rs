use rsi_agent_session_protocol::{
    ContributionId, ProjectionCursor, ProjectionEntry, ProjectionValue, SessionId,
    SessionProjectionSnapshot,
};
use serde_json::json;

#[test]
fn projection_values_are_complete_bounded_and_failures_are_isolated_entries() {
    let entry = ProjectionEntry::value(
        ContributionId::new("fixture.plan").unwrap(),
        ProjectionValue::new(json!({"active":true})).unwrap(),
    );
    let failed = ProjectionEntry::failed(
        ContributionId::new("fixture.failed").unwrap(),
        "codec unavailable",
    )
    .unwrap();
    let snapshot = SessionProjectionSnapshot::new(
        SessionId::new("session").unwrap(),
        "a".repeat(64),
        "b".repeat(64),
        ProjectionCursor::Durable {
            fact_seq: 8,
            control_seq: 5,
        },
        vec![entry.clone(), failed],
    )
    .unwrap();
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        serde_json::from_value::<SessionProjectionSnapshot>(wire.clone()).unwrap(),
        snapshot
    );
    assert_eq!(snapshot.entries().len(), 2);
    let mut changed = wire.clone();
    changed["entries"][1] = changed["entries"][0].clone();
    assert!(serde_json::from_value::<SessionProjectionSnapshot>(changed).is_err());
    let mut changed = wire.clone();
    changed["entries"][0]["content"]["value"] = json!("x".repeat(64 * 1024));
    assert!(serde_json::from_value::<SessionProjectionSnapshot>(changed).is_err());
    let mut changed = wire;
    changed["header_sha256"] = json!("not-a-hash");
    assert!(serde_json::from_value::<SessionProjectionSnapshot>(changed).is_err());
    assert!(
        ProjectionEntry::failed(
            ContributionId::new("fixture.bad").unwrap(),
            "x".repeat(4097)
        )
        .is_err()
    );
    assert!(ProjectionEntry::failed(ContributionId::new("fixture.bad").unwrap(), "").is_err());
    assert!(
        SessionProjectionSnapshot::new(
            SessionId::new("session").unwrap(),
            "a".repeat(64),
            "b".repeat(64),
            ProjectionCursor::Draft { revision: 0 },
            vec![entry; 65]
        )
        .is_err()
    );
}

#[test]
fn projection_cursors_track_both_durable_horizons_and_never_return_to_a_draft() {
    let draft = ProjectionCursor::Draft { revision: 3 };
    let durable = ProjectionCursor::Durable {
        fact_seq: 8,
        control_seq: 5,
    };
    assert!(durable.can_follow(draft));
    assert!(!draft.can_follow(durable));
    assert!(!ProjectionCursor::Draft { revision: 2 }.can_follow(draft));
    assert!(
        ProjectionCursor::Durable {
            fact_seq: 8,
            control_seq: 6
        }
        .can_follow(durable)
    );
    assert!(
        ProjectionCursor::Durable {
            fact_seq: 9,
            control_seq: 5
        }
        .can_follow(durable)
    );
    assert!(
        !ProjectionCursor::Durable {
            fact_seq: 9,
            control_seq: 4
        }
        .can_follow(durable)
    );
    assert!(
        !ProjectionCursor::Durable {
            fact_seq: 7,
            control_seq: 6
        }
        .can_follow(durable)
    );
}
