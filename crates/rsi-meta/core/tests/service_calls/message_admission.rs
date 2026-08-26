use super::*;
use futures_util::{pin_mut, poll};

#[derive(Debug)]
struct NeverReceiveRequests {
    entered: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for NeverReceiveRequests {
    async fn serve(&self, _: InvocationContext, channel: ProviderChannel<'_>) -> Result<()> {
        self.entered.notify_one();
        channel.cancellation().cancelled().await;
        Ok(())
    }
}

#[tokio::test]
async fn budget_waiters_do_not_occupy_channel_positions() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_message_bytes: 4,
            maximum_buffered_message_bytes: 4,
            ..rsi_meta::PayloadLimits::default()
        },
        execution: rsi_meta::ExecutionLimits {
            channel_capacity: 3,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(NeverReceiveRequests {
            entered: Arc::clone(&entered),
        }),
    )
    .await;
    let mut call = service.open().unwrap();
    entered.notified().await;

    call.send(Message::new([1_u8; 4])).await.unwrap();
    {
        let second = call.send(Message::new([2_u8; 4]));
        let third = call.send(Message::new([3_u8; 4]));
        pin_mut!(second, third);
        assert!(poll!(&mut second).is_pending());
        assert!(poll!(&mut third).is_pending());
        assert_eq!(
            runtime.resource_snapshot().buffered_message_bytes.current,
            4
        );

        tokio::time::timeout(
            Duration::from_millis(100),
            call.send(Message::new(Box::<[u8]>::default())),
        )
        .await
        .expect("budget waiters occupied the two otherwise-free channel positions")
        .unwrap();
    }
    call.cancel();
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);
}

#[tokio::test]
async fn fitting_message_bypasses_budget_blocked_sender_without_channel_head_of_line() {
    let runtime = byte_runtime();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(NeverReceiveRequests {
            entered: Arc::clone(&entered),
        }),
    )
    .await;

    let mut occupying_call = service.open().unwrap();
    entered.notified().await;
    occupying_call.send(Message::new([1_u8; 3])).await.unwrap();

    let mut contested_call = service.open().unwrap();
    entered.notified().await;
    {
        let blocked = contested_call.send(Message::new([2_u8; 3]));
        pin_mut!(blocked);
        assert!(poll!(&mut blocked).is_pending());

        tokio::time::timeout(
            Duration::from_millis(100),
            contested_call.send(Message::new([3_u8; 1])),
        )
        .await
        .expect("a budget-blocked sender occupied the channel ahead of a fitting message")
        .unwrap();
    }
    occupying_call.cancel();
    contested_call.cancel();
    assert_eq!(
        occupying_call.recv().await.unwrap_err(),
        MetaError::Cancelled
    );
    assert_eq!(
        contested_call.recv().await.unwrap_err(),
        MetaError::Cancelled
    );
}

#[tokio::test]
async fn fitting_sender_enters_a_saturated_same_channel_candidate_window() {
    let runtime = byte_runtime();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(NeverReceiveRequests {
            entered: Arc::clone(&entered),
        }),
    )
    .await;

    let mut occupying_call = service.open().unwrap();
    entered.notified().await;
    occupying_call.send(Message::new([1_u8; 3])).await.unwrap();
    let mut contested_call = service.open().unwrap();
    entered.notified().await;

    {
        let mut blocked = Vec::with_capacity(65);
        for byte in 0_u8..65 {
            blocked.push(Box::pin(contested_call.send(Message::new([byte; 4]))));
        }
        for waiter in &mut blocked {
            assert!(poll!(waiter.as_mut()).is_pending());
        }
        assert_eq!(
            runtime.resource_snapshot().pending_message_sends.current,
            65
        );

        tokio::time::timeout(
            Duration::from_millis(100),
            contested_call.send(Message::new([255_u8])),
        )
        .await
        .expect("a full nonfitting candidate window hid a newly fitting sender")
        .unwrap();
    }

    occupying_call.cancel();
    contested_call.cancel();
    assert_eq!(
        occupying_call.recv().await.unwrap_err(),
        MetaError::Cancelled
    );
    assert_eq!(
        contested_call.recv().await.unwrap_err(),
        MetaError::Cancelled
    );
}

