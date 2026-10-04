use rsi_agent_context::{ContextError, ContextFold, ContextLimits};
use rsi_agent_session_protocol::{
    ActivationId, AgentMessageContent, AgentPath, AgentPresetId, EffectId, ForkOrigin,
    ForkTurnSelection, FrozenAgentSettings, InputMessageSource, MessageId, SessionFact,
    SessionFactBody, SessionHeader, SessionId, StepId, TurnId, TurnOutcome,
};
use rsi_ai_protocol::{
    AiCapability, ContentDelta, ContentStart, FinishReason, LanguageEvent, MessageContent,
    MessageRole, ModelRef, PreparedCallSnapshot, ProviderExtension, RetryPolicy, ToolCallKind,
};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
use serde_json::json;

#[path = "projection/builders.rs"]
mod builders;
#[path = "projection/compaction.rs"]
mod compaction;
#[path = "projection/contributions.rs"]
mod contributions;
#[path = "projection/images.rs"]
mod images;

fn header(system: &str) -> SessionHeader {
    SessionHeader::new_local(
        SessionId::new("session-1").unwrap(),
        1,
        "/secret/workspace-name",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "default",
            system,
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

fn fork_header(system: &str, terminal_seq: u64, effective_turns: u64) -> SessionHeader {
    let parent = header(system);
    let parent_id = parent.session_id().clone();
    let parent_fingerprint = parent.fingerprint().unwrap();
    parent
        .forked_child(
            SessionId::new("session-child").unwrap(),
            2,
            ForkOrigin {
                parent_session_id: parent_id.clone(),
                root_session_id: parent_id,
                path: AgentPath::new(vec![1]).unwrap(),
                task_name: "child".into(),
                parent_header_fingerprint: parent_fingerprint,
                invoking_turn_id: TurnId::new("turn-spawn").unwrap(),
                resolved_after_seq: 0,
                resolved_terminal_seq: terminal_seq,
                terminal_prefix_sha256: if terminal_seq == 0 {
                    "0".repeat(64)
                } else {
                    "a".repeat(64)
                },
                resolved_terminal_control_seq: effective_turns,
                terminal_control_prefix_sha256: if terminal_seq == 0 {
                    "0".repeat(64)
                } else {
                    "c".repeat(64)
                },
                requested_turns: ForkTurnSelection::All,
                effective_turns,
            },
            rsi_agent_session_protocol::ModelSelection::baseline(parent.settings()),
        )
        .unwrap()
}

fn snapshot() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        language_settings: None,
        call_id: "call-model".into(),
        deployment_id: "deployment".into(),
        provider_family: "test".into(),
        capability: AiCapability::Language,
        model: "model".into(),
        protocol: "test".into(),
        transport: "memory".into(),
        endpoint_fingerprint: "endpoint".into(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "a".repeat(64),
    }
}

fn facts(bodies: Vec<SessionFactBody>) -> Vec<SessionFact> {
    bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| {
            let seq = u64::try_from(index).unwrap() + 1;
            SessionFact::new(seq, seq, body).unwrap()
        })
        .collect()
}

fn facts_after(after_seq: u64, bodies: Vec<SessionFactBody>) -> Vec<SessionFact> {
    bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| {
            let seq = after_seq + u64::try_from(index).unwrap() + 1;
            SessionFact::new(seq, seq, body).unwrap()
        })
        .collect()
}

#[allow(clippy::too_many_lines)] // Complete correlated model and Tool Fact fixture.
fn tool_result_facts(result: ToolResult) -> Vec<SessionFact> {
    let turn = TurnId::new("turn-1").unwrap();
    let effect = EffectId::new("model-1").unwrap();
    let tool_identity =
        ToolResultIdentity::new("tool-f1-g1-r1", "effect-1", "tool-call", "b".repeat(64)).unwrap();
    facts(vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: turn.clone(),
            text: "use a tool".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::ModelIntent {
            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
            },
            price_quote: None,
            purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            snapshot: snapshot(),
        },
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::ToolCall {
                    id: "tool-call".into(),
                    name: "lookup".into(),
                    kind: ToolCallKind::Function,
                },
            },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::ToolArguments("{}".into()),
            },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentFinished { index: 0 },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: turn.clone(),
            effect_id: effect,
            event: LanguageEvent::Finished {
                reason: FinishReason::ToolCalls,
                replay: None,
            },
        },
        SessionFactBody::ToolIntent {
            turn_id: turn.clone(),
            effect_id: EffectId::new("tool-1").unwrap(),
            identity: tool_identity.clone(),
            origin: rsi_agent_session_protocol::ToolOrigin::Model {
                effect_id: EffectId::new("model-1").unwrap(),
            },
            program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
            name: "lookup".into(),
            arguments: json!({}),
            approval: None,
            parallel_safe: false,
        },
        SessionFactBody::ToolStarted {
            turn_id: turn.clone(),
            effect_id: EffectId::new("tool-1").unwrap(),
            identity: tool_identity.clone(),
        },
        SessionFactBody::ToolResult {
            turn_id: turn,
            effect_id: EffectId::new("tool-1").unwrap(),
            identity: tool_identity,
            result,
            conclusion: None,
        },
    ])
}

