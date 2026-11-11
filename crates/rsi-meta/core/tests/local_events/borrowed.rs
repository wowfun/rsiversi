use super::*;
use futures_util::FutureExt;

struct CountedInput {
    copies: Arc<AtomicUsize>,
    bytes: Vec<u8>,
}

impl Clone for CountedInput {
    fn clone(&self) -> Self {
        self.copies.fetch_add(1, Ordering::Relaxed);
        Self {
            copies: self.copies.clone(),
            bytes: self.bytes.clone(),
        }
    }
}

struct Broadcast;
impl LocalEvent for Broadcast {
    const KEY: &'static str = "test.borrowed-broadcast";
    type Value = CountedInput;
    type Error = Infallible;
    type Mode = Parallel;
}

struct Query;
impl LocalEvent for Query {
    const KEY: &'static str = "test.borrowed-query";
    type Value = CountedInput;
    type Error = Infallible;
    type Mode = Serial;
}

#[derive(Debug)]
struct ReadInput(Arc<AtomicUsize>);

impl ReadInput {
    async fn read(&self, input: &CountedInput) {
        let bytes = input.bytes.as_slice();
        tokio::task::yield_now().await;
        assert_eq!(bytes.len(), 1024 * 1024);
        assert!(bytes.iter().all(|byte| *byte == 7));
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[async_trait]
impl ParallelEventHandler<Broadcast> for ReadInput {
    async fn handle(&self, input: &CountedInput) -> std::result::Result<(), Infallible> {
        self.read(input).await;
        Ok(())
    }
}

#[async_trait]
impl SerialEventHandler<Query> for ReadInput {
    async fn handle(
        &self,
        input: &CountedInput,
    ) -> std::result::Result<Option<CountedInput>, Infallible> {
        self.read(input).await;
        Ok(None)
    }
}

#[tokio::test]
async fn asynchronous_events_do_not_clone_the_dispatch_input() {
    let runtime = Runtime::default();
    let capture = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            resolved(Arc::new(CaptureContext(capture.clone()))),
            Value::Null,
        )
        .await
        .unwrap();
    let context = capture.lock().unwrap().clone().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        context
            .on_parallel::<Broadcast, _>(
                Arc::new(ReadInput(calls.clone())),
                LocalEventOptions::default(),
            )
            .unwrap();
        context
            .on_serial::<Query, _>(
                Arc::new(ReadInput(calls.clone())),
                LocalEventOptions::default(),
            )
            .unwrap();
    }
    let copies = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .dispatch_local::<Broadcast>(CountedInput {
            copies: copies.clone(),
            bytes: vec![7; 1024 * 1024],
        })
        .unwrap()
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .dispatch_local::<Query>(CountedInput {
                copies: copies.clone(),
                bytes: vec![7; 1024 * 1024]
            })
            .unwrap()
            .await
            .unwrap()
            .is_none()
    );
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    assert_eq!(
        copies.load(Ordering::Relaxed),
        0,
        "broadcast and query callbacks can borrow the dispatch-owned payload"
    );
}

// Intentionally not Clone: the dispatch is the only owner of this allocation.
struct OwnedInput {
    bytes: Box<[u8]>,
    drops: Arc<AtomicUsize>,
}

impl Drop for OwnedInput {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

struct BorrowedParallel;
impl LocalEvent for BorrowedParallel {
    const KEY: &'static str = "test.borrowed-parallel-lifetime";
    type Value = OwnedInput;
    type Error = Infallible;
    type Mode = Parallel;
}

struct BorrowedSerial;
impl LocalEvent for BorrowedSerial {
    const KEY: &'static str = "test.borrowed-serial-lifetime";
    type Value = OwnedInput;
    type Error = Infallible;
    type Mode = Serial;
}

#[derive(Debug)]
struct BlockedReader {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    drops: Arc<AtomicUsize>,
}

impl BlockedReader {
    async fn read(&self, input: &OwnedInput) {
        let bytes = &input.bytes;
        self.entered.notify_one();
        self.release.notified().await;
        assert_eq!(bytes.as_ref(), &[7, 8, 9]);
    }
}

impl Drop for BlockedReader {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[async_trait]
impl ParallelEventHandler<BorrowedParallel> for BlockedReader {
    async fn handle(&self, input: &OwnedInput) -> std::result::Result<(), Infallible> {
        self.read(input).await;
        Ok(())
    }
}

#[async_trait]
impl SerialEventHandler<BorrowedSerial> for BlockedReader {
    async fn handle(
        &self,
        input: &OwnedInput,
    ) -> std::result::Result<Option<OwnedInput>, Infallible> {
        self.read(input).await;
        Ok(None)
    }
}

#[tokio::test]
async fn suspended_callbacks_retain_non_clone_input_and_disposed_listeners() {
    for parallel in [false, true] {
        for cancelled in [false, true] {
            let runtime = Runtime::default();
            let capture = Arc::new(Mutex::new(None));
            let owner = runtime
                .root()
                .apply(
                    resolved(Arc::new(CaptureContext(capture.clone()))),
                    Value::Null,
                )
                .await
                .unwrap();
            let context = capture.lock().unwrap().clone().unwrap();
            let input_drops = Arc::new(AtomicUsize::new(0));
            let handler_drops = Arc::new(AtomicUsize::new(0));
            let entered = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let handler = Arc::new(BlockedReader {
                entered: entered.clone(),
                release: release.clone(),
                drops: handler_drops.clone(),
            });
            let listener = if parallel {
                context
                    .on_parallel::<BorrowedParallel, _>(handler, LocalEventOptions::default())
                    .unwrap()
            } else {
                context
                    .on_serial::<BorrowedSerial, _>(handler, LocalEventOptions::default())
                    .unwrap()
            };
            let input = OwnedInput {
                bytes: Box::new([7, 8, 9]),
                drops: input_drops.clone(),
            };
            let mut dispatch = if parallel {
                runtime
                    .root()
                    .dispatch_local::<BorrowedParallel>(input)
                    .unwrap()
                    .map(|result| result.unwrap())
                    .boxed()
            } else {
                runtime
                    .root()
                    .dispatch_local::<BorrowedSerial>(input)
                    .unwrap()
                    .map(|result| assert!(result.unwrap().is_none()))
                    .boxed()
            };
            tokio::select! {
                () = &mut dispatch => panic!("callback did not suspend"),
                () = entered.notified() => {},
            }
            assert!(listener.dispose().await.is_clean());
            assert!(owner.dispose().await.is_clean());
            assert_eq!(input_drops.load(Ordering::Relaxed), 0);
            assert_eq!(handler_drops.load(Ordering::Relaxed), 0);
            if !cancelled {
                release.notify_one();
                dispatch.as_mut().await;
            }
            drop(dispatch);
            assert_eq!(input_drops.load(Ordering::Relaxed), 1);
            assert_eq!(handler_drops.load(Ordering::Relaxed), 1);
            assert!(runtime.shutdown().await.is_complete());
        }
    }
}
