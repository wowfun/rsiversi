use super::*;

#[derive(Debug)]
struct CallerRecordingEcho {
    callers: Arc<Mutex<Vec<FiberId>>>,
}

#[async_trait]
impl ServiceEndpoint for CallerRecordingEcho {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        self.callers
            .lock()
            .expect("caller log poisoned")
            .push(invocation.immediate_caller());
        let request = channel.recv().await.expect("echo request");
        channel.send(request).await
    }
}

#[derive(Debug)]
struct ForwardingRelay;

#[async_trait]
impl ServiceEndpoint for ForwardingRelay {
    async fn serve(&self, _: InvocationContext, mut channel: ProviderChannel<'_>) -> Result<()> {
        let request = channel.recv().await.expect("relay request");
        let (_, mut capabilities) = request.into_parts();
        let capability = capabilities.pop().expect("one transferred capability");
        assert!(capabilities.is_empty());

        let nested = capability
            .invoke(Message::new(b"from-provider".as_slice()))
            .await?;
        assert_eq!(nested.as_bytes(), b"from-provider");
        channel
            .send(Message::from_parts(
                b"returned".as_slice(),
                vec![capability],
            ))
            .await
    }
}

#[tokio::test]
async fn request_and_response_transfer_rebind_the_exact_call_context() {
    let runtime = Runtime::default();
    let callers = Arc::new(Mutex::new(Vec::new()));
    let _echo_provider = support::install_provider(
        &runtime,
        "cap-echo-provider",
        "echo",
        Arc::new(CallerRecordingEcho {
            callers: Arc::clone(&callers),
        }),
    )
    .await;
    let relay_provider = support::install_provider(
        &runtime,
        "cap-relay-provider",
        "relay",
        Arc::new(ForwardingRelay),
    )
    .await;
    let (consumer, capture) =
        support::install_consumer(&runtime, "cap-transfer-consumer", vec!["echo", "relay"]).await;
    let echo = capture.capabilities[0].clone();
    let relay = capture.capabilities[1].clone();
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 2);
    let response = relay
        .invoke(Message::from_parts(b"transfer".as_slice(), vec![echo]))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"returned");
    assert_eq!(response.capabilities().len(), 1);
    assert_eq!(response.capabilities()[0].key().as_str(), "echo");
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 2);

    let final_response = response.capabilities()[0]
        .invoke(Message::new(b"from-caller".as_slice()))
        .await
        .unwrap();
    assert_eq!(final_response.as_bytes(), b"from-caller");
    assert_eq!(
        *callers.lock().expect("caller log poisoned"),
        vec![relay_provider.id(), consumer.id()],
        "request capabilities must bind to the provider Context and returned capabilities to the caller Context",
    );
}
