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
            conclusion: None,
        }],
    )
    .await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One exact source claim proves route, role retry and authority expiry together.
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
        output_contract: None,
        role: None,
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
    let mut inherited = request(caller.clone(), "inherited", None, None);
    inherited.role = Some(rsi_agent_session_protocol::DelegationRole {
        name: "auditor".into(),
        persona: Some("frozen role text".into()),
        allow: Some(std::collections::BTreeSet::default()),
        deny: std::collections::BTreeSet::default(),
    });
    let first = kernel.spawn_agent(inherited.clone()).await.unwrap();
    assert_eq!(kernel.spawn_agent(inherited.clone()).await.unwrap(), first);
    let mut changed_role = inherited.clone();
    changed_role.role.as_mut().unwrap().persona = Some("changed".into());
    assert!(kernel.spawn_agent(changed_role).await.is_err());
    let header = store.header(&first.session_id).await.unwrap();
    assert_eq!(
        header.delegation_policy().unwrap().persona(),
        Some("frozen role text")
    );
    assert!(header.delegation_policy().unwrap().tools().is_empty());
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

#[tokio::test]
async fn frozen_tool_policy_is_enforced_before_kernel_publication() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let header = header("restricted-root")
        .with_delegation_policy(Some(
            rsi_agent_session_protocol::DelegationPolicy::freeze(
                None,
                std::collections::BTreeSet::new(),
                None,
            )
            .unwrap(),
        ))
        .unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header),
            message: mailbox_message("restricted-input"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel.register("restricted-worker".into()).unwrap();
    let claim = kernel
        .claim("restricted-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    publish_model_source(&kernel, &claim, "denied-call", "denied_tool", &snapshot()).await;
    let before = store.read_watermarks(claim.session_id()).await.unwrap();
    let result = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ToolIntent {
                turn_id: claim.turn_id().clone(),
                effect_id: EffectId::new("denied-effect").unwrap(),
                source_model_effect_id: EffectId::new("source-model").unwrap(),
                identity: ToolResultIdentity::new(
                    "owner",
                    "denied-effect",
                    "denied-call",
                    "a".repeat(64),
                )
                .unwrap(),
                name: "denied_tool".into(),
                arguments: serde_json::json!({}),
                approval: None,
                parallel_safe: false,
            }],
        )
        .await;
    assert!(
        matches!(result, Err(TurnError::Invalid(_))),
        "denied ToolIntent was admitted"
    );
    assert_eq!(
        store.read_watermarks(claim.session_id()).await.unwrap(),
        before
    );
    assert!(
        kernel
            .tool_caller(&claim, &EffectId::new("denied-effect").unwrap())
            .is_err()
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}
