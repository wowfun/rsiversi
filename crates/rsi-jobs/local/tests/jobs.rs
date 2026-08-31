use async_trait::async_trait;
use rsi_jobs::{
    JobControl, JobOutputRead, JobProducer, JobProducerRegistration, JobRequest, JobScopeId,
    JobStatus, JobStream, JobSubmission, JobTerminal, Jobs, JobsContract, JobsError, Result,
};
use rsi_jobs_local::JobsLocalFactory;
use rsi_meta::{FiberHandle, ResolvedFactory, Runtime, UpdateMode};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
enum TestSettlement {
    Immediate(JobStatus),
    OnRelease,
    OnCancel,
    IgnoreCancelUntilRelease,
    ErrorWithNul,
}

#[derive(Debug)]
struct TestControl {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    settlement: TestSettlement,
    cancelled: CancellationToken,
    release: CancellationToken,
    cancel_count: AtomicUsize,
    wait_count: AtomicUsize,
}

impl TestControl {
    fn new(settlement: TestSettlement, stdout: &[u8], stderr: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
            settlement,
            cancelled: CancellationToken::new(),
            release: CancellationToken::new(),
            cancel_count: AtomicUsize::new(0),
            wait_count: AtomicUsize::new(0),
        })
    }

    fn terminal(status: JobStatus) -> JobTerminal {
        JobTerminal {
            status,
            exit_code: (status == JobStatus::Completed).then_some(0),
            signal: (status == JobStatus::Cancelled).then_some(15),
            message: (status == JobStatus::Failed).then(|| "producer failed".into()),
        }
    }
}

#[async_trait]
impl JobControl for TestControl {
    fn read(&self, stream: JobStream, offset: u64) -> Result<JobOutputRead> {
        let bytes = match stream {
            JobStream::Stdout => &self.stdout,
            JobStream::Stderr => &self.stderr,
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        Ok(JobOutputRead {
            bytes: bytes[start..].to_vec(),
            oldest_offset: 0,
            next_offset: bytes.len() as u64,
            lossy: false,
        })
    }

    fn cancel(&self) {
        self.cancel_count.fetch_add(1, Ordering::AcqRel);
        self.cancelled.cancel();
    }

    async fn wait(&self) -> Result<JobTerminal> {
        self.wait_count.fetch_add(1, Ordering::AcqRel);
        match self.settlement {
            TestSettlement::Immediate(status) => Ok(Self::terminal(status)),
            TestSettlement::OnRelease => {
                self.release.cancelled().await;
                Ok(Self::terminal(JobStatus::Completed))
            }
            TestSettlement::OnCancel => {
                self.cancelled.cancelled().await;
                Ok(Self::terminal(JobStatus::Cancelled))
            }
            TestSettlement::IgnoreCancelUntilRelease => {
                self.release.cancelled().await;
                Ok(Self::terminal(JobStatus::Cancelled))
            }
            TestSettlement::ErrorWithNul => Err(JobsError::Execution("producer\0failure".into())),
        }
    }
}

#[derive(Debug)]
struct RacingReadControl {
    stdout_reads: AtomicUsize,
    release: CancellationToken,
    settled: Mutex<bool>,
    settled_changed: Condvar,
}

impl RacingReadControl {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stdout_reads: AtomicUsize::new(0),
            release: CancellationToken::new(),
            settled: Mutex::new(false),
            settled_changed: Condvar::new(),
        })
    }
}

