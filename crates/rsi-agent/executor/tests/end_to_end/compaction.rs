use super::*;
use rsi_agent_session_protocol::ModelPurpose;
use rsi_ai_protocol::{TokenUsage, ToolChoice};

fn long_answer(usage: Option<u64>) -> Vec<LanguageEvent> {
    let mut script = answer_script();
    script[1] = LanguageEvent::ContentDelta {
        index: 0,
        delta: ContentDelta::Text("verified earlier work ".repeat(5000)),
    };
    if let Some(input_tokens) = usage {
        script.insert(
            3,
            LanguageEvent::Usage {
                usage: TokenUsage {
                    input_tokens,
                    ..Default::default()
                },
            },
        );
    }
    script
}
fn fixture(stack: &BaseStack, outcomes: Vec<StartOutcome>) -> Arc<LanguageFixture> {
    Arc::new(LanguageFixture {
        outcomes: Mutex::new(outcomes.into()),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    })
}
fn capacity_error(dispatch: DispatchStatus) -> StartOutcome {
    StartOutcome::Error(
        AiError::new(
            ErrorKind::ContextLimit,
            ErrorPhase::FirstEvent,
            dispatch,
            "fixture context rejection",
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn small_history_with_high_reported_usage_still_reaches_the_next_provider_call() {
    let stack = BaseStack::activate().await;
    let mut pressured = answer_script();
    pressured.insert(
        3,
        LanguageEvent::Usage {
            usage: TokenUsage {
                input_tokens: 80_000,
                ..Default::default()
            },
        },
    );
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(pressured),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.short-pressure", provider.clone())
        .await;
    let executor = stack.activate_executor("short-pressure").await;
    let (first, outcome) = stack.submit_and_wait("short task").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let (_, outcome) = stack.resume_and_wait("next task", first.session_id).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn pressure_summary_is_durable_before_conversation_resumes_and_charges_provider_budget() {
    let stack = BaseStack::activate().await;
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(Some(80_000))),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.compaction", provider.clone())
        .await;
    let executor = stack.activate_executor("summary").await;
    let (first, outcome) = stack.submit_and_wait("older task").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let (second, outcome) = stack
        .resume_and_wait("new original task", first.session_id.clone())
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].tool_choice(), &ToolChoice::None);
    assert!(requests[1].tools().is_empty());
    assert_eq!(requests[1].settings().max_output_tokens(), Some(8192));
    let resumed = serde_json::to_string(&requests[2]).unwrap();
    assert!(resumed.contains("Internal context summary"));
    assert!(resumed.contains("new original task"));
    assert!(!resumed.contains("verified earlier work"));
    let facts = stack
        .store
        .read_facts(&first.session_id, 0, 512)
        .await
        .unwrap()
        .facts;
    let purposes: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ModelIntent {
                turn_id, purpose, ..
            } if turn_id == &second.turn_id => Some(purpose),
            _ => None,
        })
        .collect();
    assert!(matches!(
        purposes.as_slice(),
        [
            ModelPurpose::ContextCompaction(_),
            ModelPurpose::Conversation
        ]
    ));
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn provider_capacity_allows_one_summary_recovery_and_one_ordinary_resubmission() {
    let stack = BaseStack::activate().await;
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(None)),
            capacity_error(DispatchStatus::Dispatched),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.capacity-summary", provider.clone())
        .await;
    let executor = stack.activate_executor("capacity-summary").await;
    let (first, outcome) = stack.submit_and_wait("older task").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let (_, outcome) = stack.resume_and_wait("continue", first.session_id).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(provider.starts.load(Ordering::SeqCst), 4);
    assert_eq!(
        provider.requests.lock().unwrap()[2].tool_choice(),
        &ToolChoice::None
    );
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn invalid_summary_stops_without_an_ordinary_resubmission() {
    let stack = BaseStack::activate().await;
    let mut invalid = answer_script();
    *invalid.last_mut().unwrap() = LanguageEvent::Finished {
        reason: FinishReason::MaxTokens,
        replay: None,
    };
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(Some(80_000))),
            StartOutcome::Stream(invalid),
        ],
    );
    let language = stack
        .activate_language("test.language.invalid-summary", provider.clone())
        .await;
    let executor = stack.activate_executor("invalid-summary").await;
    let (first, outcome) = stack.submit_and_wait("older task").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let (_, outcome) = stack.resume_and_wait("continue", first.session_id).await;
    assert!(
        matches!(outcome, TurnOutcome::Failed { code, .. } if code == "context.compaction_failed")
    );
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn naturally_finished_summary_that_expands_the_view_fails_without_resubmission() {
    let stack = BaseStack::activate().await;
    let mut earlier = answer_script();
    earlier.insert(
        3,
        LanguageEvent::Usage {
            usage: TokenUsage {
                input_tokens: 80_000,
                ..Default::default()
            },
        },
    );
    let mut summary = answer_script();
    summary[1] = LanguageEvent::ContentDelta {
        index: 0,
        delta: ContentDelta::Text("VERBOSE_SUMMARY ".repeat(1000)),
    };
    let provider = fixture(
        &stack,
        vec![StartOutcome::Stream(earlier), StartOutcome::Stream(summary)],
    );
    let language = stack
        .activate_language("test.language.expanding-summary", provider.clone())
        .await;
    let executor = stack.activate_executor("expanding-summary").await;
    let (first, outcome) = stack.submit_and_wait("small older task").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    // The protected current input occupies the recent tail. Only the much
    // smaller previous interaction is selected, so the bounded summary grows it.
    let (second, outcome) = stack
        .resume_and_wait(&"current task ".repeat(8000), first.session_id.clone())
        .await;
    assert!(matches!(outcome, TurnOutcome::Failed { code, message }
        if code == "context.compaction_failed" && message == "summary did not install against its exact source view"));
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    let facts = stack
        .store
        .read_facts(&first.session_id, 0, 512)
        .await
        .unwrap()
        .facts;
    let mut effect = None;
    let mut finished = 0;
    for fact in &facts {
        match fact.body() {
            SessionFactBody::ModelIntent {
                turn_id,
                effect_id,
                purpose,
                ..
            } if turn_id == &second.turn_id => {
                assert!(matches!(purpose, ModelPurpose::ContextCompaction(_)));
                assert!(effect.replace(effect_id.clone()).is_none());
            }
            SessionFactBody::ModelEvent {
                effect_id,
                event: LanguageEvent::Finished { reason, .. },
                ..
            } if Some(effect_id) == effect.as_ref() => {
                assert_eq!(*reason, FinishReason::Stop);
                finished += 1;
            }
            _ => {}
        }
    }
    assert_eq!(finished, 1);
    let mut replay = ModelContextState::open(
        Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
        stack.store.header(&first.session_id).await.unwrap(),
        ContextLimits::default(),
    )
    .unwrap();
    let facts: Vec<_> = facts.into_iter().map(Arc::new).collect();
    replay.ingest(ContextPage::Canonical(&facts)).unwrap();
    assert!(!replay.summary_installed(&effect.unwrap()));
    let retained = serde_json::to_string(&replay.build(vec![]).unwrap()).unwrap();
    assert!(retained.contains("small older task"));
    assert!(!retained.contains("VERBOSE_SUMMARY"));
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn unknown_capacity_outcome_never_starts_compaction_or_resubmits() {
    let stack = BaseStack::activate().await;
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(None)),
            capacity_error(DispatchStatus::Unknown),
        ],
    );
    let language = stack
        .activate_language("test.language.unknown-summary", provider.clone())
        .await;
    let executor = stack.activate_executor("unknown-summary").await;
    let (first, _) = stack.submit_and_wait("older task").await;
    let (_, outcome) = stack.resume_and_wait("continue", first.session_id).await;
    assert!(matches!(outcome, TurnOutcome::Interrupted { .. }));
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn summary_capacity_retries_once_with_a_smaller_whole_source_batch() {
    let stack = BaseStack::activate().await;
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(Some(80_000))),
            capacity_error(DispatchStatus::Dispatched),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.shrink-summary", provider.clone())
        .await;
    let executor = stack.activate_executor("shrink-summary").await;
    let (first, _) = stack.submit_and_wait(&"older task ".repeat(10_000)).await;
    let (_, outcome) = stack.resume_and_wait("continue", first.session_id).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[1].tool_choice(), &ToolChoice::None);
        assert_eq!(requests[2].tool_choice(), &ToolChoice::None);
        assert!(
            serde_json::to_vec(&requests[2]).unwrap().len()
                < serde_json::to_vec(&requests[1]).unwrap().len()
        );
    }
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn summary_consumes_the_same_frozen_provider_attempt_budget() {
    let stack = BaseStack::activate().await;
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(Some(80_000))),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.summary-budget", provider.clone())
        .await;
    let executor = stack.activate_executor("summary-budget").await;
    let budget = TurnBudget::new(30_000, 1, 256, 65_536, 64 * 1024 * 1024).unwrap();
    let (first, outcome) = stack
        .submit_and_wait_with_header("older task", None, header_with_budget(budget))
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let (_, outcome) = stack.resume_and_wait("continue", first.session_id).await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::BudgetExceeded {
                dimension: BudgetDimension::ProviderAttempts,
                consumed: 2,
                limit: 1
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    stack.dispose(language, executor).await;
}

