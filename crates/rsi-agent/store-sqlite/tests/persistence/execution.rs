use super::*;
use rsi_agent_session_protocol::{
    EffectId, EvidenceUnavailable, ModelAttemptLog, ModelEventPurpose, ModelPurpose,
    RequestEvidence, StepId, ToolOrigin,
};
use rsi_agent_store_protocol::AppendBatch;
use rsi_ai_protocol::{
    AiCapability, ContentDelta, ContentStart, FinishReason, LanguageEvent, PreparedCallSnapshot,
    RetryPolicy, ToolCallKind,
};
use rsi_media_protocol::{MediaId, MediaRef};
use rsi_tools_protocol::{ToolContent, ToolProgramRole, ToolResult, ToolResultIdentity};
use serde_json::json;
use std::sync::Arc;

fn attempt(tool: bool) -> ModelAttemptLog {
    let mut events = vec![
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Reasoning,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Reasoning("fixture reasoning".into()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::ContentStarted {
            index: 1,
            content: ContentStart::Text,
        },
        // Crosses an encoded compact-attempt chunk boundary, including Unicode.
        LanguageEvent::ContentDelta {
            index: 1,
            delta: ContentDelta::Text("界".repeat(50_000)),
        },
        LanguageEvent::ContentFinished { index: 1 },
    ];
    if tool {
        events.extend([
            LanguageEvent::ContentStarted {
                index: 2,
                content: ContentStart::ToolCall {
                    id: "call-1".into(),
                    name: "fixture".into(),
                    kind: ToolCallKind::Function,
                },
            },
            LanguageEvent::ContentDelta {
                index: 2,
                delta: ContentDelta::ToolArguments("{\"text\":\"中\"}".into()),
            },
            LanguageEvent::ContentFinished { index: 2 },
        ]);
    }
    events.push(LanguageEvent::Finished {
        reason: if tool {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        },
        replay: None,
    });
    ModelAttemptLog::from_events(
        events
            .into_iter()
            .enumerate()
            .map(|(i, event)| (i as u64 * 16_384, event)),
        true,
    )
    .unwrap()
}

fn intent(turn: &TurnId, effect: &EffectId) -> SessionFactBody {
    SessionFactBody::ModelIntent {
        turn_id: turn.clone(),
        effect_id: effect.clone(),
        snapshot: PreparedCallSnapshot {
            call_id: effect.as_str().into(),
            deployment_id: "fixture".into(),
            provider_family: "fixture".into(),
            capability: AiCapability::Language,
            model: "fixture".into(),
            protocol: "fixture".into(),
            transport: "memory".into(),
            endpoint_fingerprint: "fixture".into(),
            config_generation: 1,
            credential_source: None,
            retry_policy: RetryPolicy::default(),
            request_sha256: hex::encode(Sha256::digest(b"deterministic fixture request")),
            language_settings: None,
        },
        purpose: ModelPurpose::Conversation,
        price_quote: None,
        evidence: RequestEvidence::Unavailable {
            reason: EvidenceUnavailable::NotCaptured,
        },
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "One real-writer timeline keeps conversation, Tool and fork evidence together."
)]
pub(super) async fn capture(store: &SqliteStore, root: &Path) {
    let header = header("model-tools");
    let turn = TurnId::new("turn-1").unwrap();
    let first = EffectId::new("model-1").unwrap();
    let second = EffectId::new("model-2").unwrap();
    let tool = EffectId::new("tool-1").unwrap();
    let arguments = json!({"text": "中"});
    let identity = ToolResultIdentity::new(
        "fixture",
        "invocation-1",
        "call-1",
        hex::encode(Sha256::digest(serde_json::to_vec(&arguments).unwrap())),
    )
    .unwrap();
    // Generated RGBA 1x1 PNG. This covers MediaRef encoding and identity, not
    // the separate Media import/normalization writer or its publication policy.
    let png = hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63304898f01f0004040220e4f2a5820000000049454e44ae426082").unwrap();
    std::fs::write(root.join("media-fixture.png"), &png).unwrap();
    let media = MediaRef {
        id: MediaId::new(hex::encode(Sha256::digest(&png))).unwrap(),
        mime: "image/png".into(),
        bytes: png.len() as u64,
        width: 1,
        height: 1,
    };
    let mut result = ToolResult::new(
        json!({"opaque": [null, true, 1.25, "中", {"z": 1, "a": 2}]}),
        vec![
            ToolContent::Text {
                text: "tool output\n\"\\ 🦀".into(),
            },
            ToolContent::Image { media },
        ],
        false,
    )
    .unwrap();
    // Cache existence is deliberately independent of durable reference encoding.
    result.spills.push(
        serde_json::from_value(json!({
            "locator": format!("{}-{}", "7".repeat(64), "8".repeat(32)),
            "bytes": 100_000,
        }))
        .unwrap(),
    );
    let mut bodies = vec![
        facts()[0].body().clone(),
        SessionFactBody::StepStarted {
            turn_id: turn.clone(),
            step_id: StepId::new("step-1").unwrap(),
        },
        intent(&turn, &first),
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: first.clone(),
        },
        SessionFactBody::ModelAttemptSettled {
            turn_id: turn.clone(),
            effect_id: first.clone(),
            attempt: attempt(true),
            purpose: ModelEventPurpose::Conversation,
        },
        SessionFactBody::ToolIntent {
            turn_id: turn.clone(),
            effect_id: tool.clone(),
            origin: ToolOrigin::Model { effect_id: first },
            program_role: ToolProgramRole::Unavailable,
            identity: identity.clone(),
            name: "fixture".into(),
            arguments,
            approval: None,
            parallel_safe: false,
        },
        SessionFactBody::ToolStarted {
            turn_id: turn.clone(),
            effect_id: tool.clone(),
            identity: identity.clone(),
        },
        SessionFactBody::ToolResult {
            turn_id: turn.clone(),
            effect_id: tool,
            identity,
            result,
            conclusion: None,
        },
        intent(&turn, &second),
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: second.clone(),
        },
        SessionFactBody::ModelAttemptSettled {
            turn_id: turn.clone(),
            effect_id: second,
            attempt: attempt(false),
            purpose: ModelEventPurpose::Conversation,
        },
    ];
    bodies.push(SessionFactBody::TurnTerminal {
        turn_id: turn,
        outcome: TurnOutcome::Completed,
        result: None,
    });
    let facts = bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| Arc::new(SessionFact::new(i as u64 + 1, i as u64 + 1, body).unwrap()))
        .collect();
    rsi_agent_testkit::append_history_fixture(
        store,
        AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: 0,
            header: Some(header),
            facts,
        },
    )
    .await
    .unwrap();
}
