use super::*;
use rsi_process::{DuplexControl, DuplexInput, DuplexOutput, DuplexRead};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct Ports {
    output: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
    input: Mutex<Vec<u8>>,
    calls: AtomicUsize,
    reads: AtomicUsize,
    panic_read: std::sync::atomic::AtomicBool,
    terminations: AtomicUsize,
    settlements: AtomicUsize,
    settled: tokio::sync::Notify,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    consumed: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl DuplexInput for Ports {
    async fn write(&self, bytes: &[u8]) -> rsi_process::Result<usize> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        let count = if first {
            bytes.len().min(5)
        } else {
            bytes.len()
        };
        self.input
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..count]);
        if first {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(count)
    }
    async fn close(&self) -> rsi_process::Result<()> {
        Ok(())
    }
}
#[async_trait::async_trait]
impl DuplexOutput for Ports {
    async fn read(&self, _: usize) -> rsi_process::Result<DuplexRead> {
        assert!(
            !self.panic_read.swap(false, Ordering::SeqCst),
            "fixture pump panic"
        );
        let bytes = self.output.lock().await.recv().await.unwrap_or_default();
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.consumed.notify_one();
        Ok(DuplexRead {
            eof: bytes.is_empty(),
            bytes,
        })
    }
}
#[derive(Debug)]
struct Control(Arc<Ports>);
#[async_trait::async_trait]
impl DuplexControl for Control {
    fn pid(&self) -> u32 {
        1
    }
    fn stdin(&self) -> Arc<dyn DuplexInput> {
        self.0.clone()
    }
    fn stdout(&self) -> Arc<dyn DuplexOutput> {
        self.0.clone()
    }
    fn stderr(&self) -> Arc<dyn rsi_process::ProcessOutput> {
        panic!("not part of protocol fixture")
    }
    fn terminate(&self) {
        self.0.terminations.fetch_add(1, Ordering::SeqCst);
    }
    async fn wait(&self) -> rsi_process::Result<rsi_process::ProcessOutcome> {
        panic!("settlement is separate")
    }
    async fn wait_settlement(&self) -> rsi_process::Result<()> {
        self.0.settlements.fetch_add(1, Ordering::SeqCst);
        self.0.settled.notify_one();
        Ok(())
    }
}
fn process() -> (ManagedDuplexProcess, Arc<Ports>, mpsc::Sender<Vec<u8>>) {
    let (send, output) = mpsc::channel(64);
    let ports = Arc::new(Ports {
        output: tokio::sync::Mutex::new(output),
        input: Mutex::new(Vec::new()),
        calls: AtomicUsize::new(0),
        reads: AtomicUsize::new(0),
        panic_read: std::sync::atomic::AtomicBool::new(false),
        terminations: AtomicUsize::new(0),
        settlements: AtomicUsize::new(0),
        settled: tokio::sync::Notify::new(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        consumed: tokio::sync::Notify::new(),
    });
    (
        ManagedDuplexProcess::new(Arc::new(Control(ports.clone()))),
        ports,
        send,
    )
}
fn frame(value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap();
    [
        format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes(),
        &body,
    ]
    .concat()
}
fn pump() -> (Pump, mpsc::Sender<Vec<u8>>) {
    let (process, _, send) = process();
    let (_, commands) = mpsc::channel(1);
    let (terminal, _) = watch::channel(None);
    (
        Pump::new(
            process,
            Value::Null,
            commands,
            terminal,
            CancellationToken::new(),
        ),
        send,
    )
}
fn command(pump: &mut Pump, action: Action) {
    let (reply, _) = oneshot::channel();
    pump.command(Command { action, reply }).unwrap();
}

#[tokio::test]
async fn reads_do_not_recreate_a_write_with_an_accepted_prefix() {
    let (process, ports, send) = process();
    let mut wire = Wire::new(
        process,
        Value::Null,
        &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        CancellationToken::new(),
    );
    {
        let notify = wire.call(Action::Send {
            method: "notification".into(),
            params: json!({"text":"payload"}),
            request: false,
        });
        tokio::pin!(notify);
        assert!(futures_util::poll!(&mut notify).is_pending());
        ports.entered.notified().await;
        for _ in 0..32 {
            send.send(frame(
                &json!({"jsonrpc":"2.0","method":"progress","params":{}}),
            ))
            .await
            .unwrap();
        }
        while ports.reads.load(Ordering::SeqCst) < 32 {
            ports.consumed.notified().await;
        }
        assert_eq!(
            ports.calls.load(Ordering::SeqCst),
            1,
            "the pending write must not be replayed on each read"
        );
        ports.release.notify_one();
        notify.await.unwrap();
        assert_eq!(
            *ports.input.lock().unwrap(),
            frame(&json!({"jsonrpc":"2.0","method":"notification","params":{"text":"payload"}}))
        );
    }
    drop(send);
    wire.close().await.unwrap();
}

#[tokio::test]
async fn pre_read_bytes_and_requests_are_charged_at_query_admission() {
    let (mut pump, _send) = pump();
    let message = frame(
        &json!({"jsonrpc":"2.0","id":"idle","method":"window/workDoneProgress/create","params":{"token":"fixture"}}),
    );
    pump.append(&message[..10]).unwrap();
    command(
        &mut pump,
        Action::Begin(Instant::now() + Duration::from_secs(30)),
    );
    assert_eq!(pump.budget.as_ref().unwrap().bytes, 10);
    pump.append(&message[10..20]).unwrap();
    assert_eq!(pump.budget.as_ref().unwrap().bytes, 20);
    assert!(!pump.decode().unwrap()); // the incomplete frame survives query A
    command(&mut pump, Action::End);
    command(
        &mut pump,
        Action::Begin(Instant::now() + Duration::from_secs(30)),
    );
    assert_eq!(pump.budget.as_ref().unwrap().bytes, 20);
    pump.append(&message[20..]).unwrap();
    pump.decode().unwrap();
    assert_eq!(pump.budget.as_ref().unwrap().bytes, message.len());
    assert_eq!(pump.budget.as_ref().unwrap().requests, 1);
    pump.budget.as_mut().unwrap().bytes = 4 * MESSAGE_BYTES;
    assert_eq!(pump.append(b"x"), Err(Error::Limit));
}

#[tokio::test(start_paused = true)]
async fn reply_capacity_includes_current_write_and_deadline_survives_query_boundaries() {
    let (mut pump, _send) = pump();
    for id in 0..256 {
        pump.message(json!({"jsonrpc":"2.0","id":id,"method":"window/workDoneProgress/create","params":{"token":"fixture"}}))
            .unwrap();
    }
    let deadline = pump.replies.front().unwrap().deadline;
    pump.start_next().unwrap();
    assert_eq!(pump.outstanding, 256);
    assert_eq!(
        pump.message(json!({"jsonrpc":"2.0","id":257,"method":"window/workDoneProgress/create","params":{"token":"fixture"}})),
        Err(Error::Limit)
    );
    command(
        &mut pump,
        Action::Begin(Instant::now() + Duration::from_secs(30)),
    );
    command(&mut pump, Action::End);
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(matches!(pump.writing.as_ref().unwrap().kind, WriteKind::Server(at) if at == deadline));
    assert_eq!(pump.step(false).await, Err(Error::Deadline));
}

#[tokio::test]
async fn cancelled_or_expired_pump_does_not_dequeue_pending_work() {
    for cancelled in [false, true] {
        let (mut pump, _send) = pump();
        command(&mut pump, Action::Begin(Instant::now()));
        let (reply, _receive) = oneshot::channel();
        pump.pending = Some(Command {
            action: Action::Send {
                method: "fixture".into(),
                params: json!({}),
                request: false,
            },
            reply,
        });
        if cancelled {
            pump.stop.cancel();
        }
        assert_eq!(
            pump.step(true).await,
            Err(if cancelled {
                Error::Cancelled
            } else {
                Error::Deadline
            })
        );
        assert!(pump.pending.is_some());
        assert!(pump.writing.is_none());
    }
}

#[test]
fn supported_server_requests_reject_malformed_shapes() {
    for params in [
        json!({"items":[null]}),
        json!({"items":["section"]}),
        json!({"items":[{"scopeUri":7}]}),
        json!({"items":[{"section":false}]}),
    ] {
        let (mut pump, _send) = pump();
        assert_eq!(
            pump.message(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace/configuration","params":params})
            ),
            Err(Error::Protocol)
        );
    }
    for params in [
        json!({}),
        json!({"token":null}),
        json!({"token":true}),
        json!({"token":1.5}),
    ] {
        let (mut pump, _send) = pump();
        assert_eq!(pump.message(json!({"jsonrpc":"2.0","id":1,"method":"window/workDoneProgress/create","params":params})), Err(Error::Protocol));
    }
}

#[test]
fn drained_idle_requests_still_have_a_cumulative_budget() {
    let (mut pump, _send) = pump();
    for id in 0..256 {
        pump.message(json!({"jsonrpc":"2.0","id":id,"method":"window/workDoneProgress/create","params":{"token":id}})).unwrap();
        pump.replies.clear();
        pump.outstanding = 0; // each reply drained before the next request
    }
    assert_eq!(pump.message(json!({"jsonrpc":"2.0","id":257,"method":"window/workDoneProgress/create","params":{"token":257}})), Err(Error::Limit));
}

#[test]
fn drained_idle_notifications_still_have_a_byte_budget() {
    let (mut pump, _send) = pump();
    let message =
        frame(&json!({"jsonrpc":"2.0","method":"progress","params":{"text":"x".repeat(64000)}}));
    let mut count = 0;
    while (count + 1) * message.len() <= 4 * MESSAGE_BYTES {
        pump.append(&message).unwrap();
        pump.decode().unwrap();
        assert!(pump.buffer.is_empty());
        count += 1;
    }
    assert_eq!(pump.append(&message), Err(Error::Limit));
}

#[tokio::test]
async fn idle_pump_panic_publishes_failure_and_reaps_without_close() {
    let (process, ports, _send) = process();
    ports.panic_read.store(true, Ordering::SeqCst);
    let mut wire = Wire::new(
        process,
        Value::Null,
        &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        CancellationToken::new(),
    );
    tokio::time::timeout(Duration::from_secs(1), ports.settled.notified())
        .await
        .unwrap();
    assert!(wire.failed());
    assert_eq!(
        wire.call(Action::Begin(Instant::now() + Duration::from_secs(1)))
            .await,
        Err(Error::Unavailable)
    );
    assert_eq!(wire.close().await, Err(Error::Unavailable));
    assert!(ports.terminations.load(Ordering::SeqCst) > 0);
    assert_eq!(ports.settlements.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_queued_reply_does_not_suppress_safe_shutdown_grace() {
    let (process, ports, send) = process();
    ports.release.notify_one();
    send.send(frame(&json!({"jsonrpc":"2.0","id":1,"result":null})))
        .await
        .unwrap();
    let (_commands, receiver) = mpsc::channel(1);
    let (terminal, _) = watch::channel(None);
    let mut pump = Pump::new(
        process,
        Value::Null,
        receiver,
        terminal,
        CancellationToken::new(),
    );
    pump.replies.push_back(ServerReply {
        id: json!("expired"),
        body: ReplyBody::Unsupported,
        deadline: Instant::now() - Duration::from_secs(1),
    });
    pump.outstanding = 1;
    tokio::time::timeout(Duration::from_secs(1), pump.run())
        .await
        .unwrap()
        .unwrap();
    let written = String::from_utf8(ports.input.lock().unwrap().clone()).unwrap();
    assert!(
        written.contains("shutdown"),
        "safe retirement must attempt shutdown: {written}"
    );
    assert!(written.contains("exit"));
    assert_eq!(ports.settlements.load(Ordering::SeqCst), 1);
}
