use super::*;
use rsi_agent_composition_protocol::{
    ContributionResult, DomainHandle, ForkTransform, ForkTransformContext, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{ForkTurnSelection, MessageId};
use rsi_agent_turn_protocol::{AgentCallerAuthority, SpawnAgentRequest, TurnService};

async fn wait_for_message(turns: &Arc<dyn TurnService>, session: &SessionId, message: &MessageId) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let receipt = turns.message_status(session, message).await.unwrap();
            if let rsi_agent_turn_protocol::MessageState::Claimed { turn_id, .. } = receipt.state
                && let Some(outcome) = turns.outcome(session, &turn_id).await.unwrap()
            {
                assert_eq!(outcome, TurnOutcome::Completed);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[derive(Debug)]
struct CountedTransform {
    adapter: Arc<rsi_extension_agent::AgentAdapter>,
    calls: AtomicUsize,
    reader: Mutex<Option<Arc<dyn rsi_agent_composition_protocol::ContributionFactReader>>>,
}
#[async_trait]
impl ForkTransform for CountedTransform {
    async fn transform(
        &self,
        context: &ForkTransformContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        *self.reader.lock().unwrap() = Some(context.facts.clone());
        self.adapter.transform(context, cancellation).await
    }
}

#[derive(Debug)]
struct ForkTool {
    turns: Arc<dyn TurnService>,
    store: Arc<MemoryStore>,
    handle: DomainHandle<ExtensionJournal>,
    transform: Arc<CountedTransform>,
}
#[async_trait]
impl ToolExecutor for ForkTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> ToolResultType<ToolResult> {
        let caller = execution.extension::<AgentCallerAuthority>().unwrap();
        let nested = arguments["nested"] == true;
        let start_calls = self.transform.calls.load(Ordering::Acquire);
        let mut children = Vec::new();
        for (name, selection, first, second) in [
            ("full", ForkTurnSelection::All, true, true),
            ("last", ForkTurnSelection::Last(1), false, true),
            ("none", ForkTurnSelection::None, false, false),
        ] {
            let second = second && (!nested || name == "full");
            let name = if nested {
                format!("nested-{name}")
            } else {
                name.into()
            };
            children.push(name.clone());
            let request = SpawnAgentRequest {
                caller: (*caller).clone(),
                child_session_id: SessionId::new(&name).unwrap(),
                task_name: name.clone(),
                message_id: MessageId::new(format!("task-{name}")).unwrap(),
                message: format!("CHILD_{name}"),
                fork_turns: selection,
                cancellation: execution.cancellation.clone(),
                output_contract: None,
                role: None,
                model: None,
                reasoning_effort: None,
            };
            let receipt = self.turns.spawn_agent(request.clone()).await.unwrap();
            let calls = self.transform.calls.load(Ordering::Acquire);
            assert_eq!(receipt, self.turns.spawn_agent(request).await.unwrap());
            assert_eq!(
                calls,
                self.transform.calls.load(Ordering::Acquire),
                "retry must reuse the durable child"
            );
            let states = self
                .store
                .read_domain_states(&receipt.session_id, None, None)
                .await
                .unwrap();
            let journal = self.handle.decode(&states.states[0].snapshot).unwrap();
            let budget = rsi_api_protocol::ByteBudget::new(1024 * 1024).unwrap();
            let events = rsi_extension_agent::restore(self.store.as_ref(), &journal, || {
                budget.clone().into()
            })
            .await
            .unwrap();
            let text = serde_json::to_string(&events).unwrap();
            assert_eq!(
                text.contains("FIRST_PRIVATE_INPUT"),
                first,
                "{name}: {text}"
            );
            assert_eq!(
                text.contains("SECOND_SELECTED_INPUT"),
                second,
                "{name}: {text}"
            );
            assert!(!text.contains("CURRENT_EXCLUDED_INPUT"), "{name}: {text}");
            assert!(!text.contains("NESTED_CURRENT_EXCLUDED"));
            if name != "none" && name != "nested-none" {
                assert_eq!(journal.checkpoint.value()["mapping"]["factCursor"], 0);
                if !nested {
                    assert_eq!(
                        journal.checkpoint.value()["views"]["values"]["todos"][0]["content"],
                        "Selected Todo"
                    );
                }
                let reader = self.transform.reader.lock().unwrap().clone().unwrap();
                assert!(
                    reader.read(0, 1).await.is_err(),
                    "captured reader must close after transformation"
                );
            } else {
                assert!(events.is_empty());
            }
        }
        assert_eq!(
            self.transform.calls.load(Ordering::Acquire),
            start_calls + 2
        );
        let parking = execution.extension::<ToolLaneParkingAuthority>().unwrap();
        let parked = parking.park().await?;
        for name in children {
            wait_for_message(
                &self.turns,
                &SessionId::new(&name).unwrap(),
                &MessageId::new(format!("task-{name}")).unwrap(),
            )
            .await;
        }
        parked.resume(execution.cancellation).await?;
        ToolResult::new(json!({"created":3}), vec![], false)
    }
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT built from original upstream"]
#[allow(clippy::too_many_lines)] // One native fixture verifies transformed CAS baselines, retry authority and actual child model context.
async fn original_plugins_fork_at_the_captured_terminal_without_replaying_parent_callbacks() {
    let cohort = std::path::PathBuf::from(
        std::env::var("RSI_DSH_INSTALLED_COHORT").expect("independent cohort"),
    );
    let stack = BaseStack::activate().await;
    let (process, extension, connection) = connect(
        &stack,
        &cohort,
        &cohort.join("node_modules/rsi-dsh-plugin-bridge/src/index.mjs"),
    )
    .await;
    let definition = rsi_extension_agent::definition("a".repeat(64)).unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = domains.bind(&definition).unwrap();
    *stack.composition.domains.lock().unwrap() = domains;
    let adapter = rsi_extension_agent::AgentAdapter::new(
        connection.clone(),
        stack.store.clone(),
        stack
            .runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .unwrap(),
        handle.clone(),
        "a".repeat(64),
    )
    .unwrap();
    let transform = Arc::new(CountedTransform {
        adapter: adapter.clone(),
        calls: AtomicUsize::new(0),
        reader: Mutex::new(None),
    });
    let callbacks = contributions::install(
        &stack,
        vec![
            ContributionRegistration::new(
                ContributionId::new("fixture.extension.step").unwrap(),
                0,
                ContributionKind::BeforeStep(adapter.clone()),
            ),
            ContributionRegistration::new(
                ContributionId::new("fixture.extension.settlement").unwrap(),
                0,
                ContributionKind::ToolSettlement(adapter.clone()),
            ),
            ContributionRegistration::new(
                ContributionId::new("fixture.extension.fork").unwrap(),
                0,
                ContributionKind::ForkTransform(transform.clone()),
            ),
        ],
    )
    .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let todo = stack
        .tool_registrar
        .register(ToolRegistration {
            definition: original_todo(&connection).await,
            output: None,
            executor: adapter.tool("todo_write".into()),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
        })
        .unwrap();
    let fork = stack
        .tool_registrar
        .register(ToolRegistration {
            definition: ToolDefinition::new(
                "fork_cut",
                "Fork at completed history",
                json!({"type":"object"}),
            )
            .unwrap()
            .with_scheduling(ToolScheduling::ExclusiveFinal),
            output: None,
            executor: Arc::new(ForkTool {
                turns: turns.clone(),
                store: stack.store.clone(),
                handle: handle.clone(),
                transform: transform.clone(),
            }),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
        })
        .unwrap();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(tool_calls_script(&[(
                "selected-todo",
                "todo_write",
                r#"{"todos":[{"content":"Selected Todo","status":"pending"}]}"#,
            )])),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(tool_calls_script(&[("fork-call", "fork_cut", "{}")])),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(tool_calls_script(&[(
                "nested-call",
                "fork_cut",
                r#"{"nested":true}"#,
            )])),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
            StartOutcome::Stream(answer_script()),
            // The independently resumed child sends its completion to the idle root.
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("extension.fork.provider", provider.clone())
        .await;
    let executor = stack
        .activate_executor_with_config(
            json!({"executor_id":"extension-fork","maximum_active_turns":1}),
        )
        .await;
    let parent = stack
        .submit_fresh(&turns, "parent", "FIRST_PRIVATE_INPUT")
        .await;
    assert_eq!(
        wait_for_outcome(&turns, &parent).await,
        TurnOutcome::Completed
    );
    verify_journal_resources(
        &adapter,
        &stack.store.header(&parent.session_id).await.unwrap(),
    )
    .await;
    let second = {
        let text = "SECOND_SELECTED_INPUT";
        let submitted = turns
            .submit(SubmitTurn {
                session: SubmitSession::Resume(
                    turns.prepare_resume(&parent.session_id).await.unwrap(),
                ),
                turn_id: client_turn_id(),
                text: text.into(),
                model: None,
                reasoning_effort: None,
                sandbox: None,
            })
            .await
            .unwrap();
        assert_eq!(
            wait_for_outcome(&turns, &submitted).await,
            TurnOutcome::Completed
        );
        submitted
    };
    let message = MessageId::new("spawn-activation").unwrap();
    turns
        .submit_message(rsi_agent_turn_protocol::SubmitMessage {
            session: SubmitSession::Resume(turns.prepare_resume(&parent.session_id).await.unwrap()),
            message: rsi_agent_session_protocol::AgentMessage {
                message_id: message.clone(),
                source: rsi_agent_session_protocol::AgentMessageSource::Human { attribution: None },
                content: vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                    text: "CURRENT_EXCLUDED_INPUT".into(),
                }],
                options: rsi_agent_session_protocol::MessageOptions::default(),
            },
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    wait_for_message(&turns, &parent.session_id, &message).await;
    let children_started = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while provider.starts.load(Ordering::Acquire) != 8 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    if children_started.is_err() {
        eprintln!(
            "ready: {:?}, settlement: {:?}",
            turns.ready_health(),
            turns.settlement_health()
        );
        for name in ["full", "last", "none"] {
            let facts = stack
                .store
                .read_facts(
                    &SessionId::new(name).unwrap(),
                    0,
                    256,
                    rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
                )
                .await
                .unwrap();
            eprintln!("{name}: {:?}", facts.facts);
        }
        let facts = stack
            .store
            .read_facts(
                &parent.session_id,
                0,
                256,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap();
        for fact in facts.facts {
            if matches!(fact.body(), SessionFactBody::ToolResult { .. }) {
                eprintln!("{fact:?}");
            }
        }
        panic!(
            "child requests missing: {} starts, {} transforms",
            provider.starts.load(Ordering::Acquire),
            transform.calls.load(Ordering::Acquire)
        );
    }
    let requests = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| serde_json::to_string(request).unwrap())
        .collect::<Vec<_>>();
    for (name, first, second) in [
        ("full", true, true),
        ("last", false, true),
        ("none", false, false),
    ] {
        let request = requests
            .iter()
            .find(|request| request.contains(&format!("CHILD_{name}")))
            .unwrap();
        assert_eq!(request.contains("FIRST_PRIVATE_INPUT"), first, "{name}");
        assert_eq!(request.contains("SECOND_SELECTED_INPUT"), second, "{name}");
        assert!(!request.contains("CURRENT_EXCLUDED_INPUT"), "{name}");
    }
    let source = SessionId::new("full").unwrap();
    let nested_message = MessageId::new("nested-activation").unwrap();
    turns
        .submit_message(rsi_agent_turn_protocol::SubmitMessage {
            session: SubmitSession::Resume(turns.prepare_resume(&source).await.unwrap()),
            message: rsi_agent_session_protocol::AgentMessage {
                message_id: nested_message.clone(),
                source: rsi_agent_session_protocol::AgentMessageSource::Human { attribution: None },
                content: vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                    text: "NESTED_CURRENT_EXCLUDED".into(),
                }],
                options: rsi_agent_session_protocol::MessageOptions::default(),
            },
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    wait_for_message(&turns, &source, &nested_message).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if provider.starts.load(Ordering::Acquire) >= 14
                && stack
                    .store
                    .list_open_turns(&parent.session_id, 0, 1)
                    .await
                    .unwrap()
                    .turns
                    .is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(provider.starts.load(Ordering::Acquire), 14);
    assert_eq!(transform.calls.load(Ordering::Acquire), 4);
    let requests = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| serde_json::to_string(request).unwrap())
        .collect::<Vec<_>>();
    for (name, ancestor, direct) in [
        ("full", true, true),
        ("last", false, true),
        ("none", false, false),
    ] {
        let request = requests
            .iter()
            .find(|request| request.contains(&format!("CHILD_nested-{name}")))
            .unwrap();
        assert_eq!(
            request.contains("FIRST_PRIVATE_INPUT"),
            ancestor,
            "nested {name}"
        );
        assert_eq!(
            request.contains("SECOND_SELECTED_INPUT"),
            ancestor,
            "nested {name}"
        );
        assert_eq!(request.contains("CHILD_full"), direct, "nested {name}");
        assert!(
            !request.contains("NESTED_CURRENT_EXCLUDED"),
            "nested {name}"
        );
    }
    for (name, selection, first, second_input) in [
        ("manual-full", ForkTurnSelection::All, true, true),
        ("manual-last", ForkTurnSelection::Last(1), false, true),
        ("manual-none", ForkTurnSelection::None, false, false),
    ] {
        let prepared = turns
            .prepare_fork(rsi_agent_turn_protocol::PrepareFork {
                source: stack.store.header(&parent.session_id).await.unwrap(),
                session_id: SessionId::new(name).unwrap(),
                created_at_ms: 42,
                agent_preset_id: stack
                    .store
                    .header(&parent.session_id)
                    .await
                    .unwrap()
                    .agent_preset_id()
                    .clone(),
                point: rsi_agent_session_protocol::ForkPoint::through(&second.turn_id),
                turns: selection,
                cancellation: CancellationToken::new(),
            })
            .await
            .unwrap();
        assert!(
            stack
                .store
                .header(prepared.header().session_id())
                .await
                .is_err()
        );
        assert!(prepared.header().execution_owner().is_none());
        let journal = handle
            .decode(&prepared.baseline().initial_states()[0])
            .unwrap();
        let budget = rsi_api_protocol::ByteBudget::new(1024 * 1024).unwrap();
        let events =
            rsi_extension_agent::restore(stack.store.as_ref(), &journal, || budget.clone().into())
                .await
                .unwrap();
        let text = serde_json::to_string(&events).unwrap();
        assert_eq!(text.contains("FIRST_PRIVATE_INPUT"), first, "{name}");
        assert_eq!(
            text.contains("SECOND_SELECTED_INPUT"),
            second_input,
            "{name}"
        );
        assert!(!text.contains("CURRENT_EXCLUDED_INPUT"), "{name}");
        provider
            .outcomes
            .lock()
            .unwrap()
            .push_back(StartOutcome::Stream(answer_script()));
        let submitted = turns
            .submit(SubmitTurn {
                session: SubmitSession::Fresh(prepared),
                turn_id: client_turn_id(),
                text: format!("INPUT_{name}"),
                model: None,
                reasoning_effort: None,
                sandbox: None,
            })
            .await
            .unwrap();
        assert_eq!(
            wait_for_outcome(&turns, &submitted).await,
            TurnOutcome::Completed
        );
        let request =
            serde_json::to_string(provider.requests.lock().unwrap().last().unwrap()).unwrap();
        assert_eq!(request.contains("FIRST_PRIVATE_INPUT"), first, "{name}");
        assert_eq!(
            request.contains("SECOND_SELECTED_INPUT"),
            second_input,
            "{name}"
        );
        assert!(!request.contains("CURRENT_EXCLUDED_INPUT"), "{name}");
    }
    assert_eq!(transform.calls.load(Ordering::Acquire), 6);
    assert_eq!(provider.starts.load(Ordering::Acquire), 17);
    drop((todo, fork));
    assert!(callbacks.dispose().await.is_clean());
    drop(transform);
    drop(adapter);
    drop(connection);
    assert!(extension.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());
    stack.dispose(language, executor).await;
}

async fn verify_journal_resources(
    adapter: &rsi_extension_agent::AgentAdapter,
    header: &SessionHeader,
) {
    use rsi_agent_composition_protocol::SessionResourceReader as _;
    use rsi_agent_session_protocol::SessionResourceValue;
    let head = adapter
        .read(header, None, Some("head"), CancellationToken::new())
        .await
        .unwrap();
    let SessionResourceValue::Read { text, .. } = head else {
        panic!("journal head")
    };
    let head: Value = serde_json::from_str(&text).unwrap();
    let segment = &head["segments"][0];
    let result = adapter
        .read(
            header,
            None,
            segment["sha256"].as_str(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    result.validate().unwrap();
    let SessionResourceValue::Read { text, .. } = result else {
        panic!("journal segment")
    };
    let events: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(events.len() as u64, segment["count"].as_u64().unwrap());
    assert!(
        adapter
            .read(
                header,
                None,
                Some(&"f".repeat(64)),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        adapter
            .read(header, None, Some("head"), cancelled)
            .await
            .is_err()
    );
}
