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
                usage: TokenUsage::new(input_tokens, 0, None, None, None).unwrap(),
            },
        );
    }
    script
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
async fn plugin_checkpoint_is_durable_with_no_registered_model() {
    use rsi_agent_session_protocol::{EffectId, PluginCheckpoint};
    use rsi_agent_turn_protocol::ManualCompactorContract;

    let stack = BaseStack::activate().await;
    let seed_language = stack
        .activate_language(
            "fixture.previous-provider",
            language_fixture(&stack, vec![StartOutcome::Stream(answer_script())]),
        )
        .await;
    let seed_executor = stack.activate_executor("seed-offline-history").await;
    let (previous, outcome) = stack.submit_and_wait("historical source").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert!(seed_executor.dispose().await.is_clean());
    assert!(seed_language.dispose().await.is_clean());

    let credentials = activate_fixture(
        &stack.runtime,
        "fixture.empty-credentials",
        "1",
        Arc::new(rsi_credentials_testkit::MemoryCredentialsFactory),
    )
    .await;
    let router = activate_fixture(
        &stack.runtime,
        "fixture.empty-router",
        "1",
        Arc::new(rsi_ai::LanguageRouterFactory),
    )
    .await;
    let (observer, owner) = observation::install(&stack).await;
    let executor = stack
        .activate_executor_with_config(json!({
            "executor_id":"offline-checkpoint", "observe_execution":true
        }))
        .await;
    let root = stack.runtime.root();
    let turns = root.lookup_local::<TurnServiceContract>().unwrap();
    let compactor = root.lookup_local::<ManualCompactorContract>().unwrap();
    let session = &previous.session_id;
    let turn = turns
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(turns.prepare_resume(session).await.unwrap()),
            text: "continue offline".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    observer.entered.notified().await;
    let through_seq = stack
        .store
        .session_summary(session)
        .await
        .unwrap()
        .durable_fact_seq;
    let unavailable = root
        .lookup_local::<LanguageCallContract>()
        .unwrap()
        .describe(header().settings().default_model())
        .unwrap_err();
    let checkpoint = PluginCheckpoint {
        plugin: "offline-engine".into(),
        through_seq,
        start: 0,
        end: 0,
        message: rsi_ai_protocol::Message::user_text("offline summary").unwrap(),
        summary_blocks: None,
        presentation: None,
    };
    compactor
        .install_plugin_checkpoint(
            session,
            &turn.turn_id,
            EffectId::new("offline-summary").unwrap(),
            checkpoint.clone(),
        )
        .await
        .unwrap();
    assert_latest_is(&stack.store, |fact| {
        matches!(fact, SessionFactBody::PluginContextCompacted { checkpoint: recorded, .. }
            if recorded.as_ref() == &checkpoint)
    })
    .await;
    observer.release.cancel();
    assert!(matches!(
        wait_for_outcome(&turns, &turn).await,
        TurnOutcome::Failed { message, .. } if message == unavailable.safe_summary()
    ));
    drop((compactor, turns));
    assert!(executor.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
    stack.dispose_services().await;
}

#[tokio::test]
async fn plugin_checkpoint_changes_the_next_request_without_dispatching_a_summary() {
    use rsi_agent_session_protocol::{EffectId, PluginCheckpoint};
    use rsi_agent_turn_protocol::ManualCompactorContract;
    let mut stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = language_fixture(
        &stack,
        vec![
            gated_answer(&entered, &release),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.plugin-checkpoint", provider.clone())
        .await;
    let executor = stack.activate_executor("plugin-checkpoint").await;
    let root = stack.runtime.root();
    let turns = root.lookup_local::<TurnServiceContract>().unwrap();
    let compactor = root.lookup_local::<ManualCompactorContract>().unwrap();
    let turn = stack
        .submit_fresh(&turns, "plugin-checkpoint", "ORIGINAL-source")
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let session = &turn.session_id;
    let facts = stack
        .store
        .read_facts(
            session,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    let original = PluginCheckpoint {
        plugin: "template-summarizer".into(),
        through_seq: facts.last().unwrap().seq(),
        start: 0,
        end: 0,
        message: rsi_ai_protocol::Message::user_text("OFFLINE-ORCHID").unwrap(),
        summary_blocks: None,
        presentation: None,
    };
    let effect = EffectId::new("template-operation").unwrap();
    compactor
        .install_plugin_checkpoint(session, &turn.turn_id, effect.clone(), original.clone())
        .await
        .unwrap();
    let committed = stack
        .store
        .read_facts(
            session,
            original.through_seq,
            32,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert_eq!(committed.len(), 1);
    assert!(
        matches!(committed[0].body(), SessionFactBody::PluginContextCompacted { checkpoint, .. }
        if checkpoint.as_ref() == &original)
    );
    assert!(
        compactor
            .install_plugin_checkpoint(session, &turn.turn_id, effect, original)
            .await
            .is_err()
    );
    assert_eq!(provider.starts.load(Ordering::SeqCst), 1);
    release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &turn).await,
        TurnOutcome::Completed
    );
    drop((turns, compactor));
    assert!(executor.dispose().await.is_clean());
    stack.restart_kernel().await;
    let replacement = stack.activate_executor("plugin-reopen").await;
    assert_eq!(
        stack.resume_and_wait("follow up", session.clone()).await.1,
        TurnOutcome::Completed
    );
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            serde_json::to_string(&requests[0])
                .unwrap()
                .contains("ORIGINAL-source")
        );
        let next = serde_json::to_string(&requests[1]).unwrap();
        assert!(next.contains("OFFLINE-ORCHID") && !next.contains("ORIGINAL-source"));
    }
    stack.dispose(language, replacement).await;
}

#[tokio::test]
async fn trusted_region_summarizes_selected_input_while_the_foreground_model_is_streaming() {
    use rsi_agent_session_protocol::ModelSelection;
    use rsi_agent_turn_protocol::{CompactionRegion, ManualCompactorContract};
    let stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let summary_entered = Arc::new(Notify::new());
    let summary_hold = Arc::new(Notify::new());
    let provider = language_fixture(
        &stack,
        vec![
            gated_answer(&entered, &release),
            gated_answer(&summary_entered, &summary_hold),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.region", provider.clone())
        .await;
    let executor = stack.activate_executor("region").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let compactor = stack
        .runtime
        .root()
        .lookup_local::<ManualCompactorContract>()
        .unwrap();
    let original = "source-ORCHID ".repeat(256);
    let turn = stack.submit_fresh(&turns, "region", &original).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let session = &turn.session_id;
    let prefix = stack
        .store
        .read_facts(
            session,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    let region = CompactionRegion {
        turn_id: turn.turn_id.clone(),
        through_seq: prefix.last().unwrap().seq(),
        start: 0,
        end: 0,
    };
    let model = ModelSelection::baseline(header().settings());
    assert!(
        Arc::clone(&compactor)
            .compact_region(
                session,
                CompactionRegion {
                    end: 99,
                    ..region.clone()
                },
                model.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(provider.starts.load(Ordering::SeqCst), 1);
    let mut pending = Arc::clone(&compactor).compact_region(session, region.clone(), model.clone());
    tokio::select! {
        result = &mut pending => panic!("region settled before releasing its provider: {result:?}"),
        () = summary_entered.notified() => {}
    }
    let duplicate = Arc::clone(&compactor)
        .compact_region(session, region.clone(), model.clone())
        .await;
    assert!(
        duplicate
            .unwrap_err()
            .to_string()
            .contains("already running")
    );
    drop(pending);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let page = stack
                .store
                .read_facts(
                    session,
                    region.through_seq,
                    32,
                    rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
                )
                .await
                .unwrap();
            let cancelled = page.facts.iter().any(|fact| {
                matches!(fact.body(),
                    SessionFactBody::ModelAttemptSettled {
                        purpose: rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction,
                        attempt, ..
                    } if matches!(attempt.terminal(), LanguageEvent::Failed { .. })
                )
            });
            if cancelled {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let installed = Arc::clone(&compactor)
        .compact_region(session, region.clone(), model.clone())
        .await
        .unwrap();
    let installed_facts = stack
        .store
        .read_facts(
            session,
            region.through_seq,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert!(
        matches!(installed_facts.last().unwrap().body(), SessionFactBody::ContextCompactionInstalled { effect_id, .. } if effect_id == &installed)
    );
    release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &turn).await,
        TurnOutcome::Completed
    );
    assert!(
        compactor
            .compact_region(session, region, model)
            .await
            .is_err()
    );
    let (_, outcome) = stack.resume_and_wait("follow up", session.clone()).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(
            serde_json::to_string(&requests[2])
                .unwrap()
                .contains("source-ORCHID")
        );
        assert!(requests[2].tools().is_empty());
        let next = serde_json::to_string(&requests[3]).unwrap();
        assert!(next.contains("Internal context summary"));
        assert!(!next.contains("source-ORCHID"));
    }
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn small_history_with_high_reported_usage_still_reaches_the_next_provider_call() {
    let stack = BaseStack::activate().await;
    let mut pressured = answer_script();
    pressured.insert(
        3,
        LanguageEvent::Usage {
            usage: TokenUsage::new(80_000, 0, None, None, None).unwrap(),
        },
    );
    let provider = language_fixture(
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
    let provider = language_fixture(
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
        .read_facts(
            &first.session_id,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
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
    let installed: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ContextCompactionInstalled {
                turn_id,
                effect_id,
                settlement_seq,
            } if turn_id == &second.turn_id => Some((fact.seq(), effect_id, *settlement_seq)),
            _ => None,
        })
        .collect();
    let [(confirmed, effect, settlement)] = installed.as_slice() else {
        panic!("expected one installed summary")
    };
    assert!(*settlement < *confirmed);
    assert!(facts.iter().any(|fact| fact.seq() == *settlement && matches!(fact.body(),
        SessionFactBody::ModelAttemptSettled { effect_id, purpose: rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction, .. } if effect_id == *effect)));
    assert!(facts.iter().any(|fact| fact.seq() > *confirmed && matches!(fact.body(),
        SessionFactBody::ModelIntent { turn_id, purpose: ModelPurpose::Conversation, .. } if turn_id == &second.turn_id)));
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn provider_capacity_allows_one_summary_recovery_and_one_ordinary_resubmission() {
    let stack = BaseStack::activate().await;
    let provider = language_fixture(
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
async fn truncated_summary_stops_without_recovery_or_an_ordinary_resubmission() {
    let stack = BaseStack::activate().await;
    let mut invalid = answer_script();
    *invalid.last_mut().unwrap() = LanguageEvent::Finished {
        reason: FinishReason::MaxTokens,
        replay: None,
    };
    let provider = language_fixture(
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
    let (second, outcome) = stack
        .resume_and_wait("continue", first.session_id.clone())
        .await;
    assert!(matches!(outcome, TurnOutcome::Failed { code, message }
            if code == "context.compaction_failed" && message == "summary reached its output limit"));
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    let facts = stack
        .store
        .read_facts(
            &first.session_id,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
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
        [ModelPurpose::ContextCompaction(_)]
    ));
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.body(), SessionFactBody::ModelAttemptSettled { .. }))
            .count(),
        2
    );
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn naturally_finished_summary_that_expands_the_view_fails_without_resubmission() {
    let stack = BaseStack::activate().await;
    let mut earlier = answer_script();
    earlier.insert(
        3,
        LanguageEvent::Usage {
            usage: TokenUsage::new(80_000, 0, None, None, None).unwrap(),
        },
    );
    let mut summary = answer_script();
    summary[1] = LanguageEvent::ContentDelta {
        index: 0,
        delta: ContentDelta::Text("VERBOSE_SUMMARY ".repeat(1000)),
    };
    let provider = language_fixture(
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
        .read_facts(
            &first.session_id,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
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
            SessionFactBody::ModelAttemptSettled {
                effect_id,
                attempt: log,
                ..
            } if Some(effect_id) == effect.as_ref() => {
                assert!(matches!(
                    log.events().last().unwrap().unwrap().1,
                    LanguageEvent::Finished {
                        reason: FinishReason::Stop,
                        ..
                    }
                ));
                finished += 1;
            }
            _ => {}
        }
    }
    assert_eq!(finished, 1);
    assert!(!facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::ContextCompactionInstalled { .. }
    )));
    let mut replay = ModelContextState::open(
        Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
        stack.store.header(&first.session_id).await.unwrap(),
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    let facts: Vec<_> = facts.into_iter().map(Arc::new).collect();
    replay.ingest(ContextPage::Canonical(&facts)).unwrap();
    assert!(!replay.summary_installed(&effect.unwrap()));
    let retained = serde_json::to_string(
        &replay
            .build(
                rsi_ai_protocol::LanguageRequestOptions::default(),
                &context_test_profile(),
            )
            .unwrap(),
    )
    .unwrap();
    assert!(retained.contains("small older task"));
    assert!(!retained.contains("VERBOSE_SUMMARY"));
    stack.dispose(language, executor).await;
}

#[tokio::test]
async fn unknown_capacity_outcome_never_starts_compaction_or_resubmits() {
    let stack = BaseStack::activate().await;
    let provider = language_fixture(
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
    let provider = language_fixture(
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
async fn summary_and_answer_have_independent_settlements() {
    let stack = BaseStack::activate().await;
    let provider = language_fixture(
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
    let (first, outcome) = stack
        .submit_and_wait_with_header("older task", None, header())
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let (_, outcome) = stack.resume_and_wait("continue", first.session_id).await;
    assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
    assert_eq!(provider.starts.load(Ordering::SeqCst), 3);
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
    fn describe(
        &self,
        model: &ModelRef,
    ) -> Result<rsi_ai_protocol::LanguageModelDescription, AiError> {
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
    let mut stack = BaseStack::activate().await;
    let provider = language_fixture(
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
            reasoning_effort: None,
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
        .read_facts(
            &first.session_id,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert!(matches!(
        facts.last().unwrap().body(),
        SessionFactBody::ModelAttemptSettled {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction,
            ..
        }
    ));
    assert!(executor.dispose().await.is_clean());
    drop(turns);
    stack.restart_kernel().await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
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
    let mut stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = language_fixture(
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
            reasoning_effort: None,
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
    drop(turns);
    stack.restart_kernel().await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
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

#[tokio::test]
async fn idle_manual_summary_holds_later_prompt_and_replay_preserves_turn_count() {
    use rsi_agent_session_protocol::{DomainRequestId, ModelSelection};
    use rsi_agent_turn_protocol::{
        CompactionOutcome, ManualCompactionAdmission, ManualCompactorContract,
    };
    let model = ModelSelection::baseline(header().settings());
    let stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = language_fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(None)),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()), // Prepared, then refused at the durable start boundary.
            gated_answer(&entered, &release),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.manual", provider.clone())
        .await;
    let executor = stack.activate_executor("manual-summary").await;
    let (first, outcome) = stack.submit_and_wait("old task").await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let id = &first.session_id;
    let (_, outcome) = stack.resume_and_wait("keep this task", id.clone()).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    turns.wait_idle(id).await.unwrap();
    let compactor = stack
        .runtime
        .root()
        .lookup_local::<ManualCompactorContract>()
        .unwrap();
    #[derive(Debug)]
    struct Archived;
    #[async_trait::async_trait]
    impl rsi_agent_turn_protocol::SessionArchive for Archived {
        async fn is_archived(&self, _: &SessionHeader) -> rsi_agent_turn_protocol::Result<bool> {
            Ok(true)
        }
    }
    #[async_trait]
    impl PluginFactory for Archived {
        fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            let archive = plan
                .context()
                .provide_local::<rsi_agent_turn_protocol::SessionArchiveContract>(Arc::new(Self))?;
            plan.defer(
                "withdraw archive fixture",
                Box::new(move || {
                    Box::pin(async move {
                        drop(archive);
                        Ok(())
                    })
                }),
            )
        }
    }
    let archive = activate_fixture(
        &stack.runtime,
        "fixture.archive",
        "test",
        Arc::new(Archived),
    )
    .await;
    let blocked = DomainRequestId::new("archived-summary").unwrap();
    let ManualCompactionAdmission::Idle(lease) =
        turns.reserve_manual_compaction(id, &blocked).await.unwrap()
    else {
        panic!("expected idle admission");
    };
    assert!(matches!(compactor.compact(lease, model.clone()).await,
        Err(rsi_agent_turn_protocol::TurnError::Invalid(message)) if message.contains("archived")));
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    assert!(
        stack
            .store
            .manual_compaction_fact(id, &blocked)
            .await
            .unwrap()
            .is_none()
    );
    assert!(archive.dispose().await.is_clean());
    let failed = DomainRequestId::new("failed-start-prefix").unwrap();
    let ManualCompactionAdmission::Idle(lease) =
        turns.reserve_manual_compaction(id, &failed).await.unwrap()
    else {
        panic!("expected idle admission");
    };
    stack.store.fail_next_appends(1);
    assert!(compactor.compact(lease, model.clone()).await.is_err());
    assert_eq!(provider.starts.load(Ordering::SeqCst), 2);
    assert!(
        stack
            .store
            .manual_compaction_fact(id, &failed)
            .await
            .unwrap()
            .is_none()
    );
    let command = DomainRequestId::new("compact-once").unwrap();
    let ManualCompactionAdmission::Idle(lease) =
        turns.reserve_manual_compaction(id, &command).await.unwrap()
    else {
        panic!("idle cut was busy")
    };
    let compact = tokio::spawn(async move { compactor.compact(lease, model).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert!(matches!(
        turns
            .reserve_manual_compaction(id, &DomainRequestId::new("competing").unwrap())
            .await
            .unwrap(),
        ManualCompactionAdmission::Busy
    ));
    let anchor = {
        let next = stack.resume_and_wait("after the summary", id.clone());
        tokio::pin!(next);
        tokio::select! {
            _ = &mut next => panic!("a later prompt overtook compaction"),
            () = tokio::time::sleep(std::time::Duration::from_millis(15)) => {}
        }
        assert_eq!(provider.starts.load(Ordering::SeqCst), 3);
        release.notify_one();
        assert_eq!(
            compact.await.unwrap().unwrap(),
            CompactionOutcome::Compacted
        );
        let (submitted, outcome) = next.await;
        assert_eq!(outcome, TurnOutcome::Completed);
        submitted.turn_id
    };
    turns.wait_idle(id).await.unwrap();
    assert!(matches!(
        turns.reserve_manual_compaction(id, &command).await.unwrap(),
        ManualCompactionAdmission::Recorded(CompactionOutcome::Compacted)
    ));
    assert_summary_replay_and_fork(&stack, &turns, id, &anchor, &provider).await;
    stack.dispose(language, executor).await;
}

async fn assert_summary_replay_and_fork(
    stack: &BaseStack,
    turns: &Arc<dyn rsi_agent_turn_protocol::TurnService>,
    id: &SessionId,
    anchor: &TurnId,
    provider: &LanguageFixture,
) {
    let facts = stack
        .store
        .read_facts(
            id,
            0,
            512,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|f| f.body().turn_id().is_none())
            .count(),
        4
    );
    assert_eq!(
        facts
            .iter()
            .filter(|f| matches!(f.body(), SessionFactBody::TurnTerminal { .. }))
            .count(),
        3
    );
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        let last = serde_json::to_string(&requests[4]).unwrap();
        assert!(last.contains("Internal context summary") && last.contains("after the summary"));
        assert!(!last.contains("verified earlier work"));
    }
    let source = stack.store.header(id).await.unwrap();
    let fork = turns
        .prepare_fork(rsi_agent_turn_protocol::PrepareFork {
            agent_preset_id: source.agent_preset_id().clone(),
            source,
            session_id: SessionId::new("manual-summary-child").unwrap(),
            created_at_ms: 42,
            point: rsi_agent_session_protocol::ForkPoint::through(anchor),
            turns: rsi_agent_session_protocol::ForkTurnSelection::All,
            cancellation: CancellationToken::new(),
        })
        .await
        .unwrap();
    provider
        .outcomes
        .lock()
        .unwrap()
        .push_back(StartOutcome::Stream(answer_script()));
    let child = turns
        .submit(SubmitTurn {
            session: SubmitSession::Fresh(fork),
            turn_id: client_turn_id(),
            text: "child follows summary".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_outcome(turns, &child).await,
        TurnOutcome::Completed
    );
    let request = serde_json::to_string(provider.requests.lock().unwrap().last().unwrap()).unwrap();
    assert!(
        request.contains("Internal context summary") && request.contains("child follows summary")
    );
    assert!(!request.contains("verified earlier work"));
}

#[tokio::test]
async fn idle_manual_stop_settles_and_reopened_start_is_never_redispatched() {
    use rsi_agent_session_protocol::{DomainRequestId, ModelSelection};
    use rsi_agent_turn_protocol::{
        CancelTarget, CompactionOutcome, ManualCompactionAdmission, ManualCompactorContract,
    };
    let mut stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let hold = Arc::new(Notify::new());
    let provider = language_fixture(
        &stack,
        vec![
            StartOutcome::Stream(long_answer(None)),
            StartOutcome::Stream(answer_script()),
            gated_answer(&entered, &hold),
            gated_answer(&entered, &hold),
            StartOutcome::Stream(answer_script()),
        ],
    );
    let language = stack
        .activate_language("test.language.manual-stop", provider.clone())
        .await;
    let executor = stack.activate_executor("manual-stop").await;
    let (first, _) = stack.submit_and_wait("old source").await;
    let id = &first.session_id;
    stack.resume_and_wait("protected source", id.clone()).await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let owner = stack
        .runtime
        .root()
        .lookup_local::<ManualCompactorContract>()
        .unwrap();
    turns.wait_idle(id).await.unwrap();
    let interrupted = DomainRequestId::new("stop-manual").unwrap();
    let disconnected = DomainRequestId::new("disconnected-client").unwrap();
    for command in [&interrupted, &disconnected] {
        let ManualCompactionAdmission::Idle(lease) =
            turns.reserve_manual_compaction(id, command).await.unwrap()
        else {
            panic!("expected idle");
        };
        let compactor = Arc::clone(&owner);
        let run = tokio::spawn(async move {
            compactor
                .compact(lease, ModelSelection::baseline(header().settings()))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(4), entered.notified())
            .await
            .unwrap();
        if command == &interrupted {
            turns
                .cancel_target(id, CancelTarget::Session { keep_inbox: true }, None)
                .await
                .unwrap();
            assert_eq!(run.await.unwrap().unwrap(), CompactionOutcome::Interrupted);
        } else {
            run.abort();
            assert!(run.await.unwrap_err().is_cancelled());
        }
    }
    assert!(executor.dispose().await.is_clean());
    drop((owner, turns));
    stack.restart_kernel().await;
    let replacement = stack.activate_executor("manual-reopen").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    for (command, expected) in [
        (interrupted, CompactionOutcome::Interrupted),
        (disconnected, CompactionOutcome::Started),
    ] {
        let ManualCompactionAdmission::Recorded(outcome) =
            turns.reserve_manual_compaction(id, &command).await.unwrap()
        else {
            panic!("reopen must return the recorded effect");
        };
        assert_eq!(outcome, expected);
    }
    assert_eq!(provider.starts.load(Ordering::SeqCst), 4);
    assert_eq!(
        stack
            .resume_and_wait("explicit continuation", id.clone())
            .await
            .1,
        TurnOutcome::Completed
    );
    assert_eq!(provider.starts.load(Ordering::SeqCst), 5);
    drop(turns);
    stack.dispose(language, replacement).await;
}
