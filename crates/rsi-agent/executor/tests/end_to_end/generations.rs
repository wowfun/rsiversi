use super::*;

#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) pin: AgentCompositionPin,
    pub(super) gate: Option<(Arc<Notify>, Arc<Notify>)>,
}
#[async_trait]
impl rsi_agent_composition_protocol::AgentGenerationCandidate for Candidate {
    fn selection(&self) -> &rsi_agent_session_protocol::GenerationSelection {
        self.pin.selection()
    }

    async fn build(
        self: Box<Self>,
        _: &rsi_agent_composition_protocol::AgentGenerationSeed,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        if let Some((entered, release)) = self.gate {
            entered.notify_one();
            release.notified().await;
        }
        Ok(self.pin)
    }
}

#[derive(Debug)]
struct VersionTool {
    version: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
    replacement: Option<(Arc<CompositionFixture>, AgentCompositionPin)>,
}
#[async_trait]
impl ToolExecutor for VersionTool {
    async fn execute(&self, _: Value, _: ToolExecution) -> ToolResultType<ToolResult> {
        self.calls.lock().unwrap().push(self.version);
        if let Some((source, replacement)) = &self.replacement {
            *source.pin.lock().unwrap() = Some(replacement.clone());
        }
        ToolResult::new(json!({"version":self.version}), vec![], false)
    }
}

#[derive(Debug, Clone, Copy)]
enum Rejection {
    None,
    Open,
    Ingest,
    CapacityOpen,
    CapacityIngest,
}
impl Rejection {
    fn rejects(self) -> bool {
        matches!(self, Self::Open | Self::Ingest)
    }
}

#[derive(Debug)]
struct RejectingBuilder(
    rsi_agent_context::DefaultContextBuilder,
    Arc<AtomicUsize>,
    Rejection,
);
impl rsi_agent_context::ModelContextBuilder for RejectingBuilder {
    fn identity(&self) -> &rsi_agent_context::ContextBuilderIdentity {
        self.0.identity()
    }
    fn open(
        &self,
        init: rsi_agent_context::ContextInit<'_>,
    ) -> rsi_agent_context::Result<Box<dyn rsi_agent_context::ModelContextCursor>> {
        if matches!(self.2, Rejection::CapacityOpen) {
            return if self.1.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(rsi_agent_context::ContextError::Capacity)
            } else {
                self.0.open(init)
            };
        }
        if matches!(self.2, Rejection::Open) {
            self.1.fetch_add(1, Ordering::SeqCst);
            Err(rsi_agent_context::ContextError::Invalid(
                "candidate cannot load context".into(),
            ))
        } else {
            Ok(Box::new(RejectingCursor(
                self.0.open(init)?,
                self.1.clone(),
                matches!(self.2, Rejection::CapacityIngest),
            )))
        }
    }
}

#[derive(Debug)]
struct RejectingCursor(
    Box<dyn rsi_agent_context::ModelContextCursor>,
    Arc<AtomicUsize>,
    bool,
);
impl rsi_agent_context::ModelContextCursor for RejectingCursor {
    fn step_prompt(&self) -> &rsi_agent_session_protocol::StepPrompt {
        self.0.step_prompt()
    }
    fn runtime_context(&self) -> Option<&str> {
        self.0.runtime_context()
    }
    fn previous_context_input_at_ms(
        &self,
        producer: &rsi_agent_session_protocol::ContributionId,
    ) -> Option<u64> {
        self.0.previous_context_input_at_ms(producer)
    }
    fn claimed_inputs(
        &self,
        turn: &rsi_agent_session_protocol::TurnId,
    ) -> rsi_agent_context::Result<&[rsi_agent_session_protocol::SessionFact]> {
        self.0.claimed_inputs(turn)
    }
    fn ingest(
        &mut self,
        page: rsi_agent_context::ContextPage<'_>,
    ) -> rsi_agent_context::Result<()> {
        let attempt = self.1.fetch_add(1, Ordering::SeqCst);
        if self.2 {
            return if attempt == 0 {
                Err(rsi_agent_context::ContextError::Capacity)
            } else {
                self.0.ingest(page)
            };
        }
        Err(rsi_agent_context::ContextError::Invalid(
            "candidate cannot load context".into(),
        ))
    }
    fn build(
        &self,
        options: rsi_ai_protocol::LanguageRequestOptions,
        profile: &rsi_ai_protocol::LanguageProfile,
    ) -> rsi_agent_context::Result<rsi_ai_protocol::LanguageRequest> {
        self.0.build(options, profile)
    }
    fn checkpoint(&self) -> rsi_agent_context::Result<rsi_api_protocol::RetainedBytes> {
        self.0.checkpoint()
    }
    fn position(&self) -> rsi_agent_context::ContextPosition {
        self.0.position()
    }
}

fn registration(tool: Arc<VersionTool>) -> ToolRegistration {
    ToolRegistration {
        definition: ToolDefinition::new("echo", tool.version, json!({"type":"object"})).unwrap(),
        output: None,
        executor: tool,
        run_policy: rsi_tools_protocol::ToolRunPolicy::execution(std::time::Duration::from_secs(1)),
    }
}

