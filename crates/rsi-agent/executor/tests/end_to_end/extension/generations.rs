use super::*;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT"]
async fn dropped_generation_validation_during_cas_read_still_aborts_the_peer_load() {
    use rsi_agent_composition_protocol::GenerationValidator;
    let cohort = std::path::PathBuf::from(std::env::var("RSI_DSH_INSTALLED_COHORT").unwrap());
    let entry = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/extension-stalled-load.mjs");
    let stack = BaseStack::activate().await;
    let (process, extension, connection) = connect(&stack, &cohort, &entry).await;
    let definition = rsi_extension_agent::definition().unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = domains.bind(&definition).unwrap();
    let initial = handle.decode(&domains.baseline()[0]).unwrap();
    let prepared = rsi_extension_agent::prepare(
        stack.store.as_ref(),
        &handle,
        rsi_agent_session_protocol::DomainRevision::new(1),
        &initial,
        vec![json!({"event":"saved"})],
        rsi_agent_session_protocol::DomainStateValue::new(json!({})).unwrap(),
        rsi_api_protocol::ByteBudget::default().into(),
    )
    .await
    .unwrap();
    let updates = vec![rsi_agent_session_protocol::DomainStateView {
        revision: rsi_agent_session_protocol::DomainRevision::new(2),
        snapshot: prepared.proposal().snapshot().clone(),
    }];
    let adapter = rsi_extension_agent::AgentAdapter::new(
        connection.clone(),
        stack.store.clone(),
        stack
            .runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .unwrap(),
        handle,
    )
    .unwrap();
    let (entered, release) = stack.store.pause_next_cas_read();
    let loading = {
        let adapter = adapter.clone();
        tokio::spawn(async move {
            adapter
                .validate(&header(), &updates, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    loading.abort();
    assert!(loading.await.unwrap_err().is_cancelled());
    drop(release);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if connection
                .request("fixture.aborts", Value::Null, CancellationToken::new())
                .await
                .unwrap()
                == json!(1)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping a CAS read must not abandon peer mirror cleanup");
    // The fixture's abort never resolves; its bounded cleanup must not keep a
    // lane or transport alive indefinitely.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(extension.dispose().await.is_clean());
    drop((adapter, connection));
    assert!(process.dispose().await.is_clean());
    stack.dispose_services().await;
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT"]
async fn generation_load_failure_bounds_abort_and_preserves_transport_errors() {
    use rsi_agent_composition_protocol::{ContributionError, GenerationValidator};
    let cohort = std::path::PathBuf::from(std::env::var("RSI_DSH_INSTALLED_COHORT").unwrap());
    let entry = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/extension-stalled-load.mjs");
    let stack = BaseStack::activate().await;
    let (process, extension, connection) = connect(&stack, &cohort, &entry).await;
    let definition = rsi_extension_agent::definition().unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let adapter = rsi_extension_agent::AgentAdapter::new(
        connection.clone(),
        stack.store.clone(),
        stack
            .runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .unwrap(),
        domains.bind(&definition).unwrap(),
    )
    .unwrap();
    let updates = [rsi_agent_session_protocol::DomainStateView {
        revision: rsi_agent_session_protocol::DomainRevision::new(1),
        snapshot: domains.baseline()[0].clone(),
    }];
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        adapter.validate(&header(), &updates, CancellationToken::new()),
    )
    .await
    .expect("a responsive peer with a stalled abort must not hold validation forever");
    assert!(matches!(result, Err(ContributionError::Invalid(_))));
    // The dropped abort request sends Cancel. Verify the peer acknowledges it
    // before the connection's five-second cancellation deadline, even though
    // the plugin's abort handler itself never settles.
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    assert_eq!(
        connection
            .request("fixture.aborts", Value::Null, CancellationToken::new())
            .await
            .unwrap(),
        1
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(4),
            adapter.validate(&header(), &updates, cancelled)
        )
        .await
        .unwrap(),
        Err(ContributionError::Closed)
    );
    assert!(extension.dispose().await.is_clean());
    assert_eq!(
        adapter
            .validate(&header(), &updates, CancellationToken::new())
            .await,
        Err(ContributionError::Closed)
    );
    drop(adapter);
    drop(connection);
    assert!(process.dispose().await.is_clean());
    stack.dispose_services().await;
}

#[derive(Debug)]
struct SelectReplacement {
    inner: Arc<dyn ToolExecutor>,
    source: Arc<CompositionFixture>,
    candidate: AgentCompositionPin,
}
#[async_trait]
impl ToolExecutor for SelectReplacement {
    async fn execute(&self, input: Value, execution: ToolExecution) -> ToolResultType<ToolResult> {
        let result = self.inner.execute(input, execution).await?;
        *self.source.pin.lock().unwrap() = Some(self.candidate.clone());
        Ok(result)
    }
}

fn registration(definition: ToolDefinition, executor: Arc<dyn ToolExecutor>) -> ToolRegistration {
    ToolRegistration {
        definition,
        executor,
        output: None,
        run_policy: rsi_tools_protocol::ToolRunPolicy::execution(std::time::Duration::from_secs(
            10,
        )),
    }
}

async fn counters(connection: &rsi_extension::Connection) -> Value {
    connection
        .request("fixture.generation", Value::Null, CancellationToken::new())
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires RSI_EXTENSION_NODE and RSI_DSH_INSTALLED_COHORT with original Todo and Plan plugins"]
#[expect(
    clippy::too_many_lines,
    reason = "One real process pair follows a running Session through old-batch settlement, candidate load, commit or rejection, and journal verification"
)]
async fn real_process_replacement_loads_committed_state_before_selection_and_keeps_old_on_rejection()
 {
    for reject in [false, true] {
        let cohort = std::path::PathBuf::from(std::env::var("RSI_DSH_INSTALLED_COHORT").unwrap());
        let fixture = tempfile::tempdir().unwrap();
        let template = include_str!("../../fixtures/extension-generation.mjs");
        for (version, reject_state) in [("v1", false), ("v2", reject)] {
            std::fs::write(
                fixture.path().join(format!("{version}.mjs")),
                template.replace("__GENERATION__", version).replace(
                    "__REJECT_STATE__",
                    if reject_state { "true" } else { "false" },
                ),
            )
            .unwrap();
        }
        let stack = BaseStack::activate().await;
        let candidate_stack = BaseStack::activate().await;
        let (old_process, old_extension, old_connection) =
            connect(&stack, &cohort, &fixture.path().join("v1.mjs")).await;
        let (new_process, new_extension, new_connection) =
            connect(&candidate_stack, &cohort, &fixture.path().join("v2.mjs")).await;
        let definition = rsi_extension_agent::definition().unwrap();
        let domains = DomainCatalog::new([definition.registration()]).unwrap();
        let handle = domains.bind(&definition).unwrap();
        *stack.composition.domains.lock().unwrap() = domains;
        let turns = stack
            .runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .unwrap();
        let old_adapter = rsi_extension_agent::AgentAdapter::new(
            old_connection.clone(),
            stack.store.clone(),
            turns.clone(),
            handle.clone(),
        )
        .unwrap();
        let old_callbacks = contributions::install(
            &stack,
            vec![ContributionRegistration::new(
                ContributionId::new("fixture.old.settlement").unwrap(),
                0,
                ContributionKind::ToolSettlement(old_adapter.clone()),
            )],
        )
        .await;
        let next_domains = DomainCatalog::new([definition.registration()]).unwrap();
        let next_handle = next_domains.bind(&definition).unwrap();
        let new_adapter = rsi_extension_agent::AgentAdapter::new(
            new_connection.clone(),
            stack.store.clone(),
            turns,
            next_handle.clone(),
        )
        .unwrap();
        let new_callbacks = contributions::install(
            &candidate_stack,
            vec![
                ContributionRegistration::new(
                    ContributionId::new("fixture.new.validation").unwrap(),
                    0,
                    ContributionKind::GenerationValidation(new_adapter.clone()),
                ),
                ContributionRegistration::new(
                    ContributionId::new("fixture.new.settlement").unwrap(),
                    0,
                    ContributionKind::ToolSettlement(new_adapter.clone()),
                ),
            ],
        )
        .await;
        let provider = stack
            .runtime
            .root()
            .lookup_local::<ToolCatalogProviderContract>()
            .unwrap();
        let stage = provider.begin_stage().unwrap();
        let next_lease = stage
            .registrar()
            .register(registration(
                original_todo(&new_connection).await,
                new_adapter.tool("todo_write".into()),
            ))
            .unwrap();
        let next = AgentCompositionPin::new(
            AgentPresetId::new("test-agent").unwrap(),
            "c".repeat(64),
            stage.seal().unwrap(),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            next_domains,
            candidate_stack
                .composition
                .contributions
                .lock()
                .unwrap()
                .clone(),
            Arc::new(next_lease),
        )
        .unwrap();
        let old_lease = stack
            .tool_registrar
            .register(registration(
                original_todo(&old_connection).await,
                Arc::new(SelectReplacement {
                    inner: old_adapter.tool("todo_write".into()),
                    source: stack.composition.clone(),
                    candidate: next.clone(),
                }),
            ))
            .unwrap();
        let arguments = r#"{"todos":[{"content":"Preserve committed work","status":"completed"}]}"#;
        let language = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from([
                StartOutcome::Stream(tool_calls_script(&[
                    ("first", "todo_write", arguments),
                    ("second", "todo_write", arguments),
                ])),
                StartOutcome::Stream(tool_calls_script(&[("third", "todo_write", arguments)])),
                StartOutcome::Stream(answer_script()),
            ])),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language_fiber = stack
            .activate_language("test.process-generation", language.clone())
            .await;
        let executor = stack.activate_executor("process-generation-executor").await;
        let service = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let submitted = stack
            .submit_fresh(
                &service,
                "process-generation",
                "continue the same running Session",
            )
            .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            service.wait_idle(&submitted.session_id),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            service
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap(),
            Some(TurnOutcome::Completed)
        );
        let old = counters(&old_connection).await;
        let new = counters(&new_connection).await;
        assert_eq!(old["version"], "v1");
        assert_eq!(new["version"], "v2");
        assert_eq!(old["tools"], if reject { 3 } else { 2 }, "{old} {new}");
        assert_eq!(new["tools"], i32::from(!reject), "{old} {new}");
        let page = stack
            .store
            .read_domain_states(&submitted.session_id, None, None)
            .await
            .unwrap();
        let journal_state = page
            .states
            .iter()
            .find(|view| {
                view.head.identity.id() == rsi_agent_session_protocol::EXTENSION_JOURNAL_DOMAIN
            })
            .unwrap();
        let journal = if reject {
            handle.decode(&journal_state.snapshot)
        } else {
            next_handle.decode(&journal_state.snapshot)
        }
        .unwrap();
        let events = rsi_extension_agent::restore(stack.store.as_ref(), &journal, || {
            rsi_api_protocol::ByteBudget::default().into()
        })
        .await
        .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "todo/write")
                .count(),
            3
        );
        assert_eq!(
            page.generation.unwrap().selection == *next.selection(),
            !reject
        );
        assert!(
            new["loads"]
                .as_array()
                .unwrap()
                .iter()
                .any(|load| load["reload"] == true && load["todos"] == 2),
            "candidate must load both committed old-generation Tool outcomes: {new}"
        );
        if !reject {
            assert_eq!(
                new["finished"], 1,
                "candidate preload must avoid a second full transfer: {new}"
            );
            assert!(
                new["loads"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|load| load["reload"] == false)
            );
        }
        assert_eq!(language.requests.lock().unwrap().len(), 3);
        drop(old_lease);
        assert!(old_callbacks.dispose().await.is_clean());
        assert!(new_callbacks.dispose().await.is_clean());
        drop(old_adapter);
        drop(new_adapter);
        drop(old_connection);
        drop(new_connection);
        assert!(old_extension.dispose().await.is_clean());
        assert!(new_extension.dispose().await.is_clean());
        assert!(old_process.dispose().await.is_clean());
        assert!(new_process.dispose().await.is_clean());
        stack.dispose(language_fiber, executor).await;
        candidate_stack.dispose_services().await;
    }
}
