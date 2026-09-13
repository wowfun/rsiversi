use rsi_agent_session_protocol::{EffectId, SessionFact, SessionFactBody, TurnId, TurnOutcome};
use rsi_conversation::{
    FactField, FieldWindow, MAXIMUM_WINDOW_BYTES, SourceRef, ToolOutcome, select_field,
};
use rsi_tools_protocol::{ToolContent, ToolResult, ToolResultIdentity};
use serde::ser::SerializeSeq;
use serde_json::json;

#[test]
fn dense_patch_evidence_fits_the_pretty_source_window() {
    use rsi_conversation::{ToolValuePath, select_tool_value_path};
    let mut evidence = json!({"version":1,"omitted":false,"diffs":[]});
    for effect in 0..2000 {
        evidence["diffs"]
            .as_array_mut()
            .unwrap()
            .push(json!({"effect":effect,"unified_diff":"x"}));
        if serde_json::to_vec(&evidence).unwrap().len() > 32 * 1024 {
            evidence["diffs"].as_array_mut().unwrap().pop();
            break;
        }
    }
    assert!(serde_json::to_vec(&evidence).unwrap().len() > 32 * 1024 - 64);
    let fact = SessionFact::new(
        7,
        1,
        SessionFactBody::ToolResult {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("patch").unwrap(),
            identity: ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap(),
            result: ToolResult::new(
                json!({"unrelated":"x".repeat(2 * 1024 * 1024), "evidence":evidence}),
                vec![],
                false,
            )
            .unwrap(),
        },
    )
    .unwrap();
    let source = SourceRef {
        seq: 7,
        field: FactField::ToolValue,
    };
    let path = ToolValuePath::new(vec!["evidence".into()]).unwrap();
    let window = select_tool_value_path(&fact, source, &path)
        .unwrap()
        .window(0, 96 * 1024)
        .unwrap();
    assert!(!window.more);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&window.text).unwrap(),
        evidence
    );
}

#[test]
fn tool_subfields_select_exact_values_without_unrelated_payloads() {
    use rsi_conversation::{ToolValuePath, select_tool_value_path};
    let fact = SessionFact::new(
        7,
        1,
        SessionFactBody::ToolResult {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("effect").unwrap(),
            identity: ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap(),
            result: ToolResult::new(
                json!({"large": "x".repeat(2 * 1024 * 1024),
            "evidence": {"diffs": [{"unified_diff": "-旧\n+新\n"}]}, "a/b": true}),
                vec![],
                false,
            )
            .unwrap(),
        },
    )
    .unwrap();
    let source = SourceRef {
        seq: 7,
        field: FactField::ToolValue,
    };
    let path = ToolValuePath::new(
        ["evidence", "diffs", "0", "unified_diff"]
            .map(str::to_owned)
            .into(),
    )
    .unwrap();
    let selected = select_tool_value_path(&fact, source, &path).unwrap();
    assert_eq!(selected.window(0, 64).unwrap().text, "-旧\n+新\n");
    for key in ["01", "+0", "-1", "18446744073709551616"] {
        let path = ToolValuePath::new(vec!["evidence".into(), "diffs".into(), key.into()]).unwrap();
        assert!(select_tool_value_path(&fact, source, &path).is_none());
    }
    assert!(select_tool_value_path(&fact, SourceRef { seq: 8, ..source }, &path).is_none());
    assert!(
        select_tool_value_path(
            &fact,
            SourceRef {
                field: FactField::ToolArguments,
                ..source
            },
            &path
        )
        .is_none()
    );
    let literal = ToolValuePath::new(vec!["a/b".into()]).unwrap();
    assert_eq!(
        select_tool_value_path(&fact, source, &literal)
            .unwrap()
            .window(0, 64)
            .unwrap()
            .text,
        "true"
    );
    for value in [
        json!([]),
        json!(vec!["x"; 9]),
        json!(["x".repeat(65)]),
        json!(vec!["x".repeat(64); 5]),
    ] {
        assert!(serde_json::from_value::<ToolValuePath>(value).is_err());
    }
}

