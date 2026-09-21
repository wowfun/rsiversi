use super::*;
use rsi_agent_composition_protocol::{
    ContributionKind, ContributionRegistration, DomainCatalog, DomainDefinition, DomainHandle,
    ToolSettlementContext, ToolSettlementContributor,
};
use rsi_agent_session_protocol::{ContributionId, DomainIdentity, EffectId};

#[derive(Debug)]
struct Settlement {
    conclude: bool,
    state: DomainHandle<u64>,
    calls: AtomicUsize,
}
impl ToolSettlementContributor for Settlement {
    fn settle(
        &self,
        context: &ToolSettlementContext<'_>,
    ) -> rsi_agent_composition_protocol::ContributionResult<
        rsi_agent_composition_protocol::ToolSettlement,
    > {
        assert!(
            matches!(context.intent.body(),SessionFactBody::ToolIntent{name,..} if name=="echo")
        );
        assert!(!context.result.is_error);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let view = &context.domains[0];
        let count = self.state.decode(&view.snapshot).unwrap();
        Ok(rsi_agent_composition_protocol::ToolSettlement {
            domains: vec![self.state.propose(view.revision, &(count + 1)).unwrap()],
            conclusion: self
                .conclude
                .then_some(rsi_agent_session_protocol::ToolConclusion { structured: None }),
        })
    }
}

async fn setup(stack: &BaseStack) -> (Arc<Settlement>, FiberHandle) {
    setup_conclusion(stack, false).await
}

async fn setup_conclusion(stack: &BaseStack, conclude: bool) -> (Arc<Settlement>, FiberHandle) {
    let definition = DomainDefinition::new(
        DomainIdentity::new("fixture.settlement", 1).unwrap(),
        &0_u64,
        |_: &u64| Ok(()),
    )
    .unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let callback = Arc::new(Settlement {
        conclude,
        state: domains.bind(&definition).unwrap(),
        calls: AtomicUsize::new(0),
    });
    *stack.composition.domains.lock().unwrap() = domains;
    let fiber = contributions::install(
        stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.settlement").unwrap(),
            0,
            ContributionKind::ToolSettlement(callback.clone()),
        )],
    )
    .await;
    (callback, fiber)
}

#[tokio::test]
async fn serial_and_parallel_results_invoke_settlement_once_per_effect() {
    for parallel in [false, true] {
        let stack = BaseStack::activate().await;
        let (callback, callbacks) = setup(&stack).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let lease = stack
            .tool_registrar
            .register(ToolRegistration {
                output: None,
                definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"}))
                    .unwrap()
                    .with_scheduling(if parallel {
                        rsi_tools_protocol::ToolScheduling::ParallelSafe
                    } else {
                        rsi_tools_protocol::ToolScheduling::Exclusive
                    }),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2000 },
                executor: Arc::new(EchoTool {
                    store: stack.store.clone(),
                    calls: calls.clone(),
                }),
            })
            .unwrap();
        let script = if parallel {
            tool_calls_script(&[("a", "echo", "{}"), ("b", "echo", "{}")])
        } else {
            tool_script()
        };
        let language = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from([
                StartOutcome::Stream(script),
                StartOutcome::Stream(answer_script()),
            ])),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language_fiber = stack.activate_language("test.language", language).await;
        let executor = stack.activate_executor("settlement-effects").await;
        let (submitted, outcome) = stack.submit_and_wait("work").await;
        assert_eq!(outcome, TurnOutcome::Completed);
        let expected = if parallel { 2 } else { 1 };
        assert_eq!(calls.load(Ordering::SeqCst), expected);
        assert_eq!(callback.calls.load(Ordering::SeqCst), expected);
        let page = stack
            .store
            .read_domain_states(&submitted.session_id, None)
            .await
            .unwrap();
        assert_eq!(
            callback.state.decode(&page.states[0].snapshot).unwrap(),
            expected as u64
        );
        let facts = stack
            .store
            .read_facts(&submitted.session_id, 0, 64)
            .await
            .unwrap()
            .facts;
        let results = facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ToolResult { .. }))
            .collect::<Vec<_>>();
        assert_eq!(results.len(), expected);
        let controls = stack
            .store
            .read_controls(&submitted.session_id, 0, 64)
            .await
            .unwrap()
            .records;
        for fact in results {
            assert!(controls.iter().any(|record|matches!(record.body(),rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted{commit} if commit.fact_span().is_some_and(|span|span.first_seq()==fact.seq() && span.count()==1))));
        }
        drop(lease);
        assert!(callbacks.dispose().await.is_clean());
        stack.dispose(language_fiber, executor).await;
    }
}

