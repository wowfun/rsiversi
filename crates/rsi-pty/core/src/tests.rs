use super::*;
use rsi_process::{ManagedPtyProcess, ProcessOutcome, PtyControl, PtyRead, PtySize};
use rsi_pty_protocol::MAXIMUM_INPUT_BYTES;

#[test]
fn native_invalid_input_preserves_taxonomy_and_diagnostic_bound() {
    let error = native(rsi_process::ProcessError::InvalidInput("界".repeat(1000)));
    assert!(matches!(error, PtyError::Invalid(message) if message.chars().count() == 256));
}

#[derive(Debug, Default)]
struct Fake {
    stopped: AtomicBool,
    hold_reap: AtomicBool,
    changed: Notify,
    writes: Mutex<Vec<Vec<u8>>>,
    waits: AtomicUsize,
    entered: Notify,
    release: Notify,
    hold: AtomicBool,
    fail_write: AtomicBool,
    fail_resize: AtomicBool,
    hold_resize: AtomicBool,
    resize_entered: Notify,
    resize_release: Notify,
}
#[async_trait]
impl PtyControl for Fake {
    fn pid(&self) -> u32 {
        1
    }
    async fn read(&self) -> rsi_process::Result<PtyRead> {
        self.wait().await?;
        Ok(PtyRead {
            bytes: vec![],
            eof: true,
        })
    }
    async fn write(&self, bytes: &[u8]) -> rsi_process::Result<usize> {
        let released = self.release.notified();
        lock(&self.writes).push(bytes.to_vec());
        self.entered.notify_one();
        if self.hold.load(Ordering::Acquire) {
            released.await;
        }
        if self.fail_write.load(Ordering::Acquire) {
            return Err(rsi_process::ProcessError::Io(
                "native input deadline".into(),
            ));
        }
        Ok(bytes.len())
    }
    async fn resize(&self, size: PtySize) -> rsi_process::Result<()> {
        size.validate()?;
        self.resize_entered.notify_one();
        if self.hold_resize.load(Ordering::Acquire) {
            self.resize_release.notified().await;
        }
        if self.fail_resize.load(Ordering::Acquire) {
            Err(rsi_process::ProcessError::Io("resize fixture".into()))
        } else {
            Ok(())
        }
    }
    fn terminate(&self) {
        self.stopped.store(true, Ordering::Release);
        self.changed.notify_waiters();
        self.release.notify_waiters();
    }
    async fn wait(&self) -> rsi_process::Result<ProcessOutcome> {
        self.waits.fetch_add(1, Ordering::Relaxed);
        loop {
            let changed = self.changed.notified();
            if self.stopped.load(Ordering::Acquire) && !self.hold_reap.load(Ordering::Acquire) {
                return Ok(ProcessOutcome {
                    exit_code: Some(0),
                    signal: None,
                });
            }
            changed.await;
        }
    }
}
#[async_trait::async_trait]
impl PtyProcess for Fake {
    async fn spawn(&self, _: PtyProcessSpec) -> rsi_process::Result<ManagedPtyProcess> {
        unreachable!("fixture installs a managed process")
    }
}
fn fixture() -> (Arc<Scope>, Arc<Term>, Arc<Fake>) {
    fixture_with_process(None)
}
fn fixture_with_process(
    provider: Option<Arc<dyn PtyProcess>>,
) -> (Arc<Scope>, Arc<Term>, Arc<Fake>) {
    let process = Arc::new(Fake::default());
    let shared = Arc::new(Shared {
        processes: provider.unwrap_or_else(|| process.clone()),
        generation: "test".into(),
        next: AtomicU64::new(1),
        stopped: AtomicBool::new(false),
        creating: Creations::default(),
        scopes: Mutex::new(vec![]),
        terminals: Mutex::new(vec![]),
        slots: Arc::new(Semaphore::new(256)),
        screens: Arc::new(Semaphore::new(SCREEN_BYTES)),
        snapshots: Arc::new(Semaphore::new(SNAPSHOT_BYTES)),
        queues: Arc::new(Semaphore::new(QUEUE_BYTES)),
    });
    let size = Size {
        rows: 24,
        columns: 80,
    };
    let parser = vt100::Parser::new(24, 80, 1000);
    let term = Arc::new(Term {
        shared: shared.clone(),
        process: TermProcess::Native(ManagedPtyProcess::new(process.clone())),
        inner: Mutex::new(TermState {
            screen_size: size,
            status: Terminal {
                id: "pty".into(),
                size,
                phase: Phase::Running,
                controller: Some("writer".into()),
                controller_epoch: 1,
            },
            followers: BTreeMap::from([(
                "writer".into(),
                Follower::new(
                    shared.snapshot(&parser, size).unwrap(),
                    reserve(&shared.queues, MAXIMUM_FOLLOWER_BYTES).unwrap(),
                    1,
                ),
            )]),
            parser,
            filter: filter::Filter::default(),
            screen: reserve(&shared.screens, screen_bytes(size).unwrap()).unwrap(),
            next_input: 1,
            inflight: false,
            resizing: false,
            receipts: VecDeque::new(),
        }),
        changed: Notify::new(),
        reader_done: AtomicBool::new(false),
        _slot: reserve(&shared.slots, 1).unwrap(),
    });
    lock(&shared.terminals).push(Arc::downgrade(&term));
    let scope = Arc::new_cyclic(|weak| Scope {
        self_weak: weak.clone(),
        closing: tokio::sync::Mutex::new(()),
        creating: Creations::default(),
        shared,
        inner: Mutex::new(ScopeState {
            retired: false,
            terminals: BTreeMap::from([("pty".into(), term.clone())]),
        }),
    });
    let running = term.clone();
    let guard = ReaderGuard(term.clone());
    tokio::spawn(async move {
        running.drain().await;
        drop(guard);
    });
    (scope, term, process)
}
async fn op(scope: &Scope, operation: Operation) -> Result<Reply> {
    scope.execute(operation).await
}
#[tokio::test]
async fn takeover_fences_writes_resize_and_delayed_detach_without_stopping_shell() {
    let (scope, term, process) = fixture();
    let reader = term.attach().unwrap();
    assert_eq!(reader.terminal.controller.as_deref(), Some("writer"));
    let Reply::Terminal(next) = op(
        &scope,
        Operation::Takeover {
            terminal: "pty".into(),
            attachment: reader.id.clone(),
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(next.controller_epoch, 2);
    assert_eq!(
        op(
            &scope,
            Operation::Input {
                terminal: "pty".into(),
                attachment: "writer".into(),
                epoch: 1,
                sequence: 1,
                bytes: b"bad".to_vec()
            }
        )
        .await,
        Err(PtyError::StaleController)
    );
    assert_eq!(
        term.resize(
            "writer",
            1,
            Size {
                rows: 30,
                columns: 100
            },
            None
        )
        .await,
        Err(PtyError::StaleController)
    );
    // Reacquire the same attachment; an old detach must not revoke its newer epoch.
    op(
        &scope,
        Operation::Takeover {
            terminal: "pty".into(),
            attachment: "writer".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        op(
            &scope,
            Operation::Detach {
                terminal: "pty".into(),
                attachment: "writer".into(),
                epoch: 1
            }
        )
        .await,
        Err(PtyError::StaleController)
    );
    assert!(lock(&process.writes).is_empty());
    assert!(!process.stopped.load(Ordering::Acquire));
    scope.retire().await.unwrap();
    assert!(process.stopped.load(Ordering::Acquire));
}
#[tokio::test]
async fn cancelled_waiter_keeps_one_write_and_exact_receipt_no_replay() {
    let (scope, term, process) = fixture();
    process.hold.store(true, Ordering::Release);
    let input = Operation::Input {
        terminal: "pty".into(),
        attachment: "writer".into(),
        epoch: 1,
        sequence: 1,
        bytes: vec![0xe7, 0x95, 0x8c],
    };
    let call = {
        let scope = scope.clone();
        let input = input.clone();
        tokio::spawn(async move { scope.execute(input).await })
    };
    process.entered.notified().await;
    call.abort();
    let _ = call.await;
    assert_eq!(
        op(&scope, input.clone()).await.unwrap(),
        Reply::Input(InputReceipt {
            epoch: 1,
            sequence: 1,
            result: InputState::Pending
        })
    );
    let reader = term.attach().unwrap();
    assert_eq!(
        op(
            &scope,
            Operation::Takeover {
                terminal: "pty".into(),
                attachment: reader.id
            }
        )
        .await,
        Err(PtyError::Capacity)
    );
    process.release.notify_one();
    loop {
        let changed = term.changed.notified();
        if !lock(&term.inner).inflight {
            break;
        }
        changed.await;
    }
    assert_eq!(
        op(&scope, input).await.unwrap(),
        Reply::Input(InputReceipt {
            epoch: 1,
            sequence: 1,
            result: InputState::Accepted { bytes: 3 }
        })
    );
    assert_eq!(
        op(
            &scope,
            Operation::Input {
                terminal: "pty".into(),
                attachment: "writer".into(),
                epoch: 1,
                sequence: 1,
                bytes: b"different".to_vec()
            }
        )
        .await,
        Err(PtyError::InputConflict)
    );
    assert_eq!(lock(&process.writes).len(), 1);
    scope.retire().await.unwrap();
}
#[tokio::test]
async fn slow_follower_resets_to_bounded_unicode_screen_and_queue_accounts_tiny_chunks() {
    let (scope, term, _) = fixture();
    let initial = term.read("writer", 1, 0).await.unwrap();
    // One-byte writes also charge their retained allocation/queue metadata.
    for _ in 0..50_000 {
        term.feed(b"x").unwrap();
    }
    term.feed("\x1b[2J\x1b[H界e\u{301}".as_bytes()).unwrap();
    let page = term.read("writer", 1, initial.next_cursor).await.unwrap();
    assert!(page.reset);
    assert_eq!(page.cursor, 0);
    assert_eq!(page.stream_epoch, 2);
    let mut restored = vt100::Parser::new(24, 80, 0);
    restored.process(page.text.as_bytes());
    assert!(restored.screen().contents().contains("界e\u{301}"));
    {
        let state = lock(&term.inner);
        let follower = &state.followers["writer"];
        assert!(follower.bytes <= MAXIMUM_FOLLOWER_BYTES);
        assert!(
            follower.chunks.capacity() * std::mem::size_of::<Chunk>()
                + follower
                    .chunks
                    .iter()
                    .map(|c| c.text.len() + 16)
                    .sum::<usize>()
                <= MAXIMUM_FOLLOWER_BYTES
        );
    }
    scope.retire().await.unwrap();
}
#[tokio::test]
async fn attachment_and_memory_limits_retirement_reject_old_live_ids() {
    let (scope, term, process) = fixture();
    for _ in 1..MAXIMUM_ATTACHMENTS {
        term.attach().unwrap();
    }
    assert_eq!(term.attach(), Err(PtyError::Capacity));
    assert!(
        screen_bytes(Size {
            rows: 201,
            columns: 80
        })
        .is_err()
    );
    assert!(
        snapshot_bytes(Size {
            rows: 200,
            columns: 500
        })
        .unwrap()
            <= 16 * 1024 * 1024
    );
    term.shared.retire().await.unwrap();
    assert!(process.stopped.load(Ordering::Acquire));
    assert!(scope.execute(Operation::List).await.is_err());
    assert!(
        Provider {
            shared: term.shared.clone()
        }
        .scope()
        .is_err()
    );
}
#[tokio::test]
async fn maximum_styled_screen_snapshot_stays_within_preallocation_bound() {
    let (scope, term, _) = fixture();
    let size = Size {
        rows: 200,
        columns: 500,
    };
    term.resize("writer", 1, size, None).await.unwrap();
    for row in 1..=200 {
        for column in (1..=500).step_by(2) {
            term.feed(
                format!(
                    "\x1b[{row};{column}H\x1b[38;2;{};{};{}m界\u{301}",
                    row % 256,
                    column % 256,
                    (row + column) % 256
                )
                .as_bytes(),
            )
            .unwrap();
        }
    }
    {
        let state = lock(&term.inner);
        let snapshot = term.shared.snapshot(&state.parser, size).unwrap();
        assert!(snapshot.text.len() <= snapshot_bytes(size).unwrap());
    }

    scope.retire().await.unwrap();
}

#[tokio::test]
async fn output_page_coalesces_small_native_chunks_without_splitting_utf8() {
    let (scope, term, _) = fixture();
    let initial = term.read("writer", 1, 0).await.unwrap();
    for _ in 0..8193 {
        term.feed("界".as_bytes()).unwrap();
    }
    let page = term.read("writer", 1, initial.next_cursor).await.unwrap();
    assert_eq!(page.text.len(), 16383);
    assert_eq!(page.text, "界".repeat(5461));
    let rest = term.read("writer", 1, page.next_cursor).await.unwrap();
    assert_eq!(rest.text, "界".repeat(2732));
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn repeated_close_waits_for_the_first_native_reap() {
    let (scope, _term, process) = fixture();
    process.hold_reap.store(true, Ordering::Release);
    let first = {
        let scope = scope.clone();
        tokio::spawn(async move {
            scope
                .execute(Operation::Close {
                    terminal: "pty".into(),
                })
                .await
        })
    };
    loop {
        let changed = process.changed.notified();
        if process.stopped.load(Ordering::Acquire) {
            break;
        }
        changed.await;
    }
    tokio::select! { biased;
        result=scope.execute(Operation::Close{terminal:"pty".into()})=>panic!("duplicate close returned before native cleanup: {result:?}"),
        ()=std::future::ready(())=>{},
    }
    process.hold_reap.store(false, Ordering::Release);
    process.changed.notify_waiters();
    first.await.unwrap().unwrap();
    assert_eq!(
        scope
            .execute(Operation::Close {
                terminal: "pty".into()
            })
            .await
            .unwrap(),
        Reply::Done
    );
}

#[tokio::test]
async fn cancelled_close_keeps_native_cleanup_owned_for_retry_and_retirement() {
    for operation in [
        Operation::Close {
            terminal: "pty".into(),
        },
        Operation::CloseAll,
    ] {
        let (scope, _, process) = fixture();
        process.hold_reap.store(true, Ordering::Release);
        let first = {
            let scope = scope.clone();
            tokio::spawn(async move { scope.execute(operation).await })
        };
        loop {
            let changed = process.changed.notified();
            if process.stopped.load(Ordering::Acquire) {
                break;
            }
            changed.await;
        }
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        tokio::select! { biased;
            result = scope.retire() => panic!("cancelled close lost cleanup ownership: {result:?}"),
            () = std::future::ready(()) => {},
        }
        process.hold_reap.store(false, Ordering::Release);
        process.changed.notify_waiters();
        scope.retire().await.unwrap();
        assert!(lock(&scope.inner).terminals.is_empty());
    }
}

#[tokio::test]
async fn detach_during_backpressure_releases_follower_without_replaying_or_stopping_write() {
    let (scope, term, process) = fixture();
    process.hold.store(true, Ordering::Release);
    let call = {
        let scope = scope.clone();
        tokio::spawn(async move {
            scope
                .execute(Operation::Input {
                    terminal: "pty".into(),
                    attachment: "writer".into(),
                    epoch: 1,
                    sequence: 1,
                    bytes: vec![b'x'; MAXIMUM_INPUT_BYTES],
                })
                .await
        })
    };
    process.entered.notified().await;
    assert!(term.shared.queues.available_permits() < QUEUE_BYTES);
    op(
        &scope,
        Operation::Detach {
            terminal: "pty".into(),
            attachment: "writer".into(),
            epoch: 1,
        },
    )
    .await
    .unwrap();
    assert_eq!(term.shared.queues.available_permits(), QUEUE_BYTES);
    assert_eq!(term.shared.snapshots.available_permits(), SNAPSHOT_BYTES);
    assert!(!process.stopped.load(Ordering::Acquire));
    assert!(lock(&term.inner).status.controller.is_none());
    let reader = term.attach().unwrap();
    let takeover = Operation::Takeover {
        terminal: "pty".into(),
        attachment: reader.id,
    };
    assert_eq!(op(&scope, takeover.clone()).await, Err(PtyError::Capacity));
    process.release.notify_one();
    assert_eq!(
        call.await.unwrap().unwrap(),
        Reply::Input(InputReceipt {
            epoch: 1,
            sequence: 1,
            result: InputState::Accepted {
                bytes: MAXIMUM_INPUT_BYTES
            }
        })
    );
    op(&scope, takeover).await.unwrap();
    assert_eq!(lock(&process.writes).len(), 1);
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn create_without_runtime_fails_before_native_spawn_or_reservation() {
    let (scope, term, _) = fixture();
    let spec = spawn_spec();
    let before = term.shared.screens.available_permits();
    let caller = scope.clone();
    assert!(matches!(
        std::thread::spawn(
            move || futures_util::FutureExt::now_or_never(caller.create(spec)).unwrap()
        )
        .join()
        .unwrap(),
        Err(PtyError::Unavailable(_))
    ));
    assert_eq!(term.shared.screens.available_permits(), before);
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn shrinking_screen_keeps_retained_scrollback_and_cell_capacity_charged() {
    let (scope, term, _) = fixture();
    term.resize(
        "writer",
        1,
        Size {
            rows: 24,
            columns: 500,
        },
        None,
    )
    .await
    .unwrap();
    let charged = SCREEN_BYTES - term.shared.screens.available_permits();
    term.resize(
        "writer",
        1,
        Size {
            rows: 1,
            columns: 1,
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        SCREEN_BYTES - term.shared.screens.available_permits(),
        charged,
        "vt100 keeps row capacities and old-width scrollback after shrink"
    );
    term.resize(
        "writer",
        1,
        Size {
            rows: 200,
            columns: 1,
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        SCREEN_BYTES - term.shared.screens.available_permits(),
        screen_bytes(Size {
            rows: 200,
            columns: 500
        })
        .unwrap()
    );
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn failed_native_write_keeps_unknown_receipt_and_allows_explicit_takeover() {
    let (scope, term, process) = fixture();
    process.fail_write.store(true, Ordering::Release);
    let input = Operation::Input {
        terminal: "pty".into(),
        attachment: "writer".into(),
        epoch: 1,
        sequence: 1,
        bytes: b"uncertain".to_vec(),
    };
    let expected = Reply::Input(InputReceipt {
        epoch: 1,
        sequence: 1,
        result: InputState::Unknown,
    });
    assert_eq!(op(&scope, input.clone()).await.unwrap(), expected);
    assert_eq!(op(&scope, input).await.unwrap(), expected);
    assert_eq!(lock(&process.writes).len(), 1);
    let follower = term.attach().unwrap();
    let Reply::Terminal(status) = op(
        &scope,
        Operation::Takeover {
            terminal: "pty".into(),
            attachment: follower.id.clone(),
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(status.controller_epoch, 2);
    assert!(!process.stopped.load(Ordering::Acquire));
    process.fail_write.store(false, Ordering::Release);
    assert_eq!(
        op(
            &scope,
            Operation::Input {
                terminal: "pty".into(),
                attachment: follower.id,
                epoch: 2,
                sequence: 1,
                bytes: b"new".to_vec()
            }
        )
        .await
        .unwrap(),
        Reply::Input(InputReceipt {
            epoch: 2,
            sequence: 1,
            result: InputState::Accepted { bytes: 3 }
        })
    );
    assert_eq!(
        &*lock(&process.writes),
        &[b"uncertain".to_vec(), b"new".to_vec()]
    );
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn exhausted_snapshot_budget_still_allows_refresh_and_resize() {
    let (scope, term, _) = fixture();
    let size = Size {
        rows: 200,
        columns: 500,
    };
    term.resize("writer", 1, size, None).await.unwrap();
    for _ in 0..3 {
        term.attach().unwrap();
    }
    // Four maximum snapshots leave too little for even one new full reservation.
    assert!(term.shared.snapshots.available_permits() < snapshot_bytes(size).unwrap());
    let cursor = {
        let state = lock(&term.inner);
        state.followers["writer"].end
    };
    for _ in 0..40 {
        term.feed(&vec![b'x'; 65536]).unwrap();
    }
    let page = term.read("writer", 2, cursor).await.unwrap();
    assert!(page.reset);
    assert!(!page.text.is_empty());
    term.resize("writer", 1, size, None).await.unwrap();
    // Resize shares one snapshot; refreshing any member safely resets that group.
    let before = term.shared.snapshots.available_permits();
    let (epoch, cursor) = {
        let state = lock(&term.inner);
        let follower = &state.followers["writer"];
        (follower.stream_epoch, follower.end)
    };
    let remaining = reserve(&term.shared.snapshots, before).unwrap();
    for _ in 0..40 {
        term.feed(&vec![b'y'; 65536]).unwrap();
    }
    assert!(term.read("writer", epoch, cursor).await.unwrap().reset);
    term.resize("writer", 1, size, None).await.unwrap();
    assert_eq!(term.shared.snapshots.available_permits(), 0);
    drop(remaining);
    scope.retire().await.unwrap();
}

fn spawn_spec() -> PtyProcessSpec {
    PtyProcessSpec {
        process: rsi_sandbox::ConfinedProcess {
            owner: None,
            stdio: rsi_sandbox::ProcessStdio::Pty,
            program: "/fake/bwrap".into(),
            arguments: vec![],
            cwd: "/workspace".into(),
            stamp: rsi_sandbox::EnforcementStamp {
                requested: rsi_sandbox::SandboxMode::ReadOnly,
                backend: rsi_sandbox::SandboxBackend::Bubblewrap {
                    sha256: "a".repeat(64),
                },
                filesystem: rsi_sandbox::SandboxFileSystem::ReadOnly,
                scratch: rsi_sandbox::SandboxScratch::PrivateTmp,
                network: rsi_sandbox::SandboxNetwork::Host,
                workspace: "/workspace".into(),
            },
        },
        size: PtySize {
            rows: 24,
            columns: 80,
        },
        termination_grace_ms: 250,
        environment: vec![],
    }
}

#[derive(Debug)]
struct GatedSpawn {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    child: Arc<Fake>,
}
#[async_trait::async_trait]
impl PtyProcess for GatedSpawn {
    async fn spawn(&self, _: PtyProcessSpec) -> rsi_process::Result<ManagedPtyProcess> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(ManagedPtyProcess::new(self.child.clone()))
    }
}
#[tokio::test]
async fn slow_spawn_releases_registry_locks_and_retirement_owns_its_child() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let child = Arc::new(Fake::default());
    let (scope, term, _) = fixture_with_process(Some(Arc::new(GatedSpawn {
        entered: entered.clone(),
        release: release.clone(),
        child: child.clone(),
    })));
    scope.retire().await.unwrap();
    drop(term);
    lock(&scope.inner).retired = false;
    let caller = scope.clone();
    let spawning = tokio::spawn(async move { caller.create(spawn_spec()).await });
    entered.notified().await;
    assert!(scope.inner.try_lock().is_ok(), "spawn holds its scope lock");
    assert!(
        scope.shared.terminals.try_lock().is_ok(),
        "spawn holds the provider registry"
    );
    assert_eq!(
        scope.execute(Operation::List).await.unwrap(),
        Reply::List(vec![])
    );
    let retiring = scope.retire();
    tokio::pin!(retiring);
    tokio::select! { biased;
        result = &mut retiring => panic!("retired before admitted spawn settled: {result:?}"),
        () = std::future::ready(()) => {},
    }
    release.notify_one();
    assert!(matches!(
        spawning.await.unwrap(),
        Err(PtyError::Unavailable(_))
    ));
    retiring.await.unwrap();
    assert!(child.stopped.load(Ordering::Acquire));
    assert!(lock(&scope.inner).terminals.is_empty());
    assert_eq!(scope.shared.slots.available_permits(), 256);
}

#[tokio::test]
async fn rejected_resize_preserves_the_old_snapshot_and_reservations() {
    let (scope, term, process) = fixture();
    let before = {
        let state = lock(&term.inner);
        (
            state.status.size,
            state.followers["writer"].stream_epoch,
            state.followers["writer"].snapshot.text.clone(),
        )
    };
    let budget = term.shared.snapshots.available_permits();
    process.fail_resize.store(true, Ordering::Release);
    assert!(matches!(
        term.resize(
            "writer",
            1,
            Size {
                rows: 200,
                columns: 500
            },
            None
        )
        .await,
        Err(PtyError::Io(_))
    ));
    assert_eq!(term.shared.snapshots.available_permits(), budget);
    process.fail_resize.store(false, Ordering::Release);
    let held = reserve(&term.shared.snapshots, budget).unwrap();
    assert_eq!(
        term.resize(
            "writer",
            1,
            Size {
                rows: 200,
                columns: 500
            },
            None
        )
        .await,
        Err(PtyError::Capacity)
    );
    {
        let state = lock(&term.inner);
        assert_eq!(
            (
                state.status.size,
                state.followers["writer"].stream_epoch,
                state.followers["writer"].snapshot.text.clone()
            ),
            before
        );
    }
    drop(held);
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn spare_attachment_capacity_does_not_reclaim_idle_followers() {
    let (scope, term, _) = fixture();
    lock(&term.inner)
        .followers
        .get_mut("writer")
        .unwrap()
        .last_read = std::time::Instant::now()
        .checked_sub(FOLLOWER_IDLE)
        .unwrap();
    let attached = term.attach().unwrap();
    assert_eq!(attached.terminal.controller.as_deref(), Some("writer"));
    assert_eq!(lock(&term.inner).followers.len(), 2);
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn shared_queue_exhaustion_reclaims_idle_followers_before_retry() {
    let (scope, term, _) = fixture();
    let held = reserve(&term.shared.queues, QUEUE_BYTES - MAXIMUM_FOLLOWER_BYTES).unwrap();
    lock(&term.inner)
        .followers
        .get_mut("writer")
        .unwrap()
        .last_read = std::time::Instant::now()
        .checked_sub(FOLLOWER_IDLE)
        .unwrap();
    let attached = term.attach().unwrap();
    assert_eq!(attached.terminal.controller, None);
    assert_eq!(lock(&term.inner).followers.len(), 1);
    drop(held);
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn abandoned_followers_release_capacity_and_authority_without_killing_the_shell() {
    let (scope, term, process) = fixture();
    for _ in 1..MAXIMUM_ATTACHMENTS {
        term.attach().unwrap();
    }
    assert!(matches!(term.attach(), Err(PtyError::Capacity)));
    let expired = std::time::Instant::now()
        .checked_sub(FOLLOWER_IDLE)
        .unwrap();
    for follower in lock(&term.inner).followers.values_mut() {
        follower.last_read = expired;
    }
    // This one remains live even when the rest of the client's followers disappear.
    term.read("writer", 1, 0).await.unwrap();
    let next = term.attach().unwrap();
    assert_eq!(lock(&term.inner).followers.len(), 2);
    assert_eq!(next.terminal.controller.as_deref(), Some("writer"));
    for _ in 2..MAXIMUM_ATTACHMENTS {
        term.attach().unwrap();
    }
    for (id, follower) in &mut lock(&term.inner).followers {
        if id != &next.id {
            follower.last_read = expired;
        }
    }
    let another = term.attach().unwrap();
    assert_eq!(another.terminal.controller, None);
    assert_eq!(another.terminal.controller_epoch, 2);
    assert_eq!(
        term.shared.queues.available_permits(),
        QUEUE_BYTES - 2 * MAXIMUM_FOLLOWER_BYTES
    );
    assert!(!process.stopped.load(Ordering::Acquire));
    op(
        &scope,
        Operation::Takeover {
            terminal: "pty".into(),
            attachment: another.id,
        },
    )
    .await
    .unwrap();
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn close_all_waits_for_all_native_reapers_concurrently() {
    let (scope, _, first) = fixture();
    let (other_scope, other, second) = fixture();
    lock(&scope.inner).terminals.insert("second".into(), other);
    first.hold_reap.store(true, Ordering::Release);
    second.hold_reap.store(true, Ordering::Release);
    let before_first = first.waits.load(Ordering::Relaxed);
    let before_second = second.waits.load(Ordering::Relaxed);
    let closing = scope.close_all(false);
    tokio::pin!(closing);
    assert!(futures_util::poll!(&mut closing).is_pending());
    assert_eq!(first.waits.load(Ordering::Relaxed), before_first + 1);
    assert_eq!(second.waits.load(Ordering::Relaxed), before_second + 1);
    for process in [&first, &second] {
        assert!(process.stopped.load(Ordering::Acquire));
        process.hold_reap.store(false, Ordering::Release);
        process.changed.notify_waiters();
    }
    closing.await.unwrap();
    other_scope.retire().await.unwrap();
}

#[tokio::test]
async fn cancelled_resize_waiter_keeps_old_dimensions_until_ack_and_retains_snapshot_credit() {
    let (scope, term, process) = fixture();
    process.hold_resize.store(true, Ordering::Release);
    let original = lock(&term.inner).status.size;
    let requested = Size {
        rows: 30,
        columns: 100,
    };
    let resizing = tokio::spawn({
        let term = term.clone();
        async move { term.resize("writer", 1, requested, None).await }
    });
    process.resize_entered.notified().await;
    assert_eq!(lock(&term.inner).status.size, original);
    assert_eq!(term.attach(), Err(PtyError::Capacity));
    assert_eq!(
        op(
            &scope,
            Operation::Takeover {
                terminal: "pty".into(),
                attachment: "writer".into()
            }
        )
        .await,
        Err(PtyError::Capacity)
    );
    resizing.abort();
    assert!(resizing.await.unwrap_err().is_cancelled());
    op(
        &scope,
        Operation::Detach {
            terminal: "pty".into(),
            attachment: "writer".into(),
            epoch: 1,
        },
    )
    .await
    .unwrap();
    let occupied = reserve(
        &term.shared.snapshots,
        term.shared.snapshots.available_permits(),
    )
    .unwrap();
    process.resize_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let changed = term.changed.notified();
            if !lock(&term.inner).resizing {
                break;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
    assert_eq!(lock(&term.inner).status.size, requested);
    drop(occupied);
    assert_eq!(term.shared.snapshots.available_permits(), SNAPSHOT_BYTES);
    scope.retire().await.unwrap();
}

#[tokio::test]
async fn cancelled_create_waiter_does_not_release_the_inflight_child_during_retirement() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let child = Arc::new(Fake::default());
    let (scope, term, _) = fixture_with_process(Some(Arc::new(GatedSpawn {
        entered: entered.clone(),
        release: release.clone(),
        child: child.clone(),
    })));
    scope.close_all(false).await.unwrap();
    drop(term);
    let caller = scope.clone();
    let creating = tokio::spawn(async move { caller.create(spawn_spec()).await });
    entered.notified().await;
    creating.abort();
    assert!(creating.await.unwrap_err().is_cancelled());
    assert_eq!(scope.creating.active.load(Ordering::Acquire), 1);
    let caller = scope.clone();
    let retiring = tokio::spawn(async move { caller.retire().await });
    tokio::task::yield_now().await;
    assert!(!retiring.is_finished());
    release.notify_one();
    retiring.await.unwrap().unwrap();
    assert!(child.stopped.load(Ordering::Acquire));
    assert_eq!(scope.shared.slots.available_permits(), 256);
}

mod execution;

#[tokio::test]
async fn admitted_creation_panic_is_unknown_and_releases_capacity_without_replay() {
    #[derive(Debug, Default)]
    struct PanicProvider(AtomicUsize);
    #[async_trait]
    impl PtyProcess for PanicProvider {
        async fn spawn(&self, _: PtyProcessSpec) -> rsi_process::Result<ManagedPtyProcess> {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("lost admitted creation result");
        }
    }
    let provider = Arc::new(PanicProvider::default());
    let (scope, term, _) = fixture_with_process(Some(provider.clone()));
    let before = term.shared.slots.available_permits();
    assert_eq!(
        scope.create(spawn_spec()).await,
        Err(PtyError::OutcomeUnknown)
    );
    assert_eq!(provider.0.load(Ordering::SeqCst), 1);
    assert_eq!(term.shared.slots.available_permits(), before);
    assert_eq!(
        native(rsi_process::ProcessError::OutcomeUnknown),
        PtyError::OutcomeUnknown
    );
    scope.retire().await.unwrap();
}
