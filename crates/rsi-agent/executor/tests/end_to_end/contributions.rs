use super::*;
use rsi_agent_composition_protocol::{
    ContextContributor, ContributionCatalog, ContributionContext, ContributionError,
    ContributionInput, ContributionKind, ContributionOutput, ContributionRegistration,
    ContributionResult, PostToolContributor, ToolPolicy, ToolPolicyDecision, ToolPolicyRequest,
};
use rsi_agent_session_protocol::{ContributionId, InputMessageSource};
use std::time::Duration;

#[derive(Debug)]
struct ToolSubset {
    names: Vec<String>,
    sampled: AtomicUsize,
}
#[async_trait]
impl rsi_agent_composition_protocol::BeforeStepContributor for ToolSubset {
    async fn prepare(
        &self,
        context: &rsi_agent_composition_protocol::BeforeStepContext<'_>,
        _: CancellationToken,
    ) -> ContributionResult<rsi_agent_composition_protocol::BeforeStepOutput> {
        assert!(context.tools.iter().any(|name| name == "echo"));
        self.sampled.fetch_add(1, Ordering::AcqRel);
        Ok(rsi_agent_composition_protocol::BeforeStepOutput {
            decision: None,
            instructions: String::new(),
            contribution: ContributionOutput {
                inputs: vec![ContributionInput::context("selected Tools")],
                domains: vec![],
            },
            tools: Some(self.names.clone()),
            publications: vec![],
        })
    }
}

