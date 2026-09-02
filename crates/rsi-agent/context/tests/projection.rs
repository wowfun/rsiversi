use rsi_agent_context::{ContextError, ContextFold, ContextLimits};
use rsi_agent_session_protocol::{
    AgentPresetId, EffectId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader,
    SessionId, TurnId, TurnOutcome,
};
use rsi_ai_protocol::{
    AiCapability, ContentDelta, ContentStart, FinishReason, LanguageEvent, MessageContent,
    MessageRole, ModelRef, PreparedCallSnapshot, RetryPolicy, ToolCallKind,
};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
use serde_json::json;

fn header(system: &str) -> SessionHeader {
    SessionHeader::new(
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

fn snapshot() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
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

#[test]
fn tool_call_and_result_remain_adjacent_and_workspace_is_not_implicit() {
    let turn = TurnId::new("turn-1").unwrap();
    let effect = EffectId::new("model-1").unwrap();
    let tool_identity =
        ToolResultIdentity::new("tool-f1-g1-r1", "effect-1", "tool-call", "b".repeat(64)).unwrap();
    let mut fold = ContextFold::new(header("")).unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: turn.clone(),
            text: "use a tool".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::ModelIntent {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            snapshot: snapshot(),
        },
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
        },
        SessionFactBody::ModelEvent {
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
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::ToolArguments("{}".into()),
            },
        },
        SessionFactBody::ModelEvent {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentFinished { index: 0 },
        },
        SessionFactBody::ModelEvent {
            turn_id: turn.clone(),
            effect_id: effect,
            event: LanguageEvent::Finished {
                reason: FinishReason::ToolCalls,
                replay: None,
            },
        },
        SessionFactBody::ToolResult {
            turn_id: turn,
            effect_id: EffectId::new("tool-1").unwrap(),
            identity: tool_identity,
            result: ToolResult::new(json!({"answer": 42}), vec![], false).unwrap(),
        },
    ]))
    .unwrap();
    let projected = fold.project(ContextLimits::default()).unwrap();
    assert_eq!(
        projected
            .messages
            .iter()
            .map(rsi_ai_protocol::Message::role)
            .collect::<Vec<_>>(),
        vec![MessageRole::User, MessageRole::Assistant, MessageRole::Tool]
    );
    assert!(matches!(
        projected.messages[1].content(),
        [MessageContent::ToolCall(_)]
    ));
    assert!(matches!(
        projected.messages[2].content(),
        [MessageContent::ToolResult { call_id, .. }] if call_id == "tool-call"
    ));
    let encoded = serde_json::to_string(&projected.messages).unwrap();
    assert!(!encoded.contains("secret/workspace-name"));
}

#[test]
fn compaction_drops_only_a_complete_oldest_turn_and_inserts_one_notice() {
    let old = TurnId::new("turn-old").unwrap();
    let current = TurnId::new("turn-current").unwrap();
    let mut fold = ContextFold::new(header("system")).unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: old.clone(),
            text: "old".repeat(200),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: old,
            outcome: TurnOutcome::Completed,
        },
        SessionFactBody::TurnAccepted {
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
    assert_eq!(projected.messages.len(), 3);
    let encoded = serde_json::to_string(&projected.messages).unwrap();
    assert!(encoded.contains("omitted 1 complete earlier turn"));
    assert!(encoded.contains("current"));
    assert!(!encoded.contains(&"old".repeat(50)));
}

