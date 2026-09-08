use super::*;
use rsi_agent_session_protocol::{ContributionId, InputMessageSource, ToolRejection};

fn plugin_input(turn: &TurnId) -> SessionFactBody {
    SessionFactBody::InputMessageEntered {
        turn_id: turn.clone(),
        step_id: StepId::new("step").unwrap(),
        source: InputMessageSource::PluginContext {
            contribution_id: ContributionId::new("fixture.time").unwrap(),
        },
        content: vec![AgentMessageContent::Text {
            text: "Sampled at 42 ms".into(),
        }],
    }
}

fn rejection(turn: &TurnId) -> SessionFactBody {
    SessionFactBody::ToolRejected {
        turn_id: turn.clone(),
        effect_id: EffectId::new("tool").unwrap(),
        identity: ToolResultIdentity::new("owner", "tool", "call", "a".repeat(64)).unwrap(),
        name: "bash".into(),
        arguments: serde_json::json!({"command":"printf hello"}),
        rejection: ToolRejection::PolicyDenied {
            contribution_id: ContributionId::new("fixture.plan").unwrap(),
            reason: "Only reading is permitted".into(),
        },
    }
}

#[tokio::test]
async fn plugin_context_requires_the_open_step_and_no_active_effect() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let workers = kernel.start_workers();
    let submitted = submit(&kernel, "contribution-admission", "work").await;
    let lease = kernel.register("worker".into()).unwrap();
    let claim = kernel
        .claim("worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        kernel
            .publish(&claim, vec![plugin_input(&submitted.turn_id)])
            .await,
        Err(TurnError::Invalid(_))
    ));
    let step = SessionFactBody::StepStarted {
        turn_id: submitted.turn_id.clone(),
        step_id: StepId::new("step").unwrap(),
    };
    let entered = kernel
        .publish(&claim, vec![step, plugin_input(&submitted.turn_id)])
        .await
        .unwrap()
        .published();
    assert_eq!(entered.len(), 2);
    let intent = SessionFactBody::ToolIntent {
        turn_id: submitted.turn_id.clone(),
        effect_id: EffectId::new("tool").unwrap(),
        identity: ToolResultIdentity::new("owner", "tool", "call", "a".repeat(64)).unwrap(),
        name: "bash".into(),
        arguments: serde_json::json!({}),
        approval: None,
        parallel_safe: false,
    };
    let facts = kernel
        .publish(&claim, vec![intent])
        .await
        .unwrap()
        .published();
    kernel
        .flush(&claim, facts.last().unwrap().seq())
        .await
        .unwrap();
    for body in [
        plugin_input(&submitted.turn_id),
        rejection(&submitted.turn_id),
        SessionFactBody::InputMessageEntered {
            turn_id: submitted.turn_id.clone(),
            step_id: StepId::new("step").unwrap(),
            source: InputMessageSource::AgentInstructions {
                source: "workspace".into(),
                sha256: "a".repeat(64),
                replacement: true,
                tombstone: false,
            },
            content: vec![AgentMessageContent::Text {
                text: "must wait for a safe boundary".into(),
            }],
        },
    ] {
        assert!(matches!(
            kernel.publish(&claim, vec![body]).await,
            Err(TurnError::Invalid(_))
        ));
    }
    let page = store
        .read_facts(&submitted.session_id, 0, 16)
        .await
        .unwrap();
    assert_eq!(page.facts.len(), 4);
    kernel
        .finish_turn(&claim, &TurnOutcome::Cancelled)
        .await
        .unwrap();
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
}

