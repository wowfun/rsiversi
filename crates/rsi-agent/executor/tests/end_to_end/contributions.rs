use super::*;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionCatalog, ContributionContext, ContributionError,
    ContributionInput, ContributionKind, ContributionOutput, ContributionRegistration,
    ContributionResult, PostToolContributor, ToolPolicy, ToolPolicyDecision, ToolPolicyRequest,
};
use rsi_agent_session_protocol::{ContributionId, InputMessageSource};

#[derive(Debug)]
struct CallbacksFactory(Vec<ContributionRegistration>, Arc<CompositionFixture>);
#[async_trait]
impl PluginFactory for CallbacksFactory {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let context = plan.context().registration_context()?;
        let mut entries = Vec::new();
        let mut leases = Vec::new();
        for entry in &self.0 {
            let (position, lease) = context.register("fixture callback", || Ok(()), Ok)?;
            entries.push((entry.clone(), position));
            leases.push(lease);
        }
        *self.1.contributions.lock().unwrap() = ContributionCatalog::freeze(entries).unwrap();
        plan.defer(
            "withdraw callbacks",
            Box::new(move || {
                Box::pin(async move {
                    drop(leases);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn provider_retry_reuses_the_committed_sample() {
    let stack = BaseStack::activate().await;
    let sample = Arc::new(SampleTime::default());
    let callbacks = install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.time").unwrap(),
            0,
            ContributionKind::Context(sample.clone()),
        )],
    )
    .await;
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Error(
                AiError::new(
                    ErrorKind::RateLimited,
                    ErrorPhase::Connect,
                    DispatchStatus::NotDispatched,
                    "retry",
                )
                .unwrap(),
            ),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::new(1, vec![ErrorKind::RateLimited], 1, 1, 0).unwrap(),
    });
    let language_fiber = stack
        .activate_language("test.language", language.clone())
        .await;
    let executor = stack
        .activate_executor("executor-retry-contributions")
        .await;
    let (submitted, outcome) = stack.submit_and_wait("work").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(sample.0.load(Ordering::Acquire), 1);
    assert_eq!(language.starts.load(Ordering::Acquire), 2);
    for request in language.requests.lock().unwrap().iter() {
        let wire = serde_json::to_string(request).unwrap();
        assert!(wire.contains("sample 0"));
        assert!(!wire.contains("sample 1"));
    }
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageEntered {
                    source: InputMessageSource::PluginContext { .. },
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}

#[derive(Debug)]
struct BrokenContext;
#[async_trait]
impl ContextContributor for BrokenContext {
    async fn contribute(
        &self,
        _: &ContributionContext,
        _: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        Err(ContributionError::Invalid("fixture failure".into()))
    }
}

#[tokio::test]
async fn later_callback_failure_discards_the_entire_stage_before_provider_io() {
    let stack = BaseStack::activate().await;
    let sample = Arc::new(SampleTime::default());
    let callbacks = install(
        &stack,
        vec![
            ContributionRegistration::new(
                ContributionId::new("a.sample").unwrap(),
                0,
                ContributionKind::Context(sample.clone()),
            ),
            ContributionRegistration::new(
                ContributionId::new("b.broken").unwrap(),
                0,
                ContributionKind::Context(Arc::new(BrokenContext)),
            ),
        ],
    )
    .await;
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::new()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language", language.clone())
        .await;
    let executor = stack
        .activate_executor("executor-broken-contributions")
        .await;
    let (submitted, outcome) = stack.submit_and_wait("work").await;
    assert!(
        matches!(outcome, TurnOutcome::Failed { ref code, ref message } if code == "contribution.failed" && message.contains("b.broken/BeforeStep"))
    );
    assert_eq!(sample.0.load(Ordering::Acquire), 1);
    assert_eq!(language.starts.load(Ordering::Acquire), 0);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    assert!(!facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::InputMessageEntered { .. } | SessionFactBody::ModelIntent { .. }
    )));
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}

