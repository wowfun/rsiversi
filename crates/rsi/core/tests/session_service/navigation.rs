use super::*;
use rsi_api_protocol::{ApiError, ApiOutput, ByteBudget, CallOrigin};
use rsi_navigation_api::{NavigationFilter, NavigationOperation, NavigationPage, SessionMetadata};
use serde_json::{Value, json};

async fn wire(
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
        .admit(&spec.id, origin)?
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
            json!({"filter":NavigationFilter { archived, ..Default::default() },"after":null}),
        )
        .await
        .unwrap(),
    )
    .unwrap()
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
            workspace_id: workspace.id.clone(),
            session_id: session.clone(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        })
        .await
        .unwrap();
    let metadata = SessionMetadata {
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
    let header = handle.header().await.unwrap();
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
    let receipt = wire(
        &running,
        origin.clone(),
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
        Err(ApiError::OutcomeUnknown)
    ));
    database
        .execute_batch("DROP TRIGGER fixture_reject_navigation;")
        .unwrap();
    assert!(matches!(
        wire(
            &running,
            CallOrigin::Local,
            NavigationOperation::Replace,
            request
        )
        .await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert!(matches!(
        wire(
            &running,
            CallOrigin::Local,
            NavigationOperation::Query,
            json!({"filter":NavigationFilter::default(),"after":null})
        )
        .await,
        Err(ApiError::OutcomeUnknown)
    ));
    drop(database);
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}
