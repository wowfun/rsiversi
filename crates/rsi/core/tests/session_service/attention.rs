use super::*;
use rsi_api_protocol::{ApiError, ApiOutput, ByteBudget, CallOrigin};
use rsi_navigation_api::attention::{Operation, Page, Status};
use serde_json::{Value, json};

async fn wire(
    running: &RunningRsi,
    origin: CallOrigin,
    op: Operation,
    input: Value,
) -> Result<Value, ApiError> {
    let spec = op.spec();
    let input = ByteBudget::default().encode(&input, spec.maximum_request_bytes)?;
    let ApiOutput::Reply(reply) = running
        .api_dispatch()
        .unwrap()
        .admit(&spec.call(), origin)?
        .invoke(input)
        .await?
    else {
        panic!("finite attention reply")
    };
    Ok(serde_json::from_slice(reply.json.as_bytes()).unwrap())
}
async fn read(running: &RunningRsi, origin: CallOrigin) -> Page {
    let page: Page = serde_json::from_value(
        wire(running, origin, Operation::Read, json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    page.validate().unwrap();
    page
}

async fn follow(running: &RunningRsi, origin: CallOrigin) -> rsi_api_protocol::ApiStream {
    let spec = Operation::Follow.spec();
    let input = ByteBudget::default()
        .encode(&(), spec.maximum_request_bytes)
        .unwrap();
    let ApiOutput::Stream(source) = running
        .api_dispatch()
        .unwrap()
        .admit(&spec.call(), origin)
        .unwrap()
        .invoke(input)
        .await
        .unwrap()
    else {
        panic!("attention observation")
    };
    source
}
async fn next_page(source: &mut rsi_api_protocol::ApiStream) -> Page {
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), source.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(frame.binary.is_none());
    let page: Page = serde_json::from_slice(frame.json.as_bytes()).unwrap();
    page.validate().unwrap();
    page
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One public lifecycle covers shared reminders, exact cuts, and restart"
)]
async fn attention_shares_live_completions_and_manual_reminders_and_resets_on_restart() {
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let profile = host_profile(&fixture);
    let boot = || RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &profile);
    let running = boot().await.unwrap();
    let service = running.session_service().unwrap();
    let id = SessionId::new("attention-fast").unwrap();
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let handle = service
        .create(CreateSession {
            fork: None,
            workspace_id: workspace.id,
            session_id: id.clone(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    assert!(
        read(&running, CallOrigin::Local).await.entries.is_empty(),
        "Unpublished drafts are not durable activity"
    );
    // No attention poll occurs between admission and completion.
    run_message_to_terminal(&handle, "attention-first").await;
    let page = read(&running, CallOrigin::Local).await;
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].status, Status::Unread);
    let position = page.entries[0].position.clone();
    let before = handle.inspect().await.unwrap();
    let mut future = position.clone();
    future.sequence = (future.sequence.parse::<u64>().unwrap() + 1).to_string();
    assert!(matches!(
        wire(
            &running,
            CallOrigin::Local,
            Operation::Mark,
            json!({"action":"read","host_epoch":page.host_epoch,"position":future})
        )
        .await,
        Err(ApiError::Invalid(_))
    ));
    assert!(matches!(
        wire(
            &running,
            CallOrigin::Local,
            Operation::Mark,
            json!({"action":"read","host_epoch":HostEpoch::generate().unwrap(),"position":position})
        )
        .await,
        Err(ApiError::Invalid(_))
    ));
    wire(
        &running,
        CallOrigin::Local,
        Operation::Mark,
        json!({"action":"read","host_epoch":page.host_epoch,"position":position}),
    )
    .await
    .unwrap();
    assert!(read(&running, CallOrigin::Local).await.entries.is_empty());
    let device = running
        .device_administration()
        .unwrap()
        .register("attention-device")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    let mut peer = follow(&running, origin.clone()).await;
    let mut local = follow(&running, CallOrigin::Local).await;
    assert!(
        next_page(&mut peer).await.entries.is_empty(),
        "Another device sees the Local read"
    );
    assert!(next_page(&mut local).await.entries.is_empty());
    wire(
        &running,
        origin.clone(),
        Operation::Mark,
        json!({"action":"unread","host_epoch":page.host_epoch,"position":position}),
    )
    .await
    .unwrap();
    wire(
        &running,
        CallOrigin::Local,
        Operation::Mark,
        json!({"action":"acknowledge","host_epoch":page.host_epoch,"position":position}),
    )
    .await
    .unwrap();
    let shared = next_page(&mut local).await;
    assert_eq!(next_page(&mut peer).await, shared);
    drop((peer, local));
    assert!(shared.entries[0].reading.manual);
    assert!(!shared.entries[0].reading.unseen);
    assert_eq!(
        handle.inspect().await.unwrap(),
        before,
        "Navigation does not mutate Agent history"
    );
    drop(handle);
    drop(service);
    assert!(running.shutdown().await.is_clean());
    let running = boot().await.unwrap();
    assert!(
        read(&running, CallOrigin::Local).await.entries.is_empty(),
        "Startup does not discover cold history"
    );
    let cold = json!({"conversation":position.conversation,"epoch":"0","sequence":"0"});
    let fresh_epoch = read(&running, CallOrigin::Local).await.host_epoch;
    wire(
        &running,
        CallOrigin::Local,
        Operation::Mark,
        json!({"action":"unread","host_epoch":fresh_epoch,"position":cold}),
    )
    .await
    .unwrap();
    assert!(
        read(&running, CallOrigin::Local).await.entries[0]
            .reading
            .manual,
        "A cold sidebar row can be marked without attachment"
    );
    wire(
        &running,
        CallOrigin::Local,
        Operation::Mark,
        json!({"action":"read","host_epoch":fresh_epoch,"position":cold}),
    )
    .await
    .unwrap();
    let handle = running
        .session_service()
        .unwrap()
        .attach(&id)
        .await
        .unwrap();
    assert!(
        read(&running, CallOrigin::Local).await.entries.is_empty(),
        "Reopening old history does not synthesize a new completion"
    );
    run_message_to_terminal(&handle, "attention-second").await;
    let advanced = read(&running, CallOrigin::Local).await;
    assert_eq!(advanced.entries.len(), 1);
    assert!(
        advanced.entries[0]
            .position
            .sequence
            .parse::<u64>()
            .unwrap()
            > position.sequence.parse::<u64>().unwrap()
    );
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    wire(
        &running,
        origin.clone(),
        Operation::Mark,
        json!({"action":"read","host_epoch":fresh_epoch,"position":position}),
    )
    .await
    .unwrap();
    let stale = read(&running, origin).await;
    assert_eq!(
        stale.entries.len(),
        1,
        "Reading an older observed cut preserves the latest completion"
    );
    assert!(stale.entries[0].reading.unseen);
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}