#[test]
fn projection_uses_the_exact_canonical_json_byte_boundary() {
    let old = TurnId::new("turn-old").unwrap();
    let current = TurnId::new("turn-current").unwrap();
    let mut fold = ContextFold::new(header("system")).unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: old.clone(),
            text: "old".repeat(200),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: old,
            outcome: TurnOutcome::Completed,
        },
        SessionFactBody::TurnAccepted {
            turn_id: current,
            text: "current".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    ]))
    .unwrap();
    let complete = fold.project(ContextLimits::default()).unwrap();
    let exact = serde_json::to_vec(&complete.messages).unwrap().len();

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
    let mut fold = ContextFold::new(header("")).unwrap();
    fold.apply(&facts(vec![SessionFactBody::TurnAccepted {
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
    let mut fold = ContextFold::new(header("")).unwrap();
    let first = facts(vec![SessionFactBody::TurnAccepted {
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
    let mut fold = ContextFold::with_limits(header("system"), limits).unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: old.clone(),
            text: "old user message".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: old,
            outcome: TurnOutcome::Completed,
        },
    ]))
    .unwrap();
    let expected = fold.project(limits).unwrap();
    let bytes = fold.checkpoint_bytes().unwrap();
    let mut restored = ContextFold::from_checkpoint(header("system"), limits, &bytes).unwrap();
    assert_eq!(restored.project(limits).unwrap(), expected);

    restored
        .apply(&facts_after(
            2,
            vec![SessionFactBody::TurnAccepted {
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
        serde_json::to_string(&projected.messages)
            .unwrap()
            .contains("suffix only")
    );
}

#[test]
fn checkpoint_rejects_active_assembler_corruption_and_identity_mismatch() {
    let limits = ContextLimits::default();
    let mut active = ContextFold::with_limits(header("system"), limits).unwrap();
    let active_turn = TurnId::new("turn-active").unwrap();
    active
        .apply(&facts(vec![
            SessionFactBody::TurnAccepted {
                turn_id: active_turn.clone(),
                text: "active".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
            SessionFactBody::ModelIntent {
                turn_id: active_turn,
                effect_id: EffectId::new("effect-active").unwrap(),
                snapshot: snapshot(),
            },
        ]))
        .unwrap();
    assert!(active.checkpoint_bytes().is_err());

    let turn = TurnId::new("turn-complete").unwrap();
    let mut complete = ContextFold::with_limits(header("system"), limits).unwrap();
    complete
        .apply(&facts(vec![
            SessionFactBody::TurnAccepted {
                turn_id: turn.clone(),
                text: "complete".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
            SessionFactBody::TurnTerminal {
                turn_id: turn,
                outcome: TurnOutcome::Completed,
            },
        ]))
        .unwrap();
    let bytes = complete.checkpoint_bytes().unwrap();
    assert!(ContextFold::from_checkpoint(header("different"), limits, &bytes).is_err());
    assert!(
        ContextFold::from_checkpoint(
            header("system"),
            ContextLimits::new(limits.max_messages - 1, limits.max_bytes).unwrap(),
            &bytes,
        )
        .is_err()
    );
    let mut corrupt = bytes.to_vec();
    corrupt.truncate(corrupt.len() - 1);
    assert!(ContextFold::from_checkpoint(header("system"), limits, &corrupt).is_err());

    let mut injected = bytes.to_vec();
    let original = b"complete";
    let replacement = b"injected";
    let offset = injected
        .windows(original.len())
        .position(|window| window == original)
        .expect("checkpoint contains the projected user message");
    injected[offset..offset + replacement.len()].copy_from_slice(replacement);
    assert!(
        ContextFold::from_checkpoint(header("system"), limits, &injected).is_err(),
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
            },
        )
        .unwrap(),
    ];
    let mut fold = ContextFold::with_limits(header("system"), limits).unwrap();
    fold.apply_page(&visible, 3).unwrap();
    assert!(fold.checkpoint_bytes().is_err());
}

#[test]
fn checkpoint_round_trip_preserves_accepted_queued_turn_state() {
    let first = TurnId::new("turn-first").unwrap();
    let queued = TurnId::new("turn-queued").unwrap();
    let limits = ContextLimits::default();
    let mut fold = ContextFold::with_limits(header("system"), limits).unwrap();
    fold.apply(&facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: first.clone(),
            text: "first".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::TurnTerminal {
            turn_id: first,
            outcome: TurnOutcome::Completed,
        },
        SessionFactBody::TurnAccepted {
            turn_id: queued.clone(),
            text: "queued".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    ]))
    .unwrap();

    let bytes = fold.checkpoint_bytes().unwrap();
    let mut restored = ContextFold::from_checkpoint(header("system"), limits, &bytes).unwrap();
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
            }],
        ))
        .unwrap();
}
