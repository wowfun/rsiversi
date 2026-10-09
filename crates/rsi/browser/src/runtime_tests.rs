use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Debug, Default)]
struct ProcessReceipt {
    terminated: AtomicBool,
    settled: CancellationToken,
    waiting: CancellationToken,
    failure: Option<rsi_process::ProcessError>,
    waits: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl rsi_process::DuplexControl for ProcessReceipt {
    fn pid(&self) -> u32 {
        1
    }
    fn stdin(&self) -> Arc<dyn rsi_process::DuplexInput> {
        unreachable!()
    }
    fn stdout(&self) -> Arc<dyn rsi_process::DuplexOutput> {
        unreachable!()
    }
    fn stderr(&self) -> Arc<dyn rsi_process::ProcessOutput> {
        unreachable!()
    }
    fn terminate(&self) {
        self.terminated.store(true, Ordering::SeqCst);
    }
    async fn wait(&self) -> rsi_process::Result<rsi_process::ProcessOutcome> {
        unreachable!()
    }
    async fn wait_settlement(&self) -> rsi_process::Result<()> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        self.waiting.cancel();
        self.settled.cancelled().await;
        self.failure.clone().map_or(Ok(()), Err)
    }
}
fn reservation() -> LaunchReservation {
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    LaunchReservation {
        permit: Some(Arc::new(slots.clone().try_acquire_owned().unwrap())),
        processes: vec![],
        runtime: tokio::runtime::Handle::current(),
        slots,
        verified: Arc::new(AtomicBool::new(true)),
        diagnostic: Arc::new(std::sync::Mutex::new(None)),
    }
}