#[test]
fn tool_call_and_result_remain_adjacent_and_workspace_is_not_implicit() {
    let mut fold =
        ContextFold::new(header(""), rsi_agent_context::ContextBudget::default()).unwrap();
    fold.apply(&tool_result_facts(
        ToolResult::new(json!({"answer":42}), vec![], false).unwrap(),
    ))
    .unwrap();
    let projected = fold.project(ContextLimits::default()).unwrap();
    assert_eq!(
        projected
            .messages()
            .iter()
            .map(rsi_ai_protocol::Message::role)
            .collect::<Vec<_>>(),
        vec![MessageRole::User, MessageRole::Assistant, MessageRole::Tool]
    );
    assert!(matches!(
        projected.messages()[1].content(),
        [MessageContent::ToolCall(_)]
    ));
    assert!(matches!(
        projected.messages()[2].content(),
        [MessageContent::ToolResult { call_id, .. }] if call_id == "tool-call"
    ));
    let encoded = serde_json::to_string(projected.messages()).unwrap();
    assert!(!encoded.contains("secret/workspace-name"));
}

#[test]
fn provider_replay_does_not_elide_history_without_an_exact_route_identity() {
    let previous = TurnId::new("turn-previous").unwrap();
    let current = TurnId::new("turn-current").unwrap();
    let effect = EffectId::new("model-replay").unwrap();
    let replay = ProviderExtension::new(
        "openai.responses.replay",
        0,
        json!({"response_id": "resp-1"}),
    )
    .unwrap();
    let mut fold = ContextFold::new(
        header("system"),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: previous.clone(),
            text: "previous user input".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::ModelIntent {
            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
            },
            price_quote: None,
            purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
            turn_id: previous.clone(),
            effect_id: effect.clone(),
            snapshot: snapshot(),
        },
        SessionFactBody::ModelStarted {
            turn_id: previous.clone(),
            effect_id: effect.clone(),
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: previous.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Reasoning,
            },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: previous.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Reasoning("private reasoning".into()),
            },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: previous.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentFinished { index: 0 },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: previous.clone(),
            effect_id: effect,
            event: LanguageEvent::Finished {
                reason: FinishReason::Stop,
                replay: Some(replay.clone()),
            },
        },
        SessionFactBody::TurnTerminal {
            turn_id: previous,
            outcome: TurnOutcome::Completed,
            result: None,
        },
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: current,
            text: "current user input".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    ]))
    .unwrap();

    let request = fold
        .request(
            ContextLimits::default(),
            rsi_ai_protocol::LanguageRequestOptions::default(),
        )
        .unwrap();

    assert!(request.extensions().is_empty());
    assert_eq!(request.messages().len(), 3);
    let encoded = serde_json::to_string(request.messages()).unwrap();
    assert!(encoded.contains("current user input"));
    assert!(encoded.contains("previous user input"));
    assert!(!encoded.contains("private reasoning"));
    assert!(!encoded.contains("resp-1"));
}