#[async_trait]
impl JobControl for RacingReadControl {
    fn read(&self, stream: JobStream, offset: u64) -> Result<JobOutputRead> {
        let bytes = match stream {
            JobStream::Stdout if self.stdout_reads.fetch_add(1, Ordering::AcqRel) == 0 => {
                self.release.cancel();
                b"before".as_slice()
            }
            JobStream::Stdout => b"before-after".as_slice(),
            JobStream::Stderr => {
                let mut settled = self.settled.lock().unwrap();
                while !*settled {
                    settled = self.settled_changed.wait(settled).unwrap();
                }
                b"".as_slice()
            }
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        Ok(JobOutputRead {
            bytes: bytes[start..].to_vec(),
            oldest_offset: 0,
            next_offset: bytes.len() as u64,
            lossy: false,
        })
    }

    fn cancel(&self) {
        self.release.cancel();
    }

    async fn wait(&self) -> Result<JobTerminal> {
        self.release.cancelled().await;
        *self.settled.lock().unwrap() = true;
        self.settled_changed.notify_all();
        Ok(TestControl::terminal(JobStatus::Completed))
    }
}

#[derive(Debug)]
struct EvictionRaceControl {
    stdout_reads: AtomicUsize,
    read_state: Mutex<(bool, bool)>,
    read_changed: Condvar,
    settlement: CancellationToken,
}

#[derive(Debug)]
struct FinalizationReportRaceControl {
    stdout_reads: AtomicUsize,
    read_state: Mutex<([bool; 2], [bool; 2])>,
    read_changed: Condvar,
}

impl FinalizationReportRaceControl {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stdout_reads: AtomicUsize::new(0),
            read_state: Mutex::new(([false; 2], [false; 2])),
            read_changed: Condvar::new(),
        })
    }

    fn wait_until_blocked(&self, index: usize) {
        let mut state = self.read_state.lock().unwrap();
        while !state.0[index] {
            state = self.read_changed.wait(state).unwrap();
        }
    }

    fn release(&self, index: usize) {
        self.read_state.lock().unwrap().1[index] = true;
        self.read_changed.notify_all();
    }
}

#[async_trait]
impl JobControl for FinalizationReportRaceControl {
    fn read(&self, stream: JobStream, _offset: u64) -> Result<JobOutputRead> {
        if stream == JobStream::Stdout {
            let call = self.stdout_reads.fetch_add(1, Ordering::AcqRel);
            if (1..=2).contains(&call) {
                let index = call - 1;
                let mut state = self.read_state.lock().unwrap();
                state.0[index] = true;
                self.read_changed.notify_all();
                while !state.1[index] {
                    state = self.read_changed.wait(state).unwrap();
                }
            }
        }
        Ok(JobOutputRead {
            bytes: b"stable".to_vec(),
            oldest_offset: 0,
            next_offset: 6,
            lossy: false,
        })
    }

    fn cancel(&self) {}

    async fn wait(&self) -> Result<JobTerminal> {
        Ok(TestControl::terminal(JobStatus::Completed))
    }
}

impl EvictionRaceControl {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stdout_reads: AtomicUsize::new(0),
            read_state: Mutex::new((false, false)),
            read_changed: Condvar::new(),
            settlement: CancellationToken::new(),
        })
    }

    fn wait_until_reading(&self) {
        let mut state = self.read_state.lock().unwrap();
        while !state.0 {
            state = self.read_changed.wait(state).unwrap();
        }
    }

    fn release_read(&self) {
        self.read_state.lock().unwrap().1 = true;
        self.read_changed.notify_all();
    }
}

#[async_trait]
impl JobControl for EvictionRaceControl {
    fn read(&self, stream: JobStream, offset: u64) -> Result<JobOutputRead> {
        let bytes = if stream == JobStream::Stdout
            && self.stdout_reads.fetch_add(1, Ordering::AcqRel) == 0
        {
            let mut state = self.read_state.lock().unwrap();
            state.0 = true;
            self.read_changed.notify_all();
            while !state.1 {
                state = self.read_changed.wait(state).unwrap();
            }
            b"stable".as_slice()
        } else if stream == JobStream::Stdout {
            b"stable".as_slice()
        } else {
            b"".as_slice()
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        Ok(JobOutputRead {
            bytes: bytes[start..].to_vec(),
            oldest_offset: 0,
            next_offset: bytes.len() as u64,
            lossy: false,
        })
    }

    fn cancel(&self) {
        self.settlement.cancel();
    }

    async fn wait(&self) -> Result<JobTerminal> {
        self.settlement.cancelled().await;
        Ok(TestControl::terminal(JobStatus::Completed))
    }
}

#[derive(Debug)]
struct PanickingCallbackControl {
    panic_read: bool,
    panic_cancel: bool,
    release: CancellationToken,
}

#[async_trait]
impl JobControl for PanickingCallbackControl {
    fn read(&self, _stream: JobStream, _offset: u64) -> Result<JobOutputRead> {
        assert!(!self.panic_read, "injected read panic");
        Ok(JobOutputRead {
            bytes: Vec::new(),
            oldest_offset: 0,
            next_offset: 0,
            lossy: false,
        })
    }

