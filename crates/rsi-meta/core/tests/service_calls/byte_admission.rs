use super::*;

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
                channel.send(ServiceFrame::new(vec![1; 2])).await?;
                self.first_sent.notify_one();
            }
            1 => {
                self.second_entered.notify_one();
                channel.send(ServiceFrame::new(vec![2; 4])).await?;
                self.second_sent.notify_one();
            }
            2 => {
                self.third_entered.notify_one();
                channel.send(ServiceFrame::new(vec![3])).await?;
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
        runtime.resource_snapshot().buffered_service_bytes.current,
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
        runtime.resource_snapshot().buffered_service_bytes.current,
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
                channel.send(ServiceFrame::new(vec![1; 3])).await?;
                self.first_sent.notify_one();
            }
            1 => {
                self.large_entered.notify_one();
                channel.send(ServiceFrame::new(vec![2; 4])).await?;
                self.large_sent.notify_one();
            }
            _ => channel.send(ServiceFrame::new(vec![3])).await?,
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
        runtime.resource_snapshot().buffered_service_bytes.current,
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
        runtime.resource_snapshot().buffered_service_bytes.current,
        0
    );
}

fn byte_runtime() -> Runtime {
    Runtime::new(RuntimeLimits {
        payloads: rsi_meta::PayloadLimits {
            maximum_frame_bytes: 4,
            maximum_buffered_service_bytes: 4,
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
