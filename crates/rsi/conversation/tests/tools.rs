use rsi_agent_session_protocol::{
    ContributionId, EffectId, MessageId, SessionFact, SessionFactBody, ToolRejection, TurnId,
};
use rsi_conversation::{BlockIdentity, OutputRef, ToolOutcome, ToolPhase, ToolState};
use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
use serde_json::json;

fn fact(seq: u64, kind: &str, owner: &str) -> SessionFact {
    let turn_id = TurnId::new("same").unwrap();
    let effect_id = EffectId::new("effect").unwrap();
    let identity = ToolResultIdentity::new(owner, "invoke", "call", "a".repeat(64)).unwrap();
    let body = match kind {
        "intent" => SessionFactBody::ToolIntent {
            turn_id, effect_id, identity, name: "bash".into(), arguments: json!({"command":"exit 7"}), approval: None, parallel_safe: false,
        },
        "started" => SessionFactBody::ToolStarted { turn_id, effect_id, identity },
        "rejected" => SessionFactBody::ToolRejected {
            turn_id, effect_id, identity, name: "bash".into(), arguments: json!({"command":"exit 7"}),
            rejection: ToolRejection::PolicyDenied { contribution_id: ContributionId::new("fixture.policy").unwrap(), reason: "denied".into() },
        },
        "result" => SessionFactBody::ToolResult {
            turn_id, effect_id, identity, result: ToolResult::new(json!({"exit_code":7,"stdout":{"full_output":"a".repeat(32)},"stderr":{"full_output":"x".repeat(1024*1024)}}), vec![], false).unwrap(),
        },
        _ => unreachable!(),
    };
    SessionFact::new(seq, 1, body).unwrap()
}

#[test]
fn partial_tool_lifecycle_preserves_exact_pairing_and_repairs_only_missing_intent() {
    let intent = fact(4, "intent", "owner");
    let started = fact(5, "started", "owner");
    let result = fact(6, "result", "owner");
    let mut state = ToolState::from_fact(&intent).unwrap();
    assert_eq!(state.phase, ToolPhase::Prepared);
    assert!(state.intent_present);
    state.observe(&started);
    assert_eq!(state.phase, ToolPhase::Running);
    state.observe(&result);
    assert_eq!(state.phase, ToolPhase::Settled(ToolOutcome::ProcessFailed));
    assert_eq!(state.title(), "bash · command failed");
    let complete = serde_json::to_value(&state).unwrap();
    let mut suffix = ToolState::from_fact(&result).unwrap();
    assert!(!suffix.intent_present);
    assert!(suffix.name.is_none());
    assert!(suffix.arguments.is_none());
    assert_eq!(suffix.outputs[0].as_ref().unwrap().as_str(), "a".repeat(32));
    assert!(
        suffix.outputs[1].is_none(),
        "invalid large output ID is never retained"
    );
    assert!(suffix.owned_bytes() < 2048);
    suffix.observe(&intent);
    suffix.observe(&started);
    suffix.observe(&result);
    assert_eq!(serde_json::to_value(&suffix).unwrap(), complete);
    assert!(!suffix.observe(&fact(7, "result", "other-owner")));
    assert_eq!(serde_json::to_value(&suffix).unwrap(), complete);
    assert_ne!(
        suffix.key(),
        ToolState::from_fact(&fact(6, "result", "other-owner"))
            .unwrap()
            .key()
    );
}

#[test]
fn rejection_has_arguments_and_provenance_without_inventing_started_or_intent() {
    let rejected = ToolState::from_fact(&fact(4, "rejected", "owner")).unwrap();
    assert_eq!(rejected.phase, ToolPhase::Rejected);
    assert_eq!(rejected.title(), "bash · rejected");
    assert!(!rejected.intent_present);
    assert!(rejected.arguments.is_some());
    assert!(rejected.rejection.is_some());
    assert!(rejected.result.is_none());
}

#[test]
fn semantic_identity_kinds_and_validated_output_references_remain_distinct() {
    let turn = TurnId::new("same").unwrap();
    let message = MessageId::new("same").unwrap();
    assert_ne!(
        BlockIdentity::TurnInput { turn: &turn }.key(),
        BlockIdentity::Message { message: &message }.key()
    );
    assert_ne!(
        BlockIdentity::TurnInput { turn: &turn }.key(),
        BlockIdentity::Terminal { turn: &turn }.key()
    );
    for invalid in [
        "",
        "a",
        "A012345678901234567890123456789012",
        "a01234567890123456789012345678901/",
        "../../private",
    ] {
        assert!(OutputRef::parse(invalid).is_none());
    }
    assert!(OutputRef::parse(&"1".repeat(32)).is_some());
}