#[test]
fn source_identity_is_lossless_and_its_wire_grammar_is_closed() {
    let source = SourceRef {
        seq: u64::MAX,
        field: FactField::ToolText { index: 7 },
    };
    let value = serde_json::to_value(source).unwrap();
    assert_eq!(value["seq"], u64::MAX.to_string());
    assert_eq!(serde_json::from_value::<SourceRef>(value).unwrap(), source);
    for value in [
        json!({"seq":1,"field":{"kind":"tool_value"}}),
        json!({"seq":"0","field":{"kind":"tool_value"}}),
        json!({"seq":"01","field":{"kind":"tool_value"}}),
        json!({"seq":"18446744073709551616","field":{"kind":"tool_value"}}),
        json!({"seq":"1","field":{"kind":"tool_value","path":"password"}}),
        json!({"seq":"1","field":{"kind":"tool_value","index":null}}),
        json!({"seq":"1","field":{"kind":"tool_value","index":0}}),
        json!({"seq":"1","field":{"kind":"raw","index":0}}),
        json!({"seq":"1","field":{"kind":"tool_text","index":65536}}),
    ] {
        assert!(
            serde_json::from_value::<SourceRef>(value.clone()).is_err(),
            "accepted unexpected source: {value}"
        );
    }
}

#[test]
fn exact_fact_and_content_kind_replace_numeric_field_aliasing() {
    let result = SessionFact::new(
        7,
        1,
        SessionFactBody::ToolResult {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("tool").unwrap(),
            identity: ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap(),
            result: ToolResult::new(
                json!({"exit_code":7}),
                vec![ToolContent::Text {
                    text: "exact output".into(),
                }],
                false,
            )
            .unwrap(),
        },
    )
    .unwrap();
    let text = SourceRef {
        seq: 7,
        field: FactField::ToolText { index: 0 },
    };
    assert_eq!(
        select_field(&result, text)
            .unwrap()
            .window(0, 64)
            .unwrap()
            .text,
        "exact output"
    );
    for source in [
        SourceRef { seq: 8, ..text },
        SourceRef {
            field: FactField::ToolArguments,
            ..text
        },
        SourceRef {
            field: FactField::InputText { index: 0 },
            ..text
        },
        SourceRef {
            field: FactField::ToolImage { index: 0 },
            ..text
        },
        SourceRef {
            field: FactField::ToolText { index: 1 },
            ..text
        },
    ] {
        assert!(select_field(&result, source).is_none());
    }
    let value = select_field(
        &result,
        SourceRef {
            field: FactField::ToolValue,
            ..text
        },
    )
    .unwrap()
    .window(0, 64)
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&value.text).unwrap(),
        json!({"exit_code":7})
    );
    let terminal = SessionFact::new(
        8,
        1,
        SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("turn").unwrap(),
            outcome: TurnOutcome::Completed,
        },
    )
    .unwrap();
    assert!(
        select_field(
            &terminal,
            SourceRef {
                seq: 8,
                field: FactField::ToolValue
            }
        )
        .is_none()
    );
    assert!(
        select_field(
            &terminal,
            SourceRef {
                seq: 8,
                field: FactField::TurnOutcome
            }
        )
        .is_some()
    );
}

#[test]
fn json_windows_match_exact_pretty_source_at_every_utf8_boundary() {
    let value = json!({"unicode":["a界😀e\u{301}","\n\t\\\""],"number":serde_json::from_str::<serde_json::Value>("1.0000000000000000000001").unwrap()});
    let whole = serde_json::to_string_pretty(&value).unwrap();
    for start in 0..whole.len() + 7 {
        for size in [4, 5, 7, 16, 32, MAXIMUM_WINDOW_BYTES] {
            assert_eq!(
                FieldWindow::json(&value, start, size).unwrap(),
                FieldWindow::text(&whole, start, size).unwrap(),
                "start={start} size={size}"
            );
        }
    }
    for size in [0, 1, 3, MAXIMUM_WINDOW_BYTES + 1] {
        assert!(FieldWindow::json(&value, 0, size).is_err());
    }
}

