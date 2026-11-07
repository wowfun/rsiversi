use super::*;
use rsi_agent_session_protocol::{ContributionId, ToolRejection};

fn contribution_history() -> Vec<SessionFact> {
    let turn = TurnId::new("contribution-turn").unwrap();
    let model = EffectId::new("model").unwrap();
    let step = StepId::new("step").unwrap();
    let mut bodies = vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
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
        SessionFactBody::InputMessageClaimed {
            turn_id: turn.clone(),
            step_id: step,
            source: InputMessageSource::PluginContext {
                tool: None,
                contribution_id: ContributionId::new("fixture.time").unwrap(),
            },
            content: vec![AgentMessageContent::Text {
                text: "Sampled at 42 ms".into(),
            }],
        },
        SessionFactBody::ModelIntent {
            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
            },
            price_quote: None,
            purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
            turn_id: turn.clone(),
            effect_id: model.clone(),
            snapshot: snapshot(),
        },
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: model.clone(),
        },
    ];
    bodies.push(SessionFactBody::ModelAttemptSettled {
        purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
        turn_id: turn.clone(),
        effect_id: model.clone(),
        attempt: rsi_agent_session_protocol::ModelAttemptLog::from_events(
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
            .into_iter()
            .map(|event| (0, event)),
            true,
        )
        .unwrap(),
    });
    bodies.push(SessionFactBody::ToolRejected {
        additional_contexts: Vec::new(),
        origin: rsi_agent_session_protocol::ToolOrigin::Model {
            effect_id: model.clone(),
        },
        program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
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
    entered_facts(bodies)
}

#[test]
fn plugin_text_and_tool_rejection_replay_without_live_producers() {
    let history = contribution_history();
    let mut fold = ContextFold::with_limits(
        header(""),
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    fold.apply(&history).unwrap();
    let producer = ContributionId::new("fixture.time").unwrap();
    let timestamp = history
        .iter()
        .find(|fact| matches!(fact.body(), SessionFactBody::InputMessageClaimed { .. }))
        .unwrap()
        .timestamp_ms();
    assert_eq!(
        fold.previous_context_input_at_ms(&producer),
        Some(timestamp)
    );
    let projected = fold.project(ContextLimits::default()).unwrap();
    assert_eq!(
        projected
            .messages()
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
        matches!(projected.messages()[1].content(), [MessageContent::Text { text }] if text == "Sampled at 42 ms")
    );
    assert!(
        matches!(projected.messages()[3].content(), [MessageContent::ToolResult { call_id, is_error: true, content }]
        if call_id == "call" && matches!(content.as_slice(), [MessageContent::Text { text }] if text == "Only reading is permitted"))
    );
    let restored = ContextFold::from_checkpoint(
        header(""),
        ContextLimits::default(),
        &fold.checkpoint_bytes().unwrap(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    assert_eq!(
        restored.previous_context_input_at_ms(&producer),
        Some(timestamp)
    );
    assert_eq!(
        restored.project(ContextLimits::default()).unwrap(),
        projected
    );
}

#[test]
fn only_entered_context_advances_producer_time() {
    use rsi_agent_session_protocol::{
        InputClaimRef, MessageAttribution, PreparedInput, PreparedInputMessage, PreparedInputRole,
        StepInputDecision,
    };
    let producer = ContributionId::new("fixture.time").unwrap();
    let turn = TurnId::new("time-turn").unwrap();
    let step = StepId::new("time-step").unwrap();
    let history = facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: turn.clone(),
            text: "work".into(),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::StepStarted {
            turn_id: turn.clone(),
            step_id: step.clone(),
        },
        SessionFactBody::InputMessageClaimed {
            turn_id: turn.clone(),
            step_id: step.clone(),
            source: InputMessageSource::PluginContext {
                contribution_id: producer.clone(),
                tool: None,
            },
            content: vec![AgentMessageContent::Text {
                text: "time".into(),
            }],
        },
    ]);
    let claim = InputClaimRef::new(3, 0).unwrap();
    for (decision, entered) in [
        (StepInputDecision::enter(vec![]).unwrap(), false),
        (
            StepInputDecision::enter(vec![PreparedInput::Replacement {
                claim,
                message: PreparedInputMessage::new(
                    PreparedInputRole::Developer,
                    MessageAttribution {
                        id: "empty".into(),
                        source: json!({"kind":"fixture"}),
                    },
                    vec![],
                )
                .unwrap(),
            }])
            .unwrap(),
            false,
        ),
        (
            StepInputDecision::enter(vec![PreparedInput::Claimed { claim }]).unwrap(),
            true,
        ),
    ] {
        let mut fold = ContextFold::with_limits(
            header(""),
            ContextLimits::default(),
            rsi_agent_context::ContextBudget::default(),
        )
        .unwrap();
        fold.apply(&history).unwrap();
        assert_eq!(fold.previous_context_input_at_ms(&producer), None);
        let mut fold = ContextFold::from_checkpoint(
            header(""),
            ContextLimits::default(),
            &fold.checkpoint_bytes().unwrap(),
            rsi_agent_context::ContextBudget::default(),
        )
        .unwrap();
        assert_eq!(fold.previous_context_input_at_ms(&producer), None);
        fold.apply(&[SessionFact::new(
            4,
            900,
            SessionFactBody::StepInputsPrepared {
                turn_id: turn.clone(),
                step_id: step.clone(),
                decision,
            },
        )
        .unwrap()])
            .unwrap();
        assert_eq!(
            fold.previous_context_input_at_ms(&producer),
            entered.then_some(3)
        );
        assert_eq!(
            fold.previous_context_input_at_ms(&ContributionId::new("other").unwrap()),
            None
        );
    }
}

#[test]
fn context_time_follows_selected_fork_history_and_retained_turn_lifetime() {
    let producer = ContributionId::new("fixture.time").unwrap();
    let mut seed = contribution_history();
    let timestamp = seed
        .iter()
        .find(|fact| matches!(fact.body(), SessionFactBody::InputMessageClaimed { .. }))
        .unwrap()
        .timestamp_ms();
    seed.extend(entered_facts_after(
        seed.len() as u64,
        vec![SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("contribution-turn").unwrap(),
            outcome: TurnOutcome::Completed,
            result: None,
        }],
    ));
    let limits = ContextLimits::new(5, 1024 * 1024).unwrap();
    let mut child = ContextFold::with_limits(
        fork_header("", seed.len() as u64, 1),
        limits,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    child.begin_seed_source(&child.header().clone()).unwrap();
    child.apply_seed_page(&seed).unwrap();
    child.finish_seed().unwrap();
    assert_eq!(
        child.previous_context_input_at_ms(&producer),
        Some(timestamp)
    );
    child
        .apply(&entered_facts(vec![SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("child-turn").unwrap(),
            text: "one more message".into(),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        }]))
        .unwrap();
    child
        .apply(&entered_facts_after(
            child.through_seq(),
            vec![SessionFactBody::InputMessageClaimed {
                turn_id: TurnId::new("child-turn").unwrap(),
                step_id: StepId::new("child-step").unwrap(),
                source: InputMessageSource::PluginContext {
                    contribution_id: ContributionId::new("other").unwrap(),
                    tool: None,
                },
                content: vec![AgentMessageContent::Text {
                    text: "different plugin".into(),
                }],
            }],
        ))
        .unwrap();
    assert_eq!(child.previous_context_input_at_ms(&producer), None);
}