#[derive(Debug)]
struct GateAfterSummary {
    inner: Arc<LanguageFixture>,
    enabled: AtomicBool,
    entered: Notify,
}
#[async_trait]
impl LanguageCall for GateAfterSummary {
    fn describe(&self, model: &ModelRef) -> Result<LanguageProfile, AiError> {
        self.inner.describe(model)
    }
    async fn prepare(
        &self,
        model: ModelRef,
        request: LanguageRequest,
    ) -> Result<Box<dyn PreparedLanguageCall>, AiError> {
        if self.enabled.load(Ordering::Acquire) && self.inner.requests.lock().unwrap().len() == 2 {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.inner.prepare(model, request).await
    }
}
#[derive(Debug)]
struct GateFactory(Arc<GateAfterSummary>);
#[async_trait]
impl PluginFactory for GateFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<LanguageCallContract>(self.0.clone())?;
        plan.defer(
            "withdraw gated provider",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn executor_loss_after_summary_finished_interrupts_original_turn_and_explicit_resume_reuses_summary()
 {
    let stack = BaseStack::activate().await;
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(Some(80_000))),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let gated = Arc::new(GateAfterSummary {
        inner: provider.clone(),
        enabled: AtomicBool::new(true),
        entered: Notify::new(),
    });
    let language = activate_fixture(
        &stack.runtime,
        "test.language.summary-crash",
        "1",
        Arc::new(GateFactory(gated.clone())),
    )
    .await;
    let executor = stack.activate_executor("before-summary-crash").await;
    let (first, _) = stack.submit_and_wait("older task").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let second = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(&first.session_id).await.unwrap()),
            text: "continue after summary".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), gated.entered.notified())
        .await
        .unwrap();
    let facts = stack
        .store
        .read_facts(&first.session_id, 0, 512)
        .await
        .unwrap()
        .facts;
    assert!(matches!(
        facts.last().unwrap().body(),
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction,
            event: LanguageEvent::Finished { .. },
            ..
        }
    ));
    assert!(executor.dispose().await.is_clean());
    let replacement = stack.activate_executor("after-summary-crash").await;
    assert!(matches!(
        wait_for_outcome(&turns, &second).await,
        TurnOutcome::Interrupted { .. }
    ));
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    gated.enabled.store(false, Ordering::Release);
    let (_, outcome) = stack
        .resume_and_wait("explicit new Turn", first.session_id)
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let last = serde_json::to_string(provider.requests.lock().unwrap().last().unwrap()).unwrap();
    assert!(last.contains("Internal context summary"));
    assert!(last.contains("explicit new Turn"));
    assert_eq!(provider.starts.load(Ordering::SeqCst), 3);
    stack.dispose(language, replacement).await;
}