#[test]
#[allow(clippy::too_many_lines)] // The regression keeps the complete visible inherited request while replay remains route-unscoped.
fn fork_seed_keeps_canonical_history_when_replay_route_is_not_preflighted() {
    let parent_turn = TurnId::new("turn-fork-parent").unwrap();
    let parent_effect = EffectId::new("model-fork-parent").unwrap();
    let replay = ProviderExtension::new(
        "openai.responses.replay",
        0,
        json!({"response_id": "resp-parent"}),
    )
    .unwrap();
    let seed = facts(vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: parent_turn.clone(),
            text: "parent input".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::ModelIntent {
            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
            },
            price_quote: None,
            purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
            turn_id: parent_turn.clone(),
            effect_id: parent_effect.clone(),
            snapshot: snapshot(),
        },
        SessionFactBody::ModelStarted {
            turn_id: parent_turn.clone(),
            effect_id: parent_effect.clone(),
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: parent_turn.clone(),
            effect_id: parent_effect.clone(),
            event: LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: parent_turn.clone(),
            effect_id: parent_effect.clone(),
            event: LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("parent output".into()),
            },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: parent_turn.clone(),
            effect_id: parent_effect.clone(),
            event: LanguageEvent::ContentFinished { index: 0 },
        },
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: parent_turn.clone(),
            effect_id: parent_effect,
            event: LanguageEvent::Finished {
                reason: FinishReason::Stop,
                replay: Some(replay.clone()),
            },
        },
        SessionFactBody::TurnTerminal {
            turn_id: parent_turn,
            outcome: TurnOutcome::Completed,
            result: None,
        },
    ]);
    let child_turn = TurnId::new("turn-fork-child").unwrap();
    let child_step = StepId::new("step-fork-child").unwrap();
    let child = facts(vec![
        SessionFactBody::MessageTurnAccepted {
            reasoning_effort: None,
            turn_id: child_turn.clone(),
            activation_id: ActivationId::new("activation-fork-child").unwrap(),
            message_ids: vec![MessageId::new("message-fork-child").unwrap()],
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::StepStarted {
            turn_id: child_turn.clone(),
            step_id: child_step.clone(),
        },
        SessionFactBody::InputMessageEntered {
            turn_id: child_turn,
            step_id: child_step,
            source: InputMessageSource::Agent {
                message_id: MessageId::new("message-fork-child").unwrap(),
                source_session_id: SessionId::new("session-parent").unwrap(),
            },
            content: vec![AgentMessageContent::Text {
                text: "child task".into(),
            }],
        },
    ]);

    let mut fold = ContextFold::new(
        fork_header("system", u64::try_from(seed.len()).unwrap(), 1),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    fold.apply_seed_page(&seed).unwrap();
    fold.finish_seed().unwrap();
    fold.apply(&child).unwrap();
    let request = fold
        .request(
            ContextLimits::default(),
            rsi_ai_protocol::LanguageRequestOptions::default(),
        )
        .unwrap();
    assert!(request.extensions().is_empty());
    assert_eq!(request.messages().len(), 4);
    assert!(matches!(
        request.messages()[3].content(),
        [MessageContent::Text { text }] if text == "child task"
    ));
}

#[test]
fn fork_seed_rejects_cross_page_overlap_and_incomplete_coverage() {
    let turn = TurnId::new("turn-seed").unwrap();
    let seed = facts(vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: turn.clone(),
            text: "parent".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: turn,
            outcome: TurnOutcome::Completed,
            result: None,
        },
    ]);

    let mut overlap = ContextFold::new(
        fork_header("", 2, 1),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    overlap.apply_seed_page(&seed[..1]).unwrap();
    assert!(matches!(
        overlap.apply_seed_page(&seed),
        Err(ContextError::Invalid(message)) if message.contains("expected parent Fact 2, got 1")
    ));

    let mut incomplete = ContextFold::new(
        fork_header("", 2, 1),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    incomplete.apply_seed_page(&seed[..1]).unwrap();
    assert!(matches!(
        incomplete.finish_seed(),
        Err(ContextError::Invalid(message)) if message.contains("complete inherited interval")
    ));

    let child = facts(vec![SessionFactBody::TurnAccepted {
        reasoning_effort: None,
        turn_id: TurnId::new("turn-child-before-seed").unwrap(),
        text: "child".into(),
        model: None,
        sandbox: SandboxMode::WorkspaceWrite,
        require_approval: false,
    }]);
    assert!(matches!(
        incomplete.apply(&child),
        Err(ContextError::Invalid(message)) if message.contains("complete inherited interval")
    ));
    let mut incomplete_page = ContextFold::new(
        fork_header("", 2, 1),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    incomplete_page.apply_seed_page(&seed[..1]).unwrap();
    assert!(matches!(
        incomplete_page.apply_page(&child, 1),
        Err(ContextError::Invalid(message)) if message.contains("complete inherited interval")
    ));
}

#[test]
fn compaction_drops_only_a_complete_oldest_turn_and_inserts_one_notice() {
    let old = TurnId::new("turn-old").unwrap();
    let current = TurnId::new("turn-current").unwrap();
    let mut fold = ContextFold::new(
        header("system"),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: old.clone(),
            text: "old".repeat(200),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: old,
            outcome: TurnOutcome::Completed,
            result: None,
        },
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: current,
            text: "current".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    ]))
    .unwrap();
    let projected = fold.project(ContextLimits::new(3, 500).unwrap()).unwrap();
    assert_eq!(projected.omitted_turns, 1);
    assert_eq!(projected.messages().len(), 3);
    let encoded = serde_json::to_string(projected.messages()).unwrap();
    assert!(encoded.contains("omitted 1 complete earlier turn"));
    assert!(encoded.contains("current"));
    assert!(!encoded.contains(&"old".repeat(50)));
}