    fn cancel(&self) {
        assert!(!self.panic_cancel, "injected cancel panic");
        self.release.cancel();
    }

    async fn wait(&self) -> Result<JobTerminal> {
        self.release.cancelled().await;
        Ok(TestControl::terminal(JobStatus::Completed))
    }
}

#[derive(Clone, Debug)]
struct TestRequest(Arc<TestControl>);

#[derive(Debug, Default)]
struct TestProducer {
    starts: AtomicUsize,
}

impl JobProducer for TestProducer {
    fn start(&self, request: &JobRequest) -> Result<Arc<dyn JobControl>> {
        self.starts.fetch_add(1, Ordering::AcqRel);
        if let Some(request) = request.downcast_ref::<TestRequest>() {
            return Ok(request.0.clone());
        }
        request
            .downcast_ref::<Arc<dyn JobControl>>()
            .cloned()
            .ok_or_else(|| {
                JobsError::InvalidInput("test producer received the wrong request type".into())
            })
    }
}

#[derive(Debug)]
struct FailingProducer;

impl JobProducer for FailingProducer {
    fn start(&self, _request: &JobRequest) -> Result<Arc<dyn JobControl>> {
        Err(JobsError::Execution("start rejected".into()))
    }
}

async fn activated(config: Value) -> (Runtime, FiberHandle, Arc<dyn Jobs>) {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.jobs.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(JobsLocalFactory),
            ),
            config,
        )
        .await
        .unwrap();
    let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
    (runtime, fiber, jobs)
}

fn scope(name: &str) -> JobScopeId {
    JobScopeId::new("test", [name]).unwrap()
}

fn registration(name: &str, producer: Arc<dyn JobProducer>) -> JobProducerRegistration {
    JobProducerRegistration {
        name: name.into(),
        producer,
    }
}

fn submission(producer: &str, name: &str, control: Arc<TestControl>) -> JobSubmission {
    JobSubmission {
        name: name.into(),
        producer: producer.into(),
        request: JobRequest::new(TestRequest(control)),
        requires_report: true,
    }
}

