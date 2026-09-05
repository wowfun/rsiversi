use rsi_agent_session_protocol::*;
use rsi_ai_protocol::{AiCapability, ImageRequest, ModelRef, PreparedCallSnapshot, RetryPolicy};
use rsi_media_protocol::{MediaId, MediaRef};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::{ToolContent, ToolResult, ToolResultIdentity};
use serde_json::json;

fn settings() -> FrozenAgentSettings {
    FrozenAgentSettings::new(
        "default",
        "Be precise.",
        ModelRef::new("openai", "gpt-test").unwrap(),
        SandboxMode::WorkspaceWrite,
        true,
    )
    .unwrap()
}

fn snapshot(capability: AiCapability) -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        call_id: "call-1".into(),
        deployment_id: "openai".into(),
        provider_family: "openai".into(),
        capability,
        model: "gpt-test".into(),
        protocol: "responses".into(),
        transport: "https".into(),
        endpoint_fingerprint: "endpoint-1".into(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "a".repeat(64),
    }
}

#[test]
fn agent_preset_id_round_trips_only_the_safe_directory_grammar() {
    let preset = AgentPresetId::new("code-agent-2").unwrap();
    assert_eq!(preset.as_str(), "code-agent-2");
    assert_eq!(
        serde_json::from_value::<AgentPresetId>(serde_json::to_value(&preset).unwrap()).unwrap(),
        preset
    );
    assert!(AgentPresetId::new("a".repeat(MAXIMUM_AGENT_PRESET_ID_BYTES)).is_ok());

    for invalid in [
        "",
        "-leading",
        "Upper",
        "dot.id",
        "under_score",
        "path/name",
    ] {
        assert!(AgentPresetId::new(invalid).is_err(), "accepted {invalid:?}");
    }
    assert!(AgentPresetId::new("a".repeat(MAXIMUM_AGENT_PRESET_ID_BYTES + 1)).is_err());
    assert!(serde_json::from_str::<AgentPresetId>(r#""../escape""#).is_err());
}

#[test]
fn header_round_trips_and_rejects_old_format_or_noncanonical_path() {
    let header = SessionHeader::new(
        SessionId::new("session-1").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("code-agent").unwrap(),
        settings(),
    )
    .unwrap();
    assert_eq!(header.agent_preset_id().as_str(), "code-agent");
    let bytes = serde_json::to_vec(&header).unwrap();
    assert_eq!(
        serde_json::from_slice::<SessionHeader>(&bytes).unwrap(),
        header
    );

    let mut value = serde_json::to_value(&header).unwrap();
    value["format_version"] = 2.into();
    value.as_object_mut().unwrap().remove("agent_preset_id");
    value["settings"]
        .as_object_mut()
        .unwrap()
        .remove("turn_budget");
    assert_eq!(
        serde_json::from_value::<SessionHeader>(value)
            .unwrap_err()
            .to_string(),
        "unsupported session format version 2"
    );

    let mut missing_preset = serde_json::to_value(&header).unwrap();
    missing_preset
        .as_object_mut()
        .unwrap()
        .remove("agent_preset_id");
    assert!(serde_json::from_value::<SessionHeader>(missing_preset).is_err());

    let mut missing_budget = serde_json::to_value(&header).unwrap();
    missing_budget["settings"]
        .as_object_mut()
        .unwrap()
        .remove("turn_budget");
    assert!(
        serde_json::from_value::<SessionHeader>(missing_budget).is_err(),
        "the current durable format must not widen an omitted frozen budget"
    );
    assert!(
        SessionHeader::new(
            SessionId::new("session-1").unwrap(),
            1,
            "relative/path",
            AgentPresetId::new("code-agent").unwrap(),
            settings()
        )
        .is_err()
    );
}

#[test]
fn turn_budget_accepts_each_hard_limit_and_rejects_limit_plus_one() {
    let maximum = TurnBudget::default();
    assert_eq!(maximum.maximum_elapsed_ms(), 1_800_000);
    assert_eq!(maximum.maximum_provider_attempts(), 64);
    assert_eq!(maximum.maximum_tool_calls(), 256);
    assert_eq!(maximum.maximum_generated_facts(), 65_536);
    assert_eq!(maximum.maximum_generated_fact_bytes(), 67_108_864);

    for invalid in [
        json!({
            "maximum_elapsed_ms": 1_800_001,
            "maximum_provider_attempts": 64,
            "maximum_tool_calls": 256,
            "maximum_generated_facts": 65_536,
            "maximum_generated_fact_bytes": 67_108_864
        }),
        json!({
            "maximum_elapsed_ms": 1_800_000,
            "maximum_provider_attempts": 65,
            "maximum_tool_calls": 256,
            "maximum_generated_facts": 65_536,
            "maximum_generated_fact_bytes": 67_108_864
        }),
        json!({
            "maximum_elapsed_ms": 1_800_000,
            "maximum_provider_attempts": 64,
            "maximum_tool_calls": 257,
            "maximum_generated_facts": 65_536,
            "maximum_generated_fact_bytes": 67_108_864
        }),
        json!({
            "maximum_elapsed_ms": 1_800_000,
            "maximum_provider_attempts": 64,
            "maximum_tool_calls": 256,
            "maximum_generated_facts": 65_537,
            "maximum_generated_fact_bytes": 67_108_864
        }),
        json!({
            "maximum_elapsed_ms": 1_800_000,
            "maximum_provider_attempts": 64,
            "maximum_tool_calls": 256,
            "maximum_generated_facts": 65_536,
            "maximum_generated_fact_bytes": 67_108_865
        }),
    ] {
        assert!(serde_json::from_value::<TurnBudget>(invalid).is_err());
    }
}

#[test]
fn exhaustion_records_cannot_widen_the_named_budget_dimension() {
    let turn_id = TurnId::new("turn-budget-bound").unwrap();
    for (dimension, maximum) in [
        (BudgetDimension::Elapsed, MAXIMUM_TURN_ELAPSED_MS),
        (
            BudgetDimension::ProviderAttempts,
            MAXIMUM_TURN_PROVIDER_ATTEMPTS,
        ),
        (BudgetDimension::ToolCalls, MAXIMUM_TURN_TOOL_CALLS),
        (
            BudgetDimension::GeneratedFacts,
            MAXIMUM_TURN_GENERATED_FACTS,
        ),
        (
            BudgetDimension::GeneratedFactBytes,
            MAXIMUM_TURN_GENERATED_FACT_BYTES,
        ),
    ] {
        let widened = maximum + 1;
        assert!(
            TurnOutcome::BudgetExceeded {
                dimension,
                consumed: widened,
                limit: widened,
            }
            .validate()
            .is_err()
        );
        assert!(
            SessionFactBody::BudgetExhausted {
                turn_id: turn_id.clone(),
                dimension,
                consumed: widened,
                limit: widened,
            }
            .validate()
            .is_err()
        );
    }
}

#[test]
fn frozen_settings_serialize_their_creation_time_turn_budget() {
    let tightened = TurnBudget::new(60_000, 3, 4, 5, 6_000).unwrap();
    let settings = FrozenAgentSettings::new_with_budget(
        "bounded",
        "Be precise.",
        ModelRef::new("openai", "gpt-test").unwrap(),
        SandboxMode::WorkspaceWrite,
        false,
        tightened.clone(),
    )
    .unwrap();

    assert_eq!(settings.turn_budget(), &tightened);
    assert_eq!(
        serde_json::from_value::<FrozenAgentSettings>(serde_json::to_value(&settings).unwrap())
            .unwrap()
            .turn_budget(),
        &tightened
    );
}

#[test]
fn maximum_escaped_header_stays_inside_its_framing_bound() {
    let settings = FrozenAgentSettings::new(
        "p".repeat(MAXIMUM_AGENT_IDENTIFIER_BYTES),
        "\u{1}".repeat(MAXIMUM_SYSTEM_PROMPT_BYTES),
        ModelRef::new(
            "d".repeat(rsi_ai_protocol::MAX_ID_BYTES),
            "m".repeat(rsi_ai_protocol::MAX_ID_BYTES),
        )
        .unwrap(),
        SandboxMode::WorkspaceWrite,
        false,
    )
    .unwrap();
    let header = SessionHeader::new(
        SessionId::new("s".repeat(MAXIMUM_AGENT_IDENTIFIER_BYTES)).unwrap(),
        1,
        format!("/{}", "\u{1}".repeat(MAXIMUM_WORKSPACE_PATH_BYTES - 1)),
        AgentPresetId::new("a".repeat(MAXIMUM_AGENT_PRESET_ID_BYTES)).unwrap(),
        settings,
    )
    .unwrap();

    assert!(serde_json::to_vec(&header).unwrap().len() <= MAXIMUM_SESSION_HEADER_BYTES);
}

#[test]
fn settings_fingerprint_is_canonical_and_secrets_have_no_field() {
    let left = settings();
    let round_trip: FrozenAgentSettings =
        serde_json::from_value(serde_json::to_value(&left).unwrap()).unwrap();
    assert_eq!(
        left.fingerprint().unwrap(),
        round_trip.fingerprint().unwrap()
    );
    let value = serde_json::to_value(left).unwrap();
    assert!(value.get("credential").is_none());
    assert!(value.get("secret").is_none());
}

#[test]
fn unconfined_settings_require_live_approval() {
    let model = ModelRef::new("openai", "gpt-test").unwrap();
    assert!(
        FrozenAgentSettings::new(
            "unsafe",
            "Be precise.",
            model.clone(),
            SandboxMode::DangerFullAccess,
            false,
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<FrozenAgentSettings>(json!({
            "settings_id": "unsafe",
            "system_prompt": "Be precise.",
            "default_model": { "deployment": "openai", "model": "gpt-test" },
            "sandbox": "danger-full-access",
            "require_approval": false
        }))
        .is_err()
    );
    assert!(
        FrozenAgentSettings::new(
            "reviewed",
            "Be precise.",
            model,
            SandboxMode::DangerFullAccess,
            true,
        )
        .is_ok()
    );
}

#[test]
fn decoded_identifiers_and_nested_model_snapshot_cannot_bypass_validation() {
    assert!(serde_json::from_str::<SessionId>(r#""bad id""#).is_err());
    let body = SessionFactBody::ModelIntent {
        turn_id: TurnId::new("turn-1").unwrap(),
        effect_id: EffectId::new("effect-1").unwrap(),
        snapshot: snapshot(AiCapability::Image),
    };
    assert!(body.validate().is_err());

    let mut value = serde_json::to_value(
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("turn-1").unwrap(),
                text: "hello".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
    )
    .unwrap();
    value["turn_id"] = "bad id".into();
    assert!(serde_json::from_value::<SessionFact>(value).is_err());
}

#[test]
fn image_request_intent_outputs_and_partial_failure_are_bounded_refs_only() {
    let turn_id = TurnId::new("turn-image").unwrap();
    let effect_id = EffectId::new("effect-image").unwrap();
    let media = MediaRef {
        id: MediaId::new("d".repeat(64)).unwrap(),
        mime: "image/png".into(),
        bytes: 4,
        width: 1,
        height: 1,
    };
    let bodies = [
        SessionFactBody::ImageRequested {
            turn_id: turn_id.clone(),
            model: ModelRef::new("openai", "image-test").unwrap(),
            request: ImageRequest::new("draw a square", 2).unwrap(),
        },
        SessionFactBody::ImageIntent {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
            snapshot: snapshot(AiCapability::Image),
        },
        SessionFactBody::ImageStarted {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
        },
        SessionFactBody::ImageOutput {
            turn_id: turn_id.clone(),
            effect_id,
            index: 0,
            media: media.clone(),
        },
        SessionFactBody::TurnTerminal {
            turn_id,
            outcome: TurnOutcome::PartialFailed {
                media: vec![media],
                code: "provider.output_validation".into(),
                message: "second image failed".into(),
            },
        },
    ];
    let facts = bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| SessionFact::new(index as u64 + 1, 1, body).unwrap())
        .collect::<Vec<_>>();
    let encoded = serde_json::to_vec(&facts).unwrap();
    let decoded: Vec<SessionFact> = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, facts);
    assert!(!String::from_utf8(encoded).unwrap().contains("image bytes"));

    let wrong = SessionFactBody::ImageIntent {
        turn_id: TurnId::new("turn-image").unwrap(),
        effect_id: EffectId::new("effect-image").unwrap(),
        snapshot: snapshot(AiCapability::Language),
    };
    assert!(wrong.validate().is_err());
    assert!(
        TurnOutcome::PartialFailed {
            media: vec![],
            code: "provider.failed".into(),
            message: "failed".into(),
        }
        .validate()
        .is_err()
    );
}

#[test]
fn tool_result_fact_persists_media_reference_without_media_bytes() {
    let identity =
        ToolResultIdentity::new("tool-f1-g1-r1", "effect-1", "call-1", "b".repeat(64)).unwrap();
    let media = MediaRef {
        id: MediaId::new("c".repeat(64)).unwrap(),
        mime: "image/png".into(),
        bytes: 123,
        width: 2,
        height: 3,
    };
    let result = ToolResult::new(
        json!({"ok": true}),
        vec![
            ToolContent::Text {
                text: "done".into(),
            },
            ToolContent::Image {
                media: media.clone(),
            },
        ],
        false,
    )
    .unwrap();
    let fact = SessionFact::new(
        1,
        1,
        SessionFactBody::ToolResult {
            turn_id: TurnId::new("turn-1").unwrap(),
            effect_id: EffectId::new("effect-1").unwrap(),
            identity,
            result,
        },
    )
    .unwrap();
    let bytes = serde_json::to_vec(&fact).unwrap();
    assert_eq!(fact.encoded_len(), bytes.len());
    let decoded: SessionFact = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded, fact);
    assert_eq!(decoded.encoded_len(), bytes.len());
    assert!(!String::from_utf8(bytes).unwrap().contains("data"));
    assert_eq!(media.id.as_str(), "c".repeat(64));
}

#[test]
fn fact_pages_are_exactly_contiguous_and_bounded() {
    let fact = |seq| {
        SessionFact::new(
            seq,
            seq,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new(format!("turn-{seq}")).unwrap(),
                text: "hello".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap()
    };
    validate_fact_sequence(0, &[fact(1), fact(2)]).unwrap();
    assert!(validate_fact_sequence(0, &[fact(2)]).is_err());
    assert!(validate_fact_sequence(1, &[fact(2), fact(4)]).is_err());
}

#[test]
fn terminal_diagnostics_are_finite_and_unknown_fields_are_rejected() {
    assert!(
        TurnOutcome::Failed {
            code: "provider".into(),
            message: "x".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES + 1),
        }
        .validate()
        .is_err()
    );
    let value = json!({
        "seq": 1,
        "timestamp_ms": 1,
        "type": "turn_terminal",
        "turn_id": "turn-1",
        "outcome": {"status": "completed"},
        "unexpected": true
    });
    assert!(serde_json::from_value::<SessionFact>(value).is_err());
}

#[test]
fn entered_agent_and_completion_messages_keep_the_agent_text_bound() {
    let turn_id = TurnId::new("turn-entered-bound").unwrap();
    let step_id = StepId::new("step-entered-bound").unwrap();
    let message_id = MessageId::new("message-entered-bound").unwrap();
    let oversized = vec![AgentMessageContent::Text {
        text: "x".repeat(MAXIMUM_AGENT_MESSAGE_BYTES + 1),
    }];
    let body = |source| SessionFactBody::InputMessageEntered {
        turn_id: turn_id.clone(),
        step_id: step_id.clone(),
        source,
        content: oversized.clone(),
    };

    assert!(matches!(
        body(InputMessageSource::Agent {
            message_id: message_id.clone(),
            source_session_id: SessionId::new("source-agent").unwrap(),
        })
        .validate(),
        Err(SessionError::Invalid(message))
            if message.contains(&MAXIMUM_AGENT_MESSAGE_BYTES.to_string())
    ));
    assert!(matches!(
        body(InputMessageSource::Completion {
            message_id: message_id.clone(),
            child_session_id: SessionId::new("source-child").unwrap(),
            activation_id: ActivationId::new("source-activation").unwrap(),
        })
        .validate(),
        Err(SessionError::Invalid(message))
            if message.contains(&MAXIMUM_AGENT_MESSAGE_BYTES.to_string())
    ));
    body(InputMessageSource::Human { message_id })
        .validate()
        .expect("the Human input bound remains the larger direct-turn bound");
}

#[test]
fn fork_selection_and_lineage_are_exact_and_tamper_evident() {
    assert_eq!(
        ForkTurnSelection::parse("none").unwrap(),
        ForkTurnSelection::None
    );
    assert_eq!(
        ForkTurnSelection::parse("all").unwrap(),
        ForkTurnSelection::All
    );
    assert_eq!(
        ForkTurnSelection::parse("18446744073709551615").unwrap(),
        ForkTurnSelection::Last(u64::MAX)
    );
    for invalid in ["", "0", "01x", "18446744073709551616"] {
        assert!(
            ForkTurnSelection::parse(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
    assert!(ForkTurnSelection::Last(0).validate().is_err());

    ForkOrigin {
        parent_session_id: SessionId::new("session-parent").unwrap(),
        root_session_id: SessionId::new("session-root").unwrap(),
        path: AgentPath::new(vec![1]).unwrap(),
        task_name: "first-turn-child".into(),
        parent_header_fingerprint: "a".repeat(64),
        invoking_turn_id: TurnId::new("turn-first").unwrap(),
        resolved_after_seq: 0,
        resolved_terminal_seq: 0,
        terminal_prefix_sha256: "b".repeat(64),
        requested_turns: ForkTurnSelection::All,
        effective_turns: 0,
    }
    .validate()
    .expect("all completed turns may resolve to an empty first-turn prefix");

    let child = SessionHeader::new(
        SessionId::new("session-child").unwrap(),
        2,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        settings(),
    )
    .unwrap()
    .with_fork_origin(ForkOrigin {
        parent_session_id: SessionId::new("session-parent").unwrap(),
        root_session_id: SessionId::new("session-root").unwrap(),
        path: AgentPath::new(vec![1]).unwrap(),
        task_name: "reviewer".into(),
        parent_header_fingerprint: "a".repeat(64),
        invoking_turn_id: TurnId::new("turn-spawn").unwrap(),
        resolved_after_seq: 0,
        resolved_terminal_seq: 7,
        terminal_prefix_sha256: "b".repeat(64),
        requested_turns: ForkTurnSelection::All,
        effective_turns: 2,
    })
    .unwrap();
    assert_eq!(child.format_version(), SESSION_FORMAT_VERSION);
    assert_eq!(child.fork_origin().unwrap().resolved_terminal_seq, 7);
    let mut encoded = serde_json::to_value(&child).unwrap();
    encoded["fork_origin"]["terminal_prefix_sha256"] = json!("not-a-digest");
    assert!(serde_json::from_value::<SessionHeader>(encoded).is_err());
}

#[test]
fn fork_lineage_rejects_nonempty_intervals_with_zero_effective_turns() {
    let malformed = ForkOrigin {
        parent_session_id: SessionId::new("parent").unwrap(),
        root_session_id: SessionId::new("parent").unwrap(),
        path: AgentPath::new(vec![1]).unwrap(),
        task_name: "child".into(),
        parent_header_fingerprint: "00".repeat(32),
        invoking_turn_id: TurnId::new("turn-1").unwrap(),
        resolved_after_seq: 0,
        resolved_terminal_seq: 7,
        terminal_prefix_sha256: "11".repeat(32),
        requested_turns: ForkTurnSelection::All,
        effective_turns: 0,
    };

    assert!(malformed.validate().is_err());
}

#[test]
fn durable_agent_tree_values_revalidate_nested_bounds() {
    assert!(serde_json::from_value::<AgentPath>(json!([0])).is_err());
    assert!(serde_json::from_value::<AgentPath>(json!([1, 2, 3, 4])).is_err());
    assert!(serde_json::from_value::<ForkTurnSelection>(json!({"last": 0})).is_err());

    assert!(
        AgentControlRecord::new(
            1,
            1,
            AgentControlRecordBody::CompletionReserved {
                activation_id: ActivationId::new("activation-1").unwrap(),
                parent_session_id: SessionId::new("session-parent").unwrap(),
                maximum_bytes: u64::try_from(MAXIMUM_AGENT_MESSAGE_BYTES).unwrap() + 1,
            },
        )
        .is_err()
    );
    for (target, wake_required) in [
        (MessageTarget::NextTurn, false),
        (MessageTarget::NextStep, true),
    ] {
        assert!(
            AgentControlRecord::new(
                3,
                3,
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: MessageId::new("message-invalid-wake-target").unwrap(),
                        source: AgentMessageSource::Human,
                        content: vec![AgentMessageContent::Text {
                            text: "invalid scheduling tuple".into(),
                        }],
                        options: MessageOptions::default(),
                    },
                    root_session_id: SessionId::new("session-root").unwrap(),
                    target,
                    wake_required,
                },
            )
            .is_err(),
            "accepted an inconsistent target/wake tuple"
        );
    }
}

#[test]
fn control_records_bound_message_authority_and_form_a_digest_chain() {
    let message = AgentMessage {
        message_id: MessageId::new("message-1").unwrap(),
        source: AgentMessageSource::Agent {
            source_session_id: SessionId::new("session-source").unwrap(),
        },
        content: vec![AgentMessageContent::Text {
            text: "hello".into(),
        }],
        options: MessageOptions::default(),
    };
    let first = AgentControlRecord::new(
        1,
        1,
        AgentControlRecordBody::MessageAccepted {
            message: message.clone(),
            root_session_id: SessionId::new("session-root").unwrap(),
            target: MessageTarget::NextTurn,
            wake_required: true,
        },
    )
    .unwrap();
    let second = AgentControlRecord::new(
        2,
        2,
        AgentControlRecordBody::MessageDiscarded {
            message_id: message.message_id.clone(),
            reason: MessageDiscardReason::Cancelled,
        },
    )
    .unwrap();
    validate_control_sequence(0, &[first.clone(), second.clone()]).unwrap();
    assert!(validate_control_sequence(0, std::slice::from_ref(&second)).is_err());
    assert_ne!(
        control_prefix_sha256([&first]).unwrap(),
        control_prefix_sha256([&first, &second]).unwrap()
    );

    let with_options = AgentMessage {
        options: MessageOptions {
            model: Some(ModelRef::new("openai", "gpt-test").unwrap()),
            sandbox: None,
        },
        ..message
    };
    assert!(
        AgentControlRecord::new(
            3,
            3,
            AgentControlRecordBody::MessageAccepted {
                message: with_options,
                root_session_id: SessionId::new("session-root").unwrap(),
                target: MessageTarget::NextStep,
                wake_required: true,
            },
        )
        .is_err()
    );
}
