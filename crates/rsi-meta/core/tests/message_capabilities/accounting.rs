use super::*;

#[derive(Debug)]
struct Echo;

#[async_trait]
impl ServiceEndpoint for Echo {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        if let Some(request) = channel.recv().await {
            channel.send(request).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct StagedRoundTrip {
    read_request: Arc<Notify>,
    request_held: Arc<Notify>,
    send_response: Arc<Notify>,
    response_sent: Arc<Notify>,
    finish: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for StagedRoundTrip {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        self.read_request.notified().await;
        let request = channel.recv().await.expect("staged request");
        self.request_held.notify_one();
        self.send_response.notified().await;
        channel.send(request).await?;
        self.response_sent.notify_one();
        self.finish.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn queue_accounting_follows_request_and_response_ownership() {
    let runtime = Runtime::default();
    let _echo =
        support::install_provider(&runtime, "accounting-echo", "echo", Arc::new(Echo)).await;
    let read_request = Arc::new(Notify::new());
    let request_held = Arc::new(Notify::new());
    let send_response = Arc::new(Notify::new());
    let response_sent = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let _queue = support::install_provider(
        &runtime,
        "accounting-queue",
        "queue",
        Arc::new(StagedRoundTrip {
            read_request: Arc::clone(&read_request),
            request_held: Arc::clone(&request_held),
            send_response: Arc::clone(&send_response),
            response_sent: Arc::clone(&response_sent),
            finish: Arc::clone(&finish),
        }),
    )
    .await;
    let (_consumer, capture) =
        support::install_consumer(&runtime, "accounting-consumer", vec!["echo", "queue"]).await;
    let transferable = capture.capabilities[0].clone();
    let mut call = capture.capabilities[1].open().unwrap();
    call.send(Message::from_parts(b"held".as_slice(), vec![transferable]))
        .await
        .unwrap();
    call.finish();

    let queued = runtime.resource_snapshot();
    assert_eq!(queued.buffered_message_bytes.current, 4);
    assert_eq!(queued.queued_capability_references.current, 1);

    read_request.notify_one();
    request_held.notified().await;
    let held_by_provider = runtime.resource_snapshot();
    assert_eq!(held_by_provider.buffered_message_bytes.current, 0);
    assert_eq!(held_by_provider.queued_capability_references.current, 0);

    send_response.notify_one();
    response_sent.notified().await;
    let queued_response = runtime.resource_snapshot();
    assert_eq!(queued_response.buffered_message_bytes.current, 4);
    assert_eq!(queued_response.queued_capability_references.current, 1);

    let response = call.recv().await.unwrap().expect("staged response");
    assert_eq!(response.as_bytes(), b"held");
    assert_eq!(response.capabilities().len(), 1);
    let held_by_caller = runtime.resource_snapshot();
    assert_eq!(held_by_caller.buffered_message_bytes.current, 0);
    assert_eq!(held_by_caller.queued_capability_references.current, 0);

    finish.notify_one();
    assert!(call.recv().await.unwrap().is_none());
}

#[derive(Debug)]
struct ReturnWithoutReading {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for ReturnWithoutReading {
    async fn serve(&self, _: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn terminal_precedes_lease_release_only_after_unread_requests_are_dropped() {
    let runtime = Runtime::default();
    let _echo = support::install_provider(&runtime, "unread-echo", "echo", Arc::new(Echo)).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let _sink = support::install_provider(
        &runtime,
        "unread-sink",
        "sink",
        Arc::new(ReturnWithoutReading {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }),
    )
    .await;
    let (_consumer, capture) =
        support::install_consumer(&runtime, "unread-consumer", vec!["echo", "sink"]).await;
    let mut call = capture.capabilities[1].open().unwrap();
    entered.notified().await;
    call.send(Message::from_parts(
        b"unread".as_slice(),
        vec![capture.capabilities[0].clone()],
    ))
    .await
    .unwrap();
    call.finish();
    assert_eq!(
        runtime
            .resource_snapshot()
            .queued_capability_references
            .current,
        1,
    );

    release.notify_one();
    assert!(call.recv().await.unwrap().is_none());
    let terminal = runtime.resource_snapshot();
    assert_eq!(terminal.buffered_message_bytes.current, 0);
    assert_eq!(terminal.queued_capability_references.current, 0);
}
