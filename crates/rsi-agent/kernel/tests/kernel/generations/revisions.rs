use super::*;
use rsi_agent_turn_protocol::DomainReadGuard;

fn program_pin(digit: char, extra: bool) -> AgentCompositionPin {
    let (base, _) = pin(digit, 1, extra, false);
    AgentCompositionPin::new(
        base.preset_id().clone(),
        digit.to_string().repeat(64),
        Arc::new(tool_origin::program::ProgramTools),
        base.context_builder(),
        base.domains().clone(),
        base.contributions().clone(),
        Arc::new(()),
    )
    .unwrap()
}

async fn detached_program(
    run: &Run,
    state: &rsi_agent_store_protocol::StoreDomainState,
) -> Arc<dyn rsi_agent_turn_protocol::ProgramRun> {
    let caller = tool_origin::program::workflow::start_workflow_tool(&run.kernel, &run.claim).await;
    let program = run
        .kernel
        .prepare_program(rsi_agent_turn_protocol::PrepareProgram {
            caller: caller.clone(),
            cancellation: CancellationToken::new(),
            script: "return 42".into(),
            fork_turns: rsi_agent_session_protocol::ForkTurnSelection::None,
            continuation_domains: vec![],
            guard: Some(rsi_agent_session_protocol::ProgramDomainGuard {
                domain: state.head.identity.clone(),
                revision: state.head.revision,
                snapshot_sha256: state.snapshot.sha256().unwrap(),
            }),
        })
        .await
        .unwrap();
    program.accept(&caller).await.unwrap();
    program.start().await.unwrap();
    program.detach().await.unwrap();
    tool_origin::flush_bodies(
        &run.kernel,
        &run.claim,
        vec![tool_origin::program::result(
            run.claim.turn_id(),
            "workflow",
            "detached",
        )],
    )
    .await;
    program
}

#[tokio::test]
async fn a_detached_program_survives_code_replacement_but_not_removed_policy_reintroduction() {
    let store = Arc::new(MemoryStore::new());
    let source = Arc::new(Source::new(program_pin('a', true)));
    let run = Run::start(store.clone(), source.clone()).await;
    let states = store
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    let extra = states
        .states
        .iter()
        .find(|state| state.head.identity.id() == "fixture.extra")
        .unwrap();
    let program = detached_program(&run, extra).await;
    adopt(&run, &source, program_pin('b', true)).await;
    program
        .progress(None, "same policy under new code".into())
        .await
        .unwrap();
    adopt(&run, &source, program_pin('c', false)).await;
    assert!(program.cancellation().is_cancelled());
    adopt(&run, &source, program_pin('d', true)).await;
    assert!(matches!(
        program.progress(None, "stale policy".into()).await,
        Err(TurnError::Cancelled)
    ));
    program.cancel().await.unwrap();
    program
        .finish(rsi_agent_session_protocol::ProgramOutcome::Cancelled, None)
        .await
        .unwrap();
    run.stop().await;
}

async fn adopt(run: &Run, source: &Source, selected: AgentCompositionPin) {
    source.select(selected);
    run.kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap()
        .commit()
        .await
        .unwrap();
}

#[tokio::test]
async fn unchanged_generation_keeps_domain_revision_and_existing_read_guard() {
    let store = Arc::new(MemoryStore::new());
    let (old, _) = pin('a', 1, false, false);
    let source = Arc::new(Source::new(old));
    let run = Run::start(store.clone(), source.clone()).await;
    let before = store
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    let (next, handle) = pin('b', 1, false, false);
    adopt(&run, &source, next).await;
    let after = store
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    assert_eq!(after.states[0].snapshot, before.states[0].snapshot);
    assert_eq!(
        after.states[0].head.revision,
        before.states[0].head.revision
    );
    assert_eq!(
        after.states[0].head.control_seq,
        before.states[0].head.control_seq
    );
    let mut request = mutation(&handle, 1, "unchanged-guard");
    request.guards.push(DomainReadGuard {
        domain: before.states[0].head.identity.clone(),
        revision: before.states[0].head.revision,
    });
    run.kernel
        .commit_domains(&run.claim, request)
        .await
        .unwrap();
    run.stop().await;
}

#[tokio::test]
async fn removing_then_reintroducing_a_domain_cannot_revive_its_old_guard() {
    let store = Arc::new(MemoryStore::new());
    let (old, _) = pin('a', 1, true, false);
    let source = Arc::new(Source::new(old));
    let run = Run::start(store.clone(), source.clone()).await;
    let before = store
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    let old_extra = before
        .states
        .iter()
        .find(|state| state.head.identity.id() == "fixture.extra")
        .unwrap();
    adopt(&run, &source, pin('b', 1, false, false).0).await;
    let (next, handle) = pin('c', 1, true, false);
    adopt(&run, &source, next).await;
    let after = store
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    let extra = after
        .states
        .iter()
        .find(|state| state.head.identity.id() == "fixture.extra")
        .unwrap();
    assert_eq!(extra.snapshot, old_extra.snapshot);
    let current = after
        .states
        .iter()
        .find(|state| state.head.identity.id() == "fixture.state")
        .unwrap();
    let mut request = mutation(&handle, current.head.revision.get(), "revoked-guard");
    request.guards.push(DomainReadGuard {
        domain: old_extra.head.identity.clone(),
        revision: old_extra.head.revision,
    });
    assert!(matches!(
        run.kernel.commit_domains(&run.claim, request).await,
        Err(TurnError::DomainRevisionConflict { .. })
    ));
    assert!(extra.head.revision > old_extra.head.revision);
    run.stop().await;
}
