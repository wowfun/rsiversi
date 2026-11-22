use super::*;
use rsi_api_protocol::{ApiError, ApiOutput, ByteBudget, CallOrigin};
use rsi_navigation_api::{NavigationFilter, NavigationOperation, NavigationPage, SessionMetadata};
use serde_json::{Value, json};

pub(super) async fn wire(
    running: &RunningRsi,
    origin: CallOrigin,
    operation: NavigationOperation,
    input: Value,
) -> Result<Value, ApiError> {
    let spec = operation.spec();
    let input = ByteBudget::default().encode(&input, spec.maximum_request_bytes)?;
    let ApiOutput::Reply(output) = running
        .api_dispatch()
        .unwrap()
        .admit(&spec.call(), origin)?
        .invoke(input)
        .await?
    else {
        panic!("unexpected navigation stream")
    };
    Ok(serde_json::from_slice(output.json.as_bytes()).unwrap())
}
async fn query(running: &RunningRsi, origin: CallOrigin, archived: bool) -> NavigationPage {
    serde_json::from_value(
        wire(
            running,
            origin,
            NavigationOperation::Query,
            json!({"filter":NavigationFilter { archived: Some(archived), ..Default::default() },"after":null}),
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_blocks_model_requests_from_other_handles_until_restored() {
    use rsi_agent_session_protocol::{ForkPoint, ForkTurnSelection, TurnOutcome};

    let (endpoint, requests, provider) = capturing_provider().await;
    let fixture = fixture(&endpoint);
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let service = running.session_service().unwrap();
    let session = SessionId::new("archive-model").unwrap();
    let create = CreateSession {
        session_id: session.clone(),
        workspace_id: workspace.id,
        agent_preset_id: None,
        fork: None,
    };
    let original = service.create(create.clone()).await.unwrap();
    run_message_to_terminal(&original, "before-archive").await;
    let second = service.attach(&session).await.unwrap();
    let before = original.history_before(None, 128).await.unwrap();
    let first = before
        .facts
        .iter()
        .find_map(|fact| match fact.body() {
            SessionFactBody::TurnTerminal {
                turn_id,
                outcome: TurnOutcome::Completed,
                ..
            } => Some(turn_id),
            _ => None,
        })
        .unwrap();
    let fork = service
        .create(CreateSession {
            session_id: SessionId::new("independent-conversation").unwrap(),
            fork: Some(rsi_session_protocol::ForkDraft {
                source_session_id: session.clone(),
                point: ForkPoint::through(first),
                turns: ForkTurnSelection::All,
            }),
            ..create
        })
        .await
        .unwrap();
    let change = |archived: bool, revision: &'static str| {
        wire(
            &running,
            CallOrigin::Local,
            NavigationOperation::Replace,
            json!({"session":session,"expected_revision":revision,"metadata":SessionMetadata { archived, ..Default::default() }}),
        )
    };
    change(true, "0").await.unwrap();
    let admitted = requests.lock().unwrap().len();
    assert!(admitted > 0);
    run_message_to_terminal(&second, "from-open-window").await;
    assert_eq!(requests.lock().unwrap().len(), admitted);
    let after = second.history_before(None, 128).await.unwrap();
    let added = after
        .facts
        .iter()
        .filter(|fact| fact.seq() > before.facts.last().unwrap().seq());
    assert!(added.clone().any(|fact| matches!(
        fact.body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Blocked,
            ..
        }
    )));
    assert!(
        !added
            .clone()
            .any(|fact| matches!(fact.body(), SessionFactBody::ModelIntent { .. }))
    );
    run_message_to_terminal(&fork, "send-in-independent-fork").await;
    assert_eq!(requests.lock().unwrap().len(), admitted + 1);
    change(false, "1").await.unwrap();
    run_message_to_terminal(&second, "after-restore").await;
    assert_eq!(requests.lock().unwrap().len(), admitted + 2);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "One observable lifecycle with shared setup and assertions"
)]
async fn navigation_metadata_is_durable_without_rewriting_history_or_requiring_configuration_grants()
 {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let session = SessionId::new("navigation-durable").unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .create(CreateSession {
            fork: None,
            workspace_id: workspace.id.clone(),
            session_id: session.clone(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let metadata = SessionMetadata {
        pinned: false,
        title: Some("调试工作区".into()),
        archived: true,
    };
    assert!(matches!(
        wire(
            &running,
            CallOrigin::Local,
            NavigationOperation::Replace,
            json!({"session":session,"expected_revision":"0","metadata":metadata})
        )
        .await,
        Err(ApiError::Invalid(_))
    ));
    assert!(
        query(&running, CallOrigin::Local, false)
            .await
            .entries
            .is_empty()
    );
    run_message_to_terminal(&handle, "navigation-first-message").await;
    let service = running.session_service().unwrap();
    let activity = service.activity().await.unwrap();
    let header = service.read_summary(&session).await.unwrap().header;
    assert_eq!(header, handle.header().await.unwrap());
    assert_eq!(
        service.activity().await.unwrap(),
        activity,
        "Header reads cannot fabricate activity"
    );
    let before = handle.inspect().await.unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("navigation-only")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    let page = query(&running, origin.clone(), false).await;
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].workspace, Some(workspace.id.clone()));
    assert!(!page.can_archive);
    for action in ["archive", "stop_and_archive", "restore"] {
        assert!(matches!(
            wire(
                &running,
                origin.clone(),
                NavigationOperation::Archive,
                json!({"session":session,"action":action})
            )
            .await,
            Err(ApiError::Unauthorized)
        ));
    }
    for stop_activity in [false, true] {
        assert!(matches!(wire(&running, origin.clone(), NavigationOperation::Replace,
            json!({"session":session, "expected_revision":"0","metadata":metadata,"stop_activity":stop_activity})).await, Err(ApiError::Unauthorized)));
    }
    let owner = running
        .device_administration()
        .unwrap()
        .rotate_managed("local-web", "Host owner")
        .await
        .unwrap();
    let managed = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&owner.token)
            .unwrap(),
    );
    assert!(query(&running, managed.clone(), false).await.can_archive);
    let receipt = wire(
        &running,
        managed,
        NavigationOperation::Replace,
        json!({"session":session,"expected_revision":"0","metadata":metadata}),
    )
    .await
    .unwrap();
    assert_eq!(receipt["revision"], "1");
    assert!(
        query(&running, origin.clone(), false)
            .await
            .entries
            .is_empty()
    );
    assert_eq!(
        query(&running, origin.clone(), true).await.entries[0].metadata,
        metadata
    );
    assert!(matches!(
        wire(
            &running,
            origin.clone(),
            NavigationOperation::Replace,
            json!({"session":session,"expected_revision":"1","metadata":SessionMetadata::default()})
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    assert_eq!(handle.header().await.unwrap(), header);
    assert_eq!(handle.inspect().await.unwrap(), before);
    assert!(matches!(
        wire(
            &running,
            origin,
            NavigationOperation::Replace,
            json!({"session":session,"expected_revision":"0","metadata":metadata})
        )
        .await,
        Err(ApiError::Invalid(_))
    ));
    running
        .workspace_registry()
        .unwrap()
        .delete_registration(&workspace.id)
        .await
        .unwrap();
    assert!(
        query(&running, CallOrigin::Local, true).await.entries[0]
            .workspace
            .is_none()
    );
    drop(handle);
    drop(service);
    assert!(running.shutdown().await.is_clean());
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let restored = query(&running, CallOrigin::Local, true).await;
    assert_eq!(restored.metadata_revision, "1");
    assert_eq!(restored.entries[0].metadata, metadata);
    assert!(restored.entries[0].workspace.is_none());
    let handle = running
        .session_service()
        .unwrap()
        .attach(&session)
        .await
        .unwrap();
    assert_eq!(handle.header().await.unwrap(), header);
    wire(
        &running,
        CallOrigin::Local,
        NavigationOperation::Replace,
        json!({"session":session,"expected_revision":"1","metadata":SessionMetadata::default()}),
    )
    .await
    .unwrap();
    assert_eq!(
        query(&running, CallOrigin::Local, false)
            .await
            .entries
            .len(),
        1
    );
    let database = rusqlite::Connection::open(fixture.paths.state().join("base.sqlite3")).unwrap();
    database.execute_batch("CREATE TRIGGER fixture_reject_navigation BEFORE INSERT ON rsi_storage_records WHEN NEW.domain = 'rsi.navigation' BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
    let request = json!({"session":session,"expected_revision":"2","metadata":metadata});
    assert!(matches!(
        wire(
            &running,
            CallOrigin::Local,
            NavigationOperation::Replace,
            request.clone()
        )
        .await,
        Err(ApiError::Unavailable)
    ));
    assert_eq!(
        query(&running, CallOrigin::Local, false)
            .await
            .metadata_revision,
        "2"
    );
    database
        .execute_batch("DROP TRIGGER fixture_reject_navigation;")
        .unwrap();
    let receipt = wire(
        &running,
        CallOrigin::Local,
        NavigationOperation::Replace,
        request,
    )
    .await
    .unwrap();
    assert_eq!(receipt["revision"], "3");
    assert_eq!(
        query(&running, CallOrigin::Local, true).await.entries[0].metadata,
        metadata
    );
    // A second window renames the archived row before the first window clicks Undo.
    let renamed = SessionMetadata {
        title: Some("Renamed elsewhere".into()),
        ..metadata.clone()
    };
    wire(
        &running,
        CallOrigin::Local,
        NavigationOperation::Replace,
        json!({
            "session":session,"expected_revision":"3","metadata":renamed
        }),
    )
    .await
    .unwrap();
    for _ in 0..2 {
        let restored = wire(
            &running,
            CallOrigin::Local,
            NavigationOperation::Archive,
            json!({"session":session,"action":"restore"}),
        )
        .await
        .unwrap();
        assert_eq!(restored["metadata"]["title"], "Renamed elsewhere");
        assert_eq!(restored["metadata"]["archived"], false);
        assert_eq!(restored["revision"], "5");
    }
    drop(database);
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_service_ownership_outlives_runtime_teardown_until_the_caller_releases_it() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let profile = host_profile(&fixture);
    let running = RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &profile)
        .await
        .unwrap();
    let service = running.session_service().unwrap();
    assert!(running.shutdown().await.is_clean());
    // Runtime retirement cannot destroy a capability owned by its caller.
    assert!(
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &profile)
            .await
            .is_err()
    );
    drop(service);
    let restarted = RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &profile)
        .await
        .unwrap();
    assert!(restarted.shutdown().await.is_clean());
    provider.abort();
}
