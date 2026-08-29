use rsi_approval_protocol::{ApprovalOutcome, ApprovalRequest, MAXIMUM_APPROVAL_FIELD_BYTES};
use serde_json::json;

#[test]
fn approval_deserialization_revalidates_request_and_outcome_bounds() {
    let oversized = "x".repeat(MAXIMUM_APPROVAL_FIELD_BYTES + 1);
    assert!(
        serde_json::from_value::<ApprovalRequest>(json!({
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
}
