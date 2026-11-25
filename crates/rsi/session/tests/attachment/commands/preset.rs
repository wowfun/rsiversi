use super::*;
use rsi_session_protocol::SelectDraftPreset;

fn select(preset: &str, revision: u64) -> SelectDraftPreset {
    SelectDraftPreset {
        preset_id: AgentPresetId::new(preset).unwrap(),
        expected_revision: revision,
    }
}

#[tokio::test]
async fn a_paired_creator_can_choose_the_initial_preset_but_only_the_host_can_change_it() {
    use rsi_api_protocol::{ApiError, AuthenticatedDevice, CallOrigin, DeviceId};
    use rsi_session_protocol::SessionIngress as _;

    let fixture = Fixture::new().await;
    let mut request = Fixture::request("paired-preset");
    request.agent_preset_id = Some(AgentPresetId::new("other").unwrap());
    let id = request.session_id.clone();
    let guest = fixture
        .service
        .create_from(
            request,
            CallOrigin::Device(AuthenticatedDevice {
                id: DeviceId::from_bytes([7; 16]),
                revoked: CancellationToken::new(),
            }),
        )
        .await
        .unwrap();
    let initial = guest.draft_snapshot().await.unwrap();
    assert_eq!(initial.header.agent_preset_id().as_str(), "other");
    assert!(matches!(
        guest
            .select_preset(select("fixture", initial.revision))
            .await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert_eq!(guest.draft_snapshot().await.unwrap(), initial);

    let owner = fixture.service.attach(&id).await.unwrap();
    let selected = owner
        .select_preset(select("fixture", initial.revision))
        .await
        .unwrap();
    assert_eq!(guest.draft_snapshot().await.unwrap(), selected);
    submit_text(guest.clone(), "accepted", "queued")
        .await
        .unwrap();
    let cut = fixture.store.read_watermarks(&id).await.unwrap();
    assert_eq!(cut.durable_fact_seq, 0);
    assert!(matches!(
        guest
            .select_agent_preset(AgentPresetId::new("other").unwrap())
            .await,
        Err(SessionError::Api(ApiError::Unauthorized))
    ));
    assert_eq!(fixture.store.read_watermarks(&id).await.unwrap(), cut);
    fixture.stop().await;
}

#[tokio::test]
async fn published_input_checks_the_selected_workflow_before_acceptance() {
    use rsi_agent_turn_protocol::SessionCommands;

    let fixture = Fixture::new().await;
    let handle = fixture.create("workflow-selection").await;
    let header = handle.header().await.unwrap();
    let id = header.session_id();
    submit_text(handle.clone(), "original", "queued")
        .await
        .unwrap();
    let cut = fixture.store.read_watermarks(id).await.unwrap();
    // A previous Host with Workflow installed could have made this selection.
    fixture
        .kernel
        .select_unstarted_preset(
            id,
            AgentPresetId::new("workflow").unwrap(),
            cut.durable_control_seq,
        )
        .await
        .unwrap();
    let selected = fixture.store.read_watermarks(id).await.unwrap();
    assert!(matches!(
        submit_text(handle.clone(), "blocked", "follow up").await,
        Err(SessionError::WorkflowUnavailable(
            rsi_session_protocol::WorkflowRuntimeKind::Absent
        ))
    ));
    assert_eq!(fixture.store.read_watermarks(id).await.unwrap(), selected);
    handle
        .select_agent_preset(header.agent_preset_id().clone())
        .await
        .unwrap();
    submit_text(handle, "allowed", "follow up").await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn preset_failure_preserves_values_and_success_resets_defaults_on_the_same_lease() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("preset").await;
    let invocation = fixture.invoke(&handle, "enable", true).await;
    fixture.callback.release.add_permits(1);
    let receipt = handle.execute_command(invocation.clone()).await.unwrap();
    let before = handle.draft_snapshot().await.unwrap();
    assert!(matches!(
        handle.select_preset(select("missing", 1)).await,
        Err(SessionError::Invalid(_))
    ));
    assert_eq!(handle.draft_snapshot().await.unwrap(), before);
    let late = fixture.invoke(&handle, "late", true).await;
    let late = tokio::spawn({
        let handle = handle.clone();
        async move { handle.execute_command(late).await }
    });
    fixture
        .callback
        .entered
        .acquire_many(2)
        .await
        .unwrap()
        .forget();
    let selected = handle.select_preset(select("other", 1)).await.unwrap();
    assert_eq!(selected.revision, 2);
    assert_eq!(selected.header.agent_preset_id().as_str(), "other");
    fixture.callback.release.add_permits(1);
    assert!(matches!(
        late.await.unwrap(),
        Err(SessionError::CommandRevisionConflict { .. })
    ));
    let reconnected = fixture.create("preset").await;
    assert_eq!(reconnected.header().await.unwrap(), selected.header);
    assert_eq!(
        reconnected
            .command_status(&invocation.request_id)
            .await
            .unwrap(),
        Some(receipt)
    );
    assert!(matches!(
        handle.select_preset(select("fixture", 1)).await,
        Err(SessionError::CommandRevisionConflict { .. })
    ));
    submit_text(handle.clone(), "first", "publish")
        .await
        .unwrap();
    let states = fixture
        .store
        .read_domain_states(&SessionId::new("preset").unwrap(), None, None)
        .await
        .unwrap();
    assert!(
        !fixture
            .callback
            .handle
            .decode(&states.states[0].snapshot)
            .unwrap()
    );
    assert_eq!(
        fixture
            .store
            .header(&SessionId::new("preset").unwrap())
            .await
            .unwrap(),
        selected.header
    );
    assert!(matches!(
        handle.select_preset(select("fixture", 2)).await,
        Err(SessionError::NotFound(_))
    ));
    fixture.stop().await;
}

#[tokio::test]
async fn preset_preparation_holds_no_mutation_lock_and_rejects_late_results() {
    let fixture = Fixture::new().await;
    for publish in [false, true] {
        let handle = fixture
            .create(if publish { "publish" } else { "command" })
            .await;
        let pending = tokio::spawn({
            let handle = handle.clone();
            async move { handle.select_preset(select("blocked", 0)).await }
        });
        fixture
            .composition
            .entered
            .acquire()
            .await
            .unwrap()
            .forget();
        if publish {
            submit_text(handle.clone(), "first", "publish")
                .await
                .unwrap();
        } else {
            let invocation = fixture.invoke(&handle, "enable", true).await;
            fixture.callback.release.add_permits(1);
            handle.execute_command(invocation).await.unwrap();
        }
        fixture.composition.release.add_permits(1);
        assert!(matches!(
            pending.await.unwrap(),
            Err(SessionError::CommandRevisionConflict { .. } | SessionError::NotFound(_))
        ));
        assert_eq!(
            handle.header().await.unwrap().agent_preset_id().as_str(),
            "fixture"
        );
    }
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn preset_deadline_and_retirement_preserve_the_previous_draft() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("deadline").await;
    let before = handle.draft_snapshot().await.unwrap();
    let pending = tokio::spawn({
        let handle = handle.clone();
        async move { handle.select_preset(select("blocked", 0)).await }
    });
    fixture
        .composition
        .entered
        .acquire()
        .await
        .unwrap()
        .forget();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    assert!(matches!(
        pending.await.unwrap(),
        Err(SessionError::Invalid(_))
    ));
    assert_eq!(handle.draft_snapshot().await.unwrap(), before);
    let pending = tokio::spawn({
        let handle = handle.clone();
        async move { handle.select_preset(select("blocked", 0)).await }
    });
    fixture
        .composition
        .entered
        .acquire()
        .await
        .unwrap()
        .forget();
    fixture.service.stop().await.unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(SessionError::ShuttingDown)
    ));
    fixture.stop().await;
}

#[tokio::test]
async fn disconnected_preset_waiter_does_not_abandon_its_owned_selection() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("disconnected-preset").await;
    let pending = tokio::spawn({
        let handle = handle.clone();
        async move { handle.select_preset(select("blocked", 0)).await }
    });
    fixture
        .composition
        .entered
        .acquire()
        .await
        .unwrap()
        .forget();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    fixture.composition.release.add_permits(1);
    let selected = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let view = handle.draft_snapshot().await.unwrap();
            if view.revision == 1 {
                break view;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(selected.header.agent_preset_id().as_str(), "blocked");
    fixture.stop().await;
}
