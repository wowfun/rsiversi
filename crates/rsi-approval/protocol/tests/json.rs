use rsi_approval_protocol::{ApprovalOutcome, ApprovalRequest, MAXIMUM_APPROVAL_FIELD_BYTES};
use serde_json::json;

#[test]
fn approval_deserialization_revalidates_request_and_outcome_bounds() {
    let oversized = "x".repeat(MAXIMUM_APPROVAL_FIELD_BYTES + 1);
    assert!(
        serde_json::from_value::<ApprovalRequest>(json!({
            "subject": {
                "session_id": "session-1",
                "turn_id": "turn-1",
                "effect_id": "effect-1"
            },
            "id": oversized,
            "action": "run",
            "reason": "needed"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ApprovalOutcome>(json!({
            "decision": "deny",
            "answerer": "",
            "reason": null
        }))
        .is_err()
    );

    for field in ["session_id", "turn_id", "effect_id"] {
        let mut subject = serde_json::Map::from_iter([
            ("session_id".into(), json!("session-1")),
            ("turn_id".into(), json!("turn-1")),
            ("effect_id".into(), json!("effect-1")),
        ]);
        subject.insert(field.into(), json!(oversized));
        assert!(
            serde_json::from_value::<ApprovalRequest>(json!({
                "subject": subject,
                "id": "approval-1",
                "action": "run",
                "reason": "needed"
            }))
            .is_err(),
            "oversized {field} must be rejected at the wire boundary"
        );
    }
}
