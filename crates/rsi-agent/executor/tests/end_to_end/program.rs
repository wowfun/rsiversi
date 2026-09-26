use super::*;
use rsi_agent_session_protocol::ToolOrigin;
use rsi_agent_turn_protocol::ProgramToolCalls;
use rsi_tools_protocol::ToolProgramRole;

#[derive(Debug)]
struct Coordinator {
    abandoned: Option<Arc<Abandoned>>,
}
#[derive(Debug, Default)]
struct Abandoned {
    entered: CancellationToken,
    release: CancellationToken,
}
#[derive(Debug)]
struct AbandonedTool(Arc<Abandoned>);
#[async_trait]
impl ToolExecutor for AbandonedTool {
    async fn execute(&self, _: Value, execution: ToolExecution) -> ToolResultType<ToolResult> {
        self.0.entered.cancel();
        tokio::select! { biased;
            () = execution.cancellation.cancelled() => Err(ToolError::Cancelled),
            () = self.0.release.cancelled() => ToolResult::new(json!({"settled":true}), vec![], false),
        }
    }
}

#[async_trait]
impl ToolExecutor for Coordinator {
    async fn execute(&self, _: Value, execution: ToolExecution) -> ToolResultType<ToolResult> {
        let calls = execution
            .extension::<ProgramToolCalls>()
            .expect("exact coordinator capability");
        assert_eq!(
            calls
                .0
                .definitions()
                .iter()
                .map(ToolDefinition::name)
                .collect::<Vec<_>>(),
            ["echo"]
        );
        assert!(matches!(
            calls
                .0
                .call("run_code".into(), json!({}), CancellationToken::new())
                .await,
            Err(ToolError::Unknown(_))
        ));
        if let Some(abandoned) = &self.abandoned {
            let call = calls
                .0
                .call("echo".into(), json!({}), CancellationToken::new());
            tokio::pin!(call);
            tokio::select! {
                _ = &mut call => panic!("nested call must remain unsettled"),
                () = abandoned.entered.cancelled() => {},
            }
            // Drop the dispatch waiter after actual Tool start, then complete the script.
            return ToolResult::new(json!({"summary":"CURATED_PROGRAM_OUTPUT"}), vec![], false);
        }
        let result = calls
            .0
            .call(
                "echo".into(),
                json!({"private":"INTERNAL_RESULT_NOT_MODEL_INPUT"}),
                CancellationToken::new(),
            )
            .await?;
        assert!(!result.is_error);
        ToolResult::new(json!({"summary":"CURATED_PROGRAM_OUTPUT"}), vec![], false)
    }
}

#[tokio::test]
async fn internal_calls_use_durable_program_provenance_and_only_curated_output_reaches_model() {
    check_program_policy(None, false).await;
}

#[derive(Debug)]
struct NestedPolicy(bool);
#[async_trait]
impl rsi_agent_composition_protocol::ToolPolicy for NestedPolicy {
    async fn decide(
        &self,
        _: &rsi_agent_composition_protocol::ContributionContext,
        request: &rsi_agent_composition_protocol::ToolPolicyRequest<'_>,
        _: CancellationToken,
    ) -> rsi_agent_composition_protocol::ContributionResult<
        rsi_agent_composition_protocol::ToolPolicyDecision,
    > {
        use rsi_agent_composition_protocol::ToolPolicyDecision;
        Ok(if request.name != "echo" {
            ToolPolicyDecision::Abstain
        } else if self.0 {
            ToolPolicyDecision::Deny {
                reason: "nested call denied".into(),
            }
        } else {
            ToolPolicyDecision::RequireApproval
        })
    }
}

#[tokio::test]
async fn internal_calls_obey_frozen_policy_and_live_approval_before_start() {
    check_program_policy(Some(false), false).await;
    check_program_policy(Some(true), false).await;
}

