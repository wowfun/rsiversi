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

async fn capability_fixture(runtime: &Runtime) -> (rsi_meta::FiberHandle, support::Capture) {
    let _provider =
        support::install_provider(runtime, "identity-provider", "echo", Arc::new(Echo)).await;
    support::install_consumer(runtime, "identity-consumer", vec!["echo"]).await
}

#[tokio::test]
async fn foreign_runtime_possession_is_rejected_before_message_admission() {
    let first_runtime = Runtime::default();
    let (_first_consumer, first) = capability_fixture(&first_runtime).await;
    let foreign = first.capabilities[0].clone();

    let second_runtime = Runtime::default();
    let (_second_consumer, second) = capability_fixture(&second_runtime).await;
    let mut call = second.capabilities[0].open().unwrap();
    assert_eq!(
        call.send(Message::from_parts(b"foreign".as_slice(), vec![foreign],))
            .await
            .unwrap_err(),
        MetaError::CapabilityFromDifferentRuntime,
    );
    let resources = second_runtime.resource_snapshot();
    assert_eq!(resources.buffered_message_bytes.current, 0);
    assert_eq!(resources.queued_capability_references.current, 0);
    call.finish();
    assert!(call.recv().await.unwrap().is_none());
}

#[tokio::test]
async fn safe_clones_share_one_entry_and_released_capacity_is_reusable() {
    let runtime = Runtime::default();
    let (_consumer, mut capture) = capability_fixture(&runtime).await;
    let capability = capture.capabilities.pop().unwrap();
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);

    let first = capability.clone();
    let second = capability.clone();
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);
    drop(capability);
    drop(first);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);
    drop(second);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 0);

    let replacement = capture.context.service("echo").unwrap();
    assert_eq!(replacement.key().as_str(), "echo");
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);
}

#[tokio::test]
async fn capability_debug_contains_only_logical_binding_facts() {
    let runtime = Runtime::default();
    let (_consumer, capture) = capability_fixture(&runtime).await;
    let diagnostic = format!("{:?}", capture.capabilities[0]);

    assert!(diagnostic.contains("echo"), "{diagnostic}");
    for private_field in [
        "holder", "entry_id", "token", "issuer", "slot", "epoch", "kind", "rights",
    ] {
        assert!(
            !diagnostic.contains(private_field),
            "private field {private_field:?} leaked through {diagnostic:?}",
        );
    }
}

#[tokio::test]
async fn generation_retirement_revokes_the_unique_entry_even_with_live_safe_handles() {
    let runtime = Runtime::default();
    let (consumer, capture) = capability_fixture(&runtime).await;
    let capability = capture.capabilities[0].clone();
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);

    assert!(consumer.dispose().await.is_clean());
    assert_eq!(capability.open().unwrap_err(), MetaError::StaleCapability);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);
    drop(capture);
    drop(capability);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 0);
}

#[tokio::test]
async fn detached_capability_preserves_charge_and_exact_holder_without_retaining_setup() {
    let runtime = Runtime::default();
    let (consumer, mut capture) = capability_fixture(&runtime).await;
    let capability = capture.capabilities.pop().unwrap();
    let owner = capability.provider();
    let detached = capability.detach();

    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);
    let upgraded = detached.upgrade().unwrap();
    assert_eq!(upgraded.provider(), owner);
    assert_eq!(
        upgraded
            .invoke(Message::new(b"detached".as_slice()))
            .await
            .unwrap()
            .as_bytes(),
        b"detached",
    );
    drop(upgraded);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);

    assert!(consumer.dispose().await.is_clean());
    assert_eq!(detached.upgrade().unwrap_err(), MetaError::StaleCapability);
    drop(capture);
    drop(detached);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 0);
}

#[tokio::test]
async fn stale_handles_gate_entry_capacity_until_the_last_reference_drops() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_capability_entries: 2,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let _provider =
        support::install_provider(&runtime, "capacity-provider", "echo", Arc::new(Echo)).await;
    let (consumer, mut first) =
        support::install_consumer(&runtime, "capacity-first", vec!["echo"]).await;
    let (_live_consumer, live) =
        support::install_consumer(&runtime, "capacity-live", vec!["echo"]).await;
    let stale = first.capabilities.pop().unwrap();

    assert!(consumer.dispose().await.is_clean());
    assert_eq!(stale.open().unwrap_err(), MetaError::StaleCapability);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 2);
    assert_eq!(
        live.context.service("echo").unwrap_err(),
        MetaError::CapacityExhausted {
            resource: "capability entries",
        },
    );

    drop(first);
    drop(stale);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 1);
    let replacement = live.context.service("echo").unwrap();
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 2);
    drop(replacement);
    drop(live);
    assert_eq!(runtime.resource_snapshot().capability_entries.current, 0);
}