#[test]
fn projection_uses_the_exact_canonical_json_byte_boundary() {
    let old = TurnId::new("turn-old").unwrap();
    let current = TurnId::new("turn-current").unwrap();
    let mut fold = ContextFold::new(
        header("system"),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: old.clone(),
            text: "old".repeat(200),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: old,
            outcome: TurnOutcome::Completed,
            result: None,
        },
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: current,
            text: "current".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    ]))
    .unwrap();
    let complete = fold.project(ContextLimits::default()).unwrap();
    let exact = serde_json::to_vec(complete.messages()).unwrap().len();

    assert_eq!(
        fold.project(ContextLimits::new(8, exact).unwrap())
            .unwrap()
            .omitted_turns,
        0
    );
    assert_eq!(
        fold.project(ContextLimits::new(8, exact - 1).unwrap())
            .unwrap()
            .omitted_turns,
        1
    );
}

#[test]
fn active_turn_is_never_split_to_force_a_fit() {
    let mut fold =
        ContextFold::new(header(""), rsi_agent_context::ContextBudget::default()).unwrap();
    fold.apply(&facts(vec![SessionFactBody::TurnAccepted {
        reasoning_effort: None,
        turn_id: TurnId::new("turn-current").unwrap(),
        text: "x".repeat(1_000),
        model: None,
        sandbox: SandboxMode::WorkspaceWrite,
        require_approval: false,
    }]))
    .unwrap();
    assert_eq!(
        fold.project(ContextLimits::new(10, 32).unwrap()),
        Err(ContextError::TooLarge)
    );
}

#[test]
fn incremental_fold_rejects_gaps_and_replays() {
    let mut fold =
        ContextFold::new(header(""), rsi_agent_context::ContextBudget::default()).unwrap();
    let first = facts(vec![SessionFactBody::TurnAccepted {
        reasoning_effort: None,
        turn_id: TurnId::new("turn-1").unwrap(),
        text: "hello".into(),
        model: None,
        sandbox: SandboxMode::WorkspaceWrite,
        require_approval: false,
    }]);
    fold.apply(&first).unwrap();
    assert!(fold.apply(&first).is_err());
}

#[test]
fn checkpoint_round_trip_preserves_projection_and_accepts_only_the_suffix() {
    let limits = ContextLimits::default();
    let old = TurnId::new("turn-old").unwrap();
    let mut fold = ContextFold::with_limits(
        header("system"),
        limits,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: old.clone(),
            text: "old user message".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: old,
            outcome: TurnOutcome::Completed,
            result: None,
        },
    ]))
    .unwrap();
    let expected = fold.project(limits).unwrap();
    let bytes = fold.checkpoint_bytes().unwrap();
    let mut restored = ContextFold::from_checkpoint(
        header("system"),
        limits,
        &bytes,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    assert_eq!(restored.project(limits).unwrap(), expected);

    restored
        .apply(&facts_after(
            2,
            vec![SessionFactBody::TurnAccepted {
                reasoning_effort: None,
                turn_id: TurnId::new("turn-current").unwrap(),
                text: "suffix only".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            }],
        ))
        .unwrap();
    let projected = restored.project(limits).unwrap();
    assert_eq!(projected.through_seq, 3);
    assert!(
        serde_json::to_string(projected.messages())
            .unwrap()
            .contains("suffix only")
    );
}

