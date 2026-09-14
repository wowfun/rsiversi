use super::*;
use rsi_agent_turn_protocol::TurnClaim;

async fn flush_bodies(kernel: &AgentKernel, claim: &TurnClaim, bodies: Vec<SessionFactBody>) {
    let facts = kernel.publish(claim, bodies).await.unwrap().published();
    kernel
        .flush(claim, facts.last().unwrap().seq())
        .await
        .unwrap();
}

/// Enters real source evidence for tests whose control Tool remains live.
pub(super) async fn control_tool_caller(
    kernel: &AgentKernel,
    claim: &TurnClaim,
) -> rsi_agent_turn_protocol::AgentCallerAuthority {
    let tool = EffectId::new("fixture-control-tool").unwrap();
    if let Ok(caller) = kernel.tool_caller(claim, &tool) {
        return caller;
    }
    let mut prepared = snapshot();
    prepared.deployment_id = claim
        .header()
        .settings()
        .default_model()
        .deployment()
        .into();
    prepared.model = claim.header().settings().default_model().model().into();
    let profile = rsi_ai_protocol::LanguageProfile::new(
        100_000,
        1_000,
        10_000,
        rsi_ai_protocol::ToolDialect::Responses,
        true,
        rsi_ai_protocol::ImageToolResultCapability::No,
        vec![],
    )
    .unwrap();
    prepared.language_settings =
        Some(rsi_ai_protocol::PreparedLanguageSettings::new(profile, None).unwrap());
    control_tool_caller_with_snapshot(kernel, claim, prepared).await
}

async fn control_tool_caller_with_snapshot(
    kernel: &AgentKernel,
    claim: &TurnClaim,
    prepared: PreparedCallSnapshot,
) -> rsi_agent_turn_protocol::AgentCallerAuthority {
    let tool = EffectId::new("fixture-control-tool").unwrap();
    publish_model_source(kernel, claim, "fixture-call", "fixture_control", &prepared).await;
    let model = EffectId::new("source-model").unwrap();
    let turn_id = claim.turn_id().clone();
    let identity = ToolResultIdentity::new(
        "fixture",
        "fixture-control-tool",
        "fixture-call",
        "a".repeat(64),
    )
    .unwrap();
    flush_bodies(
        kernel,
        claim,
        vec![SessionFactBody::ToolIntent {
            turn_id: turn_id.clone(),
            effect_id: tool.clone(),
            source_model_effect_id: model,
            identity: identity.clone(),
            name: "fixture_control".into(),
            arguments: serde_json::json!({}),
            approval: None,
            parallel_safe: false,
        }],
    )
    .await;
    flush_bodies(
        kernel,
        claim,
        vec![SessionFactBody::ToolStarted {
            turn_id,
            effect_id: tool.clone(),
            identity,
        }],
    )
    .await;
    kernel.tool_caller(claim, &tool).unwrap()
}