#[test]
fn large_json_window_stops_its_serializer_instead_of_allocating_the_whole_value() {
    struct Many(std::cell::Cell<usize>);
    impl serde::Serialize for Many {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(Some(1_000_000))?;
            for _ in 0..1_000_000 {
                self.0.set(self.0.get() + 1);
                seq.serialize_element("界😀")?;
            }
            seq.end()
        }
    }
    let many = Many(std::cell::Cell::new(0));
    let window = FieldWindow::json(&many, 1024, 128).unwrap();
    assert!(
        many.0.get() < 200,
        "serializer traversed beyond the requested window"
    );
    assert!(window.more);
    assert!(window.text.len() <= 128);
    assert_eq!(window.end - window.start, window.text.len());
    let value = json!({"large":"界😀".repeat(500_000)});
    let window = FieldWindow::json(&value, 1_000_000, MAXIMUM_WINDOW_BYTES).unwrap();
    assert!(window.more && window.text.len() <= MAXIMUM_WINDOW_BYTES);
    assert!(window.end > window.start);
}

#[test]
fn tool_and_process_failures_remain_distinct() {
    for (value, error, expected) in [
        (json!({"exit_code":0}), false, ToolOutcome::Completed),
        (json!({"exit_code":7}), false, ToolOutcome::ProcessFailed),
        (json!({"signal":9}), false, ToolOutcome::ProcessFailed),
        (json!({"signal":null}), false, ToolOutcome::Completed),
        (json!({"exit_code":0}), true, ToolOutcome::ToolFailed),
        (json!({"exit_code":7}), true, ToolOutcome::ToolFailed),
    ] {
        assert_eq!(
            ToolOutcome::from_result(&ToolResult::new(value, Vec::new(), error).unwrap()),
            expected
        );
    }
}

fn check_fields(body: SessionFactBody, fields: &[(FactField, &str)]) {
    let fact = SessionFact::new(9, 1, body).unwrap();
    for &(field, expected) in fields {
        let source = SourceRef { seq: 9, field };
        assert_eq!(
            serde_json::from_value::<SourceRef>(serde_json::to_value(source).unwrap()).unwrap(),
            source
        );
        let window = select_field(&fact, source)
            .unwrap()
            .window(0, 4096)
            .unwrap();
        assert!(
            window.text.contains(expected),
            "field={field:?}, actual={}",
            window.text
        );
        assert!(select_field(&fact, SourceRef { seq: 10, field }).is_none());
        let image = rsi_conversation::MediaSource::select(&fact, source);
        if matches!(
            field,
            FactField::InputImage { .. } | FactField::ToolImage { .. } | FactField::ImageOutput
        ) {
            let image = image.expect("exact image source");
            assert_eq!(image.source, source);
            assert_eq!(image.media, &media());
            assert_eq!(image.label(), "[Image · image/png · 1×1 · 7 bytes]");
        } else {
            assert!(image.is_none());
        }
        assert!(
            rsi_conversation::MediaSource::select(&fact, SourceRef { seq: 10, field }).is_none()
        );
    }
}
fn media() -> rsi_media_protocol::MediaRef {
    rsi_media_protocol::MediaRef {
        id: rsi_media_protocol::MediaId::new("d".repeat(64)).unwrap(),
        mime: "image/png".into(),
        bytes: 7,
        width: 1,
        height: 1,
    }
}

#[test]
fn input_sources_preserve_text_and_media_kinds() {
    use rsi_agent_session_protocol::{AgentMessageContent, InputMessageSource, MessageId, StepId};

    check_fields(
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("turn").unwrap(),
            text: "original input".into(),
            model: None,
            sandbox: rsi_sandbox::SandboxMode::ReadOnly,
            require_approval: false,
        },
        &[(FactField::TurnInput, "original input")],
    );
    check_fields(
        SessionFactBody::InputMessageEntered {
            turn_id: TurnId::new("turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            source: InputMessageSource::Human {
                message_id: MessageId::new("input").unwrap(),
            },
            content: vec![
                AgentMessageContent::Text {
                    text: "entered input".into(),
                },
                AgentMessageContent::Image { media: media() },
            ],
        },
        &[
            (FactField::InputText { index: 0 }, "entered input"),
            (FactField::InputImage { index: 1 }, "image/png"),
        ],
    );
}