#[tokio::test]
async fn before_step_subset_is_durable_and_excludes_provider_hallucinated_tools() {
    for names in [vec![], vec!["unregistered".to_owned()]] {
        let stack = BaseStack::activate().await;
        let subset = Arc::new(ToolSubset {
            names: names.clone(),
            sampled: AtomicUsize::new(0),
        });
        let callbacks = install(
            &stack,
            vec![ContributionRegistration::new(
                ContributionId::new("fixture.subset").unwrap(),
                0,
                ContributionKind::BeforeStep(subset.clone()),
            )],
        )
        .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let lease = stack
            .tool_registrar
            .register(ToolRegistration {
                definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
                output: None,
                executor: Arc::new(EchoTool {
                    store: stack.store.clone(),
                    calls: calls.clone(),
                }),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1000 },
            })
            .unwrap();
        let language = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language_fiber = stack
            .activate_language("test.subset", language.clone())
            .await;
        let executor = stack.activate_executor("subset-executor").await;
        let (submitted, outcome) = stack.submit_and_wait("use only selected Tools").await;
        assert!(
            matches!(outcome, TurnOutcome::Failed { code, .. } if code == if names.is_empty() {"tool.not_admitted"} else {"contribution.failed"})
        );
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert_eq!(subset.sampled.load(Ordering::Acquire), 1);
        let facts = stack
            .store
            .read_facts(
                &submitted.session_id,
                0,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap()
            .facts;
        assert_eq!(
            facts
                .iter()
                .filter(|fact| matches!(fact.body(), SessionFactBody::StepPrepared { .. }))
                .count(),
            usize::from(names.is_empty())
        );
        assert!(!facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::ToolIntent { .. } | SessionFactBody::ToolStarted { .. }
        )));
        if names.is_empty() {
            assert!(language.requests.lock().unwrap()[0].tools().is_empty());
        } else {
            assert_eq!(language.starts.load(Ordering::Acquire), 0);
        }
        drop(lease);
        assert!(callbacks.dispose().await.is_clean());
        stack.dispose(language_fiber, executor).await;
    }
}

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
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageClaimed {
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

#[derive(Debug, Default)]
struct EnteredTime(Mutex<Vec<Option<u64>>>);
#[async_trait]
impl rsi_agent_composition_protocol::BeforeStepContributor for EnteredTime {
    async fn prepare(
        &self,
        context: &rsi_agent_composition_protocol::BeforeStepContext<'_>,
        _: CancellationToken,
    ) -> ContributionResult<rsi_agent_composition_protocol::BeforeStepOutput> {
        self.0.lock().unwrap().push(context.previous_input_at_ms);
        Ok(rsi_agent_composition_protocol::BeforeStepOutput {
            instructions: "One current instruction".into(),
            contribution: ContributionOutput {
                inputs: if context.previous_input_at_ms.is_none() {
                    vec![ContributionInput::context("one committed time sample")]
                } else {
                    vec![]
                },
                domains: vec![],
            },
            ..Default::default()
        })
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One Tool round trip checks retained input time, instruction storage and both model requests.
async fn before_step_receives_its_entered_input_time_after_tool_round_trip() {
    let stack = BaseStack::activate().await;
    let sample = Arc::new(EnteredTime::default());
    let callbacks = install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.entered-time").unwrap(),
            0,
            ContributionKind::BeforeStep(sample.clone()),
        )],
    )
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
            output: None,
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: calls.clone(),
            }),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 1000 },
        })
        .unwrap();
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_script()),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.entered-time", language.clone())
        .await;
    let executor = stack.activate_executor("entered-time-executor").await;
    let (submitted, outcome) = stack.submit_and_wait("work").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::Acquire), 1);
    let facts = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    let samples: Vec<_> = facts
        .iter()
        .filter(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::InputMessageClaimed {
                    source: InputMessageSource::PluginContext { .. },
                    ..
                }
            )
        })
        .collect();
    assert_eq!(samples.len(), 1);
    let instructions = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::StepPrepared {
                instructions,
                names,
                ..
            } => {
                assert_eq!(names, &["echo"]);
                Some(instructions)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(instructions.len(), 2);
    assert_eq!(
        instructions[0].as_ref().unwrap()[0].text,
        "One current instruction"
    );
    assert!(
        instructions[1].is_none(),
        "unchanged preparation must not store the same instruction again"
    );
    assert_eq!(
        *sample.0.lock().unwrap(),
        vec![None, Some(samples[0].timestamp_ms())]
    );
    for request in language.requests.lock().unwrap().iter() {
        let encoded = serde_json::to_string(request).unwrap();
        for marker in ["One current instruction", "one committed time sample"] {
            assert_eq!(encoded.matches(marker).count(), 1);
        }
    }
    drop(lease);
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
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert!(!facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::InputMessageClaimed { .. } | SessionFactBody::ModelIntent { .. }
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
#[allow(clippy::too_many_lines)] // Keep accumulated policy, refused execution and the next model request in one scenario.
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
                output: None,
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
            outcomes: Mutex::new(VecDeque::from([
                StartOutcome::Stream(tool_script()),
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
        let executor = stack.activate_executor("executor-policies").await;
        let (submitted, outcome) = stack.submit_and_wait("work").await;
        assert_eq!(outcome, TurnOutcome::Completed);
        {
            let requests = language.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_complete_tool_groups(&requests[1]);
            let reply = serde_json::to_value(&requests[1]).unwrap();
            assert!(reply.to_string().contains(if deny_policy {
                "Plan mode forbids echo"
            } else {
                "denied"
            }));
        }
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
            .read_facts(
                &submitted.session_id,
                0,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
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

#[derive(Debug, Default)]
struct RefusalObserver(Mutex<Vec<String>>);

#[derive(Debug, Default)]
struct HeldRefusal(CancellationToken);
#[async_trait]
impl ToolPolicy for HeldRefusal {
    async fn decide(
        &self,
        _: &ContributionContext,
        _: &ToolPolicyRequest<'_>,
        _: CancellationToken,
    ) -> ContributionResult<ToolPolicyDecision> {
        Ok(ToolPolicyDecision::Deny {
            reason: "refusal before user stop".into(),
        })
    }
}
#[async_trait]
impl PostToolContributor for HeldRefusal {
    async fn contribute(
        &self,
        _: &ContributionContext,
        settled: &[Arc<SessionFact>],
        _: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        assert!(matches!(
            settled[0].body(),
            SessionFactBody::ToolRejected { .. }
        ));
        self.0.cancel();
        std::future::pending().await
    }
}

#[tokio::test]
async fn user_stop_after_tool_refusal_does_not_issue_another_model_request() {
    let stack = BaseStack::activate().await;
    let held = Arc::new(HeldRefusal::default());
    let callbacks = install(
        &stack,
        vec![
            ContributionRegistration::new(
                ContributionId::new("fixture.refusal").unwrap(),
                0,
                ContributionKind::ToolPolicy(held.clone()),
            ),
            ContributionRegistration::new(
                ContributionId::new("fixture.hold").unwrap(),
                0,
                ContributionKind::PostTool(held.clone()),
            ),
        ],
    )
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: calls.clone(),
            }),
        })
        .unwrap();
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(tool_script())])),
        requests: Mutex::default(),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.stop-refusal", language.clone())
        .await;
    let executor = stack.activate_executor("executor-stop-refusal").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = stack
        .submit_fresh(&turns, "session-e2e", "stop after refusal")
        .await;
    tokio::time::timeout(Duration::from_secs(2), held.0.cancelled())
        .await
        .unwrap();
    turns
        .cancel(
            &submitted.session_id,
            &submitted.turn_id,
            Some("user stopped".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        wait_for_outcome(&turns, &submitted).await,
        TurnOutcome::Cancelled
    );
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert_eq!(language.starts.load(Ordering::Acquire), 1);
    let facts = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert!(
        facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::ToolRejected { .. }))
    );
    assert!(!facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ToolStarted { .. } | SessionFactBody::ToolResult { .. }
    )));
    drop(lease);
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}
#[async_trait]
impl ToolPolicy for RefusalObserver {
    async fn decide(
        &self,
        _: &ContributionContext,
        request: &ToolPolicyRequest<'_>,
        _: CancellationToken,
    ) -> ContributionResult<ToolPolicyDecision> {
        Ok(if request.arguments["deny"] == true {
            ToolPolicyDecision::Deny {
                reason: "fixture refuses this call".into(),
            }
        } else {
            ToolPolicyDecision::Abstain
        })
    }
}
#[async_trait]
impl PostToolContributor for RefusalObserver {
    async fn contribute(
        &self,
        _: &ContributionContext,
        settled: &[Arc<SessionFact>],
        _: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        self.0
            .lock()
            .unwrap()
            .extend(settled.iter().map(|fact| match fact.body() {
                SessionFactBody::ToolRejected { identity, .. } => {
                    format!("rejected:{}", identity.call_id())
                }
                SessionFactBody::ToolResult { identity, .. } => {
                    format!("executed:{}", identity.call_id())
                }
                _ => panic!("only actual settlement Facts reach PostTool"),
            }));
        Ok(ContributionOutput::default())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The shared matrix checks source order across scheduling modes and mixed/all-denied batches.
async fn tool_refusals_settle_mixed_and_all_denied_batches_in_source_order() {
    use rsi_ai_protocol::MessageContent;
    for scheduling in [ToolScheduling::Exclusive, ToolScheduling::ParallelSafe] {
        for denied in [vec![true, false, true, false], vec![true, true]] {
            let stack = BaseStack::activate().await;
            let observer = Arc::new(RefusalObserver::default());
            let callbacks = install(
                &stack,
                vec![
                    ContributionRegistration::new(
                        ContributionId::new("fixture.refusal").unwrap(),
                        0,
                        ContributionKind::ToolPolicy(observer.clone()),
                    ),
                    ContributionRegistration::new(
                        ContributionId::new("fixture.observe").unwrap(),
                        0,
                        ContributionKind::PostTool(observer.clone()),
                    ),
                ],
            )
            .await;
            let calls = Arc::new(AtomicUsize::new(0));
            let lease = stack
                .tool_registrar
                .register(ToolRegistration {
                    output: None,
                    definition: ToolDefinition::new("echo", "echo", json!({"type":"object"}))
                        .unwrap()
                        .with_scheduling(scheduling),
                    timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
                    executor: Arc::new(EchoTool {
                        store: stack.store.clone(),
                        calls: calls.clone(),
                    }),
                })
                .unwrap();
            let ids: Vec<_> = (0..denied.len()).map(|i| format!("call-{i}")).collect();
            let args: Vec<_> = denied
                .iter()
                .map(|deny| json!({"deny":deny}).to_string())
                .collect();
            let batch: Vec<_> = ids
                .iter()
                .zip(&args)
                .map(|(id, args)| (id.as_str(), "echo", args.as_str()))
                .collect();
            let language = Arc::new(LanguageFixture {
                outcomes: Mutex::new(VecDeque::from([
                    StartOutcome::Stream(tool_calls_script(&batch)),
                    StartOutcome::Stream(answer_script()),
                ])),
                requests: Mutex::default(),
                starts: Arc::new(AtomicUsize::new(0)),
                store: stack.store.clone(),
                retry_policy: RetryPolicy::default(),
            });
            let language_fiber = stack
                .activate_language("test.refusals", language.clone())
                .await;
            let executor = stack.activate_executor("executor-refusals").await;
            let (submitted, outcome) = stack.submit_and_wait("handle each call").await;
            assert_eq!(outcome, TurnOutcome::Completed);
            assert_eq!(
                calls.load(Ordering::Acquire),
                denied.iter().filter(|deny| !**deny).count()
            );
            let expected: Vec<_> = ids
                .iter()
                .zip(&denied)
                .map(|(id, deny)| format!("{}:{id}", if *deny { "rejected" } else { "executed" }))
                .collect();
            assert_eq!(*observer.0.lock().unwrap(), expected);
            {
                let requests = language.requests.lock().unwrap();
                assert_eq!(requests.len(), 2);
                assert_complete_tool_groups(&requests[1]);
                let replies: Vec<_> = requests[1]
                    .messages()
                    .iter()
                    .flat_map(rsi_ai_protocol::Message::content)
                    .filter_map(|content| match content {
                        MessageContent::ToolResult {
                            call_id,
                            is_error,
                            content,
                        } => {
                            if *is_error {
                                assert!(
                                    serde_json::to_string(content)
                                        .unwrap()
                                        .contains("fixture refuses this call")
                                );
                            }
                            Some((call_id.as_str(), *is_error))
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    replies,
                    ids.iter()
                        .zip(&denied)
                        .map(|(id, deny)| (id.as_str(), *deny))
                        .collect::<Vec<_>>()
                );
            }
            let facts = stack
                .store
                .read_facts(
                    &submitted.session_id,
                    0,
                    128,
                    rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
                )
                .await
                .unwrap()
                .facts;
            for (id, deny) in ids.iter().zip(&denied) {
                assert_eq!(facts.iter().filter(|fact| matches!(fact.body(),SessionFactBody::ToolRejected { identity, .. } if identity.call_id()==id)).count(), usize::from(*deny));
                assert_eq!(facts.iter().filter(|fact| matches!(fact.body(),SessionFactBody::ToolStarted { identity, .. } if identity.call_id()==id)).count(), usize::from(!deny));
            }
            drop(lease);
            assert!(callbacks.dispose().await.is_clean());
            stack.dispose(language_fiber, executor).await;
        }
    }
}

#[derive(Debug)]
struct RacingReport {
    state: rsi_agent_composition_protocol::DomainHandle<bool>,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    calls: AtomicUsize,
}
#[async_trait]
impl PostToolContributor for RacingReport {
    async fn contribute(
        &self,
        context: &ContributionContext,
        _: &[Arc<SessionFact>],
        _: CancellationToken,
    ) -> ContributionResult<ContributionOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let proposal = self
            .state
            .propose(context.domains[0].revision, &false)
            .unwrap();
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        Ok(ContributionOutput {
            inputs: vec![ContributionInput::context(
                "stale report must not enter context",
            )],
            domains: vec![proposal],
        })
    }
}
#[async_trait]
impl rsi_agent_composition_protocol::SessionCommand for RacingReport {
    async fn execute(
        &self,
        context: &rsi_agent_composition_protocol::SessionCommandContext,
        _: &rsi_agent_session_protocol::CommandArguments,
        _: CancellationToken,
    ) -> ContributionResult<rsi_agent_composition_protocol::SessionCommandOutput> {
        Ok(vec![
            self.state
                .propose(context.domains[0].revision, &true)
                .unwrap(),
        ]
        .into())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the gated public command, captured report and terminal assertions in one race scenario.
async fn concurrent_command_drops_stale_post_tool_batch_without_failing_or_replaying_the_turn() {
    use rsi_agent_composition_protocol::{
        DomainCatalog, DomainDefinition, SessionCommandRegistration,
    };
    use rsi_agent_session_protocol::{
        CommandArguments, DomainIdentity, DomainRequestId, SessionCommandDescriptor,
        SessionCommandInvocation,
    };
    use rsi_agent_turn_protocol::SessionCommandsContract;
    let stack = BaseStack::activate().await;
    let definition = DomainDefinition::new(
        DomainIdentity::new("fixture.report", 1).unwrap(),
        &false,
        |_| Ok(()),
        rsi_agent_composition_protocol::DomainReferencePolicy::None,
    )
    .unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let report = Arc::new(RacingReport {
        state: domains.bind(&definition).unwrap(),
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    *stack.composition.domains.lock().unwrap() = domains;
    let command_id = ContributionId::new("fixture.pause").unwrap();
    let callbacks = install(
        &stack,
        vec![
            ContributionRegistration::new(
                ContributionId::new("fixture.report").unwrap(),
                0,
                ContributionKind::PostTool(report.clone()),
            ),
            ContributionRegistration::new(
                command_id.clone(),
                0,
                ContributionKind::Command(SessionCommandRegistration::new(
                    SessionCommandDescriptor::new(
                        command_id.clone(),
                        "pause",
                        "Pause report",
                        true,
                    )
                    .unwrap(),
                    report.clone(),
                )),
            ),
        ],
    )
    .await;
    let tool = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"})).unwrap(),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2000 },
            executor: Arc::new(EchoTool {
                store: stack.store.clone(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        })
        .unwrap();
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[("call-a", "echo", "{\"value\":42}")])),
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
    let executor = stack.activate_executor("executor-racing-report").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let commands = stack
        .runtime
        .root()
        .lookup_local::<SessionCommandsContract>()
        .unwrap();
    let submitted = stack.submit_fresh(&turns, "racing-report", "work").await;
    report.entered.acquire().await.unwrap().forget();
    let prepared = turns.prepare_resume(&submitted.session_id).await.unwrap();
    let revision = commands.list(prepared).await.unwrap().revision();
    commands
        .execute(
            turns.prepare_resume(&submitted.session_id).await.unwrap(),
            SessionCommandInvocation {
                command: command_id,
                request_id: DomainRequestId::new("pause-report").unwrap(),
                expected_revision: revision,
                arguments: CommandArguments::new(json!(true)).unwrap(),
            },
        )
        .await
        .unwrap();
    report.release.add_permits(1);
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
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
    assert_eq!(report.calls.load(Ordering::SeqCst), 1);
    assert!(
        !serde_json::to_string(&*language.requests.lock().unwrap())
            .unwrap()
            .contains("stale report must not enter context")
    );
    let current = stack
        .store
        .read_domain_states(&submitted.session_id, None, None)
        .await
        .unwrap();
    assert!(
        report.state.decode(&current.states[0].snapshot).unwrap(),
        "user command must remain authoritative"
    );
    let facts = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            128,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap();
    assert_eq!(
        facts
            .facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ToolResult { .. }))
            .count(),
        1
    );
    drop((turns, commands, tool));
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}
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
            output: None,
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

#[derive(Debug)]
struct SelectionCommandFixture(
    rsi_agent_composition_protocol::DomainHandle<
        Option<rsi_agent_session_protocol::ModelSelection>,
    >,
);
#[async_trait]
impl rsi_agent_composition_protocol::SessionCommand for SelectionCommandFixture {
    async fn execute(
        &self,
        context: &rsi_agent_composition_protocol::SessionCommandContext,
        arguments: &rsi_agent_session_protocol::CommandArguments,
        _: CancellationToken,
    ) -> ContributionResult<rsi_agent_composition_protocol::SessionCommandOutput> {
        let selection: rsi_agent_session_protocol::ModelSelection =
            serde_json::from_value(arguments.value().clone()).unwrap();
        let state = context
            .domains
            .iter()
            .find(|state| state.snapshot.identity().id() == rsi_agent_model_selection::DOMAIN)
            .unwrap();
        Ok(vec![self.0.propose(state.revision, &Some(selection)).unwrap()].into())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Concurrent command and queued input span a gated safe retry.
async fn nondispatched_retry_keeps_selection_and_does_not_claim_new_step_input() {
    use rsi_agent_composition_protocol::{
        DomainCatalog, DomainDefinition, SessionCommandRegistration,
    };
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageContent, AgentMessageSource, CommandArguments, DomainIdentity,
        DomainRequestId, MessageDelivery, MessageId, MessageOptions, ModelSelection,
        SessionCommandDescriptor, SessionCommandInvocation,
    };
    use rsi_agent_turn_protocol::{SessionCommandsContract, SubmitMessage};
    let stack = BaseStack::activate().await;
    let selection = ModelSelection {
        model: ModelRef::new("test", "before").unwrap(),
        reasoning_effort: Some(rsi_ai_protocol::ReasoningEffortId::new("low").unwrap()),
    };
    let definition = DomainDefinition::new(
        DomainIdentity::new(rsi_agent_model_selection::DOMAIN, 1).unwrap(),
        &Some(selection),
        |_: &Option<ModelSelection>| Ok(()),
        rsi_agent_composition_protocol::DomainReferencePolicy::None,
    )
    .unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let command = Arc::new(SelectionCommandFixture(domains.bind(&definition).unwrap()));
    *stack.composition.domains.lock().unwrap() = domains;
    let id = ContributionId::new("fixture.selection").unwrap();
    let callbacks = install(
        &stack,
        vec![ContributionRegistration::new(
            id.clone(),
            0,
            ContributionKind::Command(SessionCommandRegistration::new(
                SessionCommandDescriptor::new(id.clone(), "selection", "Set selection", true)
                    .unwrap(),
                command,
            )),
        )],
    )
    .await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::GatedError {
                entered: entered.clone(),
                release: release.clone(),
                error: AiError::new(
                    ErrorKind::RateLimited,
                    ErrorPhase::Connect,
                    DispatchStatus::NotDispatched,
                    "not sent",
                )
                .unwrap(),
            },
            StartOutcome::Stream(answer_script()),
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
    let executor = stack.activate_executor("selection-retry").await;
    let captured_selections = Arc::new(Mutex::new(Vec::new()));
    let source = activate_fixture(
        &stack.runtime,
        "test.selection-tools",
        "selection-tools",
        Arc::new(super::tool_execution::RequestSourceFactory(Arc::new(
            SelectionTools {
                catalogs: stack
                    .runtime
                    .root()
                    .lookup_local::<ToolCatalogProviderContract>()
                    .unwrap(),
                selections: captured_selections.clone(),
            },
        ))),
    )
    .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let commands = stack
        .runtime
        .root()
        .lookup_local::<SessionCommandsContract>()
        .unwrap();
    let session_id = SessionId::new("selection-retry").unwrap();
    let message_id = MessageId::new("initial").unwrap();
    turns
        .submit_message(SubmitMessage {
            session: stack.fresh(header_for_session("selection-retry")).await,
            delivery: MessageDelivery::NextTurn,
            message: AgentMessage {
                message_id: message_id.clone(),
                source: AgentMessageSource::Human { attribution: None },
                content: vec![AgentMessageContent::Text {
                    text: "first input".into(),
                }],
                options: MessageOptions::default(),
            },
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let turn_id = stack
        .store
        .inspect_session(&session_id)
        .await
        .unwrap()
        .active_turn_id
        .unwrap();
    let revision = commands
        .list(turns.prepare_resume(&session_id).await.unwrap())
        .await
        .unwrap()
        .revision();
    commands.execute(turns.prepare_resume(&session_id).await.unwrap(),SessionCommandInvocation {
        command:id,request_id:DomainRequestId::new("change-selection").unwrap(),expected_revision:revision,
        arguments:CommandArguments::new(json!({"model":{"deployment":"test","model":"after"},"reasoning_effort":"high"})).unwrap(),
    }).await.unwrap();
    turns
        .submit_message(SubmitMessage {
            session: SubmitSession::Resume(turns.prepare_resume(&session_id).await.unwrap()),
            delivery: MessageDelivery::NextStep,
            message: AgentMessage {
                message_id: MessageId::new("queued-step").unwrap(),
                source: AgentMessageSource::Human { attribution: None },
                content: vec![AgentMessageContent::Text {
                    text: "new input after first prepare".into(),
                }],
                options: MessageOptions::default(),
            },
        })
        .await
        .unwrap();
    release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(outcome) = turns.outcome(&session_id, &turn_id).await.unwrap() {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);
    {
        let requests = language.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for request in requests.iter().take(2) {
            assert_eq!(
                request.settings().reasoning_effort().unwrap().as_str(),
                "low"
            );
            assert!(
                !serde_json::to_string(request)
                    .unwrap()
                    .contains("new input after first prepare")
            );
        }
        assert_eq!(
            requests[2].settings().reasoning_effort().unwrap().as_str(),
            "high"
        );
        assert!(
            serde_json::to_string(&requests[2])
                .unwrap()
                .contains("new input after first prepare")
        );
    }
    let facts = stack
        .store
        .read_facts(
            &session_id,
            0,
            128,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    let models = facts
        .iter()
        .filter_map(|fact| {
            if let SessionFactBody::ModelIntent { snapshot, .. } = fact.body() {
                Some(snapshot.model.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(models, vec!["before", "before", "after"]);
    assert_eq!(
        captured_selections
            .lock()
            .unwrap()
            .iter()
            .map(|selection| (
                selection.model.deployment().to_owned(),
                selection.model.model().to_owned(),
                selection
                    .reasoning_effort
                    .as_ref()
                    .map(|effort| effort.as_str().to_owned()),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("test".into(), "before".into(), Some("low".into())),
            ("test".into(), "after".into(), Some("high".into())),
        ]
    );
    assert!(source.dispose().await.is_clean());
    assert!(callbacks.dispose().await.is_clean());
    stack.dispose(language_fiber, executor).await;
}

#[derive(Debug)]
struct SelectionTools {
    catalogs: Arc<dyn rsi_tools_protocol::ToolCatalogProvider>,
    selections: Arc<Mutex<Vec<rsi_agent_session_protocol::ModelSelection>>>,
}

#[async_trait]
impl rsi_agent_composition_protocol::RequestToolSource for SelectionTools {
    async fn capture(
        &self,
        _: &SessionHeader,
        selection: &rsi_agent_session_protocol::ModelSelection,
        _: &std::collections::BTreeSet<String>,
        _: CancellationToken,
    ) -> rsi_agent_composition_protocol::Result<rsi_agent_composition_protocol::RequestToolCatalog>
    {
        self.selections.lock().unwrap().push(selection.clone());
        Ok(rsi_agent_composition_protocol::RequestToolCatalog {
            middleware: None,
            inputs: None,
            instructions: String::new(),
            generation: "selected-model-tools".into(),
            receipt_source: None,
            registrations: std::collections::BTreeMap::default(),
            runtime: self.catalogs.begin_stage().unwrap().seal().unwrap(),
        })
    }
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
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    let input_index = facts
        .iter()
        .position(|fact| {
            matches!(
                fact.body(),
                SessionFactBody::InputMessageClaimed {
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

#[derive(Clone, Copy, Debug)]
enum PreparationExit {
    Start,
    Fail,
    Cancel,
}

#[derive(Debug)]
struct PreparationCheckpoint {
    language: Arc<LanguageFixture>,
    exit: PreparationExit,
    entered: Notify,
    prepared_was_durable: Mutex<Vec<bool>>,
}

#[async_trait]
impl LanguageCall for PreparationCheckpoint {
    fn describe(
        &self,
        model: &ModelRef,
    ) -> Result<rsi_ai_protocol::LanguageModelDescription, AiError> {
        self.language.describe(model)
    }

    async fn prepare(
        &self,
        model: ModelRef,
        request: LanguageRequest,
    ) -> Result<Box<dyn PreparedLanguageCall>, AiError> {
        // The request already includes the contribution even when its Fact is live-only.
        assert!(
            serde_json::to_string(&request)
                .unwrap()
                .contains("sample 0")
        );
        let session = self
            .language
            .store
            .list_sessions(None, 1)
            .await
            .unwrap()
            .sessions
            .remove(0);
        let facts = self
            .language
            .store
            .read_facts(
                &session,
                0,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap()
            .facts;
        self.prepared_was_durable.lock().unwrap().push(
            facts
                .iter()
                .any(|fact| matches!(fact.body(), SessionFactBody::StepInputsPrepared { .. })),
        );
        match self.exit {
            PreparationExit::Start => self.language.prepare(model, request).await,
            PreparationExit::Fail => Err(AiError::new(
                ErrorKind::InvalidRequest,
                ErrorPhase::Prepare,
                DispatchStatus::NotDispatched,
                "fixture preparation rejected",
            )
            .unwrap()),
            PreparationExit::Cancel => {
                self.entered.notify_one();
                std::future::pending().await
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn preparation_uses_the_next_effect_or_terminal_durability_barrier() {
    for exit in [
        PreparationExit::Start,
        PreparationExit::Fail,
        PreparationExit::Cancel,
    ] {
        let stack = BaseStack::activate().await;
        let callbacks = install(
            &stack,
            vec![ContributionRegistration::new(
                ContributionId::new("fixture.time").unwrap(),
                0,
                ContributionKind::Context(Arc::new(SampleTime::default())),
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
        let checkpoint = Arc::new(PreparationCheckpoint {
            language: language.clone(),
            exit,
            entered: Notify::new(),
            prepared_was_durable: Mutex::new(vec![]),
        });
        let language_fiber = activate_fixture(
            &stack.runtime,
            "test.preparation",
            "1",
            Arc::new(LanguageFixtureFactory {
                fixture: checkpoint.clone(),
            }),
        )
        .await;
        let executor = stack.activate_executor("executor-preparation").await;
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let submitted = stack
            .submit_fresh(&turns, "preparation-prefix", "work")
            .await;
        if matches!(exit, PreparationExit::Cancel) {
            checkpoint.entered.notified().await;
            turns
                .cancel(&submitted.session_id, &submitted.turn_id, None)
                .await
                .unwrap();
        }
        let outcome = wait_for_outcome(&turns, &submitted).await;
        match exit {
            PreparationExit::Start => assert_eq!(outcome, TurnOutcome::Completed),
            PreparationExit::Fail => {
                assert!(matches!(outcome, TurnOutcome::Failed { ref code, .. }
                if code == "provider.invalid_request"))
            }
            PreparationExit::Cancel => assert_eq!(outcome, TurnOutcome::Cancelled),
        }
        let facts = stack
            .store
            .read_facts(
                &submitted.session_id,
                0,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap()
            .facts;
        assert!(
            facts
                .iter()
                .any(|fact| matches!(fact.body(), SessionFactBody::StepInputsPrepared { .. }))
        );
        assert!(facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::InputMessageClaimed {
                source: InputMessageSource::PluginContext { .. },
                ..
            }
        )));
        // PreparedScript::start independently checks the durable intent/start prefix.
        assert_eq!(
            language.starts.load(Ordering::Acquire),
            usize::from(matches!(exit, PreparationExit::Start))
        );
        assert!(callbacks.dispose().await.is_clean());
        stack.dispose(language_fiber, executor).await;
        assert_eq!(
            *checkpoint.prepared_was_durable.lock().unwrap(),
            [false],
            "{exit:?}"
        );
    }
}