#[derive(Debug)]
struct Decision(ToolPolicyDecision, Arc<Mutex<Vec<bool>>>);
#[async_trait]
impl ToolPolicy for Decision {
    async fn decide(
        &self,
        _: &ContributionContext,
        request: &ToolPolicyRequest<'_>,
        _: CancellationToken,
    ) -> ContributionResult<ToolPolicyDecision> {
        assert_eq!(request.name, "echo");
        assert_eq!(request.arguments, &json!({"value":42}));
        self.1.lock().unwrap().push(request.require_approval);
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn policy_constraints_accumulate_and_denial_never_starts_the_tool() {
    for deny_policy in [false, true] {
        let stack = BaseStack::activate_with_approval(ApprovalDecision::Deny).await;
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut entries = vec![
            ContributionRegistration::new(
                ContributionId::new("a.require").unwrap(),
                0,
                ContributionKind::ToolPolicy(Arc::new(Decision(
                    ToolPolicyDecision::RequireApproval,
                    observed.clone(),
                ))),
            ),
            ContributionRegistration::new(
                ContributionId::new("b.abstain").unwrap(),
                0,
                ContributionKind::ToolPolicy(Arc::new(Decision(
                    ToolPolicyDecision::Abstain,
                    observed.clone(),
                ))),
            ),
        ];
        if deny_policy {
            entries.push(ContributionRegistration::new(
                ContributionId::new("c.deny").unwrap(),
                0,
                ContributionKind::ToolPolicy(Arc::new(Decision(
                    ToolPolicyDecision::Deny {
                        reason: "Plan mode forbids echo".into(),
                    },
                    observed.clone(),
                ))),
            ));
        }
        let callbacks = install(&stack, entries).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let lease = stack
            .tool_registrar
            .register(ToolRegistration {
                definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"}))
                    .unwrap(),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
                executor: Arc::new(EchoTool {
                    store: stack.store.clone(),
                    calls: calls.clone(),
                }),
            })
            .unwrap();
        let language = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language_fiber = stack.activate_language("test.language", language).await;
        let executor = stack.activate_executor("executor-policies").await;
        let (submitted, outcome) = stack.submit_and_wait("work").await;
        let expected = if deny_policy {
            "policy.denied"
        } else {
            "approval.denied"
        };
        assert!(matches!(outcome, TurnOutcome::Failed { ref code, .. } if code == expected));
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert_eq!(
            *observed.lock().unwrap(),
            if deny_policy {
                vec![false, true, true]
            } else {
                vec![false, true]
            }
        );
        let facts = stack
            .store
            .read_facts(&submitted.session_id, 0, 64)
            .await
            .unwrap()
            .facts;
        assert_eq!(
            facts
                .iter()
                .filter(|fact| matches!(fact.body(), SessionFactBody::ToolRejected { .. }))
                .count(),
            1
        );
        assert!(!facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::ToolIntent { .. } | SessionFactBody::ToolStarted { .. }
        )));
        drop(lease);
        assert!(callbacks.dispose().await.is_clean());
        stack.dispose(language_fiber, executor).await;
    }
}

#[derive(Debug, Default)]
struct Settled(AtomicUsize);
#[async_trait]
impl PostToolContributor for Settled {
    async fn contribute(
        &self,
        context: &ContributionContext,
        settled: &[Arc<SessionFact>],
        _: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        assert!(
            settled
                .iter()
                .all(|fact| fact.seq() <= context.horizon.fact_seq)
        );
        let names: Vec<_> = settled
            .iter()
            .map(|fact| match fact.body() {
                SessionFactBody::ToolResult { identity, .. } => identity.call_id(),
                _ => panic!("post-tool input must be a settled result"),
            })
            .collect();
        assert_eq!(names, ["call-a", "call-b"]);
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context("Use the settled evidence")],
            domains: vec![],
        })
    }
}