fn next_pin(
    stack: &BaseStack,
    calls: &Arc<Mutex<Vec<&'static str>>>,
    rejection: Rejection,
    rejections: &Arc<AtomicUsize>,
) -> AgentCompositionPin {
    let provider = stack
        .runtime
        .root()
        .lookup_local::<ToolCatalogProviderContract>()
        .unwrap();
    let stage = provider.begin_stage().unwrap();
    let next_lease = stage
        .registrar()
        .register(registration(Arc::new(VersionTool {
            version: "new",
            calls: calls.clone(),
            replacement: None,
        })))
        .unwrap();
    AgentCompositionPin::new(
        AgentPresetId::new("test-agent").unwrap(),
        "c".repeat(64),
        stage.seal().unwrap(),
        if matches!(rejection, Rejection::None) {
            Arc::new(rsi_agent_context::DefaultContextBuilder::default())
        } else {
            Arc::new(RejectingBuilder(
                rsi_agent_context::DefaultContextBuilder::default(),
                rejections.clone(),
                rejection,
            ))
        },
        rsi_agent_composition_protocol::DomainCatalog::default(),
        rsi_agent_composition_protocol::ContributionCatalog::default(),
        Arc::new(next_lease),
    )
    .unwrap()
}

#[tokio::test]
async fn provider_retry_keeps_its_original_generation_until_the_response_batch_settles() {
    let stack = BaseStack::activate().await;
    let calls = Arc::new(Mutex::new(vec![]));
    let old_lease = stack
        .tool_registrar
        .register(registration(Arc::new(VersionTool {
            version: "old",
            calls: calls.clone(),
            replacement: None,
        })))
        .unwrap();
    let next = next_pin(&stack, &calls, Rejection::None, &Arc::default());
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
            StartOutcome::Stream(tool_calls_script(&[("old-call", "echo", "{}")])),
            StartOutcome::Stream(tool_calls_script(&[("new-call", "echo", "{}")])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::new(1, vec![ErrorKind::RateLimited], 1, 1, 0).unwrap(),
    });
    let language_fiber = stack
        .activate_language("test.generation-retry", language.clone())
        .await;
    let executor = stack.activate_executor("generation-retry-executor").await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = stack
        .submit_fresh(&turns, "generation-retry", "continue through retry")
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    *stack.composition.pin.lock().unwrap() = Some(next);
    release.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        turns.wait_idle(&submitted.session_id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        turns
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(*calls.lock().unwrap(), vec!["old", "new"]);
    let history = stack
        .store
        .read_facts(
            &submitted.session_id,
            0,
            64,
            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
        )
        .await
        .unwrap();
    assert_scheduled_retry_started(&history.facts);
    {
        let requests = language.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            serde_json::to_value(&requests[0]).unwrap(),
            serde_json::to_value(&requests[1]).unwrap()
        );
    }
    drop(old_lease);
    stack.dispose(language_fiber, executor).await;
}

