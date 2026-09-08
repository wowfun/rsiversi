use rsi_agent_session_protocol::{
    AgentMessageContent, ContributionId, EffectId, InputMessageSource,
    MAXIMUM_AGENT_DIAGNOSTIC_BYTES, SessionFact, SessionFactBody, StepId, ToolRejection, TurnId,
};
use rsi_approval_protocol::{ApprovalDecision, ApprovalOutcome};
use rsi_tools_protocol::ToolResultIdentity;
use serde_json::json;

#[test]
fn plugin_input_has_validated_provenance_and_preserves_actual_text() {
    let body = SessionFactBody::InputMessageEntered {
        turn_id: TurnId::new("turn").unwrap(),
        step_id: StepId::new("step").unwrap(),
        source: InputMessageSource::PluginContext {
            contribution_id: ContributionId::new("fixture.time").unwrap(),
        },
        content: vec![AgentMessageContent::Text {
            text: "Sampled at 42 ms".into(),
        }],
    };
    let fact = SessionFact::new(3, 42, body).unwrap();
    let wire = serde_json::to_value(&fact).unwrap();
    assert_eq!(
        serde_json::from_value::<SessionFact>(wire.clone()).unwrap(),
        fact
    );
    let mut invalid = wire;
    invalid["source"]["contribution_id"] = "".into();
    assert!(serde_json::from_value::<SessionFact>(invalid).is_err());
    let mut media = serde_json::to_value(&fact).unwrap();
    media["content"] = json!([{
        "type":"image", "media": {
            "id":"a".repeat(64), "mime":"image/png", "bytes":8, "width":1, "height":1,
        },
    }]);
    assert!(
        matches!(serde_json::from_value::<SessionFact>(media), Err(error)
        if error.to_string().contains("PluginContext must contain only text"))
    );
}

#[test]
fn rejected_tool_preserves_preparation_and_validates_the_actual_denial() {
    let body = |rejection| SessionFactBody::ToolRejected {
        turn_id: TurnId::new("turn").unwrap(),
        effect_id: EffectId::new("effect").unwrap(),
        identity: ToolResultIdentity::new("owner", "invocation", "call", "a".repeat(64)).unwrap(),
        name: "bash".into(),
        arguments: json!({"command":"printf hello"}),
        rejection,
    };
    let approval = |decision| ToolRejection::ApprovalDenied {
        outcome: ApprovalOutcome {
            decision,
            answerer: "fixture".into(),
            reason: None,
        },
    };
    let fact = SessionFact::new(4, 42, body(approval(ApprovalDecision::Deny))).unwrap();
    assert_eq!(
        serde_json::from_value::<SessionFact>(serde_json::to_value(&fact).unwrap()).unwrap(),
        fact
    );
    assert!(
        body(approval(ApprovalDecision::AllowOnce))
            .validate()
            .is_err()
    );
    let policy = |reason: String| ToolRejection::PolicyDenied {
        contribution_id: ContributionId::new("fixture.plan").unwrap(),
        reason,
    };
    body(policy("This Tool is outside the plan allowlist".into()))
        .validate()
        .unwrap();
    assert!(
        body(policy("x".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES + 1)))
            .validate()
            .is_err()
    );
    assert!(body(policy(String::new())).validate().is_err());
    let mut invalid = serde_json::to_value(&fact).unwrap();
    invalid["rejection"]["outcome"]["decision"] = "allow_once".into();
    assert!(serde_json::from_value::<SessionFact>(invalid).is_err());
}
