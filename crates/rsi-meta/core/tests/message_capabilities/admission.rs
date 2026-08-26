use super::*;
use futures_util::{FutureExt as _, pin_mut, poll};
use rsi_meta::ExecutionLimits;

#[derive(Debug)]
struct WaitForCancellation {
    entered: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for WaitForCancellation {
    async fn serve(&self, _: InvocationContext, channel: ProviderChannel<'_>) -> Result<()> {
        self.entered.notify_one();
        channel.cancellation().cancelled().await;
        Ok(())
    }
}

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

#[tokio::test]
async fn invalid_holder_fails_before_waiting_for_a_full_local_queue() {
    let runtime = Runtime::new(RuntimeLimits {
        execution: ExecutionLimits {
            channel_capacity: 1,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let _sink = support::install_provider(
        &runtime,
        "immediate-sink",
        "sink",
        Arc::new(WaitForCancellation {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let _echo = support::install_provider(&runtime, "immediate-echo", "echo", Arc::new(Echo)).await;
    let (_caller, caller) =
        support::install_consumer(&runtime, "immediate-caller", vec!["sink"]).await;
    let (_other, other) =
        support::install_consumer(&runtime, "immediate-other", vec!["echo"]).await;
    let mut call = caller.capabilities[0].open().unwrap();
    entered.notified().await;
    call.send(Message::new(b"fills-slot".as_slice()))
        .await
        .unwrap();

    let result = call
        .send(Message::from_parts(
            Box::<[u8]>::default(),
            vec![other.capabilities[0].clone()],
        ))
        .now_or_never()
        .expect("invalid authority must fail before local queue admission");
    assert_eq!(result.unwrap_err(), MetaError::StaleCapability);

    call.cancel();
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
}

#[tokio::test]
async fn per_message_capability_limit_is_enforced_before_queue_admission() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_capabilities_per_message: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let _sink = support::install_provider(
        &runtime,
        "per-message-sink",
        "sink",
        Arc::new(WaitForCancellation {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let _echo =
        support::install_provider(&runtime, "per-message-echo", "echo", Arc::new(Echo)).await;
    let (_caller, capture) =
        support::install_consumer(&runtime, "per-message-caller", vec!["echo", "sink"]).await;
    let mut call = capture.capabilities[1].open().unwrap();
    entered.notified().await;
    let transferable = capture.capabilities[0].clone();
    assert_eq!(
        call.send(Message::from_parts(
            Box::<[u8]>::default(),
            vec![transferable.clone(), transferable],
        ))
        .await
        .unwrap_err(),
        MetaError::CapacityExhausted {
            resource: "capabilities per message",
        },
    );
    assert_eq!(
        runtime
            .resource_snapshot()
            .queued_capability_references
            .current,
        0,
    );
    call.cancel();
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
}

#[tokio::test]
async fn bytes_and_capability_references_wait_and_rollback_as_one_admission() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_capabilities_per_message: 1,
            maximum_queued_capability_references: 1,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_message_bytes: 4,
            maximum_buffered_message_bytes: 4,
            ..PayloadLimits::default()
        },
        execution: ExecutionLimits {
            channel_capacity: 2,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let _sink = support::install_provider(
        &runtime,
        "joint-sink",
        "sink",
        Arc::new(WaitForCancellation {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let _echo = support::install_provider(&runtime, "joint-echo", "echo", Arc::new(Echo)).await;
    let (_caller, capture) =
        support::install_consumer(&runtime, "joint-caller", vec!["echo", "sink"]).await;
    let mut call = capture.capabilities[1].open().unwrap();
    entered.notified().await;
    let transferable = capture.capabilities[0].clone();
    call.send(Message::from_parts(
        [1_u8].as_slice(),
        vec![transferable.clone()],
    ))
    .await
    .unwrap();
    let first = runtime.resource_snapshot();
    assert_eq!(first.buffered_message_bytes.current, 1);
    assert_eq!(first.queued_capability_references.current, 1);

    {
        let blocked = call.send(Message::from_parts(
            [2_u8; 3].as_slice(),
            vec![transferable],
        ));
        pin_mut!(blocked);
        assert!(poll!(&mut blocked).is_pending());
        let waiting = runtime.resource_snapshot();
        assert_eq!(waiting.buffered_message_bytes.current, 1);
        assert_eq!(waiting.queued_capability_references.current, 1);

        call.cancel();
        assert_eq!(blocked.as_mut().await.unwrap_err(), MetaError::Cancelled);
    }
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
    let terminal = runtime.resource_snapshot();
    assert_eq!(terminal.buffered_message_bytes.current, 0);
    assert_eq!(terminal.queued_capability_references.current, 0);
}

#[tokio::test]
async fn byte_only_pressure_does_not_report_a_capability_rejection() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_capabilities_per_message: 1,
            maximum_queued_capability_references: 2,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_message_bytes: 1,
            maximum_buffered_message_bytes: 1,
            ..PayloadLimits::default()
        },
        execution: ExecutionLimits {
            channel_capacity: 2,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let _sink = support::install_provider(
        &runtime,
        "byte-pressure-sink",
        "sink",
        Arc::new(WaitForCancellation {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let _echo =
        support::install_provider(&runtime, "byte-pressure-echo", "echo", Arc::new(Echo)).await;
    let (_caller, capture) =
        support::install_consumer(&runtime, "byte-pressure-caller", vec!["echo", "sink"]).await;
    let mut call = capture.capabilities[1].open().unwrap();
    entered.notified().await;
    call.send(Message::new([1_u8].as_slice())).await.unwrap();

    {
        let blocked = call.send(Message::from_parts(
            [2_u8].as_slice(),
            vec![capture.capabilities[0].clone()],
        ));
        pin_mut!(blocked);
        assert!(poll!(&mut blocked).is_pending());
        call.cancel();
        assert_eq!(blocked.await.unwrap_err(), MetaError::Cancelled);
    }
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
    let snapshot = runtime.resource_snapshot();
    assert_eq!(snapshot.buffered_message_bytes.rejected, 1);
    assert_eq!(snapshot.queued_capability_references.rejected, 0);
}

#[tokio::test]
async fn capability_only_pressure_does_not_report_a_byte_rejection() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_capabilities_per_message: 1,
            maximum_queued_capability_references: 1,
            ..TopologyLimits::default()
        },
        payloads: PayloadLimits {
            maximum_message_bytes: 2,
            maximum_buffered_message_bytes: 2,
            ..PayloadLimits::default()
        },
        execution: ExecutionLimits {
            channel_capacity: 2,
            ..ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let _sink = support::install_provider(
        &runtime,
        "cap-pressure-sink",
        "sink",
        Arc::new(WaitForCancellation {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let _echo =
        support::install_provider(&runtime, "cap-pressure-echo", "echo", Arc::new(Echo)).await;
    let (_caller, capture) =
        support::install_consumer(&runtime, "cap-pressure-caller", vec!["echo", "sink"]).await;
    let mut call = capture.capabilities[1].open().unwrap();
    entered.notified().await;
    call.send(Message::from_parts(
        Box::<[u8]>::default(),
        vec![capture.capabilities[0].clone()],
    ))
    .await
    .unwrap();

    {
        let blocked = call.send(Message::from_parts(
            [1_u8].as_slice(),
            vec![capture.capabilities[0].clone()],
        ));
        pin_mut!(blocked);
        assert!(poll!(&mut blocked).is_pending());
        call.cancel();
        assert_eq!(blocked.await.unwrap_err(), MetaError::Cancelled);
    }
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
    let snapshot = runtime.resource_snapshot();
    assert_eq!(snapshot.buffered_message_bytes.rejected, 0);
    assert_eq!(snapshot.queued_capability_references.rejected, 1);
}