#[test]
fn model_sources_preserve_delta_failure_and_generated_image_kinds() {
    use rsi_ai_protocol::{
        AiError, ContentDelta, DispatchStatus, ErrorKind, ErrorPhase, LanguageEvent,
    };
    for (delta, field, text) in [
        (
            ContentDelta::Text("assistant".into()),
            FactField::ModelText,
            "assistant",
        ),
        (
            ContentDelta::Reasoning("reasoning".into()),
            FactField::ModelReasoning,
            "reasoning",
        ),
        (
            ContentDelta::ToolArguments("{\"command\":".into()),
            FactField::ModelToolArguments,
            "{\"command\":",
        ),
    ] {
        let fact = SessionFact::new(
            9,
            1,
            SessionFactBody::ModelEvent {
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("model").unwrap(),
                event: LanguageEvent::ContentDelta { index: 0, delta },
            },
        )
        .unwrap();
        for candidate in [
            FactField::ModelText,
            FactField::ModelReasoning,
            FactField::ModelToolArguments,
        ] {
            let selected = select_field(
                &fact,
                SourceRef {
                    seq: 9,
                    field: candidate,
                },
            );
            assert_eq!(selected.is_some(), candidate == field);
            if let Some(value) = selected {
                assert_eq!(value.window(0, 64).unwrap().text, text);
            }
        }
    }
    check_fields(
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("model").unwrap(),
            event: LanguageEvent::Failed {
                error: AiError::new(
                    ErrorKind::Server,
                    ErrorPhase::Stream,
                    DispatchStatus::Dispatched,
                    "safe provider diagnostic",
                )
                .unwrap(),
                replay: None,
            },
        },
        &[(FactField::ModelFailure, "safe provider diagnostic")],
    );
    check_fields(
        SessionFactBody::ImageOutput {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("image").unwrap(),
            index: 0,
            media: media(),
        },
        &[(FactField::ImageOutput, "image/png")],
    );
}

#[test]
fn structured_sources_keep_rejected_arguments_and_redacted_provider_identity() {
    use rsi_agent_session_protocol::{ContributionId, ToolRejection};
    use rsi_ai_protocol::{AiCapability, PreparedCallSnapshot, RetryPolicy};
    let identity = ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64)).unwrap();
    let turn = TurnId::new("turn").unwrap();
    let effect = EffectId::new("tool").unwrap();
    check_fields(
        SessionFactBody::ToolIntent {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            identity: identity.clone(),
            name: "bash".into(),
            arguments: json!({"command":"exit 7"}),
            approval: None,
            parallel_safe: false,
        },
        &[(FactField::ToolArguments, "exit 7")],
    );
    check_fields(
        SessionFactBody::ToolRejected {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            identity: identity.clone(),
            name: "bash".into(),
            arguments: json!({"command":"never executed"}),
            rejection: ToolRejection::PolicyDenied {
                contribution_id: ContributionId::new("fixture.policy").unwrap(),
                reason: "plan mode denied".into(),
            },
        },
        &[
            (FactField::ToolArguments, "never executed"),
            (FactField::ToolRejection, "plan mode denied"),
        ],
    );
    check_fields(
        SessionFactBody::ToolResult {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            identity,
            result: ToolResult::new(
                json!({}),
                vec![ToolContent::Image { media: media() }],
                false,
            )
            .unwrap(),
        },
        &[(FactField::ToolImage { index: 0 }, "image/png")],
    );
    let snapshot = PreparedCallSnapshot {
        call_id: "call".into(),
        deployment_id: "fixture".into(),
        provider_family: "deepseek".into(),
        capability: AiCapability::Language,
        model: "fixture".into(),
        protocol: "openai-responses".into(),
        transport: "http".into(),
        endpoint_fingerprint: "sha256-fixture".into(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "b".repeat(64),
    };
    check_fields(
        SessionFactBody::ModelIntent {
            purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            snapshot: snapshot.clone(),
        },
        &[(FactField::ModelSnapshot, "openai-responses")],
    );
    let snapshot = PreparedCallSnapshot {
        capability: AiCapability::Image,
        ..snapshot
    };
    check_fields(
        SessionFactBody::ImageIntent {
            turn_id: turn,
            effect_id: effect,
            snapshot,
        },
        &[(FactField::ModelSnapshot, "image")],
    );
}