async fn publish(execution: &dyn TurnExecution, claim: &TurnClaim, bodies: Vec<SessionFactBody>) {
    let PublishAttempt::Published(facts) = execution.publish(claim, bodies).await.unwrap() else {
        panic!("bounded fixture batch");
    };
    execution
        .flush(claim, facts.last().unwrap().seq())
        .await
        .unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One retained result identity crosses ambiguous settlement and recovery.
async fn a_retained_returned_result_settles_without_reexecuting_the_tool() {
    let stack = BaseStack::activate().await;
    let (callback, callbacks) = setup(&stack).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let tool_lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: calls.clone(),
            }),
        })
        .unwrap();
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language", language.clone())
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let execution = stack
        .runtime
        .root()
        .lookup_local::<TurnExecutionContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            session: stack.fresh(header()).await,
            turn_id: TurnId::new("retained-settlement").unwrap(),
            text: "work".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let lease = execution.register("fixture-source".into()).unwrap();
    let claim = execution
        .claim("fixture-source", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let composition = execution.composition(&claim).unwrap();
    let model = claim.header().settings().default_model();
    let source = EffectId::new("source-model").unwrap();
    let snapshot = PreparedCallSnapshot {
        call_id: "source".into(),
        deployment_id: model.deployment().into(),
        model: model.model().into(),
        provider_family: "fixture".into(),
        capability: AiCapability::Language,
        protocol: "fixture".into(),
        transport: "memory".into(),
        endpoint_fingerprint: "fixture".into(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "a".repeat(64),
        language_settings: Some(
            rsi_ai_protocol::PreparedLanguageSettings::new(
                language.describe(model).unwrap().into_profile(),
                None,
            )
            .unwrap(),
        ),
    };
    publish(
        execution.as_ref(),
        &claim,
        vec![SessionFactBody::ModelIntent {
            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
            },
            price_quote: None,
            turn_id: claim.turn_id().clone(),
            effect_id: source.clone(),
            purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
            snapshot,
        }],
    )
    .await;
    publish(
        execution.as_ref(),
        &claim,
        vec![SessionFactBody::ModelStarted {
            turn_id: claim.turn_id().clone(),
            effect_id: source.clone(),
        }],
    )
    .await;
    publish(
        execution.as_ref(),
        &claim,
        tool_script()
            .into_iter()
            .map(|event| SessionFactBody::ModelEvent {
                turn_id: claim.turn_id().clone(),
                effect_id: source.clone(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event,
            })
            .collect(),
    )
    .await;
    let effect = EffectId::new("retained-tool").unwrap();
    let prepared = composition
        .tools()
        .prepare(
            effect.as_str(),
            rsi_tools_protocol::ToolCall {
                id: "tool-call-1".into(),
                name: "echo".into(),
                arguments: json!({"value":42}),
            },
        )
        .unwrap();
    let identity = prepared.identity().clone();
    publish(
        execution.as_ref(),
        &claim,
        vec![SessionFactBody::ToolIntent {
            turn_id: claim.turn_id().clone(),
            effect_id: effect.clone(),
            source_model_effect_id: source,
            identity: identity.clone(),
            name: "echo".into(),
            arguments: json!({"value":42}),
            approval: None,
            parallel_safe: false,
        }],
    )
    .await;
    publish(
        execution.as_ref(),
        &claim,
        vec![SessionFactBody::ToolStarted {
            turn_id: claim.turn_id().clone(),
            effect_id: effect,
            identity: identity.clone(),
        }],
    )
    .await;
    prepared
        .start(rsi_tools_protocol::ToolStart {
            cancellation: CancellationToken::new(),
            policy: rsi_tools_protocol::ToolExecutionPolicy {
                mode: SandboxMode::WorkspaceWrite,
                cwd: "/workspace".into(),
                workspace: "/workspace".into(),
            },
            sandbox: Arc::new(TestSandbox),
            job_scope: None,
            extensions: rsi_tools_protocol::ToolExecutionExtensions::default(),
        })
        .await
        .unwrap();
    assert!(matches!(
        composition.tools().query(&identity).unwrap(),
        RetainedToolResult::Returned(_)
    ));
    execution.release(&claim).unwrap();
    drop(lease);
    let executor = stack.activate_executor("recover-settlement").await;
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(callback.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        composition.tools().query(&identity).unwrap(),
        RetainedToolResult::Absent
    ));
    let page = stack
        .store
        .read_domain_states(&submitted.session_id, None)
        .await
        .unwrap();
    assert_eq!(callback.state.decode(&page.states[0].snapshot).unwrap(), 1);
    drop(tool_lease);
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}

#[tokio::test]
async fn conclusion_and_domain_update_share_the_exact_result_commit() {
    let stack = BaseStack::activate().await;
    let (callback, callbacks) = setup_conclusion(&stack, true).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: calls.clone(),
            }),
        })
        .unwrap();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_calls_script(
            &[("first", "echo", "{}"), ("never", "echo", "{}")],
        ))])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("concluding.provider", provider.clone())
        .await;
    let executor = stack.activate_executor("concluding-executor").await;
    let (submitted, outcome) = stack.submit_and_wait("conclude after one result").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(callback.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    let fact = facts
        .iter()
        .find(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::ToolResult {
                    conclusion: Some(_),
                    ..
                }
            )
        })
        .unwrap();
    let controls = stack
        .store
        .read_controls(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .records;
    assert!(controls.iter().any(|record| matches!(record.body(), rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted { commit } if commit.fact_span().is_some_and(|span| span.first_seq() == fact.seq() && span.count() == 1))));
    drop(lease);
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language, executor).await;
}