#[tokio::test]
async fn post_tool_contribution_observes_one_source_ordered_durable_batch() {
    let stack = BaseStack::activate().await;
    let settled = Arc::new(Settled::default());
    let callbacks = install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.settled").unwrap(),
            0,
            ContributionKind::PostTool(settled.clone()),
        )],
    )
    .await;
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[
                ("call-a", "echo", "{\"value\":42}"),
                ("call-b", "echo", "{\"value\":42}"),
            ])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language", language.clone())
        .await;
    let executor = stack.activate_executor("executor-settled").await;
    let (_, outcome) = stack.submit_and_wait("work").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(settled.0.load(Ordering::Acquire), 1);
    assert!(
        serde_json::to_string(&language.requests.lock().unwrap()[1])
            .unwrap()
            .contains("Use the settled evidence")
    );
    drop(lease);
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}

pub(super) async fn install(
    stack: &BaseStack,
    callbacks: Vec<ContributionRegistration>,
) -> FiberHandle {
    stack
        .runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fixture.callbacks",
                "v1",
                UpdateMode::Replayable,
                Arc::new(CallbacksFactory(callbacks, stack.composition.clone())),
            ),
            Value::Null,
        )
        .await
        .unwrap()
}

#[derive(Debug, Default)]
struct SampleTime(AtomicUsize);
#[async_trait]
impl ContextContributor for SampleTime {
    async fn contribute(
        &self,
        _: &ContributionContext,
        _: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        let value = self.0.fetch_add(1, Ordering::AcqRel);
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context(format!("sample {value}"))],
            domains: vec![],
        })
    }
}

#[derive(Debug, Default)]
struct PendingContext {
    entered: Notify,
    captured: Mutex<Option<(ContributionContext, CancellationToken)>>,
}
#[async_trait]
impl ContextContributor for PendingContext {
    async fn contribute(
        &self,
        context: &ContributionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        *self.captured.lock().unwrap() = Some((context.clone(), cancellation));
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_callback_hits_the_stage_deadline_and_revokes_captured_access() {
    let stack = BaseStack::activate().await;
    let pending = Arc::new(PendingContext::default());
    let callbacks = install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.pending").unwrap(),
            0,
            ContributionKind::Context(pending.clone()),
        )],
    )
    .await;
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::new()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.language", language.clone())
        .await;
    let executor = stack
        .activate_executor("executor-pending-contributions")
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = stack
        .submit_fresh(&turns, "stalled-contribution", "work")
        .await;
    pending.entered.notified().await;
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), async {
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
    assert!(
        matches!(outcome, TurnOutcome::Failed { ref code, ref message } if code == "contribution.timeout" && message.contains("fixture.pending/BeforeStep"))
    );
    let (context, token) = pending.captured.lock().unwrap().take().unwrap();
    assert!(token.is_cancelled());
    assert!(context.facts.read(0, 1).await.is_err());
    assert_eq!(language.starts.load(Ordering::Acquire), 0);
    drop(context);
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}

#[tokio::test]
async fn actual_contribution_is_durable_before_the_first_provider_request() {
    let stack = BaseStack::activate().await;
    let sample = Arc::new(SampleTime::default());
    let callbacks = install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.time").unwrap(),
            0,
            ContributionKind::Context(sample.clone()),
        )],
    )
    .await;
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
    let executor = stack.activate_executor("executor-contributions").await;
    let (submitted, outcome) = stack.submit_and_wait("work").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(sample.0.load(Ordering::Acquire), 1);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    let input_index = facts
        .iter()
        .position(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::InputMessageEntered {
                    source: InputMessageSource::PluginContext { .. },
                    ..
                }
            )
        })
        .unwrap();
    let intent_index = facts
        .iter()
        .position(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
        .unwrap();
    assert!(input_index < intent_index);
    let wire = serde_json::to_string(&language.requests.lock().unwrap()[0]).unwrap();
    assert!(wire.contains("sample 0"));
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}
