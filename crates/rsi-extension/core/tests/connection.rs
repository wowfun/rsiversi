use async_trait::async_trait;
use rsi_extension::{Connection, Error, Handler, RejectRequests};
use rsi_process::{
    DuplexControl, DuplexInput, DuplexOutput, DuplexRead, ManagedDuplexProcess, ProcessError,
    ProcessOutcome, ProcessOutput, ProcessRead,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf},
    sync::{Mutex, Notify},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Input(Mutex<WriteHalf<DuplexStream>>, CancellationToken);
#[async_trait]
impl DuplexInput for Input {
    async fn write(&self, bytes: &[u8]) -> rsi_process::Result<usize> {
        tokio::select! { biased;
            () = self.1.cancelled() => Err(ProcessError::Io("closed".into())),
            result = async { self.0.lock().await.write(bytes).await } => result.map_err(|e| ProcessError::Io(e.to_string())),
        }
    }
    async fn close(&self) -> rsi_process::Result<()> {
        self.0
            .lock()
            .await
            .shutdown()
            .await
            .map_err(|e| ProcessError::Io(e.to_string()))
    }
}
#[derive(Debug)]
struct Output(Mutex<ReadHalf<DuplexStream>>, CancellationToken);
#[async_trait]
impl DuplexOutput for Output {
    async fn read(&self, maximum: usize) -> rsi_process::Result<DuplexRead> {
        let mut bytes = vec![0; maximum];
        let count = tokio::select! { biased;
            () = self.1.cancelled() => return Err(ProcessError::Io("closed".into())),
            result = async { self.0.lock().await.read(&mut bytes).await } => result.map_err(|e| ProcessError::Io(e.to_string()))?,
        };
        bytes.truncate(count);
        Ok(DuplexRead {
            bytes,
            eof: count == 0,
        })
    }
}
#[derive(Debug)]
struct EmptyOutput;
impl ProcessOutput for EmptyOutput {
    fn read_from(&self, _: u64) -> rsi_process::Result<ProcessRead> {
        Err(ProcessError::Io("no stderr".into()))
    }
    fn peek_tail(&self, _: usize) -> rsi_process::Result<ProcessRead> {
        self.read_from(0)
    }
}
#[derive(Debug)]
struct Control {
    input: Arc<Input>,
    output: Arc<Output>,
    stop: CancellationToken,
    settlement: Option<CancellationToken>,
}
#[async_trait]
impl DuplexControl for Control {
    fn pid(&self) -> u32 {
        1
    }
    fn stdin(&self) -> Arc<dyn DuplexInput> {
        self.input.clone()
    }
    fn stdout(&self) -> Arc<dyn DuplexOutput> {
        self.output.clone()
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        Arc::new(EmptyOutput)
    }
    fn terminate(&self) {
        self.stop.cancel();
        let input = self.input.clone();
        tokio::spawn(async move {
            let _ = input.close().await;
        });
    }
    async fn wait(&self) -> rsi_process::Result<ProcessOutcome> {
        self.stop.cancelled().await;
        Ok(ProcessOutcome {
            exit_code: Some(0),
            signal: None,
        })
    }
    async fn wait_settlement(&self) -> rsi_process::Result<()> {
        self.stop.cancelled().await;
        if let Some(settlement) = &self.settlement {
            settlement.cancelled().await;
        }
        Ok(())
    }
}
fn process(stream: DuplexStream) -> ManagedDuplexProcess {
    process_with_settlement(stream, None)
}
fn process_with_settlement(
    stream: DuplexStream,
    settlement: Option<CancellationToken>,
) -> ManagedDuplexProcess {
    let (read, write) = tokio::io::split(stream);
    let stop = CancellationToken::new();
    ManagedDuplexProcess::new(Arc::new(Control {
        input: Arc::new(Input(Mutex::new(write), stop.clone())),
        output: Arc::new(Output(Mutex::new(read), stop.clone())),
        stop,
        settlement,
    }))
}
#[tokio::test]
async fn dropping_connection_retains_launch_files_until_process_settlement() {
    #[derive(Debug)]
    struct Files(Arc<Notify>);
    impl Drop for Files {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }
    let (stream, _peer) = tokio::io::duplex(73);
    let settlement = CancellationToken::new();
    let connection = Connection::new(
        process_with_settlement(stream, Some(settlement.clone())),
        "lease".into(),
        Arc::new(RejectRequests),
    )
    .unwrap();
    let dropped = Arc::new(Notify::new());
    let files = Arc::new(Files(dropped.clone()));
    let weak = Arc::downgrade(&files);
    connection.retain_until_settled(files);
    drop(connection);
    tokio::task::yield_now().await;
    assert!(
        weak.upgrade().is_some(),
        "termination is not native settlement"
    );
    settlement.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), dropped.notified())
        .await
        .unwrap();
    assert!(weak.upgrade().is_none());
}
#[derive(Debug, Default)]
struct Echo {
    review_started: Notify,
    answer: Notify,
    cancelled: Notify,
}
#[async_trait]
impl Handler for Echo {
    async fn call(
        &self,
        method: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> rsi_extension::Result<Value> {
        if method == "review" {
            self.review_started.notify_one();
            tokio::select! {
                () = self.answer.notified() => {},
                () = cancellation.cancelled() => { self.cancelled.notify_one(); return Err(Error::Cancelled); }
            }
        }
        Ok(payload)
    }
}
fn pair(a: Arc<dyn Handler>, b: Arc<dyn Handler>) -> (Arc<Connection>, Arc<Connection>) {
    let (left, right) = tokio::io::duplex(73);
    (
        Arc::new(Connection::new(process(left), "test".into(), a).unwrap()),
        Arc::new(Connection::new(process(right), "test".into(), b).unwrap()),
    )
}
#[derive(Debug)]
struct Stalled;
#[async_trait]
impl Handler for Stalled {
    async fn call(&self, _: &str, _: Value, _: CancellationToken) -> rsi_extension::Result<Value> {
        std::future::pending().await
    }
}
#[tokio::test(start_paused = true)]
async fn stalled_peer_watchdog_terminates_pending_work_without_replay() {
    let (left, right) = pair(Arc::new(RejectRequests), Arc::new(Stalled));
    left.start_watchdog();
    let started = tokio::time::Instant::now();
    assert_eq!(
        left.request("work", Value::Null, CancellationToken::new())
            .await,
        Err(Error::Disconnected)
    );
    assert!(started.elapsed() <= std::time::Duration::from_secs(9));
    assert_eq!(
        left.disconnect_cause(),
        Some(("heartbeat deadline", Error::Disconnected))
    );
    left.shutdown().await.unwrap();
    right.shutdown().await.unwrap();
}
#[tokio::test]
async fn method_registration_withdraws_pending_and_future_authority() {
    let review = Arc::new(Echo::default());
    let (left, right) = pair(Arc::new(RejectRequests), Arc::new(RejectRequests));
    let registered = right.register_handler("review", review.clone()).unwrap();
    assert!(right.register_handler("review", review.clone()).is_err());
    let active = left.clone();
    let waiting = tokio::spawn(async move {
        active
            .request("review", Value::Null, CancellationToken::new())
            .await
    });
    review.review_started.notified().await;
    drop(registered);
    assert!(waiting.await.unwrap().is_err());
    assert!(
        left.request("review", Value::Null, CancellationToken::new())
            .await
            .is_err()
    );
    let _replacement = right.register_handler("review", review.clone()).unwrap();
    review.answer.notify_one();
    assert_eq!(
        left.request("review", json!(42), CancellationToken::new())
            .await
            .unwrap(),
        json!(42)
    );
    left.shutdown().await.unwrap();
    right.shutdown().await.unwrap();
}
#[tokio::test]
async fn review_wait_allows_command_reverse_callback_and_null_result() {
    let review = Arc::new(Echo::default());
    let (left, right) = pair(Arc::new(Echo::default()), review.clone());
    let active = left.clone();
    let waiting = tokio::spawn(async move {
        active
            .request("review", Value::Null, CancellationToken::new())
            .await
    });
    review.review_started.notified().await;
    assert_eq!(
        left.request("command", json!("plan off"), CancellationToken::new())
            .await
            .unwrap(),
        json!("plan off")
    );
    assert_eq!(
        right
            .request("receipt", json!({"seq":42}), CancellationToken::new())
            .await
            .unwrap(),
        json!({"seq":42})
    );
    review.answer.notify_one();
    assert_eq!(waiting.await.unwrap().unwrap(), Value::Null);
    left.shutdown().await.unwrap();
    right.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancelled_call_does_not_close_a_healthy_peer() {
    let review = Arc::new(Echo::default());
    let (left, right) = pair(Arc::new(RejectRequests), review.clone());
    let token = CancellationToken::new();
    let request_token = token.clone();
    let active = left.clone();
    let waiting =
        tokio::spawn(async move { active.request("review", Value::Null, request_token).await });
    review.review_started.notified().await;
    token.cancel();
    assert_eq!(waiting.await.unwrap(), Err(Error::Cancelled));
    assert_eq!(
        left.request("command", json!(true), CancellationToken::new())
            .await
            .unwrap(),
        json!(true)
    );
    left.shutdown().await.unwrap();
    right.shutdown().await.unwrap();
}
#[tokio::test]
async fn mismatched_generation_fails_both_pending_directions() {
    let (a, b) = tokio::io::duplex(128);
    let left = Connection::new(process(a), "old".into(), Arc::new(RejectRequests)).unwrap();
    let right = Connection::new(process(b), "new".into(), Arc::new(RejectRequests)).unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            left.request("test", Value::Null, CancellationToken::new())
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(
        right
            .request("test", Value::Null, CancellationToken::new())
            .await
            .is_err()
    );
    left.shutdown().await.unwrap();
    right.shutdown().await.unwrap();
}