fn assert_scheduled_retry_started(facts: &[SessionFact]) {
    let scheduled: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact.body() {
            SessionFactBody::ModelAttemptSettled { attempt, .. } => {
                attempt.scheduled_retry().map(|retry| (fact.seq(), retry))
            }
            _ => None,
        })
        .collect();
    assert_eq!(scheduled.len(), 1);
    let (settled_seq, retry) = scheduled[0];
    assert_eq!((retry.number, retry.delay_ms), (1, 1));
    assert!(facts.iter().any(|fact| fact.seq() > settled_seq && matches!(fact.body(), SessionFactBody::ModelStarted { effect_id, .. } if *effect_id == retry.effect_id)));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One running Turn proves batch retention, Context rejection, durable selection and preserved request history"
)]
async fn running_turn_switches_only_after_the_old_tool_batch_and_context_acceptance() {
    for rejection in [
        Rejection::None,
        Rejection::Open,
        Rejection::Ingest,
        Rejection::CapacityOpen,
        Rejection::CapacityIngest,
    ] {
        let stack = BaseStack::activate().await;
        let calls = Arc::new(Mutex::new(vec![]));
        let rejections = Arc::new(AtomicUsize::new(0));
        let next = next_pin(&stack, &calls, rejection, &rejections);
        let old_lease = stack
            .tool_registrar
            .register(registration(Arc::new(VersionTool {
                version: "old",
                calls: calls.clone(),
                replacement: Some((stack.composition.clone(), next.clone())),
            })))
            .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let language = Arc::new(LanguageFixture {
            outcomes: Mutex::new(VecDeque::from([
                StartOutcome::Stream(tool_calls_script(&[
                    ("first", "echo", "{}"),
                    ("second", "echo", "{}"),
                ])),
                StartOutcome::Stream(tool_calls_script(&[("third", "echo", "{}")])),
                gated_answer(&entered, &release),
            ])),
            requests: Mutex::new(vec![]),
            starts: Arc::new(AtomicUsize::new(0)),
            store: stack.store.clone(),
            retry_policy: RetryPolicy::default(),
        });
        let language_fiber = stack
            .activate_language("test.generations", language.clone())
            .await;
        let executor = stack.activate_executor("generation-executor").await;
        let turns = stack
            .runtime
            .root()
            .lookup_local::<TurnServiceContract>()
            .unwrap();
        let submitted = stack
            .submit_fresh(
                &turns,
                "running-generation",
                "keep working across plugin replacement",
            )
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            if matches!(rejection, Rejection::None) {
                vec!["old", "old", "new"]
            } else {
                vec!["old", "old", "old"]
            }
        );
        let status = turns
            .generation_status(&submitted.session_id)
            .await
            .unwrap()
            .unwrap();
        if rejection.rejects() {
            assert_eq!(
                rejections.load(Ordering::SeqCst),
                1,
                "advancing request Facts must not repeatedly rebuild a rejected Context"
            );
            assert!(
                status
                    .rejection
                    .unwrap()
                    .contains("candidate cannot load context")
            );
            assert_ne!(&status.selection, next.selection());
        } else {
            assert_eq!(&status.selection, next.selection());
            assert!(status.rejection.is_none());
        }
        release.notify_one();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            turns.wait_idle(&submitted.session_id),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            turns
                .outcome(&submitted.session_id, &submitted.turn_id)
                .await
                .unwrap(),
            Some(TurnOutcome::Completed)
        );
        let page = stack
            .store
            .read_controls(
                &submitted.session_id,
                0,
                64,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap();
        assert_eq!(page.records.iter().filter(|record| matches!(record.body(), rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted { commit } if commit.source().is_generation())).count(), if rejection.rejects() { 1 } else { 2 });
        {
            let requests = language.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            let last = serde_json::to_value(&requests[2]).unwrap().to_string();
            assert!(last.contains("keep working across plugin replacement"));
            assert!(last.contains("old"));
            if !rejection.rejects() {
                assert!(last.contains("new"));
            }
        }
        // Break the fixture's explicit source -> pin -> Tool -> source ownership cycle.
        *stack.composition.pin.lock().unwrap() = None;
        drop(old_lease);
        stack.dispose(language_fiber, executor).await;
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One concurrent generation conflict verifies the running Turn, retained context and subsequent recovery"
)]
async fn candidate_commit_conflict_keeps_the_running_turn_and_old_context() {
    use rsi_agent_session_protocol::{
        AgentControlRecord, AgentControlRecordBody, DomainMutationSource, DomainStateCommit,
        GenerationSelection,
    };
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    let stack = BaseStack::activate().await;
    let calls = Arc::new(Mutex::new(vec![]));
    let next = next_pin(&stack, &calls, Rejection::None, &Arc::default());
    let old_lease = stack
        .tool_registrar
        .register(registration(Arc::new(VersionTool {
            version: "old",
            calls: calls.clone(),
            replacement: Some((stack.composition.clone(), next.clone())),
        })))
        .unwrap();
    let candidate_entered = Arc::new(Notify::new());
    let candidate_release = Arc::new(Notify::new());
    *stack.composition.candidate_gate.lock().unwrap() =
        Some((candidate_entered.clone(), candidate_release.clone()));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let language = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[("first", "echo", "{}")])),
            gated_answer(&entered, &release),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language_fiber = stack
        .activate_language("test.generation-conflict", language.clone())
        .await;
    let executor = stack
        .activate_executor("generation-conflict-executor")
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = stack
        .submit_fresh(&turns, "generation-conflict", "continue after conflict")
        .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        candidate_entered.notified(),
    )
    .await
    .unwrap();
    let before = stack
        .store
        .read_domain_states(&submitted.session_id, None, None)
        .await
        .unwrap();
    let commit = DomainStateCommit::new(
        None,
        DomainMutationSource::Recovery {
            selection: GenerationSelection::new("d".repeat(64)).unwrap(),
        },
        vec![],
    )
    .unwrap();
    stack
        .store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: submitted.session_id.clone(),
                expected_fact_seq: before.durable_fact_seq,
                expected_control_seq: before.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![
                    AgentControlRecord::new(
                        before.durable_control_seq + 1,
                        43,
                        AgentControlRecordBody::DomainStateCommitted { commit },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    candidate_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert_ne!(
        turns
            .generation_status(&submitted.session_id)
            .await
            .unwrap()
            .unwrap()
            .selection,
        *next.selection()
    );
    assert_eq!(*calls.lock().unwrap(), vec!["old"]);
    {
        let requests = language.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let request = serde_json::to_value(&requests[1]).unwrap().to_string();
        assert!(request.contains("continue after conflict"));
        assert!(request.contains("old"));
        assert!(!request.contains("new"));
    }
    release.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        turns.wait_idle(&submitted.session_id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        turns
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    *stack.composition.pin.lock().unwrap() = None;
    drop(old_lease);
    stack.dispose(language_fiber, executor).await;
}
