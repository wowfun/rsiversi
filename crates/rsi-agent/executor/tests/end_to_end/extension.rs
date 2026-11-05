//! Explicit native integration; default deterministic tests never resolve external packages.
use super::*;
use rsi_agent_composition_protocol::{ContributionKind, ContributionRegistration, DomainCatalog};
use rsi_agent_session_protocol::{ContributionId, ExtensionJournal};
#[path = "extension/commands.rs"]
mod commands;
#[path = "extension/fork.rs"]
mod fork;

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_PACKAGE built from the original upstream"]
#[allow(clippy::too_many_lines)] // One opt-in acceptance traces the installed artifact through native execution, atomic publication and recovery.
async fn installed_original_todo_commits_through_kernel_and_restores_without_replay() {
    let source = std::path::PathBuf::from(
        std::env::var("RSI_DSH_PACKAGE").expect("explicit built payload package"),
    );
    let root = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let packages = rsi_packages::PackageStore::open(&root.path().join("packages")).unwrap();
    packages
        .install(
            &[rsi_packages::InstallSource {
                directory: source.clone(),
                manifest: std::fs::read(source.join("package.json")).unwrap(),
                provenance: rsi_packages::Provenance::Local,
            }],
            &[],
            &[],
            "windows-x86_64",
        )
        .unwrap();
    let package = Arc::new(packages.acquire("dsh-acceptance-cohort").unwrap());
    eprintln!(
        "installed and acquired original payload in {:?}",
        started.elapsed()
    );
    let payload = package.payload("node_modules").unwrap();
    let cohort = payload.path().parent().unwrap();
    let stack = BaseStack::activate().await;
    let (process, extension, connection) = connect(
        &stack,
        cohort,
        &payload.path().join("rsi-dsh-plugin-bridge/src/index.mjs"),
    )
    .await;
    let definition = rsi_extension_agent::definition(package.digest().to_owned()).unwrap();
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
        package.digest().to_owned(),
    )
    .unwrap();
    let callbacks = contributions::install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.extension.settlement").unwrap(),
            0,
            ContributionKind::ToolSettlement(adapter.clone()),
        )],
    )
    .await;
    let definition = original_todo(&connection).await;
    let tool = stack
        .tool_registrar
        .register(ToolRegistration {
            definition,
            output: None,
            executor: adapter.tool("todo_write".into()),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
        })
        .unwrap();
    let args =
        json!({"todos":[{"content":"Verify Kernel receipt","status":"in_progress"}]}).to_string();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[("invalid-todo", "todo_write", "{}")])),
            StartOutcome::Stream(tool_calls_script(&[("original-todo", "todo_write", &args)])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("extension.provider", provider)
        .await;
    let executor = stack.activate_executor("extension-executor").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            session: stack.fresh(header()).await,
            turn_id: client_turn_id(),
            text: "write original Todo".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(outcome) = turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap()
            {
                break outcome;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);
    let states = stack
        .store
        .read_domain_states(&submitted.session_id, None, None)
        .await
        .unwrap();
    let journal: ExtensionJournal = handle.decode(&states.states[0].snapshot).unwrap();
    assert_eq!(journal.segments.len(), 1);
    let budget = rsi_api_protocol::ByteBudget::new(1024 * 1024).unwrap();
    let events =
        rsi_extension_agent::restore(stack.store.as_ref(), &journal, || budget.clone().into())
            .await
            .unwrap();
    let call = events
        .iter()
        .position(|event| {
            event["type"] == "tool/call" && event["data"]["callId"] == "original-todo"
        })
        .unwrap();
    let mutation = events
        .iter()
        .position(|event| event["type"] == "todo/write")
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "todo/write")
            .count(),
        1
    );
    assert!(
        call < mutation,
        "the first Todo diff must see the list before this invocation"
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
    let results: Vec<_> = facts
        .iter()
        .filter(|fact| matches!(fact.body(), SessionFactBody::ToolResult { .. }))
        .collect();
    assert_eq!(results.len(), 2);
    for (fact, expected_call) in results.into_iter().zip(["invalid-todo", "original-todo"]) {
        let SessionFactBody::ToolResult {
            effect_id,
            identity,
            result,
            ..
        } = fact.body()
        else {
            unreachable!()
        };
        assert_eq!(identity.call_id(), expected_call);
        assert_eq!(result.is_error, expected_call == "invalid-todo");
        if result.is_error {
            assert!(
                matches!(result.content.as_slice(), [rsi_tools_protocol::ToolContent::Text { text }] if text.contains("invalid arguments") && text.contains("todos"))
            );
        }
        let receipt = stack
            .store
            .read_domain_request(
                &submitted.session_id,
                &rsi_agent_session_protocol::DomainRequestId::new(effect_id.as_str()).unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        let rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted { commit } =
            receipt.body()
        else {
            unreachable!()
        };
        assert_eq!(commit.fact_span().unwrap().first_seq(), fact.seq());
    }
    let before = connection
        .request(
            "session.project",
            json!({"sessionId": submitted.session_id}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let open = json!({"session_id": submitted.session_id, "created_at": header().created_at_ms(),
        "local_cwd":header().canonical_cwd(),"revision": states.states[0].head.revision.get(), "count": events.len()});
    let begin = connection
        .request("agent.load.begin", open.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(begin, "current");
    drop(tool);
    assert!(callbacks.dispose().await.is_clean());
    drop(adapter);
    drop(connection);
    assert!(extension.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());

    let (process, extension, connection) = connect(
        &stack,
        cohort,
        &payload.path().join("rsi-dsh-plugin-bridge/src/index.mjs"),
    )
    .await;
    let begin = connection
        .request("agent.load.begin", open, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(begin, "transfer");
    connection
        .request(
            "agent.load.events",
            json!({"session_id": submitted.session_id, "offset": 0, "events": events}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    connection
        .request(
            "agent.load.finish",
            json!({"session_id": submitted.session_id,"checkpoint":journal.checkpoint}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let after = connection
        .request(
            "session.project",
            json!({"sessionId": submitted.session_id}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(before, after);
    eprintln!(
        "original Todo Kernel publication and recovery verified in {:?}",
        started.elapsed()
    );
    drop(connection);
    assert!(extension.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());
    stack.dispose(language, executor).await;
    drop(payload);
    drop(package);
}

async fn connect(
    stack: &BaseStack,
    cohort: &std::path::Path,
    entry: &std::path::Path,
) -> (FiberHandle, FiberHandle, Arc<rsi_extension::Connection>) {
    let node = std::env::var("RSI_EXTENSION_NODE").expect("explicit native Node executable");
    let process = activate_configured_fixture(
        &stack.runtime,
        "extension.process",
        "test",
        Arc::new(rsi_process_local::ProcessLocalFactory),
        json!({}),
    )
    .await;
    let environment: std::collections::BTreeMap<&str, String> = [
        #[cfg(windows)]
        ("SystemRoot", std::env::var("SystemRoot").unwrap()),
    ]
    .into_iter()
    .collect();
    let extension = activate_configured_fixture(&stack.runtime, "extension.host", "test",
        Arc::new(rsi_extension::ExtensionFactory::new(Arc::new(rsi_extension::RejectRequests))), json!({
            "program": node, "cwd": cohort, "generation": "original-todo-kernel", "environment": environment,
            "arguments": [cohort.join("node_modules/rsi-dsh-cordis/src/host.mjs"), std::path::PathBuf::from("original-todo-kernel"), entry.to_path_buf(), cohort.to_path_buf()],
        })).await;
    let connection = stack
        .runtime
        .root()
        .lookup_local::<rsi_extension::ExtensionContract>()
        .unwrap();
    (process, extension, connection)
}
async fn original_todo(connection: &rsi_extension::Connection) -> ToolDefinition {
    original_tool(connection, "todo_write").await
}
async fn original_tool(connection: &rsi_extension::Connection, name: &str) -> ToolDefinition {
    let catalog = connection
        .request("agent.catalog", Value::Null, CancellationToken::new())
        .await
        .unwrap();
    let todo = catalog["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["definition"]["name"] == name)
        .unwrap();
    serde_json::from_value(todo["definition"].clone()).unwrap()
}

#[derive(Debug)]
struct ReviewFixture {
    choice: &'static str,
    entered: Notify,
    release: Notify,
    request: Mutex<Option<rsi_user_questions_protocol::QuestionRequest>>,
}
#[async_trait]
impl rsi_user_questions_protocol::UserQuestions for ReviewFixture {
    async fn ask(
        &self,
        request: rsi_user_questions_protocol::QuestionRequest,
        cancellation: CancellationToken,
    ) -> rsi_user_questions_protocol::Result<rsi_user_questions_protocol::QuestionAnswer> {
        request.validate()?;
        *self.request.lock().unwrap() = Some(request.clone());
        self.entered.notify_one();
        tokio::select! {
            () = cancellation.cancelled() => return Err(rsi_user_questions_protocol::QuestionError::Cancelled),
            () = self.release.notified() => {},
        }
        Ok(rsi_user_questions_protocol::QuestionAnswer {
            answers: vec![],
            review: Some(rsi_user_questions_protocol::ReviewAnswer {
                binding: request.review.unwrap().binding,
                choice_id: self.choice.into(),
                feedback: None,
            }),
        })
    }
    async fn pending(
        &self,
        _: &str,
    ) -> rsi_user_questions_protocol::Result<Vec<rsi_user_questions_protocol::QuestionRequest>>
    {
        unreachable!()
    }
    async fn pending_for_sessions(
        &self,
        _: &[String],
    ) -> rsi_user_questions_protocol::Result<Vec<rsi_user_questions_protocol::QuestionRequest>>
    {
        unreachable!()
    }
    fn watch_pending(
        &self,
        _: &[String],
    ) -> rsi_user_questions_protocol::Result<rsi_user_questions_protocol::PendingChanges> {
        unreachable!()
    }
    async fn answer(
        &self,
        _: &str,
        _: &str,
        _: rsi_user_questions_protocol::QuestionAnswer,
    ) -> rsi_user_questions_protocol::Result<bool> {
        unreachable!()
    }
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT built from original upstream"]
#[allow(clippy::too_many_lines)] // One complete native review traces exact authority, parking, independent progress and canonical publication.
async fn original_plan_review_parks_the_kernel_and_rejects_answers_invalidated_by_commands() {
    use rsi_agent_composition_protocol::{DomainDefinition, DomainReferencePolicy};
    use rsi_agent_session_protocol::{
        DomainIdentity, DomainRevision, DomainStateValue, EXTENSION_JOURNAL_DOMAIN,
    };
    let cohort = std::path::PathBuf::from(
        std::env::var("RSI_DSH_INSTALLED_COHORT").expect("independent cohort"),
    );
    for choice in ["approve", "dismiss", "command"] {
        let stack = BaseStack::activate().await;
        let (process, extension, connection) = connect(
            &stack,
            &cohort,
            &cohort.join("node_modules/rsi-dsh-plugin-bridge/src/index.mjs"),
        )
        .await;
        let definition = rsi_extension_agent::definition("a".repeat(64)).unwrap();
        let initial_catalog = DomainCatalog::new([definition.registration()]).unwrap();
        let initial_handle = initial_catalog.bind(&definition).unwrap();
        let empty = ExtensionJournal {
            artifact_sha256: "a".repeat(64),
            segments: vec![],
            sessions: vec![],
            checkpoint: DomainStateValue::new(Value::Null).unwrap(),
        };
        // This fixture starts from persisted Plan mode; lifecycle events must come
        // from the real captured RSI Facts, not a preseeded synthetic Turn.
        let prepared = rsi_extension_agent::prepare(
            stack.store.as_ref(),
            &initial_handle,
            DomainRevision::default(),
            &empty,
            vec![json!({"seq":0,"time":1,"type":"plan/mode","data":{"active":true}})],
            DomainStateValue::new(Value::Null).unwrap(),
            rsi_api_protocol::ByteBudget::new(1024 * 1024)
                .unwrap()
                .into(),
        )
        .await
        .unwrap();
        let initial = initial_handle
            .decode(prepared.proposal().snapshot())
            .unwrap();
        let definition = DomainDefinition::new(
            DomainIdentity::new(EXTENSION_JOURNAL_DOMAIN, 1).unwrap(),
            &initial,
            ExtensionJournal::validate,
            DomainReferencePolicy::Visit(ExtensionJournal::references),
        )
        .unwrap();
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
        let review = Arc::new(ReviewFixture {
            choice: if choice == "command" {
                "approve"
            } else {
                choice
            },
            entered: Notify::new(),
            release: Notify::new(),
            request: Mutex::new(None),
        });
        adapter.enable_questions(review.clone()).unwrap();
        let callbacks = contributions::install(
            &stack,
            vec![
                commands::registration(&adapter),
                ContributionRegistration::new(
                    ContributionId::new("fixture.extension.settlement").unwrap(),
                    0,
                    ContributionKind::ToolSettlement(adapter.clone()),
                ),
                ContributionRegistration::new(
                    ContributionId::new("fixture.extension.before-step").unwrap(),
                    0,
                    ContributionKind::BeforeStep(adapter.clone()),
                ),
            ],
        )
        .await;
        let tool = stack
            .tool_registrar
            .register(ToolRegistration {
                definition: original_tool(&connection, "exit_plan_mode")
                    .await
                    .with_scheduling(ToolScheduling::ExclusiveFinal),
                output: None,
                executor: adapter.tool("exit_plan_mode".into()),
                timeout: rsi_tools_protocol::ToolTimeoutPolicy::HumanInteraction,
            })
            .unwrap();
        let arguments = json!({"plan":"# Original Plan\nReview the exact content."}).to_string();
        let provider = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from([
                StartOutcome::Stream(tool_calls_script(&[(
                    "review",
                    "exit_plan_mode",
                    &arguments,
                )])),
                StartOutcome::Stream(answer_script()),
                StartOutcome::Stream(answer_script()),
            ])),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language = stack
            .activate_language("extension.provider", provider.clone())
            .await;
        let executor = stack
            .activate_executor_with_config(
                json!({"executor_id":"extension-review","maximum_active_turns":1}),
            )
            .await;
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let first = stack
            .submit_fresh(&turns, "review-session", "review original Plan")
            .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            review.entered.notified(),
        )
        .await
        .expect("original review did not park");
        let request = review.request.lock().unwrap().clone().unwrap();
        assert_eq!(request.session_id, first.session_id.as_str());
        assert_eq!(request.turn_id, first.turn_id.as_str());
        assert!(request.questions[0].prompt.contains("# Original Plan"));
        assert_eq!(
            request
                .review
                .unwrap()
                .choices
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["approve", "keep_planning", "dismiss"]
        );
        let second = stack
            .submit_fresh(&turns, "independent-session", "progress while human waits")
            .await;
        assert_eq!(
            wait_for_outcome(&turns, &second).await,
            TurnOutcome::Completed
        );
        assert!(
            turns
                .outcome(&first.session_id, &first.turn_id)
                .await
                .unwrap()
                .is_none()
        );
        if choice == "command" {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                commands::durable_command(&stack, &first.session_id, "parked-off", "off"),
            )
            .await
            .expect("parked review must release the command lane");
            assert!(
                turns
                    .outcome(&first.session_id, &first.turn_id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        review.release.notify_one();
        let outcome = wait_for_outcome(&turns, &first).await;
        if choice == "command" {
            assert!(
                matches!(outcome, TurnOutcome::Failed { ref code, .. } if code == "tool.execution"),
                "{outcome:?}"
            );
            // The original handler keeps an already selected value pending until
            // pre-step, even after terminal history becomes visible.
            commands::durable_command(&stack, &first.session_id, "after-review-off", "off").await;
        } else {
            assert_eq!(outcome, TurnOutcome::Completed);
        }
        assert_eq!(
            provider.starts.load(Ordering::Acquire),
            if choice == "approve" { 3 } else { 2 }
        );
        let states = stack
            .store
            .read_domain_states(&first.session_id, None, None)
            .await
            .unwrap();
        let journal = handle.decode(&states.states[0].snapshot).unwrap();
        assert_eq!(journal.checkpoint.value()["mapping"]["turn"], 1);
        if choice == "command" {
            assert!(journal.checkpoint.value()["mapping"]["turnId"].is_null());
        } else {
            assert_eq!(
                journal.checkpoint.value()["mapping"]["turnId"],
                first.turn_id.as_str()
            );
        }
        assert_eq!(
            journal.checkpoint.value()["views"]["values"]["plan"]["active"],
            choice != "approve"
        );
        if choice == "approve" {
            assert!(journal.checkpoint.value()["pendingIntent"].is_null());
            let requests = provider.requests.lock().unwrap();
            assert!(
                serde_json::to_string(&requests[0])
                    .unwrap()
                    .contains("Plan the work before making changes.")
            );
            assert!(
                !serde_json::to_string(&requests[2])
                    .unwrap()
                    .contains("Plan the work before making changes.")
            );
        } else if choice == "command" {
            assert_eq!(journal.checkpoint.value()["pendingIntent"]["active"], false);
            let (_, outcome) = stack
                .resume_and_wait(
                    "continue after the replaced review",
                    first.session_id.clone(),
                )
                .await;
            assert_eq!(outcome, TurnOutcome::Completed);
            let states = stack
                .store
                .read_domain_states(&first.session_id, None, None)
                .await
                .unwrap();
            let journal = handle.decode(&states.states[0].snapshot).unwrap();
            assert_eq!(
                journal.checkpoint.value()["views"]["values"]["plan"]["active"],
                false
            );
            assert!(journal.checkpoint.value()["pendingIntent"].is_null());
            assert!(
                !serde_json::to_string(&provider.requests.lock().unwrap()[2])
                    .unwrap()
                    .contains("Plan the work before making changes.")
            );
        } else {
            assert!(journal.checkpoint.value()["pendingIntent"].is_null());
        }
        drop((tool, prepared));
        assert!(callbacks.dispose().await.is_clean());
        drop(adapter);
        drop(connection);
        assert!(extension.dispose().await.is_clean());
        assert!(process.dispose().await.is_clean());
        stack.dispose(language, executor).await;
    }
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT with both packed plugins"]
#[allow(clippy::too_many_lines)] // The two Turns prove exact receipt recovery across a failed notification without Tool replay.
async fn original_todo_reconciles_lost_confirmation_before_the_next_turn() {
    lost_reply(false).await;
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT with both packed plugins"]
async fn original_todo_rolls_back_a_lost_staged_reply_only_after_terminal_absence() {
    lost_reply(true).await;
}

#[allow(clippy::too_many_lines)] // Both sides of the acknowledgement boundary share the same original-package fixture.
async fn lost_reply(lose_stage: bool) {
    let cohort = std::path::PathBuf::from(
        std::env::var("RSI_DSH_INSTALLED_COHORT").expect("explicit independent cohort"),
    );
    let stack = BaseStack::activate().await;
    let entry = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/extension-lost-ack.mjs");
    let (process, extension, connection) = connect(&stack, &cohort, &entry).await;
    if lose_stage {
        connection
            .request(
                "fixture.arm",
                json!({"mode":"stage"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
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
    let callbacks = contributions::install(
        &stack,
        vec![ContributionRegistration::new(
            ContributionId::new("fixture.extension.settlement").unwrap(),
            0,
            ContributionKind::ToolSettlement(adapter.clone()),
        )],
    )
    .await;
    let tool = stack
        .tool_registrar
        .register(ToolRegistration {
            definition: original_todo(&connection).await,
            output: None,
            executor: adapter.tool("todo_write".into()),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 30_000 },
        })
        .unwrap();
    let args = json!({"todos":[{"content":"Exactly once","status":"pending"}]}).to_string();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[("lost-ack", "todo_write", &args)])),
            StartOutcome::Stream(tool_calls_script(&[(
                "next-effect",
                "todo_write",
                "{\"todos\":[]}",
            )])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("extension.provider", provider)
        .await;
    let executor = stack.activate_executor("extension-executor").await;
    let (submitted, outcome) = stack
        .submit_and_wait("lose only the publication notification")
        .await;
    assert!(
        if lose_stage {
            matches!(outcome, TurnOutcome::Interrupted { .. })
        } else {
            matches!(outcome, TurnOutcome::Failed { .. })
        },
        "{outcome:?}"
    );
    let states = stack
        .store
        .read_domain_states(&submitted.session_id, None, None)
        .await
        .unwrap();
    assert_eq!(
        handle
            .decode(&states.states[0].snapshot)
            .unwrap()
            .segments
            .len(),
        usize::from(!lose_stage)
    );
    let (_, outcome) = stack
        .resume_and_wait(
            "confirm the prior receipt then run a different effect",
            submitted.session_id.clone(),
        )
        .await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let counts = connection
        .request("fixture.counts", Value::Null, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        counts,
        json!({"executions":2,"acknowledgements": if lose_stage {1} else {3}})
    );
    let states = stack
        .store
        .read_domain_states(&submitted.session_id, None, None)
        .await
        .unwrap();
    let journal = handle.decode(&states.states[0].snapshot).unwrap();
    let budget = rsi_api_protocol::ByteBudget::new(1024 * 1024).unwrap();
    let events =
        rsi_extension_agent::restore(stack.store.as_ref(), &journal, || budget.clone().into())
            .await
            .unwrap();
    assert_eq!(
        journal
            .segments
            .iter()
            .map(|segment| segment.count)
            .sum::<u64>(),
        u64::try_from(events.len()).unwrap(),
    );
    let mut writes = Vec::new();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["seq"].as_u64(), Some(u64::try_from(index).unwrap()));
        match event["type"].as_str() {
            Some("turn/start") => writes.push(0),
            Some("todo/write") => *writes.last_mut().expect("Todo requires an active Turn") += 1,
            _ => {}
        }
    }
    assert_eq!(writes, [usize::from(!lose_stage), 1]);
    drop(tool);
    assert!(callbacks.dispose().await.is_clean());
    drop(adapter);
    drop(connection);
    assert!(extension.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());
    stack.dispose(language, executor).await;
}