#[tokio::test]
async fn executor_loss_before_summary_finished_cannot_install_partial_summary() {
    interrupted_summary(false).await;
}

#[tokio::test]
async fn executor_shutdown_with_cancelled_summary_output_is_interrupted_not_user_cancelled() {
    interrupted_summary(true).await;
}

async fn interrupted_summary(finish_on_cancellation: bool) {
    let stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(Some(80_000))),
            if finish_on_cancellation {
                StartOutcome::FinishedOnCancellation(entered.clone())
            } else {
                gated_answer(&entered, &release)
            },
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.partial-summary", provider.clone())
        .await;
    let executor = stack.activate_executor("before-partial-summary").await;
    let (first, _) = stack.submit_and_wait("older task").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let second = turns
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(&first.session_id).await.unwrap()),
            text: "continue interrupted summary".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(executor.dispose().await.is_clean());
    let replacement = stack.activate_executor("after-partial-summary").await;
    assert!(matches!(
        wait_for_outcome(&turns, &second).await,
        TurnOutcome::Interrupted { .. }
    ));
    let (_, outcome) = stack
        .resume_and_wait("explicit new Turn", first.session_id)
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[2].tool_choice(), &ToolChoice::None);
        assert!(
            !serde_json::to_string(&requests[2])
                .unwrap()
                .contains("Internal context summary")
        );
    }
    stack.dispose(language, replacement).await;
}
