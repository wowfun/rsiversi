use rsi_agent_session_protocol::*;
use rsi_ai_protocol::{AiCapability, ImageRequest, ModelRef, PreparedCallSnapshot, RetryPolicy};
use rsi_media_protocol::{MediaId, MediaRef};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::{ToolContent, ToolResult, ToolResultIdentity};
use serde_json::json;

fn profile() -> FrozenAgentProfile {
    FrozenAgentProfile::new(
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
fn header_round_trips_and_rejects_old_format_or_noncanonical_path() {
    let header = SessionHeader::new(
        SessionId::new("session-1").unwrap(),
        1,
        "/workspace",
        profile(),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&header).unwrap();
    assert_eq!(
        serde_json::from_slice::<SessionHeader>(&bytes).unwrap(),
        header
    );

    let mut value = serde_json::to_value(&header).unwrap();
    value["format_version"] = 2.into();
    assert!(serde_json::from_value::<SessionHeader>(value).is_err());
    assert!(
        SessionHeader::new(
            SessionId::new("session-1").unwrap(),
            1,
            "relative/path",
            profile()
        )
        .is_err()
    );
}

#[test]
fn maximum_escaped_header_stays_inside_its_framing_bound() {
    let profile = FrozenAgentProfile::new(
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
        profile,
    )
    .unwrap();

    assert!(serde_json::to_vec(&header).unwrap().len() <= MAXIMUM_SESSION_HEADER_BYTES);
}

#[test]
fn profile_fingerprint_is_canonical_and_secrets_have_no_field() {
    let left = profile();
    let round_trip: FrozenAgentProfile =
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
fn unconfined_profiles_require_live_approval() {
    let model = ModelRef::new("openai", "gpt-test").unwrap();
    assert!(
        FrozenAgentProfile::new(
            "unsafe",
            "Be precise.",
            model.clone(),
            SandboxMode::DangerFullAccess,
            false,
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<FrozenAgentProfile>(json!({
            "profile_id": "unsafe",
            "system_prompt": "Be precise.",
            "default_model": { "deployment": "openai", "model": "gpt-test" },
            "sandbox": "danger-full-access",
            "require_approval": false
        }))
        .is_err()
    );
    assert!(
        FrozenAgentProfile::new(
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
