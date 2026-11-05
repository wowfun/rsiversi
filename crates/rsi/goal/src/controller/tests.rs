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
            self.snapshot
                .lock()
                .unwrap()
                .state
                .goal
                .as_mut()
                .unwrap()
                .reservation
                .as_mut()
                .unwrap()
                .settlement = Some(RoundSettlement::Abandoned);
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
    let domain = DomainSnapshot::new(
        DomainIdentity::new(rsi_agent_goal::GOAL_DOMAIN, 2).unwrap(),
        DomainStateValue::new(serde_json::json!({})).unwrap(),
    );
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
