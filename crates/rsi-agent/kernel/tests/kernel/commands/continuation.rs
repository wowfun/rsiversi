use super::*;
use rsi_agent_session_protocol::{
    ContinuationInput, DomainMutationSource, DomainRevision, MessageDelivery, MessageDiscardReason,
};
use rsi_agent_turn_protocol::{ContinuationBinding, ContinuationLease, SessionContinuations};

fn input() -> ContinuationInput {
    ContinuationInput {
        owner: DomainRequestId::new("goal-one").unwrap(),
        round: 1,
        message_id: MessageId::new("automatic-one").unwrap(),
        text: "Continue the explicitly bounded task".into(),
    }
}

#[tokio::test]
async fn shutdown_fences_an_arm_blocked_in_its_domain_binding_read() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let fixture = Fixture::start(store.clone(), false).await;
    let initial = arm(&fixture).await;
    let binding = initial.binding().clone();
    drop(initial);
    let prepared = fixture
        .kernel
        .prepare_resume(&fixture.session_id)
        .await
        .unwrap();
    store.pause_domain_read.store(true, Ordering::Release);
    let arming = tokio::spawn({
        let kernel = fixture.kernel.clone();
        async move { SessionContinuations::arm(&kernel, SubmitSession::Resume(prepared), binding).await }
    });
    store.domain_read_entered.notified().await;
    let shutdown = fixture.kernel.shutdown(fixture.workers);
    tokio::pin!(shutdown);
    let first = futures_util::poll!(shutdown.as_mut());
    store.release_domain_read.notify_one();
    assert_eq!(arming.await.unwrap().unwrap_err(), TurnError::ShuttingDown);
    match first {
        std::task::Poll::Ready(result) => result.unwrap(),
        std::task::Poll::Pending => shutdown.await.unwrap(),
    }
    assert!(fixture.runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn a_new_generation_cannot_use_the_old_generations_continuation_lease() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let lease = arm(&fixture).await;
    let request = invocation(&fixture, "reserve", "replacement-generation", true).await;
    let old = fixture.composition.pin.read().unwrap().clone();
    let replacement = AgentCompositionPin::new(
        old.preset_id().clone(),
        "c".repeat(64),
        old.tools().clone(),
        old.context_builder().clone(),
        old.domains().clone(),
        old.contributions().clone(),
        Arc::new(()),
    )
    .unwrap();
    assert!(!old.same_generation(&replacement));
    *fixture.composition.pin.write().unwrap() = replacement;
    let prepared = fixture
        .kernel
        .prepare_resume(&fixture.session_id)
        .await
        .unwrap();
    assert_eq!(
        SessionContinuations::execute(&fixture.kernel, &lease, prepared, request, Some(input()))
            .await
            .unwrap_err(),
        TurnError::ContinuationDisarmed
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 0);
    drop(lease);
    fixture.stop().await;
}

#[tokio::test]
async fn settlement_retention_never_arms_and_foreign_issuer_cannot_authorize_commands() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let initial = arm(&fixture).await;
    let binding = initial.binding().clone();
    initial.revoke();
    drop(initial);
    let retained = SessionContinuations::retain_for_settlement(
        &fixture.kernel,
        SubmitSession::Resume(
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
        ),
        binding.clone(),
    )
    .await
    .unwrap();
    assert!(!retained.is_armed());
    let request = invocation(&fixture, "reserve", "not-authorized", true).await;
    assert!(
        SessionContinuations::execute(
            &fixture.kernel,
            &retained,
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            request.clone(),
            Some(input())
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 0);
    let foreign = rsi_agent_turn_protocol::ContinuationIssuer::default().issue(
        fixture.store.header(&fixture.session_id).await.unwrap(),
        fixture.composition.pin.read().unwrap().clone(),
        binding,
    );
    assert!(
        SessionContinuations::execute(
            &fixture.kernel,
            &foreign,
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            request,
            Some(input())
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 0);
    drop((retained, foreign));
    fixture.stop().await;
}

#[tokio::test]
async fn live_lease_bound_releases_capacity_when_the_host_drops_its_last_owner() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let pin = fixture.composition.pin.read().unwrap().clone();
    let mut leases = Vec::new();
    for index in 0..=64 {
        let prepared =
            PreparedFreshSession::new(header(&format!("lease-{index}")), pin.clone()).unwrap();
        let snapshot = prepared.baseline().initial_states()[0].clone();
        let result = SessionContinuations::arm(
            &fixture.kernel,
            SubmitSession::Fresh(prepared),
            ContinuationBinding {
                domain: snapshot.identity().clone(),
                owner: input().owner,
                revision: DomainRevision::new(0),
                snapshot_sha256: snapshot.sha256().unwrap(),
            },
        )
        .await;
        if index == 64 {
            assert_eq!(result.unwrap_err(), TurnError::Capacity);
        } else {
            leases.push(result.unwrap());
        }
    }
    drop(leases);
    let replacement = arm(&fixture).await;
    assert!(replacement.is_armed());
    drop(replacement);
    fixture.stop().await;
}

pub(super) async fn arm(fixture: &Fixture) -> ContinuationLease {
    arm_at(fixture, &fixture.session_id).await
}
async fn arm_at(fixture: &Fixture, session: &SessionId) -> ContinuationLease {
    let page = fixture
        .store
        .read_domain_states(session, None)
        .await
        .unwrap();
    let state = &page.states[0];
    SessionContinuations::arm(
        &fixture.kernel,
        SubmitSession::Resume(fixture.kernel.prepare_resume(session).await.unwrap()),
        ContinuationBinding {
            domain: state.snapshot.identity().clone(),
            owner: input().owner,
            revision: state.head.revision,
            snapshot_sha256: state.snapshot.sha256().unwrap(),
        },
    )
    .await
    .unwrap()
}