pub(super) async fn publish_model_source(
    kernel: &AgentKernel,
    claim: &TurnClaim,
    call_id: &str,
    name: &str,
    prepared: &PreparedCallSnapshot,
) {
    let model = EffectId::new("source-model").unwrap();
    let turn_id = claim.turn_id().clone();
    flush_bodies(
        kernel,
        claim,
        vec![SessionFactBody::ModelIntent {
            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
            },
            price_quote: None,
            turn_id: turn_id.clone(),
            effect_id: model.clone(),
            purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
            snapshot: prepared.clone(),
        }],
    )
    .await;
    flush_bodies(
        kernel,
        claim,
        vec![SessionFactBody::ModelStarted {
            turn_id: turn_id.clone(),
            effect_id: model.clone(),
        }],
    )
    .await;
    let events = [
        LanguageEvent::ContentStarted {
            index: 0,
            content: rsi_ai_protocol::ContentStart::ToolCall {
                id: call_id.into(),
                name: name.into(),
                kind: rsi_ai_protocol::ToolCallKind::Function,
            },
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: rsi_ai_protocol::ContentDelta::ToolArguments("{}".into()),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Finished {
            reason: rsi_ai_protocol::FinishReason::ToolCalls,
            replay: None,
        },
    ];
    flush_bodies(
        kernel,
        claim,
        events
            .into_iter()
            .map(|event| SessionFactBody::ModelEvent {
                turn_id: turn_id.clone(),
                effect_id: model.clone(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event,
            })
            .collect(),
    )
    .await;
}

pub(super) async fn finish_control_tool(kernel: &AgentKernel, claim: &TurnClaim) {
    flush_bodies(
        kernel,
        claim,
        vec![SessionFactBody::ToolResult {
            turn_id: claim.turn_id().clone(),
            effect_id: EffectId::new("fixture-control-tool").unwrap(),
            identity: ToolResultIdentity::new(
                "fixture",
                "fixture-control-tool",
                "fixture-call",
                "a".repeat(64),
            )
            .unwrap(),
            result: rsi_tools_protocol::ToolResult::new(serde_json::json!({}), vec![], false)
                .unwrap(),
        }],
    )
    .await;
}

#[tokio::test]
async fn child_model_comes_from_producing_request_and_tool_authority_expires() {
    use rsi_ai_protocol::{
        ModelRef, PreparedLanguageSettings, ReasoningEffortId, ReasoningEffortProfile,
    };
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let workers = kernel.start_workers();
    submit(&kernel, "source-selection", "spawn work").await;
    let lease = kernel.register("source-worker".into()).unwrap();
    let claim = kernel
        .claim("source-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let request = |caller, id: &str, model, reasoning_effort| SpawnAgentRequest {
        caller,
        model,
        reasoning_effort,
        cancellation: CancellationToken::new(),
        child_session_id: SessionId::new(id).unwrap(),
        task_name: id.into(),
        message_id: MessageId::new(format!("start-{id}")).unwrap(),
        message: "perform child work".into(),
        fork_turns: ForkTurnSelection::None,
    };
    assert!(
        matches!(kernel.spawn_agent(request(kernel.agent_caller(&claim).unwrap(), "no-source", None, None)).await,
        Err(TurnError::Invalid(message)) if message.contains("Tool model origin"))
    );
    let mut prepared = snapshot();
    prepared.deployment_id = "source-deployment".into();
    prepared.model = "actual-model".into();
    let high = ReasoningEffortId::new("high").unwrap();
    let profile = prepared
        .language_settings
        .as_ref()
        .unwrap()
        .profile
        .clone()
        .with_reasoning_efforts(
            ReasoningEffortProfile::new(vec![high.clone()], Some(high.clone())).unwrap(),
        );
    prepared.language_settings = Some(PreparedLanguageSettings::new(profile, None).unwrap());
    let caller = control_tool_caller_with_snapshot(&kernel, &claim, prepared).await;
    let inherited = request(caller.clone(), "inherited", None, None);
    let first = kernel.spawn_agent(inherited.clone()).await.unwrap();
    assert_eq!(kernel.spawn_agent(inherited.clone()).await.unwrap(), first);
    let header = store.header(&first.session_id).await.unwrap();
    assert_eq!(
        header.settings().default_model(),
        &ModelRef::new("source-deployment", "actual-model").unwrap()
    );
    assert_eq!(header.settings().default_reasoning_effort(), Some(&high));
    assert_ne!(
        header.settings().default_model(),
        claim.header().settings().default_model()
    );
    let overridden = kernel
        .spawn_agent(request(
            caller.clone(),
            "overridden",
            Some(ModelRef::new("other", "new-model").unwrap()),
            None,
        ))
        .await
        .unwrap();
    let header = store.header(&overridden.session_id).await.unwrap();
    assert_eq!(header.settings().default_model().model(), "new-model");
    assert!(header.settings().default_reasoning_effort().is_none());
    assert!(matches!(
        kernel
            .spawn_agent(request(caller.clone(), "orphan-effort", None, Some(high)))
            .await,
        Err(TurnError::Invalid(_))
    ));
    finish_control_tool(&kernel, &claim).await;
    assert!(matches!(
        kernel.spawn_agent(inherited).await,
        Err(TurnError::StaleClaim)
    ));
    assert!(
        kernel
            .tool_caller(&claim, caller.tool_effect_id().unwrap())
            .is_err()
    );
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
}
