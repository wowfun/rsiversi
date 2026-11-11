use rsi_agent_session_protocol::{
    AgentMessageContent, ContributionId, EffectId, InputMessageSource,
    MAXIMUM_AGENT_DIAGNOSTIC_BYTES, SessionFact, SessionFactBody, StepId, ToolRejection, TurnId,
};
use rsi_approval_protocol::{ApprovalDecision, ApprovalOutcome};
use rsi_tools_protocol::ToolResultIdentity;
use serde_json::json;

#[test]
fn assembled_prompt_retains_all_existing_default_budgets() {
    use rsi_agent_session_protocol::{
        MAXIMUM_DELEGATION_PERSONA_BYTES, MAXIMUM_STEP_INSTRUCTION_BYTES,
        MAXIMUM_SYSTEM_PROMPT_BYTES, StepInstruction, StepPrompt,
    };
    let mut sections = Vec::new();
    for (name, bytes) in [
        ("header", MAXIMUM_SYSTEM_PROMPT_BYTES),
        ("persona", MAXIMUM_DELEGATION_PERSONA_BYTES),
        ("guidance", MAXIMUM_STEP_INSTRUCTION_BYTES),
    ] {
        sections.push(StepInstruction {
            producer: ContributionId::new(name).unwrap(),
            text: " ".repeat(bytes),
        });
    }
    assert!(
        StepPrompt::Assembled {
            sections: sections.clone()
        }
        .validate()
        .is_ok()
    );
    assert!(
        StepPrompt::AppendToHeader {
            sections: sections.clone()
        }
        .validate()
        .is_err()
    );
    sections[0].text.push('x');
    assert!(
        StepPrompt::Assembled { sections }
            .validate()
            .unwrap_err()
            .to_string()
            .contains("System prompt budget exceeded")
    );
}

#[test]
fn step_instruction_omission_and_explicit_clear_have_distinct_wire_meanings() {
    let body = |prompt| SessionFactBody::StepPrepared {
        turn_id: TurnId::new("turn").unwrap(),
        step_id: StepId::new("step").unwrap(),
        names: vec![],
        application: None,
        prompt,
    };
    let unchanged = SessionFact::new(3, 42, body(None)).unwrap();
    let wire = serde_json::to_value(&unchanged).unwrap();
    assert!(wire.get("prompt").is_none());
    assert_eq!(
        serde_json::from_value::<SessionFact>(wire).unwrap(),
        unchanged
    );
    let clear = SessionFact::new(
        3,
        42,
        body(Some(rsi_agent_session_protocol::StepPrompt::Assembled {
            sections: vec![],
        })),
    )
    .unwrap();
    let wire = serde_json::to_value(&clear).unwrap();
    assert_eq!(wire["prompt"], json!({"mode":"assembled", "sections":[]}));
    assert_eq!(
        serde_json::from_value::<SessionFact>(wire.clone()).unwrap(),
        clear
    );
    let mut invalid = wire;
    invalid["prompt"] =
        json!({"mode":"append_to_header", "sections":[{"producer":"fixture", "text":""}]});
    assert!(serde_json::from_value::<SessionFact>(invalid).is_err());
}

#[test]
fn plugin_input_has_validated_provenance_and_preserves_actual_text() {
    let body = SessionFactBody::InputMessageClaimed {
        turn_id: TurnId::new("turn").unwrap(),
        step_id: StepId::new("step").unwrap(),
        source: InputMessageSource::PluginContext {
            tool: None,
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
    for source in [media["source"].clone(), json!({"type":"runtime_context"})] {
        media["source"] = source;
        assert!(
            matches!(serde_json::from_value::<SessionFact>(media.clone()), Err(error)
            if error.to_string().contains("runtime and plugin context must contain only text"))
        );
    }
}

#[test]
fn rejected_tool_preserves_preparation_and_validates_the_actual_denial() {
    let body = |rejection| SessionFactBody::ToolRejected {
        additional_contexts: Vec::new(),
        origin: rsi_agent_session_protocol::ToolOrigin::Model {
            effect_id: rsi_agent_session_protocol::EffectId::new("source-model").unwrap(),
        },
        program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
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
    let mut wire = serde_json::to_value(&fact).unwrap();
    assert!(wire.get("additional_contexts").is_none());
    wire["additional_contexts"] =
        json!([{"id":"review","source":{"kind":"plugin"},"text":["Try reading the file"]}]);
    let with_context = serde_json::from_value::<SessionFact>(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(with_context).unwrap(), wire);
    let mut program = wire.clone();
    program["origin"] = json!({"kind":"program","parent_effect_id":"coordinator","ordinal":1});
    program["program_role"] = json!("callable");
    assert!(serde_json::from_value::<SessionFact>(program.clone()).is_err());
    program
        .as_object_mut()
        .unwrap()
        .remove("additional_contexts");
    assert!(serde_json::from_value::<SessionFact>(program).is_ok());
    wire["additional_contexts"][0]["text"] = json!([]);
    assert!(serde_json::from_value::<SessionFact>(wire).is_err());
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
