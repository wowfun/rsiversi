use super::*;
use rsi_agent_composition_protocol::GenerationValidator;

#[derive(Debug, Default)]
struct CapturedContributions(Mutex<Vec<ContributionRegistration>>);
#[derive(Debug)]
struct SpillDependencies {
    store: Arc<rsi_spill_local::LocalSpillStore>,
    registrar: Arc<CapturedContributions>,
}
#[async_trait]
impl PluginFactory for SpillDependencies {
    fn prepare(&self, _: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let store = plan
            .context()
            .provide_local::<rsi_spill::SpillStoreContract>(self.store.clone())?;
        let registrar = plan
            .context()
            .provide_local::<rsi_agent_composition_protocol::ContributionRegistrarContract>(
            self.registrar.clone(),
        )?;
        plan.defer(
            "withdraw Spill dependencies",
            Box::new(move || {
                Box::pin(async move {
                    drop((store, registrar));
                    Ok(())
                })
            }),
        )
    }
}
impl rsi_agent_composition_protocol::ContributionRegistrar for CapturedContributions {
    fn register(
        &self,
        context: &rsi_meta::RegistrationContext,
        contribution: ContributionRegistration,
    ) -> rsi_agent_composition_protocol::ContributionResult<rsi_meta::RegistrationLease> {
        let (_, lease) = context
            .register("spill integration", || Ok(()), Ok)
            .map_err(|e| {
                rsi_agent_composition_protocol::ContributionError::Invalid(e.to_string())
            })?;
        self.0.lock().unwrap().push(contribution);
        Ok(lease)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires RSI_EXTENSION_NODE and current RSI_DSH_INSTALLED_COHORT"]
#[allow(clippy::too_many_lines)] // One actual business trace includes native admission, publication and generation reload.
async fn native_sized_tool_arguments_publish_and_reload_without_reexecution() {
    let cohort = std::path::PathBuf::from(std::env::var("RSI_DSH_INSTALLED_COHORT").unwrap());
    let entry = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/extension-body-probe.mjs");
    let stack = BaseStack::activate().await;
    let scratch = tempfile::tempdir().unwrap();
    let mut config = rsi_spill_local::SpillLocalConfig::new(scratch.path().join("spill"));
    config.minimum_free_bytes = 0;
    let spill = rsi_spill_local::LocalSpillStore::open(config)
        .await
        .unwrap();
    let captured = Arc::new(CapturedContributions::default());
    let dependencies = stack
        .runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fixture.spill.dependencies",
                "v1",
                UpdateMode::Replayable,
                Arc::new(SpillDependencies {
                    store: spill.clone(),
                    registrar: captured.clone(),
                }),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let policy = stack
        .runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fixture.spill",
                "v1",
                UpdateMode::Replayable,
                Arc::new(rsi_spill_policy::SpillPolicyFactory),
            ),
            json!({"max_inline_tokens":12500}),
        )
        .await
        .unwrap();
    let (process, extension, connection) = connect(&stack, &cohort, &entry).await;
    let definition = rsi_extension_agent::definition().unwrap();
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
    )
    .unwrap();
    let mut registrations = std::mem::take(&mut *captured.0.lock().unwrap());
    assert_eq!(
        registrations.len(),
        1,
        "actual Spill factory must install its around contribution"
    );
    registrations.push(ContributionRegistration::new(
        ContributionId::new("fixture.body.settlement").unwrap(),
        0,
        ContributionKind::ToolSettlement(adapter.clone()),
    ));
    let callbacks = contributions::install(&stack, registrations).await;
    let tool = stack
        .tool_registrar
        .register(ToolRegistration {
            definition: original_tool(&connection, "body_probe").await,
            output: None,
            executor: adapter.tool("body_probe".into()),
            run_policy: rsi_tools_protocol::ToolRunPolicy::execution(
                std::time::Duration::from_secs(30),
            ),
        })
        .unwrap();
    let args = json!({"text":"界".repeat(1024*1024)}).to_string();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[("large", "body_probe", &args)])),
            StartOutcome::Stream(tool_calls_script(&[(
                "small",
                "body_probe",
                "{\"text\":\"after reload\"}",
            )])),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("extension.body.provider", provider)
        .await;
    let executor = stack.activate_executor("extension.body.executor").await;
    let (submitted, outcome) = run(&stack, None).await;
    assert_eq!(outcome, TurnOutcome::Completed);
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
    let result = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolResult { result, .. } => Some(result),
            _ => None,
        })
        .unwrap();
    assert_eq!(result.value, json!("界".repeat(1024 * 1024)));
    assert_eq!(result.spills.len(), 1);
    let mut restored_output = Vec::new();
    let expected_bytes = usize::try_from(result.spills[0].bytes).unwrap();
    while restored_output.len() < expected_bytes {
        use rsi_spill::SpillReader as _;
        let page = spill
            .read(
                &result.spills[0].locator,
                restored_output.len() as u64,
                65_536,
            )
            .await
            .unwrap();
        restored_output.extend_from_slice(&page.bytes);
    }
    assert_eq!(restored_output, "界".repeat(1024 * 1024).as_bytes());
    let states = stack
        .store
        .read_domain_states(&submitted.session_id, None, None)
        .await
        .unwrap();
    let journal = handle.decode(&states.states[0].snapshot).unwrap();
    assert!(
        journal
            .segments
            .iter()
            .any(|segment| segment.byte_len > 3 * 1024 * 1024)
    );
    let restored = rsi_extension_agent::restore(stack.store.as_ref(), &journal, || {
        rsi_api_protocol::ByteBudget::default().into()
    })
    .await
    .unwrap();
    assert!(restored.iter().any(|event| event["type"] == "tool/call"
        && event["data"]["arguments"].as_str() == Some(args.as_str())));
    let views: Vec<_> = states
        .states
        .into_iter()
        .map(|state| rsi_agent_session_protocol::DomainStateView {
            revision: state.head.revision,
            snapshot: state.snapshot,
        })
        .collect();
    adapter
        .validate(&header(), &views, CancellationToken::new())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "reload failed: {error}; connection: {:?}",
                connection.disconnect_cause()
            )
        });
    assert_eq!(
        connection
            .request("fixture.executions", Value::Null, CancellationToken::new())
            .await
            .unwrap(),
        json!(1)
    );
    let (_, outcome) = run(&stack, Some(submitted.session_id)).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(
        connection
            .request("fixture.executions", Value::Null, CancellationToken::new())
            .await
            .unwrap(),
        json!(2)
    );
    drop((tool, callbacks, adapter, connection));
    assert!(policy.dispose().await.is_clean());
    assert!(dependencies.dispose().await.is_clean());
    spill.shutdown().await;
    assert!(extension.dispose().await.is_clean());
    assert!(process.dispose().await.is_clean());
    stack.dispose(language, executor).await;
}

async fn run(stack: &BaseStack, session: Option<SessionId>) -> (SubmittedTurn, TurnOutcome) {
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let session = match session {
        Some(id) => SubmitSession::Resume(turns.prepare_resume(&id).await.unwrap()),
        None => stack.fresh(header()).await,
    };
    let submitted = turns
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session,
            text: "probe the native body boundary".into(),
            model: None,
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
    (submitted, outcome)
}
