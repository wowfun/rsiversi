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
    publish_model_calls(kernel, claim, &[(call_id, name)], prepared).await;
}

pub(super) async fn publish_model_calls(
    kernel: &AgentKernel,
    claim: &TurnClaim,
    calls: &[(&str, &str)],
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
    let mut events = Vec::new();
    for (index, (call_id, name)) in calls.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        events.extend([
            LanguageEvent::ContentStarted {
                index,
                content: rsi_ai_protocol::ContentStart::ToolCall {
                    id: (*call_id).into(),
                    name: (*name).into(),
                    kind: rsi_ai_protocol::ToolCallKind::Function,
                },
            },
            LanguageEvent::ContentDelta {
                index,
                delta: rsi_ai_protocol::ContentDelta::ToolArguments("{}".into()),
            },
            LanguageEvent::ContentFinished { index },
        ]);
    }
    events.push(LanguageEvent::Finished {
        reason: rsi_ai_protocol::FinishReason::ToolCalls,
        replay: None,
    });
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
    inherited.role = Some(
        rsi_agent_session_protocol::DelegationRole {
            name: "auditor".into(),
            persona: Some("frozen role text".into()),
            allow: Some(std::collections::BTreeSet::default()),
            deny: std::collections::BTreeSet::default(),
        }
        .into(),
    );
    let first = kernel.spawn_agent(inherited.clone()).await.unwrap();
    assert_eq!(kernel.spawn_agent(inherited.clone()).await.unwrap(), first);
    let mut changed_role = inherited.clone();
    let rsi_agent_turn_protocol::SpawnRoleSelection::Inline(role) =
        changed_role.role.as_mut().unwrap()
    else {
        panic!("inline role")
    };
    role.persona = Some("changed".into());
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

