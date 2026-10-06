use super::*;
use rsi_process::DuplexRead;
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

#[tokio::test]
async fn containment_survives_panic_payload_destructors() {
    struct HostilePayload(Arc<AtomicUsize>);
    impl Drop for HostilePayload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::panic::panic_any(Self(self.0.clone()));
        }
    }
    for synchronous in [true, false] {
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = HostilePayload(drops.clone());
        let result: Result<(), ProgramError> = if synchronous {
            contain_sync(|| std::panic::panic_any(payload))
        } else {
            contain(async move { std::panic::panic_any(payload) }).await
        };
        assert_eq!(result, Err(ProgramError::OutcomeUnknown));
        assert_eq!(
            drops.load(Ordering::SeqCst),
            2,
            "last hostile payload must be forgotten"
        );
    }
}

#[derive(Debug, Default)]
struct Port {
    incoming: Mutex<VecDeque<u8>>,
    written: Mutex<Vec<u8>>,
    read_bytes: AtomicUsize,
    hold_open: bool,
    capture_failure: Option<CaptureFailure>,
}
#[derive(Clone, Copy, Debug)]
enum CaptureFailure {
    Io,
    Panic,
}
#[async_trait]
impl DuplexInput for Port {
    async fn write(&self, bytes: &[u8]) -> rsi_process::Result<usize> {
        let count = bytes.len().min(7);
        self.written
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    async fn close(&self) -> rsi_process::Result<()> {
        Ok(())
    }
}
#[async_trait]
impl DuplexOutput for Port {
    async fn read(&self, maximum: usize) -> rsi_process::Result<DuplexRead> {
        let bytes: Vec<_> = {
            let mut incoming = self.incoming.lock().unwrap();
            let count = maximum.min(13).min(incoming.len());
            incoming.drain(..count).collect()
        };
        if bytes.is_empty() && self.hold_open {
            std::future::pending::<()>().await;
        }
        self.read_bytes.fetch_add(bytes.len(), Ordering::SeqCst);
        Ok(DuplexRead {
            eof: bytes.is_empty(),
            bytes,
        })
    }
}
fn frames(values: impl IntoIterator<Item = Value>, hold_open: bool) -> Arc<Port> {
    let port = Arc::new(Port {
        hold_open,
        ..Port::default()
    });
    for value in values {
        let bytes = serde_json::to_vec(&value).unwrap();
        port.incoming
            .lock()
            .unwrap()
            .extend(u32::try_from(bytes.len()).unwrap().to_be_bytes());
        port.incoming.lock().unwrap().extend(bytes);
    }
    port
}
#[derive(Debug, Default)]
struct Rpc {
    entered: AtomicUsize,
    settled: AtomicUsize,
    notified: tokio::sync::Notify,
    blocked: bool,
    uncertain: bool,
}
#[async_trait]
impl ProgramRpc for Rpc {
    fn definitions(&self) -> Value {
        json!([])
    }
    async fn call(
        &self,
        _: String,
        args: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, ProgramError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.notified.notify_one();
        if self.blocked {
            cancellation.cancelled().await;
        }
        self.settled.fetch_add(1, Ordering::SeqCst);
        if self.uncertain {
            Err(ProgramError::OutcomeUnknown)
        } else {
            Ok(args)
        }
    }
}
fn request(rpc: Arc<dyn ProgramRpc>) -> Request {
    Request {
        spec: Mutex::new(None),
        script: "return 42".into(),
        definitions: json!([]),
        rpc,
        start: CancellationToken::new(),
        cancel: CancellationToken::new(),
        cancelled_at_settlement: AtomicBool::new(false),
        outcome: watch::channel(None).1,
        completion: Mutex::new(None),
    }
}

#[tokio::test]
async fn multibyte_script_error_frames_stay_within_the_diagnostic_byte_bound() {
    let request = request(Arc::new(Rpc::default()));
    for message in ["x".repeat(16384), "界".repeat(4096)] {
        let port = frames([json!({"type":"error","message":message})], false);
        let ProgramError::Failed(diagnostic) =
            exchange(port.clone(), port, &request).await.unwrap_err()
        else {
            panic!("script error");
        };
        assert!(diagnostic.len() <= 4096);
        assert!(message.starts_with(&diagnostic));
        assert!(diagnostic.len() >= 4094);
    }
}

#[derive(Debug)]
struct UnusedProvider;
#[async_trait]
impl DuplexProcess for UnusedProvider {
    async fn spawn(&self, _: DuplexProcessSpec) -> rsi_process::Result<ManagedDuplexProcess> {
        panic!("retained spawn must not be replaced with another invocation");
    }
}

#[derive(Debug, Default)]
struct ControlledProcess {
    terminated: CancellationToken,
    settled: CancellationToken,
    port: Arc<Port>,
    panic_termination: AtomicBool,
    fail_settlement: bool,
}
impl rsi_process::ProcessOutput for Port {
    fn read_from(&self, _: u64) -> rsi_process::Result<rsi_process::ProcessRead> {
        match self.capture_failure {
            Some(CaptureFailure::Io) => {
                return Err(rsi_process::ProcessError::Io("stderr read".into()));
            }
            Some(CaptureFailure::Panic) => panic!("stderr capture panic"),
            None => {}
        }
        Ok(rsi_process::ProcessRead {
            bytes: vec![],
            oldest_offset: 0,
            next_offset: 0,
            lossy: false,
            full_output: None,
        })
    }
    fn peek_tail(&self, _: usize) -> rsi_process::Result<rsi_process::ProcessRead> {
        self.read_from(0)
    }
}
#[async_trait]
impl rsi_process::DuplexControl for ControlledProcess {
    fn pid(&self) -> u32 {
        42
    }
    fn stdin(&self) -> Arc<dyn DuplexInput> {
        self.port.clone()
    }
    fn stdout(&self) -> Arc<dyn DuplexOutput> {
        self.port.clone()
    }
    fn stderr(&self) -> Arc<dyn rsi_process::ProcessOutput> {
        self.port.clone()
    }
    fn terminate(&self) {
        self.terminated.cancel();
        assert!(
            !self.panic_termination.swap(false, Ordering::SeqCst),
            "termination panic"
        );
    }
    async fn wait(&self) -> rsi_process::Result<rsi_process::ProcessOutcome> {
        self.settled.cancelled().await;
        Ok(rsi_process::ProcessOutcome {
            exit_code: Some(0),
            signal: None,
        })
    }
    async fn wait_settlement(&self) -> rsi_process::Result<()> {
        self.settled.cancelled().await;
        if self.fail_settlement {
            Err(rsi_process::ProcessError::Io("settlement failed".into()))
        } else {
            Ok(())
        }
    }
}

fn owned_execution(
    tasks: &tokio_util::task::TaskTracker,
) -> (
    ExecutionGuard,
    watch::Receiver<Option<Result<Value, ProgramError>>>,
) {
    let mut req = request(Arc::new(Rpc::default()));
    let (sender, receiver) = watch::channel(None);
    req.outcome = receiver.clone();
    let request = Arc::new(req);
    let control = Arc::new(Control {
        request: request.clone(),
        stderr: Mutex::new(None),
    });
    let execution = Execution {
        request: request.clone(),
        control,
        provider: Arc::new(UnusedProvider),
        spawning: None,
        process: None,
        calls: Calls::new(),
        rpc_cancel: request.cancel.child_token(),
        result: None,
        completion: Completion(Some(sender)),
    };
    (ExecutionGuard::new(execution, tasks.clone()), receiver)
}

#[tokio::test]
async fn settlement_preserves_execution_failure_and_still_escalates_uncertainty() {
    for panic_termination in [false, true] {
        let tasks = tokio_util::task::TaskTracker::new();
        let (mut guard, receiver) = owned_execution(&tasks);
        let mut state = guard.state.take().unwrap();
        let process = Arc::new(ControlledProcess {
            panic_termination: AtomicBool::new(panic_termination),
            fail_settlement: true,
            ..ControlledProcess::default()
        });
        process.settled.cancel();
        state.process = Some(ManagedDuplexProcess::new(process.clone()));
        let original = ProgramError::Failed("script root cause".into());
        state.result = Some(Err(original.clone()));
        state.settle().await;
        assert_eq!(
            wait_result(receiver).await,
            Err(if panic_termination {
                ProgramError::OutcomeUnknown
            } else {
                original
            })
        );
        assert!(process.terminated.is_cancelled());
    }
}

#[tokio::test]
async fn settlement_distinguishes_diagnostic_errors_from_process_boundary_failure() {
    for (capture_failure, panic_termination, fail_settlement, initial_unknown, expected) in [
        (Some(CaptureFailure::Io), false, false, false, Ok(json!(42))),
        (
            Some(CaptureFailure::Panic),
            false,
            false,
            false,
            Err(ProgramError::OutcomeUnknown),
        ),
        (None, true, false, false, Err(ProgramError::OutcomeUnknown)),
        (
            None,
            false,
            true,
            false,
            Err("process I/O failed: settlement failed".into()),
        ),
        (
            Some(CaptureFailure::Io),
            false,
            true,
            true,
            Err(ProgramError::OutcomeUnknown),
        ),
    ] {
        let tasks = tokio_util::task::TaskTracker::new();
        let (mut guard, receiver) = owned_execution(&tasks);
        let mut state = guard.state.take().unwrap();
        let process = Arc::new(ControlledProcess {
            port: Arc::new(Port {
                capture_failure,
                ..Port::default()
            }),
            panic_termination: AtomicBool::new(panic_termination),
            fail_settlement,
            ..ControlledProcess::default()
        });
        process.settled.cancel();
        state.process = Some(ManagedDuplexProcess::new(process.clone()));
        state.result = Some(if initial_unknown {
            Err(ProgramError::OutcomeUnknown)
        } else {
            Ok(json!(42))
        });
        state.settle().await;
        let result = wait_result(receiver).await;
        assert_eq!(result, expected);
        assert!(process.terminated.is_cancelled());
    }
}

#[tokio::test]
async fn abort_during_settlement_keeps_provisional_success_unknown_until_actual_cleanup() {
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut guard, receiver) = owned_execution(&tasks);
    let process = Arc::new(ControlledProcess::default());
    guard.state.as_mut().unwrap().process = Some(ManagedDuplexProcess::new(process.clone()));
    guard.state.as_mut().unwrap().result = Some(Ok(json!(42)));
    // Begin at the fixture's parsed-result boundary and exercise the production
    // settlement and guard destructor, with process cleanup still pending.
    let task = tasks.spawn(async move {
        guard.state.as_mut().unwrap().settle().await;
        guard.state.take();
    });
    process.terminated.cancelled().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tasks.close();
    assert!(wait_result(receiver.clone()).now_or_never().is_none());
    assert!(tasks.wait().now_or_never().is_none());
    process.settled.cancel();
    assert_eq!(
        wait_result(receiver).await,
        Err(ProgramError::OutcomeUnknown)
    );
    tasks.wait().await;
}