async fn wait_terminal(jobs: &Arc<dyn Jobs>, authority: &rsi_jobs::JobScopeAuthority, id: &str) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if jobs.get(authority, id).unwrap().status.is_terminal() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn producer_wait_error_is_projected_to_a_valid_failed_terminal() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("invalid-error-text")).unwrap();
    let id = jobs
        .submit(
            &authority,
            submission(
                "test",
                "invalid-error-text",
                TestControl::new(TestSettlement::ErrorWithNul, b"", b""),
            ),
        )
        .unwrap();
    wait_terminal(&jobs, &authority, &id).await;

    let terminal = jobs
        .get(&authority, &id)
        .unwrap()
        .terminal
        .expect("failed work has terminal evidence");
    assert_eq!(terminal.status, JobStatus::Failed);
    terminal.validate().unwrap();
    assert!(!terminal.message.unwrap().contains('\0'));

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn admission_preflights_authority_producer_and_capacity_before_id_publication() {
    let (runtime, fiber, jobs) = activated(json!({"maximum_active_jobs_per_scope":1})).await;
    let producer = Arc::new(TestProducer::default());
    let _lease = jobs
        .register_producer(registration("test", producer.clone()))
        .unwrap();
    let _failing = jobs
        .register_producer(registration("fail", Arc::new(FailingProducer)))
        .unwrap();
    assert!(matches!(
        jobs.register_producer(registration("test", producer.clone())),
        Err(JobsError::DuplicateProducer(name)) if name == "test"
    ));

    let authority = jobs.acquire_scope(scope("a")).unwrap();
    let waiting = TestControl::new(TestSettlement::OnRelease, b"first", b"");
    assert!(matches!(
        jobs.submit(
            &authority,
            submission(
                "missing",
                "unknown",
                TestControl::new(TestSettlement::Immediate(JobStatus::Completed), b"", b""),
            ),
        ),
        Err(JobsError::UnknownProducer(name)) if name == "missing"
    ));
    assert!(matches!(
        jobs.submit(
            &authority,
            submission(
                "fail",
                "failed-start",
                TestControl::new(TestSettlement::Immediate(JobStatus::Completed), b"", b""),
            ),
        ),
        Err(JobsError::Execution(message)) if message == "start rejected"
    ));
    let first = jobs
        .submit(&authority, submission("test", "first", waiting.clone()))
        .unwrap();
    assert_eq!(first, "job-1");
    assert!(matches!(
        jobs.submit(
            &authority,
            submission(
                "test",
                "over-capacity",
                TestControl::new(TestSettlement::Immediate(JobStatus::Completed), b"", b""),
            ),
        ),
        Err(JobsError::Capacity)
    ));
    assert_eq!(producer.starts.load(Ordering::Acquire), 1);
    waiting.release.cancel();
    wait_terminal(&jobs, &authority, &first).await;

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn active_reads_do_not_report_but_terminal_reads_atomically_release_output() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("reporting")).unwrap();
    let control = TestControl::new(TestSettlement::OnRelease, b"hello", b"warning");
    let id = jobs
        .submit(
            &authority,
            submission("test", "observable", control.clone()),
        )
        .unwrap();

    let active = jobs.read(&authority, &id, 1, 0).unwrap();
    assert_eq!(active.stdout.bytes, b"ello");
    assert_eq!(active.stderr.bytes, b"warning");
    assert!(!active.job.reported);
    assert!(active.job.output_retained);

    control.release.cancel();
    wait_terminal(&jobs, &authority, &id).await;
    let terminal = jobs.read(&authority, &id, 0, 0).unwrap();
    assert_eq!(terminal.stdout.bytes, b"hello");
    assert_eq!(terminal.stderr.bytes, b"warning");
    assert!(terminal.job.reported);
    assert!(!terminal.job.output_retained);
    assert_eq!(terminal.job.status, JobStatus::Completed);

    let compacted = jobs.read(&authority, &id, 0, 0).unwrap();
    assert!(compacted.stdout.bytes.is_empty());
    assert_eq!(compacted.stdout.oldest_offset, 5);
    assert_eq!(compacted.stdout.next_offset, 5);
    assert!(compacted.stdout.lossy);
    assert_eq!(compacted.stderr.oldest_offset, 7);

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_racing_settlement_returns_one_coherent_active_or_terminal_snapshot() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("read-race")).unwrap();
    let control: Arc<dyn JobControl> = RacingReadControl::new();
    let id = jobs
        .submit(
            &authority,
            JobSubmission {
                name: "racing-read".into(),
                producer: "test".into(),
                request: JobRequest::new(control),
                requires_report: true,
            },
        )
        .unwrap();

    let read = jobs.read(&authority, &id, 0, 0).unwrap();

    if read.job.status.is_terminal() {
        assert_eq!(read.stdout.bytes, b"before-after");
        assert!(read.job.reported);
        assert!(!read.job.output_retained);
    } else {
        assert_eq!(read.stdout.bytes, b"before");
        assert!(!read.job.reported);
        assert!(read.job.output_retained);
    }
    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_read_survives_concurrent_terminal_tombstone_compaction() {
    let (_runtime, fiber, jobs) = activated(json!({
        "maximum_active_jobs_per_scope":1,
        "maximum_active_jobs":1,
        "maximum_retained_jobs_per_scope":1,
        "maximum_retained_jobs":1
    }))
    .await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("read-eviction-race")).unwrap();
    let control = EvictionRaceControl::new();
    let control_for_request: Arc<dyn JobControl> = control.clone();
    let id = jobs
        .submit(
            &authority,
            JobSubmission {
                name: "racing-eviction".into(),
                producer: "test".into(),
                request: JobRequest::new(control_for_request),
                requires_report: false,
            },
        )
        .unwrap();

    let reader_jobs = jobs.clone();
    let reader_scope = authority.clone();
    let reader_id = id.clone();
    let reader = std::thread::spawn(move || reader_jobs.read(&reader_scope, &reader_id, 0, 0));
    control.wait_until_reading();
    control.settlement.cancel();
    wait_terminal(&jobs, &authority, &id).await;
    let replacement = || JobSubmission {
        name: "replacement".into(),
        producer: "test".into(),
        request: JobRequest::new(
            TestControl::new(TestSettlement::OnCancel, b"", b"") as Arc<dyn JobControl>
        ),
        requires_report: false,
    };
    assert_eq!(
        jobs.submit(&authority, replacement()).unwrap_err(),
        JobsError::Capacity
    );
    control.release_read();

    let read = reader.join().unwrap().unwrap();
    assert_eq!(read.stdout.bytes, b"stable");
    assert_eq!(read.job.status, JobStatus::Completed);
    jobs.submit(&authority, replacement()).unwrap();
    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_terminal_read_wins_over_the_scope_finalization_snapshot() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs
        .acquire_scope(scope("finalization-report-race"))
        .unwrap();
    let control = FinalizationReportRaceControl::new();
    let control_for_request: Arc<dyn JobControl> = control.clone();
    let id = jobs
        .submit(
            &authority,
            JobSubmission {
                name: "report-race".into(),
                producer: "test".into(),
                request: JobRequest::new(control_for_request),
                requires_report: true,
            },
        )
        .unwrap();
    wait_terminal(&jobs, &authority, &id).await;

    let reader = std::thread::spawn({
        let jobs = Arc::clone(&jobs);
        let authority = authority.clone();
        let id = id.clone();
        move || jobs.read(&authority, &id, 0, 0)
    });
    control.wait_until_blocked(0);
    let finalization = tokio::spawn({
        let jobs = Arc::clone(&jobs);
        let authority = authority.clone();
        async move { jobs.finalize_scope(&authority).await }
    });
    control.wait_until_blocked(1);

    control.release(0);
    assert!(reader.join().unwrap().unwrap().job.reported);
    control.release(1);
    assert!(finalization.await.unwrap().unwrap().unreported.is_empty());

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn producer_read_and_cancel_panics_are_contained_as_jobs_errors() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("callback-panics")).unwrap();

    let read_release = CancellationToken::new();
    read_release.cancel();
    let read_control: Arc<dyn JobControl> = Arc::new(PanickingCallbackControl {
        panic_read: true,
        panic_cancel: false,
        release: read_release,
    });
    let read_id = jobs
        .submit(
            &authority,
            JobSubmission {
                name: "panic-read".into(),
                producer: "test".into(),
                request: JobRequest::new(read_control),
                requires_report: true,
            },
        )
        .unwrap();
    wait_terminal(&jobs, &authority, &read_id).await;
    assert!(matches!(
        jobs.read(&authority, &read_id, 0, 0),
        Err(JobsError::Execution(message)) if message.contains("read panicked")
    ));

    let cancel_release = CancellationToken::new();
    let cancel_control: Arc<dyn JobControl> = Arc::new(PanickingCallbackControl {
        panic_read: false,
        panic_cancel: true,
        release: cancel_release.clone(),
    });
    let cancel_id = jobs
        .submit(
            &authority,
            JobSubmission {
                name: "panic-cancel".into(),
                producer: "test".into(),
                request: JobRequest::new(cancel_control),
                requires_report: true,
            },
        )
        .unwrap();
    assert!(matches!(
        jobs.kill(&authority, &cancel_id).await,
        Err(JobsError::Execution(message)) if message.contains("cancellation panicked")
    ));
    cancel_release.cancel();
    jobs.wait(&authority, &cancel_id, 0, 0).await.unwrap();

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn wait_and_kill_report_terminal_work_and_are_idempotently_observable() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("wait-kill")).unwrap();

    let completed = jobs
        .submit(
            &authority,
            submission(
                "test",
                "complete",
                TestControl::new(TestSettlement::Immediate(JobStatus::Completed), b"ok", b""),
            ),
        )
        .unwrap();
    let waited = jobs.wait(&authority, &completed, 0, 0).await.unwrap();
    assert_eq!(waited.job.status, JobStatus::Completed);
    assert!(waited.job.reported);
    let waited_again = jobs.wait(&authority, &completed, 0, 0).await.unwrap();
    assert_eq!(waited_again.job, waited.job);
    assert!(waited_again.stdout.bytes.is_empty());
    assert!(waited_again.stdout.lossy);

    let cancellable = TestControl::new(TestSettlement::OnCancel, b"partial", b"");
    let killed_id = jobs
        .submit(
            &authority,
            submission("test", "cancel", cancellable.clone()),
        )
        .unwrap();
    let killed = jobs.kill(&authority, &killed_id).await.unwrap();
    assert_eq!(killed.job.status, JobStatus::Cancelled);
    assert!(killed.job.reported);
    assert!(cancellable.cancel_count.load(Ordering::Acquire) >= 1);
    let killed_again = jobs.kill(&authority, &killed_id).await.unwrap();
    assert_eq!(killed_again.job, killed.job);
    assert!(killed_again.stdout.bytes.is_empty());
    assert_eq!(killed_again.stdout.oldest_offset, 7);
    assert_eq!(killed_again.stdout.next_offset, 7);
    assert!(killed_again.stdout.lossy);

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn completed_terminal_wins_after_a_kill_request() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("completed-wins")).unwrap();
    let control = TestControl::new(TestSettlement::OnRelease, b"complete", b"");
    let id = jobs
        .submit(
            &authority,
            submission("test", "completed-wins", control.clone()),
        )
        .unwrap();

    let kill = tokio::spawn({
        let jobs = Arc::clone(&jobs);
        let authority = authority.clone();
        let id = id.clone();
        async move { jobs.kill(&authority, &id).await }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while control.cancel_count.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    control.release.cancel();

    let killed = kill.await.unwrap().unwrap();
    assert_eq!(killed.job.status, JobStatus::Completed);
    let terminal = killed.job.terminal.as_ref().unwrap();
    assert_eq!(terminal.exit_code, Some(0));
    assert_eq!(terminal.signal, None);
    assert!(killed.job.reported);

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn finalization_is_scope_isolated_reports_unobserved_work_and_revokes_old_clones() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("turn-a")).unwrap();
    let old_clone = authority.clone();
    let other = jobs.acquire_scope(scope("turn-b")).unwrap();

    let active = TestControl::new(TestSettlement::OnCancel, b"active", b"");
    let active_id = jobs
        .submit(&authority, submission("test", "active", active.clone()))
        .unwrap();
    let terminal_id = jobs
        .submit(
            &authority,
            submission(
                "test",
                "already-terminal",
                TestControl::new(TestSettlement::Immediate(JobStatus::Failed), b"", b"bad"),
            ),
        )
        .unwrap();
    wait_terminal(&jobs, &authority, &terminal_id).await;
    let other_control = TestControl::new(TestSettlement::OnRelease, b"other", b"");
    let other_id = jobs
        .submit(&other, submission("test", "other", other_control.clone()))
        .unwrap();

    let finalization = jobs.finalize_scope(&authority).await.unwrap();
    assert_eq!(
        finalization
            .unreported
            .iter()
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>(),
        [active_id.as_str(), terminal_id.as_str()]
    );
    assert_eq!(active.cancel_count.load(Ordering::Acquire), 1);
    assert!(!authority.is_active());
    assert!(matches!(
        jobs.submit(
            &old_clone,
            submission(
                "test",
                "stale",
                TestControl::new(TestSettlement::Immediate(JobStatus::Completed), b"", b""),
            ),
        ),
        Err(JobsError::ScopeClosed)
    ));
    assert_eq!(
        jobs.get(&other, &other_id).unwrap().status,
        JobStatus::Running
    );
    other_control.release.cancel();
    jobs.wait(&other, &other_id, 0, 0).await.unwrap();

    let replacement = jobs.acquire_scope(scope("turn-a")).unwrap();
    assert!(!replacement.same_generation(&authority));

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn cancelled_finalization_future_cannot_abandon_the_provider_reaper() {
    let (_runtime, fiber, jobs) = activated(json!({"shutdown_timeout_ms":5})).await;
    let lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("dropped-finalizer")).unwrap();
    let control = TestControl::new(TestSettlement::IgnoreCancelUntilRelease, b"", b"");
    let _id = jobs
        .submit(&authority, submission("test", "slow", control.clone()))
        .unwrap();

    assert_eq!(
        jobs.finalize_scope(&authority).await,
        Err(JobsError::CancellationTimeout)
    );
    assert_eq!(control.cancel_count.load(Ordering::Acquire), 1);
    control.release.cancel();
    tokio::time::timeout(Duration::from_secs(1), async {
        while control.wait_count.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    lease.retire().await.unwrap();

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn producer_retirement_withdraws_exact_generation_cancels_and_allows_replacement() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("retire")).unwrap();
    let control = TestControl::new(TestSettlement::OnCancel, b"", b"");
    let id = jobs
        .submit(&authority, submission("test", "old", control.clone()))
        .unwrap();
    lease.retire().await.unwrap();
    wait_terminal(&jobs, &authority, &id).await;
    assert_eq!(
        jobs.get(&authority, &id).unwrap().status,
        JobStatus::Cancelled
    );
    assert!(matches!(
        jobs.submit(
            &authority,
            submission(
                "test",
                "withdrawn",
                TestControl::new(TestSettlement::Immediate(JobStatus::Completed), b"", b""),
            ),
        ),
        Err(JobsError::UnknownProducer(name)) if name == "test"
    ));
    let replacement = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let replacement_id = jobs
        .submit(
            &authority,
            submission(
                "test",
                "new",
                TestControl::new(TestSettlement::Immediate(JobStatus::Completed), b"", b""),
            ),
        )
        .unwrap();
    assert_eq!(replacement_id, "job-2");
    replacement.retire().await.unwrap();

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn unreported_terminal_records_backpressure_and_reported_tombstones_evict_oldest() {
    let (_runtime, fiber, jobs) = activated(json!({
        "maximum_active_jobs_per_scope":2,
        "maximum_active_jobs":3,
        "maximum_retained_jobs_per_scope":2,
        "maximum_retained_jobs":3
    }))
    .await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("retention")).unwrap();
    let immediate = || {
        TestControl::new(
            TestSettlement::Immediate(JobStatus::Completed),
            b"done",
            b"",
        )
    };
    let first = jobs
        .submit(&authority, submission("test", "first", immediate()))
        .unwrap();
    let second = jobs
        .submit(&authority, submission("test", "second", immediate()))
        .unwrap();
    wait_terminal(&jobs, &authority, &first).await;
    wait_terminal(&jobs, &authority, &second).await;
    assert!(matches!(
        jobs.submit(&authority, submission("test", "blocked", immediate())),
        Err(JobsError::Capacity)
    ));

    jobs.read(&authority, &first, 0, 0).unwrap();
    let third = jobs
        .submit(&authority, submission("test", "third", immediate()))
        .unwrap();
    assert_eq!(third, "job-3");
    assert!(matches!(
        jobs.get(&authority, &first),
        Err(JobsError::UnknownJob(id)) if id == first
    ));
    assert_eq!(
        jobs.list(&authority)
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect::<Vec<_>>(),
        [second, third]
    );

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn terminal_jobs_not_requiring_report_compact_automatically() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let _lease = jobs
        .register_producer(registration("test", Arc::new(TestProducer::default())))
        .unwrap();
    let authority = jobs.acquire_scope(scope("auto-report")).unwrap();
    let mut request = submission(
        "test",
        "internal",
        TestControl::new(
            TestSettlement::Immediate(JobStatus::Completed),
            b"secret",
            b"",
        ),
    );
    request.requires_report = false;
    let id = jobs.submit(&authority, request).unwrap();
    wait_terminal(&jobs, &authority, &id).await;
    let summary = jobs.get(&authority, &id).unwrap();
    assert!(summary.reported);
    assert!(!summary.output_retained);
    assert!(
        jobs.read(&authority, &id, 0, 0)
            .unwrap()
            .stdout
            .bytes
            .is_empty()
    );

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}

#[test]
fn submission_without_an_entered_tokio_runtime_fails_without_starting_producer() {
    let asynchronous = tokio::runtime::Runtime::new().unwrap();
    let (runtime, fiber, jobs, producer) = asynchronous.block_on(async {
        let (runtime, fiber, jobs) = activated(json!({})).await;
        let producer = Arc::new(TestProducer::default());
        let lease = jobs
            .register_producer(registration("test", producer.clone()))
            .unwrap();
        std::mem::forget(lease);
        (runtime, fiber, jobs, producer)
    });
    let authority = jobs.acquire_scope(scope("outside-runtime")).unwrap();
    assert!(matches!(
        jobs.submit(
            &authority,
            submission(
                "test",
                "outside",
                TestControl::new(TestSettlement::Immediate(JobStatus::Completed), b"", b""),
            ),
        ),
        Err(JobsError::Execution(message)) if message == "Tokio runtime is unavailable"
    ));
    assert_eq!(producer.starts.load(Ordering::Acquire), 0);
    asynchronous.block_on(async {
        drop(jobs);
        assert!(fiber.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_complete());
    });
}

#[derive(Debug)]
struct BlockingStartProducer {
    entered: Arc<Notify>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    control: Arc<TestControl>,
}

impl JobProducer for BlockingStartProducer {
    fn start(&self, _request: &JobRequest) -> Result<Arc<dyn JobControl>> {
        self.entered.notify_one();
        let (lock, changed) = &*self.release;
        let mut released = lock.lock().unwrap();
        while !*released {
            released = changed.wait(released).unwrap();
        }
        Ok(self.control.clone())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn racing_scope_revocation_cancels_started_work_without_publishing_an_id() {
    let (_runtime, fiber, jobs) = activated(json!({})).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let control = TestControl::new(TestSettlement::IgnoreCancelUntilRelease, b"", b"");
    let _lease = jobs
        .register_producer(registration(
            "blocking",
            Arc::new(BlockingStartProducer {
                entered: entered.clone(),
                release: release.clone(),
                control: control.clone(),
            }),
        ))
        .unwrap();
    let authority = jobs.acquire_scope(scope("race")).unwrap();
    let submitting_jobs = jobs.clone();
    let submitting_authority = authority.clone();
    let submit = tokio::task::spawn_blocking(move || {
        submitting_jobs.submit(
            &submitting_authority,
            JobSubmission {
                name: "racing".into(),
                producer: "blocking".into(),
                request: JobRequest::new(()),
                requires_report: true,
            },
        )
    });
    entered.notified().await;
    let finalizing_jobs = jobs.clone();
    let finalizing_authority = authority.clone();
    let finalization =
        tokio::spawn(async move { finalizing_jobs.finalize_scope(&finalizing_authority).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while authority.is_active() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    {
        let (lock, changed) = &*release;
        *lock.lock().unwrap() = true;
        changed.notify_all();
    }
    assert_eq!(submit.await.unwrap(), Err(JobsError::ScopeClosed));
    assert!(control.cancel_count.load(Ordering::Acquire) >= 1);
    tokio::task::yield_now().await;
    assert!(
        !finalization.is_finished(),
        "scope finalization returned before unpublished work settled"
    );
    control.release.cancel();
    assert_eq!(finalization.await.unwrap().unwrap().unreported, []);

    let replacement = jobs.acquire_scope(scope("race")).unwrap();
    let next = jobs
        .submit(
            &replacement,
            JobSubmission {
                name: "next".into(),
                producer: "blocking".into(),
                request: JobRequest::new(()),
                requires_report: false,
            },
        )
        .unwrap();
    assert_eq!(next, "job-1");

    drop(jobs);
    assert!(fiber.dispose().await.is_clean());
}
