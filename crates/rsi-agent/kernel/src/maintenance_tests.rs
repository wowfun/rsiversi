use super::*;
use rsi_agent_session_protocol::{
    AgentPresetId, DomainIdentity, DomainRequestId, DomainRevision, FrozenAgentSettings,
};
use rsi_agent_turn_protocol::ContinuationBinding;

#[tokio::test]
async fn retirement_refuses_every_pending_owner_and_preserves_residency() {
    let (kernel, header, pin) = retirement_fixture().await;
    let id = header.session_id().clone();
    let first_id = SessionId::new("a-free").unwrap();
    let turn = TurnId::new("pending").unwrap();
    let (baseline, fact) = retirement_pending_records(&turn);
    for condition in [
        "loading",
        "fresh",
        "queued",
        "header",
        "facts",
        "domain",
        "flush",
        "failed",
        "admission",
        "turn",
        "work",
        "uncertain",
    ] {
        let mut running_owner = None;
        {
            let mut state = lock_state(&kernel.inner);
            let mut session = SessionRuntime::new(header.clone(), pin.clone(), 0, false);
            match condition {
                "loading" => {
                    state
                        .loading_sessions
                        .insert(id.clone(), Arc::new(SessionLoad::pending()));
                }
                "fresh" => {
                    state.fresh_reservations.insert(id.clone());
                }
                "queued" => {
                    state.queued.insert((id.clone(), turn.clone()));
                }
                "header" => session.header_pending = true,
                "facts" => session.pending.push_back(fact.clone()),
                "domain" => session.pending_domain_baseline = Some(baseline.clone()),
                "flush" => session.flush_inflight = true,
                "failed" => session.permanent_flush_error = Some("unacknowledged".into()),
                "admission" => session.admission_reservations = 1,
                "turn" => {
                    session.turns.insert(turn.clone(), TurnControl::new(1, 1));
                }
                "work" | "uncertain" => {
                    let (work, owner) = rsi_agent_turn_protocol::ControlledWork::new();
                    state
                        .controlled_work
                        .insert((id.clone(), turn.clone()), 1, work)
                        .unwrap();
                    if condition == "work" {
                        running_owner = Some(owner);
                    } else {
                        drop(owner);
                    }
                }
                _ => unreachable!(),
            }
            state.sessions.insert(id.clone(), session);
        }
        let result = TurnService::prepare_retirement(&kernel, &[first_id.clone(), id.clone()]);
        if condition == "uncertain" {
            assert!(
                matches!(result, Err(TurnError::UnsettledWork(session)) if session == id.as_str())
            );
        } else {
            assert!(matches!(result, Err(TurnError::Capacity)), "{condition}");
        }
        {
            let mut state = lock_state(&kernel.inner);
            assert!(state.sessions.contains_key(&id), "{condition}");
            state.sessions.remove(&id);
            state.loading_sessions.clear();
            state.fresh_reservations.clear();
            state.queued.clear();
            state.controlled_work = controlled_work::Registry::default();
        }
        assert!(TurnService::prepare_retirement(&kernel, std::slice::from_ref(&first_id)).is_ok());
        drop(running_owner);
        // A refused attempt releases every submission guard.
        assert!(TurnService::prepare_retirement(&kernel, std::slice::from_ref(&id)).is_ok());
    }
}

#[tokio::test]
async fn retirement_waits_for_continuations_and_releases_residency_explicitly() {
    let (kernel, header, pin) = retirement_fixture().await;
    let id = header.session_id().clone();
    let lease = kernel.inner.continuation_issuer.issue(
        header.clone(),
        pin.clone(),
        ContinuationBinding {
            domain: DomainIdentity::new("test", 1).unwrap(),
            owner: DomainRequestId::new("owner").unwrap(),
            revision: DomainRevision::new(0),
            snapshot_sha256: "a".repeat(64),
        },
        None,
    );
    kernel
        .inner
        .continuations
        .lock()
        .unwrap()
        .insert((id.clone(), "owner".into()), lease.downgrade());
    assert!(matches!(
        TurnService::prepare_retirement(&kernel, std::slice::from_ref(&id)),
        Err(TurnError::Capacity)
    ));
    drop(lease);
    lock_state(&kernel.inner)
        .sessions
        .insert(id.clone(), SessionRuntime::new(header, pin, 0, false));
    let retirement = TurnService::prepare_retirement(&kernel, std::slice::from_ref(&id)).unwrap();
    assert!(lock_state(&kernel.inner).sessions.contains_key(&id));
    assert!(matches!(
        TurnService::prepare_retirement(&kernel, std::slice::from_ref(&id)),
        Err(TurnError::Capacity)
    ));
    retirement.release_residency();
    assert!(!lock_state(&kernel.inner).sessions.contains_key(&id));
    drop(retirement);
    assert!(TurnService::prepare_retirement(&kernel, std::slice::from_ref(&id)).is_ok());
}

async fn retirement_fixture() -> (
    AgentKernel,
    SessionHeader,
    rsi_agent_composition_protocol::AgentCompositionPin,
) {
    let kernel = AgentKernel::recover(
        Arc::new(rsi_agent_testkit::MemoryStore::new()),
        Arc::new(ClaimFixture),
    )
    .await
    .unwrap();
    let id = SessionId::new("retirement-matrix").unwrap();
    let header = SessionHeader::new_local(
        id.clone(),
        1,
        "/workspace",
        AgentPresetId::new("test").unwrap(),
        FrozenAgentSettings::new(
            "test",
            "system",
            rsi_ai_protocol::ModelRef::new("test", "test").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let pin = ClaimFixture
        .pin(header.agent_preset_id(), None)
        .await
        .unwrap();
    (kernel, header, pin)
}

fn retirement_pending_records(turn: &TurnId) -> (AgentControlRecord, Arc<SessionFact>) {
    let baseline = AgentControlRecord::new(
        1,
        1,
        AgentControlRecordBody::TurnBoundaryRecorded {
            turn_id: turn.clone(),
            terminal_fact_seq: 1,
        },
    )
    .unwrap();
    let fact = Arc::new(
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: turn.clone(),
                text: "pending".into(),
                model: None,
                reasoning_effort: None,
                sandbox: rsi_sandbox::SandboxMode::ReadOnly,
                require_approval: false,
            },
        )
        .unwrap(),
    );
    (baseline, fact)
}