#[tokio::test(start_paused = true)]
async fn one_retirement_owner_settles_once_for_all_waiters_and_fences_a_deadline() {
    for stalled in [false, true] {
        let mut launch = reservation();
        let slots = launch.slots.clone();
        let verified = launch.verified.clone();
        let first = Arc::new(ProcessReceipt::default());
        let second = Arc::new(ProcessReceipt::default());
        let cleanup = launch.handoff();
        let stop = CancellationToken::new();
        let receipt = schedule_retirement(
            &tokio::runtime::Handle::current(),
            cleanup,
            vec![
                ManagedDuplexProcess::new(first.clone()),
                ManagedDuplexProcess::new(second.clone()),
            ],
            TaskTracker::new(),
            stop.clone(),
        );
        stop.cancel();
        first.waiting.cancelled().await;
        second.waiting.cancelled().await;
        assert_eq!(slots.available_permits(), 0);
        first.settled.cancel();
        if !stalled {
            second.settled.cancel();
        }
        let (a, b) = tokio::join!(
            retirement_receipt(receipt.clone()),
            retirement_receipt(receipt)
        );
        assert_eq!(a, b);
        assert_eq!(a.is_err(), stalled);
        assert_eq!(first.waits.load(Ordering::SeqCst), 1);
        assert_eq!(second.waits.load(Ordering::SeqCst), 1);
        assert_eq!(slots.is_closed(), stalled);
        assert_eq!(verified.load(Ordering::Acquire), !stalled);
        assert_eq!(slots.available_permits(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn retirement_keeps_native_capacity_until_the_bridge_receipt() {
    let mut launch = reservation();
    let slots = launch.slots.clone();
    let first = Arc::new(ProcessReceipt::default());
    let second = Arc::new(ProcessReceipt::default());
    let tasks = TaskTracker::new();
    let bridge = tasks.token();
    let stop = CancellationToken::new();
    let receipt = schedule_retirement(
        &tokio::runtime::Handle::current(),
        launch.handoff(),
        vec![
            ManagedDuplexProcess::new(first.clone()),
            ManagedDuplexProcess::new(second.clone()),
        ],
        tasks.clone(),
        stop.clone(),
    );
    stop.cancel();
    first.waiting.cancelled().await;
    second.waiting.cancelled().await;
    first.settled.cancel();
    second.settled.cancel();
    while !tasks.is_closed() {
        tokio::task::yield_now().await;
    }
    assert_eq!(slots.available_permits(), 0);
    drop(bridge);
    retirement_receipt(receipt).await.unwrap();
    assert_eq!(slots.available_permits(), 1);
}

pub(crate) fn session_with_retirement(result: Result<(), String>) -> Arc<BrowserSession> {
    let process = ManagedDuplexProcess::new(Arc::new(ProcessReceipt::default()));
    let (_, commands) = mpsc::channel(1);
    let (_, mcp_responses) = mpsc::channel(1);
    let (_, settlement) = tokio::sync::watch::channel(Some(result));
    Arc::new(BrowserSession {
        browser: process.clone(),
        client: process,
        policy: RuntimePolicy::Session(SessionPolicy::PublicWeb {}),
        resolver: Arc::new(rsi_retrieval::PublicDestinationResolver::new()),
        commands: Mutex::new(commands),
        mcp_responses: Mutex::new(mcp_responses),
        writes: Arc::new(Mutex::new(())),
        tasks: TaskTracker::new(),
        stop: CancellationToken::new(),
        permit: Arc::new(
            Arc::new(tokio::sync::Semaphore::new(1))
                .try_acquire_owned()
                .unwrap(),
        ),
        settlement,
    })
}
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct UnusedNative;
#[cfg(target_os = "linux")]
#[async_trait::async_trait]
impl DuplexProcess for UnusedNative {
    async fn spawn(&self, _: DuplexProcessSpec) -> rsi_process::Result<ManagedDuplexProcess> {
        panic!("readiness refusal must not spawn a process")
    }
}
#[cfg(target_os = "linux")]
#[async_trait::async_trait]
impl Sandbox for UnusedNative {
    async fn workspace_read(
        &self,
        _: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        unreachable!()
    }
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        panic!("readiness refusal must not plan a process")
    }
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn fenced_runtime_preserves_the_retained_cause_and_never_launches() {
    let root = tempfile::tempdir().unwrap();
    let runtime = NativeRuntime::new(
        RuntimeConfig {
            node: root.path().join("node"),
            chromium_directory: root.path().join("chrome"),
            package_directory: root.path().join("package"),
            systemd_run: root.path().join("systemd-run"),
            user_runtime_directory: root.path().join("user-runtime"),
            artifact_digest: "a".repeat(64),
        },
        Arc::new(UnusedNative),
        Arc::new(UnusedNative),
    )
    .unwrap();
    let policy = BrowserPolicy {
        entry_url: "https://readiness.invalid/".into(),
        path_prefix: "/".into(),
        dependency_hosts: std::collections::BTreeSet::new(),
    };
    let cause = "process 41 settlement: receipt unavailable";
    runtime.slots.close();
    *runtime.diagnostic.lock().unwrap() = Some(cause.into());
    for verified in [false, true] {
        runtime.verified.store(verified, Ordering::Release);
        assert_eq!(
            runtime
                .open(policy.clone(), "refused", CancellationToken::new())
                .await
                .unwrap_err()
                .to_string(),
            cause
        );
    }
    *runtime.diagnostic.lock().unwrap() = None;
    assert_eq!(runtime.require_ready().unwrap_err(), SETTLEMENT_FAILURE);
}

#[cfg(target_os = "linux")]
pub(crate) fn full_ready_runtime() -> (Arc<NativeRuntime>, Vec<tokio::sync::OwnedSemaphorePermit>) {
    let root = tempfile::tempdir().unwrap();
    let runtime = Arc::new(
        NativeRuntime::new(
            RuntimeConfig {
                node: root.path().join("node"),
                chromium_directory: root.path().join("chrome"),
                package_directory: root.path().join("package"),
                systemd_run: root.path().join("systemd-run"),
                user_runtime_directory: root.path().join("user-runtime"),
                artifact_digest: "a".repeat(64),
            },
            Arc::new(UnusedNative),
            Arc::new(UnusedNative),
        )
        .unwrap(),
    );
    runtime.verified.store(true, Ordering::Release);
    let permits = (0..2)
        .map(|_| runtime.slots.clone().try_acquire_owned().unwrap())
        .collect();
    (runtime, permits)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shared_capacity_is_a_typed_prelaunch_refusal_and_never_a_readiness_failure() {
    let (runtime, _permits) = full_ready_runtime();
    assert_eq!(
        runtime
            .open_session(
                SessionPolicy::PublicWeb {},
                "capacity",
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        OpenError::Capacity
    );
    assert!(runtime.require_ready().is_ok());
}

#[test]
fn session_results_require_an_object_and_a_policy_valid_settled_url_even_without_a_snapshot() {
    let policy = RuntimePolicy::Session(SessionPolicy::LocalDev {
        origin: "http://127.0.0.1:4321".into(),
    });
    assert!(
        validate_session_result(
            &policy,
            &json!({"status":"completed","url":"http://127.0.0.1:4321/","png":"a"})
        )
        .is_ok()
    );
    for result in [
        Value::Null,
        json!([]),
        json!({"status":"completed","png":"a"}),
        json!({"status":"completed","url":"about:blank","png":"a"}),
        json!({"status":"completed","url":"http://localhost:4321/","png":"a"}),
        json!({"status":"completed","url":"http://127.0.0.1:4321/","snapshot":{"url":"http://127.0.0.1:4321/other"}}),
    ] {
        assert!(
            validate_session_result(&policy, &result).is_err(),
            "{result}"
        );
    }
}
#[test]
fn maximum_base64_envelope_does_not_admit_two_extra_source_bytes() {
    use base64::Engine as _;
    let policy = RuntimePolicy::Session(SessionPolicy::PublicWeb {});
    for extra in [0, 1, 2] {
        let png = base64::engine::general_purpose::STANDARD.encode(vec![
            0;
            crate::session::MAXIMUM_SCREENSHOT_BYTES
                + extra
        ]);
        let result = json!({"status":"completed","url":"https://public.example/","png":png});
        assert_eq!(
            validate_session_result(&policy, &result).is_ok(),
            extra == 0
        );
    }
}
#[tokio::test]
async fn eof_socket_input_is_discarded_without_spending_scope_credit_but_live_overflow_fails() {
    let (commands, input) = mpsc::channel(2);
    let mut owner = ProxyOwner {
        commands,
        stop: CancellationToken::new(),
        inbound: std::collections::VecDeque::new(),
        outbound: std::collections::VecDeque::new(),
    };
    let total = std::sync::atomic::AtomicU64::new(0);
    drop(input);
    owner.send(vec![1], &total).unwrap();
    assert!(
        !owner.stop.is_cancelled(),
        "queued EOF output must remain deliverable"
    );
    assert!(owner.inbound.is_empty());
    assert_eq!(total.load(Ordering::SeqCst), 0);
    let (commands, _input) = mpsc::channel(2);
    owner.commands = commands;
    owner.stop = CancellationToken::new();
    owner.send(vec![1], &total).unwrap();
    owner.send(vec![2], &total).unwrap();
    assert!(owner.send(vec![3], &total).unwrap_err().contains("credit"));
}
#[tokio::test]
async fn clean_proxy_eof_keeps_final_output_available_when_late_input_is_discarded() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (commands, input) = mpsc::channel(2);
    let (events, mut receipts) = mpsc::channel(16);
    let stop = CancellationToken::new();
    let total = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let task = tokio::spawn(proxy_socket(
        1,
        async move {
            tokio::net::TcpStream::connect(address)
                .await
                .map_err(|error| error.to_string())
        },
        input,
        events,
        stop.clone(),
        total.clone(),
    ));
    let (mut remote, _) = listener.accept().await.unwrap();
    remote.write_all(&[42]).await.unwrap();
    remote.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mut owner = ProxyOwner {
        commands,
        stop: stop.clone(),
        inbound: std::collections::VecDeque::new(),
        outbound: std::collections::VecDeque::new(),
    };
    owner.send(vec![99], &total).unwrap();
    assert!(!stop.is_cancelled());
    assert_eq!(total.load(Ordering::SeqCst), 1);
    assert!(matches!(receipts.recv().await, Some(ProxyEvent::Opened(1))));
    let Some(ProxyEvent::Data(1, bytes, _credit)) = receipts.recv().await else {
        panic!("lost final EOF bytes");
    };
    assert_eq!(bytes, [42]);
}
#[tokio::test]
async fn cancellation_awaits_an_admitted_spawn_and_keeps_capacity_through_receipt() {
    let mut launch = reservation();
    let stop = CancellationToken::new();
    let process = Arc::new(ProcessReceipt::default());
    let (release, admitted) = tokio::sync::oneshot::channel();
    let mut spawning = Box::pin(launch.admit(&stop, async {
        admitted.await.map_err(|_| "lost spawn".into())
    }));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(spawning.as_mut().poll(cx).is_pending()))
            .await
    );
    stop.cancel();
    release
        .send(ManagedDuplexProcess::new(process.clone()))
        .unwrap();
    assert!(spawning.await.is_err());
    assert_eq!(launch.processes.len(), 1);
    let slots = launch.slots.clone();
    let mut cleanup = Box::pin(launch.retire());
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(cleanup.as_mut().poll(cx).is_pending()))
            .await
    );
    assert!(process.terminated.load(Ordering::SeqCst));
    assert_eq!(slots.available_permits(), 0);
    process.settled.cancel();
    cleanup.await.unwrap();
    assert_eq!(slots.available_permits(), 1);
    assert!(!slots.is_closed());
}
#[tokio::test]
async fn panic_cleanup_fences_before_capacity_release_and_joins_all_returned_handles() {
    let mut launch = reservation();
    let slots = launch.slots.clone();
    let first = Arc::new(ProcessReceipt::default());
    let failed = Arc::new(ProcessReceipt {
        failure: Some(rsi_process::ProcessError::SettlementTimeout),
        ..Default::default()
    });
    launch.processes.extend([
        ManagedDuplexProcess::new(first.clone()),
        ManagedDuplexProcess::new(failed.clone()),
    ]);
    drop(launch);
    assert!(slots.is_closed());
    assert_eq!(slots.available_permits(), 0);
    first.settled.cancel();
    tokio::task::yield_now().await;
    assert!(failed.terminated.load(Ordering::SeqCst));
    assert_eq!(slots.available_permits(), 0);
    failed.settled.cancel();
    tokio::task::yield_now().await;
    assert_eq!(slots.available_permits(), 1);
}
#[test]
fn launch_reservation_drop_after_executor_shutdown_fences_without_panicking() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut launch = {
        let _entered = executor.enter();
        reservation()
    };
    let slots = launch.slots.clone();
    let verified = launch.verified.clone();
    let process = Arc::new(ProcessReceipt::default());
    launch
        .processes
        .push(ManagedDuplexProcess::new(process.clone()));
    drop(executor);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(launch))).is_ok());
    assert!(process.terminated.load(Ordering::SeqCst));
    assert!(slots.is_closed());
    assert!(!verified.load(Ordering::SeqCst));
    assert!(
        !process.settled.is_cancelled(),
        "executor loss is not a settlement receipt"
    );
}
#[tokio::test]
async fn settlement_observes_all_receipts_concurrently_and_reports_each_failure() {
    let mut launch = reservation();
    let slots = launch.slots.clone();
    let verified = launch.verified.clone();
    let first = Arc::new(ProcessReceipt {
        failure: Some(rsi_process::ProcessError::Io(
            "browser receipt failed".into(),
        )),
        ..Default::default()
    });
    let second = Arc::new(ProcessReceipt {
        failure: Some(rsi_process::ProcessError::Io(
            "client receipt failed".into(),
        )),
        ..Default::default()
    });
    launch.processes.extend([
        ManagedDuplexProcess::new(first.clone()),
        ManagedDuplexProcess::new(second.clone()),
    ]);
    let mut cleanup = Box::pin(launch.retire());
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(cleanup.as_mut().poll(cx).is_pending()))
            .await
    );
    assert!(first.waiting.is_cancelled());
    assert!(
        second.waiting.is_cancelled(),
        "every receipt must be polled before any completes"
    );
    first.settled.cancel();
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(cleanup.as_mut().poll(cx).is_pending()))
            .await
    );
    assert_eq!(slots.available_permits(), 0);
    second.settled.cancel();
    let error = cleanup.await.unwrap_err();
    assert!(error.contains("browser receipt failed"), "{error}");
    assert!(error.contains("client receipt failed"), "{error}");
    assert!(slots.is_closed());
    assert!(!verified.load(Ordering::SeqCst));
    assert_eq!(slots.available_permits(), 1);
}
#[tokio::test]
async fn rejected_spawn_and_undispatched_cancellation_leave_the_generation_available() {
    for cancel in [false, true] {
        let mut launch = reservation();
        let slots = launch.slots.clone();
        let verified = launch.verified.clone();
        let stop = CancellationToken::new();
        if cancel {
            stop.cancel();
        }
        let dispatched = AtomicBool::new(false);
        assert!(
            launch
                .admit(&stop, async {
                    dispatched.store(true, Ordering::SeqCst);
                    Err("spawn rejected".into())
                })
                .await
                .is_err()
        );
        assert_eq!(dispatched.load(Ordering::SeqCst), !cancel);
        assert!(launch.processes.is_empty());
        launch.retire().await.unwrap();
        assert_eq!(slots.available_permits(), 1);
        assert!(!slots.is_closed());
        assert!(verified.load(Ordering::SeqCst));
    }
}
#[tokio::test(start_paused = true)]
async fn dns_and_connect_have_one_deadline_and_close_interrupts_it() {
    for cancel in [false, true] {
        let (send, _) = mpsc::channel(2);
        let (_input, receive) = mpsc::channel(2);
        let stop = CancellationToken::new();
        let started = tokio::time::Instant::now();
        let mut socket = Box::pin(proxy_socket(
            1,
            std::future::pending(),
            receive,
            send,
            stop.clone(),
            Arc::new(0.into()),
        ));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                socket.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        if cancel {
            stop.cancel();
            assert!(socket.await.is_ok());
            assert_eq!(tokio::time::Instant::now(), started);
        } else {
            assert!(socket.await.unwrap_err().contains("connection deadline"));
            assert_eq!(
                tokio::time::Instant::now() - started,
                Duration::from_secs(5)
            );
        }
    }
}
#[tokio::test]
async fn proxy_remote_reads_cannot_pass_two_unacknowledged_frames_and_close_unblocks() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (events, mut receipts) = mpsc::channel(16);
        let (_input, receive) = mpsc::channel(2);
        let stop = CancellationToken::new();
        let task = tokio::spawn(proxy_socket(
            1,
            async move {
                tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(|e| e.to_string())
            },
            receive,
            events,
            stop.clone(),
            Arc::new(0.into()),
        ));
        let (mut remote, _) = listener.accept().await.unwrap();
        assert!(matches!(receipts.recv().await, Some(ProxyEvent::Opened(1))));
        remote.write_all(&[1]).await.unwrap();
        let ProxyEvent::Data(_, _, first) = receipts.recv().await.unwrap() else {
            panic!("missing first frame");
        };
        remote.write_all(&[2]).await.unwrap();
        let ProxyEvent::Data(_, _, second) = receipts.recv().await.unwrap() else {
            panic!("missing second frame");
        };
        remote.write_all(&[3]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), receipts.recv())
                .await
                .is_err(),
            "a third frame passed the two unacknowledged frame credits"
        );
        drop(first);
        assert!(matches!(
            receipts.recv().await,
            Some(ProxyEvent::Data(1, _, _))
        ));
        stop.cancel();
        task.await.unwrap().unwrap();
        drop(second);
    })
    .await
    .expect("proxy credit and retirement handshake exceeded five seconds");
}
#[test]
fn fingerprint_directory_discovery_and_path_bytes_are_bounded_before_retention() {
    let tmp = tempfile::tempdir().unwrap();
    for index in 0..10 {
        std::fs::create_dir(tmp.path().join(format!("long-directory-{index}"))).unwrap();
    }
    for limits in [
        FingerprintLimits {
            entries: 4,
            ..FINGERPRINT_LIMITS
        },
        FingerprintLimits {
            bytes: 10,
            ..FINGERPRINT_LIMITS
        },
    ] {
        let mut count = 0;
        assert!(
            fingerprint(
                tmp.path(),
                tmp.path(),
                &mut Sha256::new(),
                &mut count,
                &mut 0,
                &mut vec![0; 65536].into_boxed_slice(),
                limits
            )
            .is_err()
        );
        assert!(count <= 5);
    }
}
#[cfg(unix)]
#[test]
fn fingerprint_accepts_depth_128_and_rejects_129() {
    let tmp = tempfile::tempdir().unwrap();
    let mut path = tmp.path().to_owned();
    for _ in 0..128 {
        path.push("d");
        std::fs::create_dir(&path).unwrap();
    }
    let fingerprint_tree = || {
        fingerprint(
            tmp.path(),
            tmp.path(),
            &mut Sha256::new(),
            &mut 0,
            &mut 0,
            &mut vec![0; 65536].into_boxed_slice(),
            FINGERPRINT_LIMITS,
        )
    };
    assert!(fingerprint_tree().is_ok());
    path.push("d");
    std::fs::create_dir(&path).unwrap();
    assert!(fingerprint_tree().unwrap_err().contains("depth"));
}