fn contribution_budget_header() -> SessionHeader {
    SessionHeader::new(
        SessionId::new("contribution-budget").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new_with_budget(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
            TurnBudget::new(1_800_000, 64, 1, 3, 67_108_864).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn contribution_capture_rejects_cancelled_and_budget_ending_turns() {
    for cancelled in [true, false] {
        let store = Arc::new(MemoryStore::new());
        let kernel = kernel(store).await;
        let workers = kernel.start_workers();
        let submitted = submit(&kernel, "contribution-closed", "work").await;
        let lease = kernel.register("worker".into()).unwrap();
        let claim = kernel
            .claim("worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let cancelled_stage = CancellationToken::new();
        cancelled_stage.cancel();
        assert!(matches!(
            kernel.contribution_context(&claim, cancelled_stage).await,
            Err(TurnError::Invalid(_))
        ));
        assert_eq!(
            kernel.read_facts(&claim, 0, 16).await.unwrap().through_seq,
            1
        );
        kernel
            .contribution_context(&claim, CancellationToken::new())
            .await
            .unwrap();
        if cancelled {
            kernel
                .cancel(&submitted.session_id, &submitted.turn_id, None)
                .await
                .unwrap();
        } else {
            let limit = claim
                .header()
                .settings()
                .turn_budget()
                .maximum_provider_attempts();
            kernel
                .publish(
                    &claim,
                    vec![SessionFactBody::BudgetExhausted {
                        turn_id: submitted.turn_id.clone(),
                        dimension: BudgetDimension::ProviderAttempts,
                        consumed: limit + 1,
                        limit,
                    }],
                )
                .await
                .unwrap();
        }
        assert!(matches!(
            kernel
                .contribution_context(&claim, CancellationToken::new())
                .await,
            Err(TurnError::Invalid(_))
        ));
        let outcome = if cancelled {
            TurnOutcome::Cancelled
        } else {
            let limit = claim
                .header()
                .settings()
                .turn_budget()
                .maximum_provider_attempts();
            TurnOutcome::BudgetExceeded {
                dimension: BudgetDimension::ProviderAttempts,
                consumed: limit + 1,
                limit,
            }
        };
        kernel.finish_turn(&claim, &outcome).await.unwrap();
        drop(lease);
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test]
async fn rejection_is_charged_without_an_intent_and_replays_on_both_stores() {
    for sqlite in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(root.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let workers = kernel.start_workers();
        let submitted = kernel
            .submit(SubmitTurn {
                session: fresh(contribution_budget_header()),
                turn_id: client_turn_id(),
                text: "work".into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
        let lease = kernel.register("worker".into()).unwrap();
        let claim = kernel
            .claim("worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let bodies = vec![
            SessionFactBody::StepStarted {
                turn_id: submitted.turn_id.clone(),
                step_id: StepId::new("step").unwrap(),
            },
            plugin_input(&submitted.turn_id),
            rejection(&submitted.turn_id),
        ];
        let published = kernel
            .publish(&claim, bodies.clone())
            .await
            .unwrap()
            .published();
        kernel
            .flush(&claim, published.last().unwrap().seq())
            .await
            .unwrap();
        assert!(matches!(
            kernel
                .publish(&claim, vec![rejection(&submitted.turn_id)])
                .await,
            Err(TurnError::BudgetExceeded {
                dimension: BudgetDimension::ToolCalls,
                consumed: 2,
                limit: 1
            })
        ));
        assert!(matches!(
            kernel
                .publish(&claim, vec![plugin_input(&submitted.turn_id)])
                .await,
            Err(TurnError::BudgetExceeded {
                dimension: BudgetDimension::GeneratedRecords,
                consumed: 4,
                limit: 3
            })
        ));
        let page = store
            .read_facts(&submitted.session_id, 1, 16)
            .await
            .unwrap();
        assert_eq!(
            page.facts
                .iter()
                .map(|f| f.body().clone())
                .collect::<Vec<_>>(),
            bodies
        );
        kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        drop(lease);
        kernel.shutdown(workers).await.unwrap();
        drop(kernel);
        if sqlite {
            drop(store);
            rsi_agent_store_sqlite::SqliteStore::verify(root.path()).unwrap();
            let reopened = rsi_agent_store_sqlite::SqliteStore::open(root.path()).unwrap();
            let page = reopened
                .read_facts(&submitted.session_id, 1, 16)
                .await
                .unwrap();
            assert_eq!(
                page.facts[..3]
                    .iter()
                    .map(|f| f.body().clone())
                    .collect::<Vec<_>>(),
                bodies
            );
        }
    }
}