#[allow(
    clippy::too_many_lines,
    reason = "One public executor fixture compares allowed, approval-required and denied nested calls."
)]
async fn check_program_policy(policy: Option<bool>, abandon: bool) {
    let abandoned = abandon.then(|| Arc::new(Abandoned::default()));
    let stack = BaseStack::activate().await;
    let callbacks = if let Some(deny) = policy {
        Some(
            contributions::install(
                &stack,
                vec![
                    rsi_agent_composition_protocol::ContributionRegistration::new(
                        rsi_agent_session_protocol::ContributionId::new("test.nested-policy")
                            .unwrap(),
                        0,
                        rsi_agent_composition_protocol::ContributionKind::ToolPolicy(Arc::new(
                            NestedPolicy(deny),
                        )),
                    ),
                ],
            )
            .await,
        )
    } else {
        None
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let echo = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new("echo", "echo JSON", json!({"type":"object"}))
                .unwrap()
                .with_program_role(ToolProgramRole::Callable),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: abandoned.as_ref().map_or_else(
                || {
                    Arc::new(EchoTool {
                        store: stack.store.clone(),
                        calls: calls.clone(),
                    }) as Arc<dyn ToolExecutor>
                },
                |state| Arc::new(AbandonedTool(state.clone())) as Arc<dyn ToolExecutor>,
            ),
        })
        .unwrap();
    let coordinator_lease = stack
        .tool_registrar
        .register(ToolRegistration {
            output: None,
            definition: ToolDefinition::new(
                "run_code",
                "execute a program",
                json!({"type":"object"}),
            )
            .unwrap()
            .with_program_role(ToolProgramRole::Coordinator),
            timeout: rsi_tools_protocol::ToolTimeoutPolicy::Execution { timeout_ms: 2_000 },
            executor: Arc::new(Coordinator {
                abandoned: abandoned.clone(),
            }),
        })
        .unwrap();
    let provider = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([
            StartOutcome::Stream(tool_calls_script(&[("program-1", "run_code", "{}")])),
            StartOutcome::Stream(answer_script()),
        ])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.program", provider.clone())
        .await;
    let executor = stack.activate_executor("program-executor").await;
    let release = async {
        if let Some(state) = &abandoned {
            state.entered.cancelled().await;
            // Keep the nested effect pending until the coordinator's retained result exists.
            loop {
                let facts = stack
                    .store
                    .read_facts(header().session_id(), 0, 128)
                    .await
                    .unwrap();
                let identity = facts
                    .facts
                    .iter()
                    .find_map(|fact| match fact.body() {
                        SessionFactBody::ToolIntent {
                            identity,
                            program_role: ToolProgramRole::Coordinator,
                            ..
                        } => Some(identity),
                        _ => None,
                    })
                    .unwrap();
                if matches!(
                    stack.tool_runtime().query(identity).unwrap(),
                    RetainedToolResult::Returned(_)
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            // With the paused clock, advance only after ready coordinator work drains.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            state.release.cancel();
        }
    };
    let ((submitted, outcome), ()) = tokio::join!(stack.submit_and_wait("execute"), release);
    let facts = stack
        .store
        .read_facts(&submitted.session_id, 0, 128)
        .await
        .unwrap()
        .facts;
    if policy == Some(true) {
        assert!(matches!(outcome, TurnOutcome::Failed { ref code, .. } if code == "policy.denied"));
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert!(facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::ToolRejected {
                origin: ToolOrigin::Program { ordinal: 1, .. },
                ..
            }
        )));
        assert!(!facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::ToolIntent {
                origin: ToolOrigin::Program { .. },
                ..
            }
        )));
        drop(echo);
        drop(coordinator_lease);
        assert!(callbacks.unwrap().dispose().await.is_clean());
        stack.dispose(language, executor).await;
        return;
    }
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(calls.load(Ordering::Acquire), usize::from(!abandon));
    if policy == Some(false) {
        assert!(facts.iter().any(|fact| matches!(
            fact.body(),
            SessionFactBody::ToolIntent {
                origin: ToolOrigin::Program { .. },
                approval: Some(ApprovalOutcome {
                    decision: ApprovalDecision::AllowOnce,
                    ..
                }),
                ..
            }
        )));
    }
    let coordinator = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolIntent {
                effect_id,
                program_role: ToolProgramRole::Coordinator,
                origin: ToolOrigin::Model { .. },
                ..
            } => Some(effect_id),
            _ => None,
        })
        .unwrap();
    assert!(facts.iter().any(|fact| matches!(fact.body(), SessionFactBody::ToolIntent {
        origin: ToolOrigin::Program { parent_effect_id, ordinal: 1 }, program_role: ToolProgramRole::Callable, ..
    } if parent_effect_id == coordinator)));
    if !abandon {
        assert!(
            serde_json::to_string(&facts)
                .unwrap()
                .contains("INTERNAL_RESULT_NOT_MODEL_INPUT")
        );
    }
    let nested = facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::ToolIntent {
                origin: ToolOrigin::Program { ordinal: 1, .. },
                effect_id,
                ..
            } => Some(effect_id),
            _ => None,
        })
        .unwrap();
    let nested_result = facts
        .iter()
        .find(|fact| {
            matches!(fact.body(), SessionFactBody::ToolResult {
        effect_id, ..
    } if effect_id == nested)
        })
        .expect("nested outcome must be durable");
    let coordinator_result = facts
        .iter()
        .find(|fact| {
            matches!(fact.body(), SessionFactBody::ToolResult {
        effect_id, ..
    } if effect_id == coordinator)
        })
        .expect("coordinator outcome must be durable");
    assert!(nested_result.seq() < coordinator_result.seq());
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let wire = serde_json::to_string(&requests[1]).unwrap();
        assert!(wire.contains("CURATED_PROGRAM_OUTPUT"));
        assert!(!wire.contains("INTERNAL_RESULT_NOT_MODEL_INPUT"));
    }
    drop(echo);
    drop(coordinator_lease);
    if let Some(callbacks) = callbacks {
        assert!(callbacks.dispose().await.is_clean());
    }
    stack.dispose(language, executor).await;
}

#[tokio::test(start_paused = true)]
async fn completed_coordinator_settles_abandoned_started_call_without_cancelling_turn() {
    check_program_policy(None, true).await;
}
