use super::*;
use rsi_agent_session_protocol::{
    ContinuationInput, ContinuationProvenance, DomainMutationSource, DomainRevision,
    MessageDelivery, MessageDiscardReason,
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
                initial_input: Some(input()),
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

async fn arm(fixture: &Fixture) -> ContinuationLease {
    let page = fixture
        .store
        .read_domain_states(&fixture.session_id, None)
        .await
        .unwrap();
    let state = &page.states[0];
    SessionContinuations::arm(
        &fixture.kernel,
        SubmitSession::Resume(
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
        ),
        ContinuationBinding {
            domain: state.snapshot.identity().clone(),
            owner: input().owner,
            revision: state.head.revision,
            snapshot_sha256: state.snapshot.sha256().unwrap(),
            initial_input: None,
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
    let invocation = invocation(fixture, "reserve", "reservation-one", true).await;
    SessionContinuations::execute(
        &fixture.kernel,
        lease,
        fixture
            .kernel
            .prepare_resume(&fixture.session_id)
            .await
            .unwrap(),
        invocation.clone(),
        Some(input()),
    )
    .await
    .unwrap();
    invocation
}

async fn submit(
    fixture: &Fixture,
    lease: &ContinuationLease,
    request: &SessionCommandInvocation,
) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::MessageReceipt> {
    SessionContinuations::submit(
        &fixture.kernel,
        lease,
        SubmitSession::Resume(
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
        ),
        input(),
        ContinuationProvenance::Command {
            request_id: request.request_id.clone(),
        },
    )
    .await
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
async fn internal_discovery_dispatch_receipts_and_reservations_are_separate() {
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
    assert!(
        fixture
            .store
            .read_agent_mailbox(&fixture.session_id, Some(&input().message_id))
            .await
            .unwrap()
            .selected
            .is_none()
    );
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
    let accepted = submit(&fixture, &lease, &request).await.unwrap();
    assert_eq!(accepted.state, MessageState::Pending);
    assert_eq!(submit(&fixture, &lease, &request).await.unwrap(), accepted);
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
        let request = reserve(&fixture, &lease).await;
        let auto = submit(&fixture, &lease, &request).await.unwrap();
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
        let request = reserve(&fixture, &lease).await;
        submit(&fixture, &lease, &request).await.unwrap();
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
        let request = reserve(&fixture, &lease).await;
        submit(&fixture, &lease, &request).await.unwrap();
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
        SessionContinuations::submit(
            &fixture.kernel,
            &lease,
            SubmitSession::Resume(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap()
            ),
            changed,
            ContinuationProvenance::Command {
                request_id: request.request_id.clone()
            }
        )
        .await
        .is_err()
    );
    submit(&fixture, &lease, &request).await.unwrap();
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
            initial_input: Some(input()),
        },
    )
    .await
    .unwrap();
    assert!(fixture.store.header(header.session_id()).await.is_err());
    let receipt = SessionContinuations::submit(
        &fixture.kernel,
        &lease,
        SubmitSession::Fresh(fresh()),
        input(),
        ContinuationProvenance::Baseline {
            snapshot_sha256: hash.clone(),
        },
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
    assert_eq!(
        SessionContinuations::submit(
            &fixture.kernel,
            &lease,
            SubmitSession::Fresh(fresh()),
            input(),
            ContinuationProvenance::Baseline {
                snapshot_sha256: hash
            }
        )
        .await
        .unwrap(),
        receipt
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
    let request = reserve(&fixture, &lease).await;
    let receipt = submit(&fixture, &lease, &request).await.unwrap();
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
    assert!(
        store
            .read_turn_boundary(
                &session,
                &TurnId::new(format!("turn-message-{}", receipt.accepted_control_seq)).unwrap()
            )
            .await
            .is_err()
    );
    drop(observation);
    drop(executor);
    kernel.shutdown(workers).await.unwrap();
}
