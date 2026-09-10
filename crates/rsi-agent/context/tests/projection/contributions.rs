use super::*;
use rsi_agent_session_protocol::{ContributionId, ToolRejection};

fn contribution_history() -> Vec<SessionFact> {
    let turn = TurnId::new("contribution-turn").unwrap();
    let model = EffectId::new("model").unwrap();
    let step = StepId::new("step").unwrap();
    let mut bodies = vec![
        SessionFactBody::TurnAccepted {
            turn_id: turn.clone(),
            text: "work".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::StepStarted {
            turn_id: turn.clone(),
            step_id: step.clone(),
        },
        SessionFactBody::InputMessageEntered {
            turn_id: turn.clone(),
            step_id: step,
            source: InputMessageSource::PluginContext {
                contribution_id: ContributionId::new("fixture.time").unwrap(),
            },
            content: vec![AgentMessageContent::Text {
                text: "Sampled at 42 ms".into(),
            }],
        },
        SessionFactBody::ModelIntent {
            turn_id: turn.clone(),
            effect_id: model.clone(),
            snapshot: snapshot(),
        },
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: model.clone(),
        },
    ];
    bodies.extend(
        [
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::ToolCall {
                    id: "call".into(),
                    name: "bash".into(),
                    kind: ToolCallKind::Function,
                },
            },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::ToolArguments("{}".into()),
            },
            LanguageEvent::ContentFinished { index: 0 },
            LanguageEvent::Finished {
                reason: FinishReason::ToolCalls,
                replay: None,
            },
        ]
        .map(|event| SessionFactBody::ModelEvent {
            turn_id: turn.clone(),
            effect_id: model.clone(),
            event,
        }),
    );
    bodies.push(SessionFactBody::ToolRejected {
        turn_id: turn,
        effect_id: EffectId::new("tool").unwrap(),
        identity: ToolResultIdentity::new("owner", "tool", "call", "a".repeat(64)).unwrap(),
        name: "bash".into(),
        arguments: json!({}),
        rejection: ToolRejection::PolicyDenied {
            contribution_id: ContributionId::new("fixture.plan").unwrap(),
            reason: "Only reading is permitted".into(),
        },
    });
    facts(bodies)
}

#[test]
fn plugin_text_and_tool_rejection_replay_without_live_producers() {
    let history = contribution_history();
    let mut fold = ContextFold::with_limits(header(""), ContextLimits::default()).unwrap();
    fold.apply(&history).unwrap();
    let projected = fold.project(ContextLimits::default()).unwrap();
    assert_eq!(
        projected
            .messages
            .iter()
            .map(rsi_ai_protocol::Message::role)
            .collect::<Vec<_>>(),
        [
            MessageRole::User,
            MessageRole::Developer,
            MessageRole::Assistant,
            MessageRole::Tool
        ]
    );
    assert!(
        matches!(projected.messages[1].content(), [MessageContent::Text { text }] if text == "Sampled at 42 ms")
    );
    assert!(
        matches!(projected.messages[3].content(), [MessageContent::ToolResult { call_id, is_error: true, content }]
        if call_id == "call" && matches!(content.as_slice(), [MessageContent::Text { text }] if text == "Only reading is permitted"))
    );
    let restored = ContextFold::from_checkpoint(
        header(""),
        ContextLimits::default(),
        &fold.checkpoint_bytes().unwrap(),
    )
    .unwrap();
    assert_eq!(
        restored.project(ContextLimits::default()).unwrap(),
        projected
    );
}