async fn invocation(
    fixture: &Fixture,
    name: &str,
    id: &str,
    value: bool,
) -> SessionCommandInvocation {
    let mut invocation = fixture.invocation(id, value).await;
    invocation.command = ContributionId::new(format!("fixture.{name}")).unwrap();
    invocation
}

async fn reserve(fixture: &Fixture, lease: &ContinuationLease) -> SessionCommandInvocation {
    let view = fixture
        .kernel
        .list(
            fixture
                .kernel
                .prepare_resume(lease.session_id())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    let invocation = SessionCommandInvocation {
        command: ContributionId::new("fixture.reserve").unwrap(),
        request_id: DomainRequestId::new("reservation-one").unwrap(),
        expected_revision: view.revision(),
        arguments: CommandArguments::new(true.into()).unwrap(),
    };
    SessionContinuations::execute(
        &fixture.kernel,
        lease,
        fixture
            .kernel
            .prepare_resume(lease.session_id())
            .await
            .unwrap(),
        invocation.clone(),
        Some(input()),
    )
    .await
    .unwrap();
    invocation
}

async fn claim(
    fixture: &Fixture,
) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::SubmittedTurn> {
    fixture
        .kernel
        .claim_message(ClaimMessage {
            session: fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            message_id: input().message_id,
            activation_id: ActivationId::new("automatic-activation").unwrap(),
            path: AgentPath::root(),
            turn_id: TurnId::new("automatic-turn").unwrap(),
            step_id: StepId::new("automatic-step").unwrap(),
        })
        .await
}

#[tokio::test]
async fn internal_dispatch_authority_is_separate_and_reservation_acceptance_is_atomic() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let lease = arm(&fixture).await;
    let request = invocation(&fixture, "reserve", "reservation-one", true).await;
    assert!(
        SessionCommands::execute(
            &fixture.kernel,
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            request.clone()
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 0);
    let receipt = SessionContinuations::execute(
        &fixture.kernel,
        &lease,
        fixture
            .kernel
            .prepare_resume(&fixture.session_id)
            .await
            .unwrap(),
        request.clone(),
        Some(input()),
    )
    .await
    .unwrap();
    assert!(
        matches!(receipt.commit().source(), DomainMutationSource::Continuation { reservation: Some(reserved), .. } if reserved == &input())
    );
    let accepted = fixture
        .store
        .read_agent_mailbox(&fixture.session_id, Some(&input().message_id))
        .await
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(accepted.accepted_control_seq, receipt.control_seq() + 1);
    let retried = SessionContinuations::execute(
        &fixture.kernel,
        &lease,
        fixture
            .kernel
            .prepare_resume(&fixture.session_id)
            .await
            .unwrap(),
        request.clone(),
        Some(input()),
    )
    .await
    .unwrap();
    assert_eq!(receipt, retried);
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
    assert!(
        SessionCommands::query(&fixture.kernel, &fixture.session_id, &request.request_id)
            .await
            .is_err()
    );
    assert_eq!(
        SessionContinuations::query(&fixture.kernel, &lease, &request.request_id)
            .await
            .unwrap(),
        Some(receipt)
    );
    assert!(lease.is_armed());
    let accepted = fixture
        .kernel
        .message_status(&fixture.session_id, &input().message_id)
        .await
        .unwrap();
    assert_eq!(accepted.state, MessageState::Pending);
    assert_eq!(
        fixture
            .kernel
            .message_status(&fixture.session_id, &input().message_id)
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
    drop(lease);
    fixture.stop().await;
}

#[tokio::test]
async fn ordinary_input_atomically_supersedes_pending_automatic_input_in_both_stores() {
    for sqlite in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(temp.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let fixture = Fixture::start(store, false).await;
        let lease = arm(&fixture).await;
        let _request = reserve(&fixture, &lease).await;
        let auto = fixture
            .kernel
            .message_status(&fixture.session_id, &input().message_id)
            .await
            .unwrap();
        let human = fixture
            .kernel
            .submit_message(SubmitMessage {
                session: SubmitSession::Resume(
                    fixture
                        .kernel
                        .prepare_resume(&fixture.session_id)
                        .await
                        .unwrap(),
                ),
                delivery: MessageDelivery::NextTurn,
                message: AgentMessage {
                    message_id: MessageId::new("human-priority").unwrap(),
                    source: AgentMessageSource::Human,
                    content: vec![AgentMessageContent::Text {
                        text: "Work on this first".into(),
                    }],
                    options: MessageOptions::default(),
                },
            })
            .await
            .unwrap();
        let old = fixture
            .kernel
            .message_status(&fixture.session_id, &input().message_id)
            .await
            .unwrap();
        assert!(
            matches!(old.state, MessageState::Discarded { reason: MessageDiscardReason::Superseded, control_seq } if control_seq == human.accepted_control_seq - 1)
        );
        assert_eq!(human.accepted_control_seq, auto.accepted_control_seq + 2);
        assert!(claim(&fixture).await.is_err());
        let controls = fixture
            .store
            .read_controls(&fixture.session_id, auto.accepted_control_seq, 8)
            .await
            .unwrap();
        assert_eq!(controls.records.len(), 2);
        assert!(matches!(
            controls.records[0].body(),
            AgentControlRecordBody::MessageDiscarded { .. }
        ));
        assert!(matches!(
            controls.records[1].body(),
            AgentControlRecordBody::MessageAccepted { .. }
        ));
        drop(lease);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn pause_discards_only_pending_and_preserves_an_already_claimed_turn() {
    for claimed in [false, true] {
        let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
        let lease = arm(&fixture).await;
        let _request = reserve(&fixture, &lease).await;
        fixture
            .kernel
            .message_status(&fixture.session_id, &input().message_id)
            .await
            .unwrap();
        if claimed {
            claim(&fixture).await.unwrap();
        }
        lease.revoke();
        let receipt =
            SessionContinuations::discard_if_pending(&fixture.kernel, &lease, &input().message_id)
                .await
                .unwrap();
        if claimed {
            assert!(matches!(receipt.state, MessageState::Claimed { .. }));
            let facts = fixture
                .store
                .read_turn_facts(
                    &fixture.session_id,
                    &TurnId::new("automatic-turn").unwrap(),
                    0,
                    64,
                )
                .await
                .unwrap();
            assert!(
                !facts
                    .facts
                    .iter()
                    .any(|fact| matches!(fact.body(), SessionFactBody::CancelRequested { .. }))
            );
            finish_initial_turn(&fixture.kernel).await;
        } else {
            assert!(matches!(
                receipt.state,
                MessageState::Discarded {
                    reason: MessageDiscardReason::ContinuationDisarmed,
                    ..
                }
            ));
        }
        let settlement = invocation(&fixture, "settle", "settlement-one", false).await;
        SessionContinuations::execute(
            &fixture.kernel,
            &lease,
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            settlement,
            None,
        )
        .await
        .unwrap();
        assert!(!lease.is_armed());
        let allocation = invocation(&fixture, "reserve", "reservation-two", true).await;
        assert!(
            SessionContinuations::execute(
                &fixture.kernel,
                &lease,
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                allocation,
                Some(input())
            )
            .await
            .is_err()
        );
        drop(lease);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn lease_revocation_or_domain_change_is_rechecked_at_claim() {
    for revoke in [false, true] {
        let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
        let lease = arm(&fixture).await;
        let _request = reserve(&fixture, &lease).await;
        fixture
            .kernel
            .message_status(&fixture.session_id, &input().message_id)
            .await
            .unwrap();
        if revoke {
            lease.revoke();
        } else {
            SessionCommands::execute(
                &fixture.kernel,
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                fixture.invocation("manual-change", false).await,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            claim(&fixture).await.unwrap_err(),
            TurnError::ContinuationDisarmed
        );
        assert!(matches!(
            fixture
                .kernel
                .message_status(&fixture.session_id, &input().message_id)
                .await
                .unwrap()
                .state,
            MessageState::Discarded { .. }
        ));
        drop(lease);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn frozen_receipt_rejects_changed_input_and_ordinary_route_rejects_forged_source() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let lease = arm(&fixture).await;
    let request = reserve(&fixture, &lease).await;
    let mut changed = input();
    changed.text.push_str(" changed");
    assert!(
        SessionContinuations::execute(
            &fixture.kernel,
            &lease,
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            request.clone(),
            Some(changed),
        )
        .await
        .is_err()
    );
    fixture
        .kernel
        .message_status(&fixture.session_id, &input().message_id)
        .await
        .unwrap();
    let message = fixture
        .store
        .read_agent_mailbox(&fixture.session_id, Some(&input().message_id))
        .await
        .unwrap()
        .selected
        .unwrap()
        .message;
    assert!(
        fixture
            .kernel
            .submit_message(SubmitMessage {
                session: SubmitSession::Resume(
                    fixture
                        .kernel
                        .prepare_resume(&fixture.session_id)
                        .await
                        .unwrap()
                ),
                message,
                delivery: MessageDelivery::NextTurn
            })
            .await
            .is_err()
    );
    drop(lease);
    fixture.stop().await;
}

#[tokio::test]
async fn first_draft_input_publishes_header_baseline_and_acceptance_together() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let pin = fixture.composition.pin.read().unwrap().clone();
    let header = header("goal-draft");
    let fresh = || PreparedFreshSession::new(header.clone(), pin.clone()).unwrap();
    let baseline = fresh().baseline().initial_states()[0].clone();
    let hash = baseline.sha256().unwrap();
    let lease = SessionContinuations::arm(
        &fixture.kernel,
        SubmitSession::Fresh(fresh()),
        ContinuationBinding {
            domain: baseline.identity().clone(),
            owner: input().owner,
            revision: DomainRevision::new(0),
            snapshot_sha256: hash.clone(),
        },
    )
    .await
    .unwrap();
    assert!(fixture.store.header(header.session_id()).await.is_err());
    let receipt = SessionContinuations::reserve_initial(
        &fixture.kernel,
        &lease,
        fresh(),
        SessionCommandInvocation {
            command: ContributionId::new("fixture.reserve").unwrap(),
            request_id: DomainRequestId::new("initial-reservation").unwrap(),
            expected_revision: rsi_agent_session_protocol::CommandRevision::Draft { revision: 0 },
            arguments: CommandArguments::new(true.into()).unwrap(),
        },
        input(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.accepted_control_seq, 2);
    let controls = fixture
        .store
        .read_controls(header.session_id(), 0, 8)
        .await
        .unwrap();
    assert_eq!(controls.records.len(), 2);
    assert!(
        matches!(controls.records[0].body(), AgentControlRecordBody::DomainStateCommitted { commit } if matches!(commit.source(), DomainMutationSource::Baseline))
    );
    assert!(
        matches!(controls.records[1].body(), AgentControlRecordBody::MessageAccepted { message, .. } if matches!(message.source, AgentMessageSource::Continuation { .. }))
    );
    let states = fixture
        .store
        .read_domain_states(header.session_id(), None)
        .await
        .unwrap();
    assert_eq!(states.states[0].snapshot.state().value(), &true);
    assert_eq!(
        fresh().baseline().initial_states()[0].state().value(),
        &false
    );
    drop(lease);
    fixture.stop().await;
}

#[tokio::test]
async fn restart_ready_scan_discards_auto_before_any_new_turn_is_claimed() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(rsi_agent_store_sqlite::SqliteStore::open(temp.path()).unwrap());
    let fixture = Fixture::start(store, false).await;
    let lease = arm(&fixture).await;
    let _request = reserve(&fixture, &lease).await;
    let receipt = fixture
        .kernel
        .message_status(&fixture.session_id, &input().message_id)
        .await
        .unwrap();
    let composition = fixture.composition.clone();
    let session = fixture.session_id.clone();
    drop(lease);
    fixture.stop().await;
    let store = Arc::new(rsi_agent_store_sqlite::SqliteStore::open(temp.path()).unwrap());
    let kernel = AgentKernel::recover(store.clone(), composition)
        .await
        .unwrap();
    let workers = kernel.start_workers();
    let executor = kernel.register("after-restart".into()).unwrap();
    let cancellation = CancellationToken::new();
    let mut observation = kernel
        .observe_session(
            &session,
            ObservationCursor {
                control_seq: receipt.accepted_control_seq,
                fact_seq: receipt.observed_fact_seq,
            },
        )
        .await
        .unwrap();
    let claim = kernel.claim("after-restart", cancellation.clone());
    tokio::pin!(claim);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut claim => panic!("restart must not claim automatic input: {result:?}"),
                update = observation.next() => {
                    if let Some(Ok(SessionObservation::Control { record, .. })) = update
                        && matches!(record.body(), AgentControlRecordBody::MessageDiscarded { reason: MessageDiscardReason::ContinuationDisarmed, .. }) { break; }
                }
            }
        }
    }).await.unwrap();
    cancellation.cancel();
    assert!(matches!(claim.await, Ok(None) | Err(TurnError::Cancelled)));
    assert_eq!(
        store
            .read_watermarks(&session)
            .await
            .unwrap()
            .durable_fact_seq,
        receipt.observed_fact_seq,
        "discarded continuation must not admit a new Turn"
    );
    drop(observation);
    drop(executor);
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Legal same-tree admission, stale cleanup and claim order share one fixture.
async fn busy_ordinary_backlog_skips_mailbox_while_same_tree_disarmed_input_is_cleaned() {
    for backlog in [1, 16, 64] {
        let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
        let fixture = Fixture::start(store.clone(), false).await;
        let kernel = &fixture.kernel;
        kernel
            .submit_message(SubmitMessage {
                session: super::super::resume(kernel, fixture.session_id.clone()).await,
                message: mailbox_message("root-active"),
                delivery: MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        let _root = kernel.register("root".into()).unwrap();
        let root = kernel
            .claim("root", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let busy_id = SessionId::new("busy-child").unwrap();
        let caller = control_tool_caller(kernel, &root).await;
        let spawn = |id: SessionId, name: &str| SpawnAgentRequest {
            output_contract: None,
            role: None,
            model: None,
            reasoning_effort: None,
            cancellation: CancellationToken::new(),
            caller: caller.clone(),
            child_session_id: id,
            task_name: name.into(),
            message_id: MessageId::new(format!("start-{name}")).unwrap(),
            message: name.into(),
            fork_turns: ForkTurnSelection::None,
        };
        // Admit the automatic input on an idle child. A busy root may no longer
        // allocate a continuation; the ready-index regression still exercises
        // stale cleanup beside an active sibling's full ordinary backlog.
        let auto_id = SessionId::new("automatic-child").unwrap();
        kernel
            .spawn_agent(spawn(auto_id.clone(), "automatic"))
            .await
            .unwrap();
        let auto_executor = kernel.register("automatic".into()).unwrap();
        let automatic = kernel
            .claim("automatic", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(automatic.session_id(), &auto_id);
        kernel
            .finish_turn(&automatic, &TurnOutcome::Completed)
            .await
            .unwrap();
        drop(auto_executor);
        kernel
            .spawn_agent(spawn(busy_id.clone(), "busy"))
            .await
            .unwrap();
        let _busy = kernel.register("busy".into()).unwrap();
        let busy = kernel
            .claim("busy", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(busy.session_id(), &busy_id);
        for index in 0..backlog {
            kernel
                .submit_message(SubmitMessage {
                    session: super::super::resume(kernel, busy_id.clone()).await,
                    message: mailbox_message(&format!("backlog-{index}")),
                    delivery: MessageDelivery::NextTurn,
                })
                .await
                .unwrap();
        }
        let lease = arm_at(&fixture, &auto_id).await;
        let _reservation = reserve(&fixture, &lease).await;
        fixture
            .kernel
            .message_status(lease.session_id(), &input().message_id)
            .await
            .unwrap();
        lease.revoke();
        let idle_id = SessionId::new("idle-child").unwrap();
        kernel
            .spawn_agent(spawn(idle_id.clone(), "idle"))
            .await
            .unwrap();
        let ready = store
            .list_ready_messages(&fixture.session_id, None, 256)
            .await
            .unwrap();
        assert_eq!(ready.messages.len(), backlog + 2);
        assert!(ready.messages.windows(2).all(|pair| {
            let a = &pair[0];
            let b = &pair[1];
            (a.timestamp_ms, &a.session_id, a.control_seq)
                < (b.timestamp_ms, &b.session_id, b.control_seq)
        }));
        store.mailbox_reads.lock().unwrap().clear();
        let _idle = kernel.register("idle".into()).unwrap();
        let idle = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            kernel.claim("idle", CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(idle.session_id(), &idle_id);
        assert_eq!(
            store
                .mailbox_reads
                .lock()
                .unwrap()
                .get(&busy_id)
                .copied()
                .unwrap_or(0),
            0
        );
        assert!(
            store
                .mailbox_reads
                .lock()
                .unwrap()
                .get(&auto_id)
                .copied()
                .unwrap_or(0)
                > 0
        );
        assert!(matches!(
            store
                .read_agent_mailbox(&auto_id, Some(&input().message_id))
                .await
                .unwrap()
                .selected
                .unwrap()
                .state,
            rsi_agent_store_protocol::StoreAgentMessageState::Discarded {
                reason: MessageDiscardReason::ContinuationDisarmed,
                ..
            }
        ));
        kernel
            .finish_turn(&idle, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel
            .finish_turn(&busy, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel
            .finish_turn(&root, &TurnOutcome::Completed)
            .await
            .unwrap();
        drop(lease);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn independent_domains_share_a_session_but_not_a_second_owner_or_generation() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let old = fixture.composition.pin.read().unwrap().clone();
    let domains = DomainCatalog::new([
        DomainDefinition::new(
            DomainIdentity::new("fixture.goal", 1).unwrap(),
            &false,
            |_| Ok(()),
        )
        .unwrap()
        .registration(),
        DomainDefinition::new(
            DomainIdentity::new("fixture.schedule", 1).unwrap(),
            &false,
            |_| Ok(()),
        )
        .unwrap()
        .registration(),
    ])
    .unwrap();
    let pin = AgentCompositionPin::new(
        old.preset_id().clone(),
        "d".repeat(64),
        old.tools().clone(),
        old.context_builder().clone(),
        domains,
        ContributionCatalog::freeze(vec![]).unwrap(),
        Arc::new(()),
    )
    .unwrap();
    let make = |index: usize, pin: AgentCompositionPin| {
        let prepared = PreparedFreshSession::new(header("shared-continuations"), pin).unwrap();
        let snapshot = prepared.baseline().initial_states()[index].clone();
        let binding = ContinuationBinding {
            domain: snapshot.identity().clone(),
            owner: input().owner,
            revision: DomainRevision::new(0),
            snapshot_sha256: snapshot.sha256().unwrap(),
        };
        (SubmitSession::Fresh(prepared), binding)
    };
    let (session, binding) = make(0, pin.clone());
    let goal = SessionContinuations::arm(&fixture.kernel, session, binding)
        .await
        .unwrap();
    let (session, binding) = make(0, pin.clone());
    assert!(
        SessionContinuations::arm(&fixture.kernel, session, binding)
            .await
            .is_err()
    );
    let (session, binding) = make(1, pin.clone());
    let schedule = SessionContinuations::arm(&fixture.kernel, session, binding)
        .await
        .unwrap();
    assert!(goal.is_armed() && schedule.is_armed());
    goal.revoke();
    assert!(schedule.is_armed());
    drop(goal);
    let other = AgentCompositionPin::new(
        pin.preset_id().clone(),
        "e".repeat(64),
        pin.tools().clone(),
        pin.context_builder().clone(),
        pin.domains().clone(),
        pin.contributions().clone(),
        Arc::new(()),
    )
    .unwrap();
    let (session, binding) = make(0, other);
    assert_eq!(
        SessionContinuations::arm(&fixture.kernel, session, binding)
            .await
            .unwrap_err(),
        TurnError::ContinuationDisarmed
    );
    drop(schedule);
    fixture.stop().await;
}

#[tokio::test]
async fn busy_reservation_preserves_authority_and_budget_then_accepts_once_when_idle() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let lease = arm(&fixture).await;
    fixture
        .kernel
        .submit_message(SubmitMessage {
            session: super::super::resume(&fixture.kernel, fixture.session_id.clone()).await,
            message: mailbox_message("human-priority"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let before = fixture
        .store
        .read_domain_states(&fixture.session_id, None)
        .await
        .unwrap();
    let request = invocation(&fixture, "reserve", "busy-reservation", true).await;
    let revision = lease.revision();
    assert_eq!(
        SessionContinuations::execute(
            &fixture.kernel,
            &lease,
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            request.clone(),
            Some(input())
        )
        .await
        .unwrap_err(),
        TurnError::SessionBusy
    );
    assert!(lease.is_armed());
    assert_eq!(lease.revision(), revision);
    let after = fixture
        .store
        .read_domain_states(&fixture.session_id, None)
        .await
        .unwrap();
    assert_eq!(after.durable_control_seq, before.durable_control_seq);
    assert_eq!(after.states, before.states);
    assert!(
        fixture
            .kernel
            .domain_request(&fixture.session_id, &request.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .read_agent_mailbox(&fixture.session_id, Some(&input().message_id))
            .await
            .unwrap()
            .selected
            .is_none()
    );
    let executor = fixture.kernel.register("human".into()).unwrap();
    {
        let idle =
            SessionContinuations::wait_idle(&fixture.kernel, &lease, CancellationToken::new());
        tokio::pin!(idle);
        assert!(futures_util::poll!(idle.as_mut()).is_pending());
        let human = fixture
            .kernel
            .claim("human", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        fixture
            .kernel
            .finish_turn(&human, &TurnOutcome::Completed)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), idle)
            .await
            .unwrap()
            .unwrap();
    }
    let reservation = reserve(&fixture, &lease).await;
    let receipt = SessionContinuations::query(&fixture.kernel, &lease, &reservation.request_id)
        .await
        .unwrap()
        .unwrap();
    let accepted = fixture
        .store
        .read_agent_mailbox(&fixture.session_id, Some(&input().message_id))
        .await
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(accepted.accepted_control_seq, receipt.control_seq() + 1);
    drop(executor);
    drop(lease);
    fixture.stop().await;
}

#[derive(Debug)]
struct AutomaticToggle {
    domain: String,
    state: DomainHandle<bool>,
}
#[async_trait]
impl SessionCommand for AutomaticToggle {
    async fn execute(
        &self,
        context: &SessionCommandContext,
        _: &CommandArguments,
        _: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>> {
        let view = context
            .domains
            .iter()
            .find(|view| view.snapshot.identity().id() == self.domain)
            .unwrap();
        Ok(vec![self.state.propose(view.revision, &true).unwrap()])
    }
}

async fn reserve_domain(
    fixture: &Fixture,
    lease: &ContinuationLease,
    round: u64,
) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::DomainMutationReceipt> {
    let domain = lease.binding().domain.id();
    let revision = fixture
        .kernel
        .list(
            fixture
                .kernel
                .prepare_resume(lease.session_id())
                .await
                .unwrap(),
        )
        .await
        .unwrap()
        .revision();
    SessionContinuations::execute(
        &fixture.kernel,
        lease,
        fixture
            .kernel
            .prepare_resume(lease.session_id())
            .await
            .unwrap(),
        SessionCommandInvocation {
            command: ContributionId::new(format!("{domain}.reserve")).unwrap(),
            request_id: DomainRequestId::new(format!("{domain}-{round}")).unwrap(),
            expected_revision: revision,
            arguments: CommandArguments::new(true.into()).unwrap(),
        },
        Some(ContinuationInput {
            owner: lease.binding().owner.clone(),
            round,
            message_id: MessageId::new(format!("{domain}-{round}")).unwrap(),
            text: format!("{domain} {round}"),
        }),
    )
    .await
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Both domains share one atomic admission history and loser-budget assertions."
)]
async fn queued_automatic_domains_alternate_without_charging_the_loser() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let (_owner, context) = rsi_agent_testkit::activate_contribution_owner(&fixture.runtime.root())
        .await
        .unwrap();
    let (position, _registration) = context
        .registration_context()
        .unwrap()
        .register("two automatic domains", || Ok(()), Ok)
        .unwrap();
    let definitions = ["fixture.goal", "fixture.schedule"].map(|id| {
        DomainDefinition::new(DomainIdentity::new(id, 1).unwrap(), &false, |_| Ok(())).unwrap()
    });
    let domains =
        DomainCatalog::new(definitions.iter().map(DomainDefinition::registration)).unwrap();
    let registrations = definitions
        .iter()
        .zip(["fixture.goal", "fixture.schedule"])
        .map(|(definition, domain)| {
            let id = ContributionId::new(format!("{domain}.reserve")).unwrap();
            let descriptor = SessionCommandDescriptor::new(
                id.clone(),
                domain.replace('.', "-"),
                "Reserve automatic round",
                false,
            )
            .unwrap();
            (
                ContributionRegistration::new(
                    id,
                    0,
                    ContributionKind::Command(
                        SessionCommandRegistration::new(
                            descriptor,
                            Arc::new(AutomaticToggle {
                                domain: domain.into(),
                                state: domains.bind(definition).unwrap(),
                            }),
                        )
                        .continuation_only(
                            rsi_agent_composition_protocol::ContinuationCommand::Reserve,
                        ),
                    ),
                ),
                position.clone(),
            )
        })
        .collect();
    let old = fixture.composition.pin.read().unwrap().clone();
    let pin = AgentCompositionPin::new(
        old.preset_id().clone(),
        "f".repeat(64),
        old.tools().clone(),
        old.context_builder().clone(),
        domains,
        ContributionCatalog::freeze(registrations).unwrap(),
        Arc::new(()),
    )
    .unwrap();
    *fixture.composition.pin.write().unwrap() = pin.clone();
    let session = header("automatic-fairness");
    let session_id = session.session_id().clone();
    fixture
        .kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Fresh(PreparedFreshSession::new(session, pin).unwrap()),
            message: mailbox_message("human-first"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let executor = fixture.kernel.register("fairness".into()).unwrap();
    let human = fixture
        .kernel
        .claim("fairness", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let states = fixture
        .store
        .read_domain_states(&session_id, None)
        .await
        .unwrap();
    let mut leases = Vec::new();
    for state in &states.states {
        leases.push(
            SessionContinuations::arm(
                &fixture.kernel,
                super::super::resume(&fixture.kernel, session_id.clone()).await,
                ContinuationBinding {
                    domain: state.snapshot.identity().clone(),
                    owner: DomainRequestId::new(state.snapshot.identity().id()).unwrap(),
                    revision: state.head.revision,
                    snapshot_sha256: state.snapshot.sha256().unwrap(),
                },
            )
            .await
            .unwrap(),
        );
    }
    let [goal, schedule] = leases.as_slice() else {
        panic!("two domains");
    };
    assert_eq!(
        reserve_domain(&fixture, goal, 1).await.unwrap_err(),
        TurnError::SessionBusy
    );
    assert_eq!(
        reserve_domain(&fixture, schedule, 1).await.unwrap_err(),
        TurnError::SessionBusy
    );
    fixture
        .kernel
        .finish_turn(&human, &TurnOutcome::Completed)
        .await
        .unwrap();
    reserve_domain(&fixture, goal, 1).await.unwrap();
    let first = fixture
        .kernel
        .claim("fairness", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        reserve_domain(&fixture, goal, 2).await.unwrap_err(),
        TurnError::SessionBusy
    );
    fixture
        .kernel
        .finish_turn(&first, &TurnOutcome::Completed)
        .await
        .unwrap();
    let before = fixture
        .store
        .read_domain_states(&session_id, None)
        .await
        .unwrap();
    assert_eq!(
        reserve_domain(&fixture, goal, 2).await.unwrap_err(),
        TurnError::SessionBusy
    );
    assert_eq!(
        fixture
            .store
            .read_domain_states(&session_id, None)
            .await
            .unwrap()
            .states,
        before.states
    );
    reserve_domain(&fixture, schedule, 1).await.unwrap();
    let second = fixture
        .kernel
        .claim("fairness", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        reserve_domain(&fixture, schedule, 2).await.unwrap_err(),
        TurnError::SessionBusy
    );
    fixture
        .kernel
        .finish_turn(&second, &TurnOutcome::Completed)
        .await
        .unwrap();
    reserve_domain(&fixture, goal, 2).await.unwrap();
    assert!(goal.is_armed() && schedule.is_armed());
    let third = fixture
        .kernel
        .claim("fairness", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        reserve_domain(&fixture, goal, 3).await.unwrap_err(),
        TurnError::SessionBusy
    );
    fixture
        .kernel
        .finish_turn(&third, &TurnOutcome::Completed)
        .await
        .unwrap();
    let mut leases = leases.into_iter();
    let goal = leases.next().unwrap();
    let schedule = leases.next().unwrap();
    {
        let idle =
            SessionContinuations::wait_idle(&fixture.kernel, &goal, CancellationToken::new());
        tokio::pin!(idle);
        assert!(futures_util::poll!(idle.as_mut()).is_pending());
        drop(schedule);
        tokio::time::timeout(std::time::Duration::from_secs(1), idle)
            .await
            .unwrap()
            .unwrap();
    }
    drop(goal);
    drop(executor);
    fixture.stop().await;
}

#[tokio::test]
async fn settlement_revision_race_retains_lease_and_retries_without_second_allocation() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let lease = arm(&fixture).await;
    let _reservation = reserve(&fixture, &lease).await;
    let mut stale = invocation(&fixture, "settle", "raced-settlement", false).await;
    // A mailbox control commits between listing command state and settlement.
    fixture
        .kernel
        .message_status(&fixture.session_id, &input().message_id)
        .await
        .unwrap();
    SessionContinuations::discard_if_pending(&fixture.kernel, &lease, &input().message_id)
        .await
        .unwrap();
    let calls = fixture.callback.calls.load(Ordering::SeqCst);
    let result = SessionContinuations::execute(
        &fixture.kernel,
        &lease,
        fixture
            .kernel
            .prepare_resume(&fixture.session_id)
            .await
            .unwrap(),
        stale.clone(),
        None,
    )
    .await;
    assert!(
        matches!(result, Err(TurnError::CommandRevisionConflict { .. })),
        "{result:?}"
    );
    assert!(
        lease.is_armed(),
        "precommit revision contention must not revoke the lease"
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), calls);
    stale.expected_revision = invocation(&fixture, "settle", "raced-settlement", false)
        .await
        .expected_revision;
    SessionContinuations::execute(
        &fixture.kernel,
        &lease,
        fixture
            .kernel
            .prepare_resume(&fixture.session_id)
            .await
            .unwrap(),
        stale,
        None,
    )
    .await
    .unwrap();
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), calls + 1);
    drop(lease);
    fixture.stop().await;
}

#[tokio::test]
async fn revocation_during_committed_reservation_returns_its_receipt_without_rearming() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let fixture = Fixture::start(store.clone(), false).await;
    let lease = arm(&fixture).await;
    let request = invocation(&fixture, "reserve", "overlapping-revoke", true).await;
    let prepared = fixture
        .kernel
        .prepare_resume(&fixture.session_id)
        .await
        .unwrap();
    store.pause_next_agent_commit_after_apply();
    let pending = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let lease = lease.clone();
        async move {
            SessionContinuations::execute(&kernel, &lease, prepared, request, Some(input())).await
        }
    });
    store.wait_until_agent_commit_is_applied().await;
    lease.revoke();
    store.release_applied_agent_commit();
    let receipt = pending.await.unwrap().unwrap();
    assert!(!lease.is_armed());
    assert_eq!(lease.revision(), receipt.commit().updates()[0].revision());
    assert_eq!(
        fixture
            .kernel
            .domain_request(&fixture.session_id, receipt.commit().request_id().unwrap())
            .await
            .unwrap(),
        Some(receipt)
    );
    drop(lease);
    fixture.stop().await;
}

#[tokio::test]
async fn total_lease_bound_spans_domains_and_releases_one_slot() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let old = fixture.composition.pin.read().unwrap().clone();
    let definitions = ["one", "two", "three"].map(|id| {
        DomainDefinition::new(DomainIdentity::new(id, 1).unwrap(), &false, |_| Ok(())).unwrap()
    });
    let pin = AgentCompositionPin::new(
        old.preset_id().clone(),
        "e".repeat(64),
        old.tools().clone(),
        old.context_builder().clone(),
        DomainCatalog::new(definitions.iter().map(DomainDefinition::registration)).unwrap(),
        ContributionCatalog::default(),
        Arc::new(()),
    )
    .unwrap();
    let mut leases = Vec::new();
    for index in 0..=128 {
        let fresh =
            PreparedFreshSession::new(header(&format!("total-{index}")), pin.clone()).unwrap();
        let snapshot = fresh.baseline().initial_states()[index % 3].clone();
        let arm = || {
            SessionContinuations::arm(
                &fixture.kernel,
                SubmitSession::Fresh(
                    PreparedFreshSession::new(fresh.header().clone(), pin.clone()).unwrap(),
                ),
                ContinuationBinding {
                    domain: snapshot.identity().clone(),
                    owner: input().owner,
                    revision: DomainRevision::new(0),
                    snapshot_sha256: snapshot.sha256().unwrap(),
                },
            )
        };
        if index == 128 {
            assert_eq!(arm().await.unwrap_err(), TurnError::Capacity);
            leases.pop();
            leases.push(arm().await.unwrap());
        } else {
            leases.push(arm().await.unwrap());
        }
    }
    assert_eq!(leases.len(), 128);
    drop(leases);
    fixture.stop().await;
}

#[tokio::test]
async fn fresh_reservation_busy_leaves_baseline_and_authority_uncharged() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let pin = fixture.composition.pin.read().unwrap().clone();
    let header = header("fresh-busy");
    let fresh = || PreparedFreshSession::new(header.clone(), pin.clone()).unwrap();
    let snapshot = fresh().baseline().initial_states()[0].clone();
    let lease = SessionContinuations::arm(
        &fixture.kernel,
        SubmitSession::Fresh(fresh()),
        ContinuationBinding {
            domain: snapshot.identity().clone(),
            owner: input().owner,
            revision: DomainRevision::new(0),
            snapshot_sha256: snapshot.sha256().unwrap(),
        },
    )
    .await
    .unwrap();
    fixture
        .kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Fresh(fresh()),
            message: mailbox_message("human-wins-draft"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let before = fixture
        .store
        .read_domain_states(header.session_id(), None)
        .await
        .unwrap();
    let result = SessionContinuations::reserve_initial(
        &fixture.kernel,
        &lease,
        fresh(),
        SessionCommandInvocation {
            command: ContributionId::new("fixture.reserve").unwrap(),
            request_id: DomainRequestId::new("fresh-busy").unwrap(),
            expected_revision: rsi_agent_session_protocol::CommandRevision::Draft { revision: 0 },
            arguments: CommandArguments::new(true.into()).unwrap(),
        },
        input(),
    )
    .await;
    assert_eq!(result.unwrap_err(), TurnError::SessionBusy);
    assert!(lease.is_armed());
    assert_eq!(lease.revision(), DomainRevision::new(0));
    let after = fixture
        .store
        .read_domain_states(header.session_id(), None)
        .await
        .unwrap();
    assert_eq!(before.states, after.states);
    assert_eq!(before.durable_control_seq, after.durable_control_seq);
    assert!(
        fixture
            .store
            .read_agent_mailbox(header.session_id(), Some(&input().message_id))
            .await
            .unwrap()
            .selected
            .is_none()
    );
    drop(lease);
    fixture.stop().await;
}

#[tokio::test]
async fn commit_time_quiescence_rejection_is_retryable_busy_without_charging() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let fixture = Fixture::start(store.clone(), false).await;
    let lease = arm(&fixture).await;
    let request = invocation(&fixture, "reserve", "commit-busy", true).await;
    let before = store.read_watermarks(&fixture.session_id).await.unwrap();
    let revision = lease.revision();
    store.reject_quiescent_commit.store(true, Ordering::Release);
    let result = SessionContinuations::execute(
        &fixture.kernel,
        &lease,
        fixture
            .kernel
            .prepare_resume(&fixture.session_id)
            .await
            .unwrap(),
        request.clone(),
        Some(input()),
    )
    .await;
    assert_eq!(result.unwrap_err(), TurnError::SessionBusy);
    assert!(
        !store.reject_quiescent_commit.load(Ordering::Acquire),
        "test must reach the Store commit guard"
    );
    assert_eq!(
        store.read_watermarks(&fixture.session_id).await.unwrap(),
        before
    );
    assert!(lease.is_armed());
    assert_eq!(lease.revision(), revision);
    assert!(
        fixture
            .kernel
            .domain_request(&fixture.session_id, &request.request_id)
            .await
            .unwrap()
            .is_none()
    );
    SessionContinuations::execute(
        &fixture.kernel,
        &lease,
        fixture
            .kernel
            .prepare_resume(&fixture.session_id)
            .await
            .unwrap(),
        request,
        Some(input()),
    )
    .await
    .unwrap();
    drop(lease);
    fixture.stop().await;
}
