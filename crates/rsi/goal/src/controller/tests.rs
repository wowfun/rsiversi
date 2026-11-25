use super::*;
use rsi_agent_composition_protocol::{AgentCompositionPin, ContributionCatalog, DomainCatalog};
use rsi_agent_goal::{GoalState, RoundSettlement};
use rsi_agent_session_protocol::*;
use rsi_agent_turn_protocol::{ContinuationIssuer, DomainMutationReceipt, MessageReceipt};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug)]
struct PausedSession {
    id: SessionId,
    snapshot: Mutex<GoalSnapshot>,
    before: bool,
    entered: Semaphore,
    release: Semaphore,
    calls: AtomicUsize,
    committed: AtomicBool,
    receipt: Mutex<Option<DomainMutationReceipt>>,
}
impl PausedSession {
    async fn reserve(
        &self,
        lease: &ContinuationLease,
        invocation: SessionCommandInvocation,
        input: ContinuationInput,
    ) -> GoalResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.before {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        if self.before && !lease.is_armed() {
            return Err(GoalError::Disarmed);
        }
        {
            let mut snapshot = self.snapshot.lock().unwrap();
            snapshot.state.goal.as_mut().unwrap().reserve().unwrap();
            let source = DomainMutationSource::Continuation {
                invocation: invocation.clone(),
                domain: lease.binding().domain.clone(),
                owner: lease.binding().owner.clone(),
                reservation: Some(input),
            };
            // The fixture retains a durable receipt for the later settlement response.
            let mut source = source;
            if let DomainMutationSource::Continuation { invocation, .. } = &mut source {
                invocation.expected_revision = CommandRevision::Durable { control_seq: 1 };
            }
            let commit = DomainStateCommit::new(
                Some(invocation.request_id),
                source,
                vec![
                    DomainStateUpdate::new(
                        DomainRevision::new(1),
                        snapshot.domain.snapshot.clone(),
                    )
                    .unwrap(),
                ],
            )
            .unwrap();
            *self.receipt.lock().unwrap() = Some(
                DomainMutationReceipt::new(
                    self.id.clone(),
                    AgentControlRecord::new(
                        2,
                        1,
                        AgentControlRecordBody::DomainStateCommitted { commit },
                    )
                    .unwrap(),
                )
                .unwrap(),
            );
            self.committed.store(true, Ordering::SeqCst);
            snapshot.revision = CommandRevision::Durable { control_seq: 2 };
        }
        if !self.before {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        Err(GoalError::OutcomeUnknown(
            "fixture lost reservation reply".into(),
        ))
    }
    fn message(&self, id: &MessageId) -> MessageReceipt {
        MessageReceipt {
            session_id: self.id.clone(),
            message_id: id.clone(),
            accepted_control_seq: 2,
            observed_fact_seq: 0,
            state: MessageState::Pending,
        }
    }
}
#[async_trait]
impl GoalSession for PausedSession {
    fn session_id(&self) -> &SessionId {
        &self.id
    }
    async fn snapshot(&self) -> GoalResult<GoalSnapshot> {
        Ok(self.snapshot.lock().unwrap().clone())
    }
    async fn application_command(
        &self,
        _: SessionCommandInvocation,
    ) -> GoalResult<SessionCommandReceipt> {
        unreachable!()
    }
    async fn command_status(
        &self,
        _: &DomainRequestId,
    ) -> GoalResult<Option<SessionCommandReceipt>> {
        unreachable!()
    }
    async fn arm(&self, _: ContinuationBinding) -> GoalResult<ContinuationLease> {
        unreachable!()
    }
    async fn retain_for_settlement(&self, _: ContinuationBinding) -> GoalResult<ContinuationLease> {
        unreachable!()
    }
    async fn wait_idle(&self, _: &ContinuationLease, _: CancellationToken) -> GoalResult<()> {
        unreachable!()
    }
    async fn reserve_initial(
        &self,
        lease: &ContinuationLease,
        invocation: SessionCommandInvocation,
        input: ContinuationInput,
    ) -> GoalResult<MessageReceipt> {
        let message = input.message_id.clone();
        self.reserve(lease, invocation, input).await?;
        Ok(self.message(&message))
    }
    async fn internal_command(
        &self,
        lease: &ContinuationLease,
        invocation: SessionCommandInvocation,
        input: Option<ContinuationInput>,
    ) -> GoalResult<DomainMutationReceipt> {
        if let Some(input) = input {
            self.reserve(lease, invocation, input).await?;
        } else {
            let args: rsi_agent_goal::SettleGoal =
                serde_json::from_value(invocation.arguments.value().clone()).unwrap();
            self.snapshot
                .lock()
                .unwrap()
                .state
                .goal
                .as_mut()
                .unwrap()
                .settle(&args.message_id, args.settlement)
                .unwrap();
        }
        Ok(self.receipt.lock().unwrap().clone().unwrap())
    }
    async fn internal_status(
        &self,
        _: &ContinuationLease,
        _: &DomainRequestId,
    ) -> GoalResult<Option<DomainMutationReceipt>> {
        Ok(self.receipt.lock().unwrap().clone())
    }
    async fn message_status(&self, message: &MessageId) -> GoalResult<Option<MessageReceipt>> {
        Ok(self
            .committed
            .load(Ordering::SeqCst)
            .then(|| self.message(message)))
    }
    async fn wait_round(
        &self,
        _: &MessageId,
        cancellation: CancellationToken,
    ) -> GoalResult<RoundSettlement> {
        if cancellation.is_cancelled() {
            Err(GoalError::ShuttingDown)
        } else {
            Ok(RoundSettlement::Abandoned)
        }
    }
    async fn discard_if_pending(
        &self,
        _: &ContinuationLease,
        message: &MessageId,
    ) -> GoalResult<Option<MessageReceipt>> {
        Ok(Some(self.message(message)))
    }
    async fn cancel(&self, _: &MessageId) -> GoalResult<()> {
        Ok(())
    }
}

async fn driver_fixture(
    draft: bool,
    before: bool,
) -> (rsi_meta::FiberHandle, Arc<PausedSession>, ContinuationLease) {
    let runtime = rsi_meta::Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "tools",
                "test",
                rsi_meta::UpdateMode::Replayable,
                Arc::new(rsi_tools::ToolsFactory),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let tools = runtime
        .root()
        .lookup_local::<rsi_tools_protocol::ToolCatalogProviderContract>()
        .unwrap()
        .begin_stage()
        .unwrap()
        .seal()
        .unwrap();
    let preset = AgentPresetId::new("fixture").unwrap();
    let header = SessionHeader::new_local(
        SessionId::new("driver").unwrap(),
        1,
        "/workspace",
        preset.clone(),
        FrozenAgentSettings::new(
            "settings",
            "system",
            rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let mut state = GoalState::default();
    state
        .apply(
            GoalAction::Create {
                id: DomainRequestId::new("goal").unwrap(),
                objective: "verify".into(),
                constraints: String::new(),
                max_rounds: Some(1),
            },
            1,
            draft,
        )
        .unwrap();
    let domain = DomainSnapshot::new(
        DomainIdentity::new(
            rsi_agent_goal::GOAL_DOMAIN,
            rsi_agent_goal::GOAL_CODEC_VERSION,
        )
        .unwrap(),
        DomainStateValue::new(serde_json::to_value(&state).unwrap()).unwrap(),
    );
    let snapshot = GoalSnapshot {
        header: header.clone(),
        revision: if draft {
            CommandRevision::Draft { revision: 1 }
        } else {
            CommandRevision::Durable { control_seq: 1 }
        },
        domain: DomainStateView {
            revision: DomainRevision::new(1),
            snapshot: domain.clone(),
        },
        state,
    };
    let binding = binding(&snapshot, snapshot.state.goal.as_ref().unwrap()).unwrap();
    let pin = AgentCompositionPin::new(
        preset,
        "a".repeat(64),
        tools,
        Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
        DomainCatalog::new([]).unwrap(),
        ContributionCatalog::freeze(vec![]).unwrap(),
        Arc::new(()),
    )
    .unwrap();
    let lease = ContinuationIssuer::default().issue(header, pin, binding, None);
    let session = Arc::new(PausedSession {
        id: SessionId::new("driver").unwrap(),
        snapshot: Mutex::new(snapshot),
        before,
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        calls: AtomicUsize::new(0),
        committed: AtomicBool::new(false),
        receipt: Mutex::new(None),
    });
    (fiber, session, lease)
}

#[tokio::test(start_paused = true)]
async fn only_an_armed_edit_reissues_a_discarded_unclaimed_round() {
    for armed in [true, false] {
        let (fiber, session, lease) = driver_fixture(false, false).await;
        let service = GoalService::default();
        let owner = service.owner(&session.id).unwrap();
        let live = Live {
            session: session.clone(),
            lease: lease.clone(),
            stop: service.inner.stopped.child_token(),
            pending: Arc::default(),
        };
        let snapshot = session.snapshot().await.unwrap();
        let goal = snapshot.state.goal.as_ref().unwrap();
        let proposal = goal.next_reservation().unwrap();
        let invocation = SessionCommandInvocation {
            command: ContributionId::new(rsi_agent_goal::GOAL_RESERVE).unwrap(),
            request_id: proposal.request_id,
            expected_revision: snapshot.revision,
            arguments: CommandArguments::new(serde_json::json!({"id": goal.id})).unwrap(),
        };
        session.release.add_permits(1);
        assert!(matches!(
            session.reserve(&lease, invocation, proposal.input).await,
            Err(GoalError::OutcomeUnknown(_))
        ));
        let message = {
            let mut snapshot = session.snapshot.lock().unwrap();
            snapshot
                .state
                .apply(
                    GoalAction::Edit {
                        id: goal.id.clone(),
                        objective: Some("Revised".into()),
                        max_rounds: None,
                    },
                    1,
                    false,
                )
                .unwrap();
            snapshot
                .state
                .goal
                .as_ref()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .message_id
                .clone()
        };
        if !armed {
            lease.revoke();
        }
        service
            .settle(
                &owner,
                &live,
                &message,
                RoundSettlement::ContinuationDisarmed,
            )
            .await
            .unwrap();
        let goal = session.snapshot().await.unwrap().state.goal.unwrap();
        if armed {
            let next = goal.next_reservation().unwrap();
            assert_eq!(next.input.round, 1);
            assert_ne!(next.input.message_id, message);
            assert!(next.input.text.contains("Revised"));
        } else {
            assert_eq!(goal.phase, GoalPhase::Paused);
            assert!(!goal.has_round_capacity());
        }
        service.stop().await.unwrap();
        drop((live, owner, service, session, lease));
        assert!(fiber.dispose().await.is_clean());
    }
}

#[tokio::test(start_paused = true)]
async fn disarm_preempts_unaccepted_work_without_allocating_an_idle_owner() {
    let (fiber, session, lease) = driver_fixture(false, true).await;
    let service = GoalService::default();
    service.disarm(&session.id).await.unwrap();
    assert!(service.inner.sessions.lock().unwrap().is_empty());
    let owner = service.owner(&session.id).unwrap();
    service
        .start(
            &owner,
            Live {
                session: session.clone(),
                lease: lease.clone(),
                stop: service.inner.stopped.child_token(),
                pending: Arc::default(),
            },
        )
        .unwrap();
    session.entered.acquire().await.unwrap().forget();
    service.disarm(&session.id).await.unwrap();
    assert!(!lease.is_armed());
    session.release.add_permits(1);
    service.inner.tasks.close();
    service.inner.tasks.wait().await;
    assert!(!session.committed.load(Ordering::SeqCst));
    assert_eq!(
        service.status(&session.id).unwrap().stage,
        GoalDriverStage::Disarmed
    );
    service.stop().await.unwrap();
    drop((owner, session, service, lease));
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test(start_paused = true)]
async fn started_driver_drains_the_original_reservation_with_one_stop_deadline() {
    for draft in [false, true] {
        for before in [false, true] {
            for expire in [false, true] {
                let (fiber, session, lease) = driver_fixture(draft, before).await;
                let service = GoalService::default();
                let owner = service.owner(&session.id).unwrap();
                service
                    .start(
                        &owner,
                        Live {
                            session: session.clone(),
                            lease: lease.clone(),
                            stop: service.inner.stopped.child_token(),
                            pending: Arc::default(),
                        },
                    )
                    .unwrap();
                session.entered.acquire().await.unwrap().forget();
                let stopped = tokio::spawn({
                    let service = service.clone();
                    async move { service.stop().await }
                });
                tokio::task::yield_now().await;
                assert!(!lease.is_armed());
                assert!(!stopped.is_finished());
                if expire {
                    tokio::time::advance(std::time::Duration::from_secs(30)).await;
                    let error = stopped.await.unwrap().unwrap_err().to_string();
                    assert!(error.contains("unresolved reserve request"), "{error}");
                    assert!(error.contains("message"));
                } else {
                    session.release.add_permits(1);
                    stopped.await.unwrap().unwrap();
                }
                assert_eq!(session.calls.load(Ordering::SeqCst), 1);
                assert!(owner.live().is_none());
                drop((owner, session, service, lease));
                assert!(fiber.dispose().await.is_clean());
            }
        }
    }
}
