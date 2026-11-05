use super::*;
use rsi_agent_turn_protocol::TurnExecution;
use rsi_session_protocol::{ForkDraft, SelectDraftPreset, SessionDraftControl, SessionIngress};

#[tokio::test]
async fn manual_before_first_active_turn_creates_an_independent_empty_prefix() {
    let fixture = Fixture::new().await;
    let source = fixture.create("active-fork-source").await;
    let source_id = source.header().await.unwrap().session_id().clone();
    submit_text(source.clone(), "parent-input", "keep working")
        .await
        .unwrap();
    let registration = fixture.kernel.register("before-fork-test".into()).unwrap();
    let parent = fixture
        .kernel
        .claim("before-fork-test", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let before = fixture.store.read_watermarks(&source_id).await.unwrap();
    let mut request = Fixture::request("before-active-draft");
    request.fork = Some(ForkDraft {
        source_session_id: source_id.clone(),
        point: rsi_agent_session_protocol::ForkPoint::before(parent.turn_id()),
        turns: rsi_agent_session_protocol::ForkTurnSelection::All,
    });
    let draft = fixture.service.create(request.clone()).await.unwrap();
    let header = draft.header().await.unwrap();
    assert!(header.execution_owner().is_none());
    let history = header.history_origin().unwrap();
    assert_eq!(history.point, request.fork.as_ref().unwrap().point);
    assert_eq!(history.effective_turns, 0);
    assert_eq!(history.resolved_terminal_seq, 0);
    assert!(fixture.store.header(&request.session_id).await.is_err());
    submit_text(draft, "independent-input", "side question")
        .await
        .unwrap();
    let child = fixture
        .kernel
        .claim("before-fork-test", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child.session_id(), &request.session_id);
    fixture
        .kernel
        .finish_turn(&child, &rsi_agent_session_protocol::TurnOutcome::Completed)
        .await
        .unwrap();
    assert_eq!(
        fixture.store.read_watermarks(&source_id).await.unwrap(),
        before
    );
    assert_eq!(
        source.inspect().await.unwrap().active_turn_id.as_ref(),
        Some(parent.turn_id())
    );
    fixture
        .kernel
        .finish_turn(&parent, &rsi_agent_session_protocol::TurnOutcome::Completed)
        .await
        .unwrap();
    drop(registration);
    fixture.stop().await;
}

#[tokio::test]
async fn manual_draft_retries_rebinds_captured_state_and_publishes_only_with_input() {
    let fixture = Fixture::new().await;
    let source = fixture.create("fork-source").await;
    let source_id = source.header().await.unwrap().session_id().clone();
    let invocation = fixture.invoke(&source, "enable", true).await;
    fixture.callback.release.add_permits(1);
    source.execute_command(invocation).await.unwrap();
    submit_text(source.clone(), "source-input", "capture me")
        .await
        .unwrap();
    let registration = fixture.kernel.register("fork-test".into()).unwrap();
    let claim = fixture
        .kernel
        .claim("fork-test", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let mut request = Fixture::request("fork-draft");
    request.fork = Some(ForkDraft {
        source_session_id: source_id.clone(),
        point: rsi_agent_session_protocol::ForkPoint::through(claim.turn_id()),
        turns: rsi_agent_session_protocol::ForkTurnSelection::All,
    });
    assert!(fixture.service.create(request.clone()).await.is_err());
    // A failed creation has its own retained outcome. A new identity starts a new request.
    request.session_id = SessionId::new("completed-fork-draft").unwrap();
    fixture
        .kernel
        .finish_turn(&claim, &rsi_agent_session_protocol::TurnOutcome::Completed)
        .await
        .unwrap();
    let before = fixture.store.read_watermarks(&source_id).await.unwrap();
    let draft = create_enumerated_fork(&fixture, &request).await;
    let retry = fixture.service.create(request.clone()).await.unwrap();
    assert_eq!(draft.header().await.unwrap(), retry.header().await.unwrap());
    assert!(fixture.store.header(&request.session_id).await.is_err());
    let mut conflict = request.clone();
    conflict.fork.as_mut().unwrap().turns = rsi_agent_session_protocol::ForkTurnSelection::None;
    assert!(fixture.service.create(conflict).await.is_err());
    let header = draft
        .select_preset(SelectDraftPreset {
            preset_id: AgentPresetId::new("other").unwrap(),
            expected_revision: 0,
        })
        .await
        .unwrap()
        .header;
    assert!(header.history_origin().is_some());
    assert!(header.execution_owner().is_none());
    submit_text(draft.clone(), "fork-input", "continue independently")
        .await
        .unwrap();
    let states = fixture
        .store
        .read_domain_states(&request.session_id, None, None)
        .await
        .unwrap();
    assert!(
        fixture
            .callback
            .handle
            .decode(&states.states[0].snapshot)
            .unwrap()
    );
    assert_eq!(
        fixture.store.header(&request.session_id).await.unwrap(),
        header
    );
    assert_eq!(
        fixture.store.read_watermarks(&source_id).await.unwrap(),
        before
    );
    request.session_id = SessionId::new("released-fork").unwrap();
    let released_id = request.session_id.clone();
    let released = fixture.service.create(request).await.unwrap();
    assert_eq!(
        fixture.service.release_draft(&released_id),
        rsi_session_protocol::DraftRelease::Released
    );
    assert!(released.header().await.is_err());
    assert!(fixture.store.header(&released_id).await.is_err());
    drop(registration);
    fixture.stop().await;
}

async fn create_enumerated_fork(
    fixture: &Fixture,
    request: &CreateSession,
) -> Arc<dyn SessionHandle> {
    let owner = rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([11; 16]),
        revoked: CancellationToken::new(),
    };
    let draft = fixture
        .service
        .create_from(
            request.clone(),
            rsi_api_protocol::CallOrigin::Device(owner.clone()),
        )
        .await
        .unwrap();
    let drafts = fixture.service.device_drafts(&owner).await.unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].session_id, request.session_id);
    assert_eq!(drafts[0].inherited_turns, 1);
    let mut empty = request.clone();
    empty.session_id = SessionId::new("empty-history-draft").unwrap();
    empty.fork.as_mut().unwrap().turns = rsi_agent_session_protocol::ForkTurnSelection::None;
    let empty_handle = fixture
        .service
        .create_from(
            empty.clone(),
            rsi_api_protocol::CallOrigin::Device(owner.clone()),
        )
        .await
        .unwrap();
    let drafts = fixture.service.device_drafts(&owner).await.unwrap();
    assert_eq!(
        drafts
            .iter()
            .find(|row| row.session_id == empty.session_id)
            .unwrap()
            .inherited_turns,
        0
    );
    assert!(fixture.store.header(&empty.session_id).await.is_err());
    drop(empty_handle);
    draft
}