#[test]
fn checkpoint_capacity_refusal_preserves_prefix_and_exact_credit_follows_last_slice() {
    let budget = rsi_agent_context::ContextBudget::new(256 * 1024).unwrap();
    let mut fold =
        ContextFold::with_limits(header("system"), ContextLimits::default(), budget.clone())
            .unwrap();
    let turn = TurnId::new("checkpoint-capacity").unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: turn.clone(),
            text: "quote\" slash\\ line\n中文".repeat(32),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: turn,
            outcome: TurnOutcome::Completed,
            result: None,
        },
    ]))
    .unwrap();
    let expected = fold.checkpoint_bytes().unwrap().as_bytes().to_vec();
    let retained = budget.used();
    let through_seq = fold.through_seq();
    let digest = fold.fact_prefix_sha256();
    let mut pressure = budget
        .reserve(budget.maximum() - retained - expected.len() + 1)
        .unwrap();
    assert!(matches!(
        fold.checkpoint_bytes(),
        Err(ContextError::Capacity)
    ));
    assert_eq!(budget.used(), retained + pressure.bytes());
    assert_eq!(fold.through_seq(), through_seq);
    assert_eq!(fold.fact_prefix_sha256(), digest);
    pressure.resize(pressure.bytes() - 1).unwrap();
    let bytes = fold.checkpoint_bytes().unwrap();
    assert_eq!(bytes.as_bytes(), expected);
    assert_eq!(budget.used(), budget.maximum());
    let slice = bytes.slice(0..1).unwrap();
    let sibling = slice.clone();
    drop(bytes);
    drop(fold);
    drop(pressure);
    assert_eq!(budget.used(), expected.len());
    drop(slice);
    assert_eq!(budget.used(), expected.len());
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one complete checkpoint corruption matrix preserves the prior cursor"
)]
fn checkpoint_rejects_active_assembler_corruption_and_identity_mismatch() {
    let limits = ContextLimits::default();
    let mut empty_turn = ContextFold::with_limits(
        header("system"),
        limits,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    empty_turn
        .apply(&facts(vec![SessionFactBody::MessageTurnAccepted {
            reasoning_effort: None,
            turn_id: TurnId::new("turn-empty").unwrap(),
            activation_id: ActivationId::new("activation-empty").unwrap(),
            message_ids: vec![MessageId::new("message-empty").unwrap()],
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        }]))
        .unwrap();
    assert!(matches!(
        empty_turn.checkpoint_bytes(),
        Err(ContextError::Invalid(message)) if message.contains("empty turn")
    ));

    let mut active = ContextFold::with_limits(
        header("system"),
        limits,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    let active_turn = TurnId::new("turn-active").unwrap();
    active
        .apply(&facts(vec![
            SessionFactBody::TurnAccepted {
                reasoning_effort: None,
                turn_id: active_turn.clone(),
                text: "active".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
            SessionFactBody::ModelIntent {
                evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                    reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
                },
                price_quote: None,
                purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
                turn_id: active_turn,
                effect_id: EffectId::new("effect-active").unwrap(),
                snapshot: snapshot(),
            },
        ]))
        .unwrap();
    assert!(active.checkpoint_bytes().is_err());

    let turn = TurnId::new("turn-complete").unwrap();
    let mut complete = ContextFold::with_limits(
        header("system"),
        limits,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    complete
        .apply(&facts(vec![
            SessionFactBody::TurnAccepted {
                reasoning_effort: None,
                turn_id: turn.clone(),
                text: "complete".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
            SessionFactBody::TurnTerminal {
                turn_id: turn,
                outcome: TurnOutcome::Completed,
                result: None,
            },
        ]))
        .unwrap();
    let bytes = complete.checkpoint_bytes().unwrap();
    assert!(
        ContextFold::from_checkpoint(
            header("different"),
            limits,
            &bytes,
            rsi_agent_context::ContextBudget::default()
        )
        .is_err()
    );
    assert!(
        ContextFold::from_checkpoint(
            header("system"),
            ContextLimits::new(limits.max_messages - 1, limits.max_bytes).unwrap(),
            &bytes,
            rsi_agent_context::ContextBudget::default()
        )
        .is_err()
    );
    let mut corrupt = bytes.to_vec();
    corrupt.truncate(corrupt.len() - 1);
    assert!(
        ContextFold::from_checkpoint(
            header("system"),
            limits,
            &corrupt,
            rsi_agent_context::ContextBudget::default()
        )
        .is_err()
    );

    let mut injected = bytes.to_vec();
    let original = b"complete";
    let replacement = b"injected";
    let offset = injected
        .windows(original.len())
        .position(|window| window == original)
        .expect("checkpoint contains the projected user message");
    injected[offset..offset + replacement.len()].copy_from_slice(replacement);
    assert!(
        ContextFold::from_checkpoint(
            header("system"),
            limits,
            &injected,
            rsi_agent_context::ContextBudget::default()
        )
        .is_err(),
        "a structurally valid same-length checkpoint mutation must be rejected"
    );
}

#[test]
fn checkpoint_rejects_a_claim_filtered_sequence_hole() {
    let limits = ContextLimits::default();
    let turn = TurnId::new("turn-visible").unwrap();
    let visible = vec![
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                reasoning_effort: None,
                turn_id: turn.clone(),
                text: "visible".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
        SessionFact::new(
            3,
            3,
            SessionFactBody::TurnTerminal {
                turn_id: turn,
                outcome: TurnOutcome::Completed,
                result: None,
            },
        )
        .unwrap(),
    ];
    let mut fold = ContextFold::with_limits(
        header("system"),
        limits,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    fold.apply_page(&visible, 3).unwrap();
    assert!(fold.checkpoint_bytes().is_err());
}

#[test]
fn checkpoint_round_trip_preserves_accepted_queued_turn_state() {
    let first = TurnId::new("turn-first").unwrap();
    let queued = TurnId::new("turn-queued").unwrap();
    let limits = ContextLimits::default();
    let mut fold = ContextFold::with_limits(
        header("system"),
        limits,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: first.clone(),
            text: "first".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: first,
            outcome: TurnOutcome::Completed,
            result: None,
        },
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: queued.clone(),
            text: "queued".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    ]))
    .unwrap();

    let bytes = fold.checkpoint_bytes().unwrap();
    let mut restored = ContextFold::from_checkpoint(
        header("system"),
        limits,
        &bytes,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    assert_eq!(
        restored.project(limits).unwrap(),
        fold.project(limits).unwrap()
    );
    restored
        .apply(&facts_after(
            3,
            vec![SessionFactBody::TurnTerminal {
                turn_id: queued,
                outcome: TurnOutcome::Completed,
                result: None,
            }],
        ))
        .unwrap();
}

#[test]
fn active_oldest_turn_cannot_bypass_absolute_message_retention() {
    for limited in [false, true] {
        let mut fold = if limited {
            ContextFold::with_limits(
                header(""),
                ContextLimits::new(8, 1024).unwrap(),
                rsi_agent_context::ContextBudget::default(),
            )
        } else {
            ContextFold::new(header(""), rsi_agent_context::ContextBudget::default())
        }
        .unwrap();
        for seq in 1..=rsi_agent_context::MAXIMUM_CONTEXT_MESSAGES as u64 {
            let fact = SessionFact::new(
                seq,
                seq,
                SessionFactBody::TurnAccepted {
                    turn_id: TurnId::new(format!("retained-{seq}")).unwrap(),
                    text: "still active".into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap();
            fold.apply(&[fact]).unwrap();
        }
        let seq = rsi_agent_context::MAXIMUM_CONTEXT_MESSAGES as u64 + 1;
        let fact = SessionFact::new(
            seq,
            seq,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("over-limit").unwrap(),
                text: "overflow".into(),
                model: None,
                reasoning_effort: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap();
        assert_eq!(
            fold.project(
                ContextLimits::new(
                    rsi_agent_context::MAXIMUM_CONTEXT_MESSAGES,
                    rsi_agent_context::MAXIMUM_CONTEXT_BYTES
                )
                .unwrap()
            )
            .unwrap()
            .messages()
            .len(),
            rsi_agent_context::MAXIMUM_CONTEXT_MESSAGES
        );
        assert!(matches!(
            fold.apply(std::slice::from_ref(&fact)),
            Err(ContextError::TooLarge)
        ));
        assert_eq!(fold.through_seq(), seq - 1);
        assert!(matches!(fold.apply(&[fact]), Err(ContextError::Invalid(_))));
        assert!(matches!(
            fold.project(ContextLimits::default()),
            Err(ContextError::Invalid(_))
        ));
    }
}

#[test]
fn active_turn_bytes_fail_before_retaining_the_overflowing_message() {
    let mut fold = ContextFold::with_limits(
        header(""),
        ContextLimits::new(8, 1024).unwrap(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    let mut rejected = false;
    for seq in 1..=128 {
        let fact = SessionFact::new(
            seq,
            seq,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new(format!("bytes-{seq}")).unwrap(),
                text: "x".repeat(512 * 1024),
                model: None,
                reasoning_effort: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap();
        match fold.apply(std::slice::from_ref(&fact)) {
            Ok(()) => {}
            Err(ContextError::TooLarge) => {
                assert_eq!(fold.through_seq(), seq - 1);
                assert!(matches!(fold.apply(&[fact]), Err(ContextError::Invalid(_))));
                rejected = true;
                break;
            }
            Err(error) => panic!("unexpected error: {error}"),
        }
    }
    assert!(rejected);
}

#[test]
fn request_materialization_uses_one_projection_admission() {
    let budget = rsi_agent_context::ContextBudget::default();
    let mut fold = ContextFold::new(header("system"), budget.clone()).unwrap();
    fold.apply(&facts(vec![SessionFactBody::TurnAccepted {
        reasoning_effort: None,
        turn_id: TurnId::new("projection-credit").unwrap(),
        text: "large input ".repeat(1024),
        model: None,
        sandbox: SandboxMode::WorkspaceWrite,
        require_approval: false,
    }]))
    .unwrap();
    let state_bytes = budget.used();
    let options = rsi_ai_protocol::LanguageRequestOptions::default();
    let spare = 4 * state_bytes + options.encoded_weight();
    let pressure = budget
        .reserve(budget.maximum() - state_bytes - spare)
        .unwrap();
    let request = fold.request(ContextLimits::default(), options).unwrap();
    assert!(
        request
            .messages()
            .iter()
            .any(|message| message.role() == MessageRole::User)
    );
    drop(request);
    drop(pressure);
    drop(fold);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_fact_releases_workspace_and_fences_all_projection_entry_points() {
    for path in 0..3 {
        let budget = rsi_agent_context::ContextBudget::new(1024 * 1024).unwrap();
        let mut fold = ContextFold::with_limits(
            if path == 2 {
                fork_header("system", 2, 1)
            } else {
                header("system")
            },
            ContextLimits::default(),
            budget.clone(),
        )
        .unwrap();
        let page = facts(vec![
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("bad-fact").unwrap(),
                text: "x".repeat(4096),
                model: None,
                reasoning_effort: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
            SessionFactBody::ModelEvent {
                turn_id: TurnId::new("bad-fact").unwrap(),
                effect_id: EffectId::new("missing-intent").unwrap(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event: LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            },
        ]);
        let apply = |fold: &mut ContextFold, input: &[SessionFact]| match path {
            0 => fold.apply(input),
            1 => fold.apply_page(input, input.last().unwrap().seq()),
            _ => fold.apply_seed_page(input),
        };
        apply(&mut fold, &page[..1]).unwrap();
        let prefix_seq = fold.through_seq();
        let prefix_digest = fold.fact_prefix_sha256();
        assert!(matches!(
            apply(&mut fold, &page[1..]),
            Err(ContextError::Invalid(_))
        ));
        assert_eq!(fold.through_seq(), prefix_seq);
        assert_eq!(fold.fact_prefix_sha256(), prefix_digest);
        let after_failure = budget.used();
        assert_eq!(
            after_failure,
            serde_json::to_vec(fold.header()).unwrap().len()
        );
        for _ in 0..8 {
            assert!(matches!(
                apply(&mut fold, &page[1..]),
                Err(ContextError::Invalid(_))
            ));
            assert_eq!(budget.used(), after_failure);
        }
        assert!(matches!(
            fold.project(ContextLimits::default()),
            Err(ContextError::Invalid(_))
        ));
        assert!(matches!(
            fold.request(
                ContextLimits::default(),
                rsi_ai_protocol::LanguageRequestOptions::default()
            ),
            Err(ContextError::Invalid(_))
        ));
        assert!(matches!(
            fold.checkpoint_bytes(),
            Err(ContextError::Invalid(_))
        ));
        drop(fold);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn immutable_fact_weight_preserves_the_exact_ingestion_admission_boundary() {
    let budget = rsi_agent_context::ContextBudget::new(64 * 1024).unwrap();
    let mut fold = ContextFold::new(header(""), budget.clone()).unwrap();
    let input = facts(vec![SessionFactBody::TurnAccepted {
        turn_id: TurnId::new("cached-fact-weight").unwrap(),
        text: "quote\" slash\\ line\n中文".repeat(32),
        model: None,
        reasoning_effort: None,
        sandbox: SandboxMode::WorkspaceWrite,
        require_approval: false,
    }]);
    let state = budget.used();
    let encoded = serde_json::to_vec(&input[0]).unwrap().len();
    assert_eq!(input[0].encoded_len(), encoded);
    let required = 3 * state + 8 * encoded;
    let mut pressure = budget.reserve(budget.maximum() - required + 1).unwrap();
    assert!(matches!(fold.apply(&input), Err(ContextError::Capacity)));
    assert_eq!(fold.through_seq(), 0);
    assert_eq!(budget.used(), state + pressure.bytes());
    pressure.resize(pressure.bytes() - 1).unwrap();
    fold.apply(&input).unwrap();
    assert_eq!(fold.through_seq(), 1);
    drop(fold);
    drop(pressure);
    assert_eq!(budget.used(), 0);
}

#[test]
fn small_shared_pool_requires_projection_headroom_above_retained_state() {
    let budget = rsi_agent_context::ContextBudget::new(512 * 1024).unwrap();
    let mut fold = ContextFold::new(header("system"), budget.clone()).unwrap();
    fold.apply(&facts(vec![SessionFactBody::TurnAccepted {
        turn_id: TurnId::new("small-pool").unwrap(),
        text: "x".repeat(32 * 1024),
        model: None,
        reasoning_effort: None,
        sandbox: SandboxMode::WorkspaceWrite,
        require_approval: false,
    }]))
    .unwrap();
    let state = budget.used();
    let options = rsi_ai_protocol::LanguageRequestOptions::default();
    let required = 4 * state + options.encoded_weight();
    println!(
        "shared pool: capacity={} retained={state} projection_admission={required} isolated_peak={}",
        budget.maximum(),
        state + required
    );
    let mut pressure = budget
        .reserve(budget.maximum() - state - required + 1)
        .unwrap();
    assert!(matches!(
        fold.request(ContextLimits::default(), options.clone()),
        Err(ContextError::Capacity)
    ));
    assert_eq!(budget.used(), state + pressure.bytes());
    pressure.resize(pressure.bytes() - 1).unwrap();
    let request = fold.request(ContextLimits::default(), options).unwrap();
    drop(request);
    drop(pressure);
    assert_eq!(budget.used(), state);
    drop(fold);
    assert_eq!(budget.used(), 0);
}

#[test]
fn workspace_refusal_preserves_exact_prefix_before_a_hidden_page_hole() {
    let budget = rsi_agent_context::ContextBudget::new(64 * 1024).unwrap();
    let mut fold =
        ContextFold::with_limits(header(""), ContextLimits::default(), budget.clone()).unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("completed").unwrap(),
            text: "input".into(),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("completed").unwrap(),
            outcome: TurnOutcome::Completed,
            result: None,
        },
    ]))
    .unwrap();
    let before = fold.checkpoint_bytes().unwrap();
    let bytes = before.as_bytes().to_vec();
    drop(before);
    let used = budget.used();
    let next = SessionFact::new(
        4,
        4,
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("not-admitted").unwrap(),
            text: "x".repeat(32 * 1024),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap();
    assert!(matches!(
        fold.apply_page(&[next], 4),
        Err(ContextError::Capacity)
    ));
    assert_eq!(fold.through_seq(), 2);
    assert_eq!(budget.used(), used);
    let after = fold.checkpoint_bytes().unwrap();
    assert_eq!(after.as_bytes(), bytes);
}

#[test]
fn shared_parked_folds_release_pressure_without_process_restart_or_cursor_rebuild() {
    let budget = rsi_agent_context::ContextBudget::new(512 * 1024).unwrap();
    let mut folds = Vec::new();
    for _ in 0..6 {
        let mut fold = ContextFold::new(header("system"), budget.clone()).unwrap();
        fold.apply(&facts(vec![SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("parked").unwrap(),
            text: "x".repeat(32 * 1024),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        }]))
        .unwrap();
        folds.push(fold);
    }
    let before = budget.used();
    let next = SessionFact::new(
        2,
        2,
        SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("parked").unwrap(),
            outcome: TurnOutcome::Completed,
            result: None,
        },
    )
    .unwrap();
    let headroom = 2 * (before / folds.len()) + 8 * next.encoded_len();
    let pressure = budget
        .reserve(budget.maximum() - before - headroom + 1)
        .unwrap();
    assert!(matches!(
        folds[0].apply(std::slice::from_ref(&next)),
        Err(ContextError::Capacity)
    ));
    assert_eq!(folds[0].through_seq(), 1);
    assert_eq!(budget.used(), before + pressure.bytes());
    drop(folds.pop().unwrap());
    folds[0].apply(&[next]).unwrap();
    assert_eq!(folds[0].through_seq(), 2);
    drop(folds);
    drop(pressure);
    assert_eq!(budget.used(), 0);
}