#[tokio::test]
async fn executor_cannot_inject_kernel_owned_supersession() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let workers = kernel.start_workers();
    let submitted = submit(&kernel, "supersession-owner", "work").await;
    let _lease = kernel.register("worker".into()).unwrap();
    let claim = kernel
        .claim("worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    publish_model_source(&kernel, &claim, "call", "read", &snapshot()).await;
    assert!(matches!(
        kernel
            .publish(
                &claim,
                vec![SessionFactBody::ToolCallsSuperseded {
                    turn_id: submitted.turn_id.clone(),
                    source_model_effect_id: EffectId::new("source-model").unwrap(),
                }]
            )
            .await,
        Err(TurnError::Invalid(_))
    ));
    flush_bodies(
        &kernel,
        &claim,
        vec![SessionFactBody::ToolIntent {
            turn_id: submitted.turn_id,
            source_model_effect_id: EffectId::new("source-model").unwrap(),
            effect_id: EffectId::new("tool").unwrap(),
            identity: ToolResultIdentity::new("owner", "tool", "call", "a".repeat(64)).unwrap(),
            name: "read".into(),
            arguments: serde_json::json!({}),
            approval: None,
            parallel_safe: false,
        }],
    )
    .await;
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One SQLite scenario proves fresh admission, exact retries, edits, deletion, and cold restoration"
)]
async fn named_spawn_resolves_once_and_retries_use_the_durable_seed() {
    use rsi_agent_session_protocol::{ModelSelection, SpawnRoleReference, SpawnRoleSeed};
    use rsi_agent_turn_protocol::{SpawnRoleResolver, SpawnRoleSelection};
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Debug)]
    struct Resolver {
        calls: AtomicUsize,
        seed: std::sync::Mutex<Option<SpawnRoleSeed>>,
    }
    #[async_trait::async_trait]
    impl SpawnRoleResolver for Resolver {
        async fn resolve(
            &self,
            _: &SessionHeader,
            _: &SpawnRoleReference,
            _: CancellationToken,
        ) -> rsi_agent_turn_protocol::Result<SpawnRoleSeed> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seed
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| TurnError::Invalid("definition removed".into()))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap());
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("named-parent")),
            message: mailbox_message("named-parent-message"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let lease = kernel.register("named-worker".into()).unwrap();
    let claim = kernel
        .claim("named-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = control_tool_caller_with_snapshot(&kernel, &claim, snapshot()).await;
    let reference = SpawnRoleReference {
        provider: "fixture.agents".into(),
        name: "reviewer".into(),
    };
    let resolver = Arc::new(Resolver {
        calls: AtomicUsize::new(0),
        seed: std::sync::Mutex::new(Some(SpawnRoleSeed {
            reference: reference.clone(),
            role: rsi_agent_session_protocol::DelegationRole {
                name: "reviewer".into(),
                persona: Some("first instructions".into()),
                allow: Some(std::collections::BTreeSet::default()),
                deny: std::collections::BTreeSet::default(),
            },
            model: Some(ModelSelection {
                model: rsi_ai_protocol::ModelRef::new("role", "chosen").unwrap(),
                reasoning_effort: None,
            }),
            source: "fixture/reviewer.md".into(),
            sha256: "a".repeat(64),
        })),
    });
    let request = SpawnAgentRequest {
        output_contract: None,
        role: Some(SpawnRoleSelection::Reference {
            reference,
            resolver: resolver.clone(),
        }),
        model: None,
        reasoning_effort: None,
        cancellation: CancellationToken::new(),
        caller,
        child_session_id: SessionId::new("named-child").unwrap(),
        task_name: "named-child".into(),
        message_id: MessageId::new("named-message").unwrap(),
        message: "review this".into(),
        fork_turns: ForkTurnSelection::None,
    };
    let (first, concurrent) = tokio::join!(
        kernel.spawn_agent(request.clone()),
        kernel.spawn_agent(request.clone())
    );
    let first = first.unwrap();
    assert_eq!(concurrent.unwrap(), first);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    let header = store.header(&first.session_id).await.unwrap();
    assert_eq!(header.settings().default_model().model(), "chosen");
    assert_eq!(
        header.delegation_policy().unwrap().persona(),
        Some("first instructions")
    );
    let restored: SessionHeader =
        serde_json::from_slice(&serde_json::to_vec(&header).unwrap()).unwrap();
    assert_eq!(restored, header);
    {
        let mut seed = resolver.seed.lock().unwrap();
        let seed = seed.as_mut().unwrap();
        seed.role.persona = Some("edited instructions".into());
        seed.sha256 = "b".repeat(64);
    }
    let mut edited = request.clone();
    edited.child_session_id = SessionId::new("edited-child").unwrap();
    edited.task_name = "edited-child".into();
    edited.message_id = MessageId::new("edited-message").unwrap();
    edited.model = Some(rsi_ai_protocol::ModelRef::new("explicit", "override").unwrap());
    let edited = kernel.spawn_agent(edited).await.unwrap();
    let edited_header = store.header(&edited.session_id).await.unwrap();
    assert_eq!(
        edited_header.delegation_policy().unwrap().persona(),
        Some("edited instructions")
    );
    assert_eq!(edited_header.settings().default_model().model(), "override");
    assert_eq!(
        header.delegation_policy().unwrap().persona(),
        Some("first instructions")
    );
    *resolver.seed.lock().unwrap() = None;
    assert_eq!(kernel.spawn_agent(request.clone()).await.unwrap(), first);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
    for (index, mut changed) in [
        request.clone(),
        request.clone(),
        request.clone(),
        request.clone(),
    ]
    .into_iter()
    .enumerate()
    {
        match index {
            0 => changed.message.push_str(" changed"),
            1 => changed.model = Some(rsi_ai_protocol::ModelRef::new("other", "model").unwrap()),
            2 => changed.fork_turns = ForkTurnSelection::All,
            _ => {
                let Some(SpawnRoleSelection::Reference { reference, .. }) = &mut changed.role
                else {
                    panic!("reference")
                };
                reference.name = "different".into();
            }
        }
        assert!(kernel.spawn_agent(changed).await.is_err());
    }
    let mut fresh = request.clone();
    fresh.child_session_id = SessionId::new("second-child").unwrap();
    fresh.task_name = "second-child".into();
    fresh.message_id = MessageId::new("second-message").unwrap();
    assert!(kernel.spawn_agent(fresh).await.is_err());
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
    assert!(
        store
            .header(&SessionId::new("second-child").unwrap())
            .await
            .is_err()
    );
    finish_control_tool(&kernel, &claim).await;
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
    drop((claim, request, kernel, store));
    rsi_agent_store_sqlite::SqliteStore::verify(directory.path()).unwrap();
    let reopened = Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap());
    assert_eq!(reopened.header(&first.session_id).await.unwrap(), header);
    assert_eq!(
        reopened.header(&edited.session_id).await.unwrap(),
        edited_header
    );
    let cold = AgentKernel::recover_with_clock(reopened, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let workers = cold.start_workers();
    let _lease = cold.register("cold-worker".into()).unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..2 {
        let claim = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            cold.claim("cold-worker", CancellationToken::new()),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "cold child claim timed out: seen={seen:?} health={:?}",
                cold.ready_health()
            )
        })
        .unwrap()
        .unwrap();
        let expected = if claim.session_id() == &first.session_id {
            &header
        } else {
            &edited_header
        };
        assert_eq!(claim.header().spawn_role(), expected.spawn_role());
        assert_eq!(
            claim.header().settings().default_model(),
            expected.settings().default_model()
        );
        assert_eq!(
            claim.header().delegation_policy(),
            expected.delegation_policy()
        );
        seen.insert(claim.session_id().clone());
        cold.finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
    }
    assert_eq!(seen, [first.session_id, edited.session_id].into());
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
    cold.shutdown(workers).await.unwrap();
}
