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
        .admit(&spec.id, origin)?
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One public lifecycle verifies fast completion and per-principal durable reading positions"
)]
async fn attention_preserves_fast_completion_without_hydration_and_persists_exact_per_principal_reads()
 {
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
            Operation::MarkRead,
            json!({"host_epoch":page.host_epoch,"position":future})
        )
        .await,
        Err(ApiError::Invalid(_))
    ));
    assert!(matches!(
        wire(
            &running,
            CallOrigin::Local,
            Operation::MarkRead,
            json!({"host_epoch":HostEpoch::generate().unwrap(),"position":position})
        )
        .await,
        Err(ApiError::Invalid(_))
    ));
    wire(
        &running,
        CallOrigin::Local,
        Operation::MarkRead,
        json!({"host_epoch":page.host_epoch,"position":position}),
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
    assert_eq!(
        read(&running, origin.clone()).await.entries.len(),
        1,
        "Local reading does not clear another principal"
    );
    wire(
        &running,
        origin,
        Operation::MarkRead,
        json!({"host_epoch":page.host_epoch,"position":position}),
    )
    .await
    .unwrap();
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
    let handle = running
        .session_service()
        .unwrap()
        .attach(&id)
        .await
        .unwrap();
    assert!(
        read(&running, CallOrigin::Local).await.entries.is_empty(),
        "Saved reading position survives restart"
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
    assert_eq!(read(&running, origin).await.entries.len(), 1);
    drop(handle);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}