#[tokio::test]
async fn pending_sender_limit_is_fail_fast_observable_and_reusable() {
    let runtime = Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_message_bytes: 4,
            maximum_buffered_message_bytes: 4,
            ..rsi_meta::PayloadLimits::default()
        },
        execution: rsi_meta::ExecutionLimits {
            channel_capacity: 1,
            maximum_pending_message_sends: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let entered = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(NeverReceiveRequests {
            entered: Arc::clone(&entered),
        }),
    )
    .await;

    let mut occupying_call = service.open().unwrap();
    entered.notified().await;
    occupying_call.send(Message::new([1_u8; 4])).await.unwrap();
    let mut waiting_call = service.open().unwrap();
    entered.notified().await;

    {
        let first_waiter = waiting_call.send(Message::new([2_u8]));
        pin_mut!(first_waiter);
        assert!(poll!(&mut first_waiter).is_pending());
        assert_eq!(runtime.resource_snapshot().pending_message_sends.current, 1);
        assert_eq!(
            waiting_call.send(Message::new([3_u8])).await.unwrap_err(),
            MetaError::CapacityExhausted {
                resource: "pending message sends",
            },
        );
        let saturated = runtime.resource_snapshot().pending_message_sends;
        assert_eq!(saturated.current, 1);
        assert_eq!(saturated.rejected, 1);
    }
    assert_eq!(runtime.resource_snapshot().pending_message_sends.current, 0);

    {
        let reused = waiting_call.send(Message::new([4_u8]));
        pin_mut!(reused);
        assert!(poll!(&mut reused).is_pending());
        assert_eq!(runtime.resource_snapshot().pending_message_sends.current, 1);
    }
    assert_eq!(runtime.resource_snapshot().pending_message_sends.current, 0);

    occupying_call.cancel();
    waiting_call.cancel();
    assert_eq!(
        occupying_call.recv().await.unwrap_err(),
        MetaError::Cancelled
    );
    assert_eq!(waiting_call.recv().await.unwrap_err(), MetaError::Cancelled);
}

#[derive(Debug)]
struct MixedBufferedResponses {
    next_call: AtomicUsize,
    first_sent: Arc<Notify>,
    second_entered: Arc<Notify>,
    second_sent: Arc<Notify>,
    third_entered: Arc<Notify>,
    third_sent: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for MixedBufferedResponses {
    async fn serve(&self, _: InvocationContext, channel: ProviderChannel<'_>) -> Result<()> {
        match self.next_call.fetch_add(1, Ordering::AcqRel) {
            0 => {
                channel.send(Message::new(vec![1; 2])).await?;
                self.first_sent.notify_one();
            }
            1 => {
                self.second_entered.notify_one();
                channel.send(Message::new(vec![2; 4])).await?;
                self.second_sent.notify_one();
            }
            2 => {
                self.third_entered.notify_one();
                channel.send(Message::new(vec![3])).await?;
                self.third_sent.notify_one();
            }
            call => panic!("unexpected mixed-weight call {call}"),
        }
        Ok(())
    }
}

#[tokio::test]
async fn fitting_frame_bypasses_an_older_frame_that_cannot_fit() {
    let runtime = byte_runtime();
    let first_sent = Arc::new(Notify::new());
    let second_entered = Arc::new(Notify::new());
    let second_sent = Arc::new(Notify::new());
    let third_entered = Arc::new(Notify::new());
    let third_sent = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(MixedBufferedResponses {
            next_call: AtomicUsize::new(0),
            first_sent: Arc::clone(&first_sent),
            second_entered: Arc::clone(&second_entered),
            second_sent: Arc::clone(&second_sent),
            third_entered: Arc::clone(&third_entered),
            third_sent: Arc::clone(&third_sent),
        }),
    )
    .await;

    let mut first = service.open().unwrap();
    first.finish();
    tokio::time::timeout(Duration::from_secs(1), first_sent.notified())
        .await
        .expect("the first mixed-weight frame was not buffered");
    assert_eq!(
        runtime.resource_snapshot().buffered_message_bytes.current,
        2
    );

    let mut second = service.open().unwrap();
    second.finish();
    tokio::time::timeout(Duration::from_secs(1), second_entered.notified())
        .await
        .expect("the large mixed-weight frame did not enter admission");
    let mut third = service.open().unwrap();
    third.finish();
    tokio::time::timeout(Duration::from_secs(1), third_entered.notified())
        .await
        .expect("the fitting mixed-weight frame did not enter admission");

    tokio::time::timeout(Duration::from_millis(100), third_sent.notified())
        .await
        .expect("an older large frame head-of-line blocked a fitting independent frame");
    assert_eq!(third.recv().await.unwrap().unwrap().as_bytes(), &[3]);
    assert!(third.recv().await.unwrap().is_none());
    assert!(second_sent.notified().now_or_never().is_none());