#[tokio::test]
async fn abort_before_first_poll_preserves_completion_for_all_observers() {
    let tasks = tokio_util::task::TaskTracker::new();
    let (owner, receiver) = owned_execution(&tasks);
    let task = tasks.spawn(owner.run());
    tasks.close();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let (first, second) = tokio::join!(wait_result(receiver.clone()), wait_result(receiver));
    assert_eq!(first, Err(ProgramError::OutcomeUnknown));
    assert_eq!(second, first);
    tasks.wait().await;
}
#[tokio::test]
async fn abort_mid_drain_preserves_the_original_handler_until_it_finishes() {
    struct HandlerDrop(Arc<AtomicUsize>);
    impl Drop for HandlerDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let tasks = tokio_util::task::TaskTracker::new();
    let (mut guard, receiver) = owned_execution(&tasks);
    let entered = CancellationToken::new();
    let release = CancellationToken::new();
    let polls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let process = Arc::new(ControlledProcess::default());
    process.settled.cancel();
    let state = guard.state.as_mut().unwrap();
    state.process = Some(ManagedDuplexProcess::new(process));
    state.result = Some(Ok(json!(42)));
    let handler = {
        let entered = entered.clone();
        let release = release.clone();
        let polls = polls.clone();
        let drops = drops.clone();
        async move {
            let _drop = HandlerDrop(drops);
            polls.fetch_add(1, Ordering::SeqCst);
            entered.cancel();
            release.cancelled().await;
            (1, Ok(Value::Null))
        }
        .boxed()
    };
    state.calls.push(handler);
    let task = tasks.spawn(async move {
        guard.state.as_mut().unwrap().settle().await;
        guard.state.take();
    });
    entered.cancelled().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tasks.close();
    assert!(wait_result(receiver.clone()).now_or_never().is_none());
    assert!(tasks.wait().now_or_never().is_none());
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "the pending handler is still owned"
    );
    release.cancel();
    assert_eq!(
        wait_result(receiver).await,
        Err(ProgramError::OutcomeUnknown)
    );
    tasks.wait().await;
    assert_eq!(
        polls.load(Ordering::SeqCst),
        1,
        "the handler was not reconstructed"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn abort_retains_the_pending_spawn_and_rpc_until_actual_settlement() {
    for pending_spawn in [false, true] {
        let tasks = tokio_util::task::TaskTracker::new();
        let (mut owner, receiver) = owned_execution(&tasks);
        let process = Arc::new(ControlledProcess::default());
        let release = CancellationToken::new();
        let spawn_polls = Arc::new(AtomicUsize::new(0));
        if pending_spawn {
            let control = process.clone();
            let release = release.clone();
            let polls = spawn_polls.clone();
            let spawning = async move {
                polls.fetch_add(1, Ordering::SeqCst);
                release.cancelled().await;
                Ok(ManagedDuplexProcess::new(control))
            }
            .boxed();
            owner.state.as_mut().unwrap().spawning = Some(spawning);
            // Poll once without consuming the retained single-use future.
            assert!(
                owner
                    .state
                    .as_mut()
                    .unwrap()
                    .spawning
                    .as_mut()
                    .unwrap()
                    .now_or_never()
                    .is_none()
            );
        } else {
            owner.state.as_mut().unwrap().process =
                Some(ManagedDuplexProcess::new(process.clone()));
            let release = release.clone();
            owner.state.as_mut().unwrap().calls.push(
                async move {
                    release.cancelled().await;
                    (1, Err("released RPC".into()))
                }
                .boxed(),
            );
        }
        let task = tasks.spawn(owner.run());
        tasks.close();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        if pending_spawn {
            assert!(wait_result(receiver.clone()).now_or_never().is_none());
            release.cancel();
        }
        process.terminated.cancelled().await;
        assert!(wait_result(receiver.clone()).now_or_never().is_none());
        assert!(
            tasks.wait().now_or_never().is_none(),
            "cleanup retains its tracker token"
        );
        release.cancel();
        process.settled.cancel();
        assert_eq!(
            wait_result(receiver).await,
            Err(ProgramError::OutcomeUnknown)
        );
        tasks.wait().await;
        assert_eq!(
            spawn_polls.load(Ordering::SeqCst),
            usize::from(pending_spawn)
        );
    }
}
#[tokio::test]
async fn oversized_prefix_is_rejected_before_body_read_and_oversized_write_emits_nothing() {
    for size in [0, MAXIMUM_FRAME + 1] {
        let port = Arc::new(Port::default());
        port.incoming
            .lock()
            .unwrap()
            .extend(u32::try_from(size).unwrap().to_be_bytes());
        port.incoming.lock().unwrap().extend(b"body");
        assert!(
            read_frame(port.clone())
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("1 MiB")
        );
        assert_eq!(port.read_bytes.load(Ordering::SeqCst), 4);
    }
    let port = Port::default();
    let maximum = Value::String("x".repeat(MAXIMUM_FRAME - 2));
    write_frame(&port, &maximum, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(port.written.lock().unwrap().len(), MAXIMUM_FRAME + 4);
    port.written.lock().unwrap().clear();
    assert!(
        write_frame(
            &port,
            &Value::String("x".repeat(MAXIMUM_FRAME - 1)),
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(port.written.lock().unwrap().is_empty());
}
#[tokio::test]
async fn fragmented_duplex_preserves_reply_and_exact_result_bound() {
    for extra in [0, 1] {
        let rpc = Arc::new(Rpc::default());
        let mut req = request(rpc.clone());
        req.script = "return {text: '中\\n\\\"'};".into();
        req.definitions = json!([{"name":"echo", "schema":{"text":"中\n\""}}]);
        let value =
            Value::String("中".repeat((MAXIMUM_RESULT - 2) / 3) + "xx" + &"x".repeat(extra));
        assert_eq!(
            serde_json::to_vec(&value).unwrap().len(),
            MAXIMUM_RESULT + extra
        );
        let port = frames(
            [
                json!({"type":"call","id":1,"method":"echo","arguments":{"text":"中\n\""}}),
                json!({"type":"result","value":value}),
            ],
            false,
        );
        let result = exchange(port.clone(), port.clone(), &req).await;
        if extra == 0 {
            assert_eq!(result.unwrap(), value);
        } else {
            assert!(result.unwrap_err().to_string().contains("256 KiB"));
        }
        assert_eq!(rpc.settled.load(Ordering::SeqCst), 1);
        let bytes = port.written.lock().unwrap();
        let mut offset = 0;
        let mut output = Vec::new();
        while offset < bytes.len() {
            let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            output.push(serde_json::from_slice::<Value>(&bytes[offset..offset + length]).unwrap());
            offset += length;
        }
        assert_eq!(output.len(), 2);
        assert_eq!(
            output[0],
            json!({
                "type":"start", "script":req.script, "definitions":req.definitions,
                "maximum_calls":MAXIMUM_PROGRAM_OUTSTANDING_CALLS,
            })
        );
        assert_eq!(
            output[1],
            json!({"type":"reply","id":1,"value":{"text":"中\n\""}})
        );
    }
}
#[tokio::test]
async fn excessive_rpc_admission_cancels_and_joins_all_sixteen_handlers() {
    let rpc = Arc::new(Rpc {
        blocked: true,
        ..Rpc::default()
    });
    let req = request(rpc.clone());
    let port = frames(
        (1..=MAXIMUM_PROGRAM_OUTSTANDING_CALLS + 1)
            .map(|id| json!({"type":"call","id":id,"method":"echo","arguments":null})),
        true,
    );
    let result = exchange(port.clone(), port, &req).await.unwrap_err();
    assert!(result.to_string().contains("excessive"));
    assert_eq!(
        rpc.entered.load(Ordering::SeqCst),
        MAXIMUM_PROGRAM_OUTSTANDING_CALLS
    );
    assert_eq!(
        rpc.settled.load(Ordering::SeqCst),
        MAXIMUM_PROGRAM_OUTSTANDING_CALLS
    );
}
#[tokio::test]
async fn cancellation_joins_admitted_rpc_before_returning() {
    let rpc = Arc::new(Rpc {
        blocked: true,
        ..Rpc::default()
    });
    let req = request(rpc.clone());
    let port = frames(
        [json!({"type":"call","id":1,"method":"echo","arguments":null})],
        true,
    );
    let exchange = exchange(port.clone(), port, &req);
    tokio::pin!(exchange);
    tokio::select! { result = &mut exchange => panic!("unexpected settlement {result:?}"), () = rpc.notified.notified() => {} }
    req.cancel.cancel();
    assert!(
        exchange
            .await
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(rpc.settled.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn uncertain_rpc_never_replies_to_script_and_survives_cancellation_drain() {
    for cancelled in [false, true] {
        let rpc = Arc::new(Rpc {
            blocked: cancelled,
            uncertain: true,
            ..Rpc::default()
        });
        let req = request(rpc.clone());
        let port = frames(
            [json!({"type":"call","id":1,"method":"effect","arguments":null})],
            true,
        );
        let exchange = exchange(port.clone(), port.clone(), &req);
        tokio::pin!(exchange);
        if cancelled {
            tokio::select! { result = &mut exchange => panic!("unexpected settlement {result:?}"), () = rpc.notified.notified() => {} }
            req.cancel.cancel();
        }
        assert_eq!(exchange.await.unwrap_err(), ProgramError::OutcomeUnknown);
        assert_eq!(rpc.settled.load(Ordering::SeqCst), 1);
        let written = port.written.lock().unwrap();
        let first = u32::from_be_bytes(written[..4].try_into().unwrap()) as usize;
        assert_eq!(
            written.len(),
            first + 4,
            "only the start frame may reach JavaScript"
        );
    }
}