    assert_eq!(first.recv().await.unwrap().unwrap().as_bytes(), &[1; 2]);
    tokio::time::timeout(Duration::from_secs(1), second_sent.notified())
        .await
        .expect("the older large frame did not receive released capacity");
    assert_eq!(second.recv().await.unwrap().unwrap().as_bytes(), &[2; 4]);
    assert!(first.recv().await.unwrap().is_none());
    assert!(second.recv().await.unwrap().is_none());
    assert_eq!(
        runtime.resource_snapshot().buffered_message_bytes.current,
        0
    );
}

#[derive(Debug)]
struct StarvationBoundResponses {
    next_call: AtomicUsize,
    first_sent: Arc<Notify>,
    large_entered: Arc<Notify>,
    large_sent: Arc<Notify>,
}

#[async_trait]
impl ServiceEndpoint for StarvationBoundResponses {
    async fn serve(&self, _: InvocationContext, channel: ProviderChannel<'_>) -> Result<()> {
        match self.next_call.fetch_add(1, Ordering::AcqRel) {
            0 => {
                channel.send(Message::new(vec![1; 3])).await?;
                self.first_sent.notify_one();
            }
            1 => {
                self.large_entered.notify_one();
                channel.send(Message::new(vec![2; 4])).await?;
                self.large_sent.notify_one();
            }
            _ => channel.send(Message::new(vec![3])).await?,
        }
        Ok(())
    }
}

#[tokio::test]
async fn older_large_frame_reserves_capacity_after_sixty_four_bypasses() {
    let runtime = byte_runtime();
    let first_sent = Arc::new(Notify::new());
    let large_entered = Arc::new(Notify::new());
    let large_sent = Arc::new(Notify::new());
    let service = captured_service(
        &runtime,
        Arc::new(StarvationBoundResponses {
            next_call: AtomicUsize::new(0),
            first_sent: Arc::clone(&first_sent),
            large_entered: Arc::clone(&large_entered),
            large_sent: Arc::clone(&large_sent),
        }),
    )
    .await;

    let mut first = service.open().unwrap();
    first.finish();
    tokio::time::timeout(Duration::from_secs(1), first_sent.notified())
        .await
        .expect("the starvation fixture did not buffer its first frame");
    let mut large = service.open().unwrap();
    large.finish();
    tokio::time::timeout(Duration::from_secs(1), large_entered.notified())
        .await
        .expect("the starvation fixture did not queue its large frame");

    for _ in 0..64 {
        let mut small = service.open().unwrap();
        small.finish();
        let frame = tokio::time::timeout(Duration::from_secs(1), small.recv())
            .await
            .expect("a fitting frame did not receive its bounded bypass")
            .unwrap()
            .unwrap();
        assert_eq!(frame.as_bytes(), &[3]);
        assert!(small.recv().await.unwrap().is_none());
    }

    let mut blocked_small = service.open().unwrap();
    blocked_small.finish();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), blocked_small.recv())
            .await
            .is_err(),
        "a younger frame bypassed an older frame after its bypass bound"
    );
    assert_eq!(
        runtime.resource_snapshot().buffered_message_bytes.current,
        3
    );

    assert_eq!(first.recv().await.unwrap().unwrap().as_bytes(), &[1; 3]);
    tokio::time::timeout(Duration::from_secs(1), large_sent.notified())
        .await
        .expect("the protected large frame did not receive released capacity");
    assert_eq!(large.recv().await.unwrap().unwrap().as_bytes(), &[2; 4]);
    let final_small = tokio::time::timeout(Duration::from_secs(1), blocked_small.recv())
        .await
        .expect("the younger frame did not resume after the protected frame")
        .unwrap()
        .unwrap();
    assert_eq!(final_small.as_bytes(), &[3]);
    assert!(first.recv().await.unwrap().is_none());
    assert!(large.recv().await.unwrap().is_none());
    assert!(blocked_small.recv().await.unwrap().is_none());
    assert_eq!(
        runtime.resource_snapshot().buffered_message_bytes.current,
        0
    );
}

fn byte_runtime() -> Runtime {
    Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_message_bytes: 4,
            maximum_buffered_message_bytes: 4,
            ..rsi_meta::PayloadLimits::default()
        },
        execution: rsi_meta::ExecutionLimits {
            channel_capacity: 1,
            ..rsi_meta::ExecutionLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap()
}
