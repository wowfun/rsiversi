//! Lossless bounded protocol pipes on the same process registry as batch jobs.
use super::*;
use rsi_process::{DuplexProcess, DuplexProcessSpec, ManagedDuplexProcess};
#[async_trait::async_trait]
impl DuplexProcess for Service {
    async fn spawn(&self, spec: DuplexProcessSpec) -> Result<ManagedDuplexProcess> {
        spec.validate()?;
        #[cfg(unix)]
        {
            let runtime = tokio::runtime::Handle::try_current()
                .map_err(|_| ProcessError::Spawn("Tokio runtime is unavailable".into()))?;
            self.spawn_duplex(spec, &runtime)
        }
        #[cfg(not(unix))]
        {
            let _ = spec;
            Err(ProcessError::Unsupported)
        }
    }
}
#[cfg(unix)]
mod native {
    use super::*;
    use rsi_process::{
        DuplexControl, DuplexInput, DuplexOutput, DuplexRead, MAXIMUM_DUPLEX_CHUNK_BYTES,
    };
    use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
    use tokio_util::sync::CancellationToken;
    #[derive(Debug)]
    struct InputCommand {
        bytes: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<usize>>,
        _permit: OwnedSemaphorePermit,
    }
    #[derive(Debug)]
    struct Input {
        sender: mpsc::Sender<InputCommand>,
        slot: Arc<Semaphore>,
        _reservation: Arc<CaptureReservation>,
    }
    impl Input {
        async fn command(&self, bytes: Option<&[u8]>) -> Result<usize> {
            let permit = self
                .slot
                .clone()
                .try_acquire_owned()
                .map_err(|_| ProcessError::Capacity)?;
            let (reply, wait) = oneshot::channel();
            self.sender
                .try_send(InputCommand {
                    bytes: bytes.map(<[u8]>::to_vec),
                    reply,
                    _permit: permit,
                })
                .map_err(|_| ProcessError::ShuttingDown)?;
            wait.await.map_err(|_| {
                ProcessError::Io("duplex write interrupted; accepted prefix is unknown".into())
            })?
        }
    }
    #[async_trait]
    impl DuplexInput for Input {
        async fn write(&self, bytes: &[u8]) -> Result<usize> {
            if bytes.is_empty() || bytes.len() > MAXIMUM_DUPLEX_CHUNK_BYTES {
                return Err(ProcessError::InvalidInput(
                    "duplex writes require 1..=64 KiB".into(),
                ));
            }
            self.command(Some(bytes)).await
        }
        async fn close(&self) -> Result<()> {
            self.command(None).await.map(|_| ())
        }
    }
    async fn write_input(
        mut stdin: tokio::process::ChildStdin,
        mut receiver: mpsc::Receiver<InputCommand>,
        stop: CancellationToken,
    ) {
        loop {
            let command = tokio::select! {biased;()=stop.cancelled()=>break,command=receiver.recv()=>match command {Some(command)=>command,None=>break}};
            let InputCommand {
                bytes,
                reply,
                _permit: permit,
            } = command;
            let Some(bytes) = bytes else {
                drop(stdin);
                drop(permit);
                let _ = reply.send(Ok(0));
                return;
            };
            let result = tokio::select! {biased;
                ()=stop.cancelled()=>Err(ProcessError::Io("duplex write interrupted; accepted prefix is unknown".into())),
                // Tokio's single write is cancellation-safe: Pending has accepted no bytes.
                // Never replace it with write_all, which may hide an accepted prefix.
                result=tokio::time::timeout(std::time::Duration::from_millis(500), stdin.write(&bytes))=>result.map_err(|_|ProcessError::Capacity).and_then(|result|result.map_err(|error|ProcessError::Io(error.to_string())).and_then(|size|if size==0 {Err(ProcessError::Io("duplex input closed during write".into()))} else {Ok(size)})),
            };
            let failed = result
                .as_ref()
                .is_err_and(|error| !matches!(error, ProcessError::Capacity));
            drop(permit);
            let _ = reply.send(result);
            if failed {
                break;
            }
        }
        // Receiver and its sole admitted command drop here; no caller future owns the pipe.
    }
    #[derive(Debug)]
    struct QueueState {
        bytes: VecDeque<u8>,
        closed: bool,
        error: Option<ProcessError>,
    }
    #[derive(Debug)]
    struct Output {
        capacity: usize,
        state: Mutex<QueueState>,
        data: Notify,
        space: Notify,
        reading: AtomicBool,
        _reservation: Arc<CaptureReservation>,
    }
    impl Output {
        fn finish(&self, result: Result<()>) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.closed {
                state.closed = true;
                state.error = result.err();
            }
            drop(state);
            self.data.notify_waiters();
            self.space.notify_waiters();
        }
    }
    /// Constructed before spawn so abort-before-first-poll also closes the stream.
    struct OutputCompletion(Arc<Output>);
    impl Drop for OutputCompletion {
        fn drop(&mut self) {
            self.0.finish(Err(ProcessError::Io(
                "duplex stdout task ended before publishing its result".into(),
            )));
        }
    }
    struct ReadGuard<'a>(&'a AtomicBool);
    impl Drop for ReadGuard<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    #[async_trait]
    impl DuplexOutput for Output {
        async fn read(&self, maximum: usize) -> Result<DuplexRead> {
            if !(1..=MAXIMUM_DUPLEX_CHUNK_BYTES).contains(&maximum) {
                return Err(ProcessError::InvalidInput(
                    "duplex reads require 1..=64 KiB".into(),
                ));
            }
            self.reading
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| ProcessError::Capacity)?;
            let _guard = ReadGuard(&self.reading);
            loop {
                let notified = self.data.notified();
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !state.bytes.is_empty() {
                        let size = maximum.min(state.bytes.len());
                        let bytes = state.bytes.drain(..size).collect();
                        let eof = state.closed && state.bytes.is_empty() && state.error.is_none();
                        drop(state);
                        self.space.notify_one();
                        return Ok(DuplexRead { bytes, eof });
                    }
                    if state.closed {
                        return state.error.clone().map_or_else(
                            || {
                                Ok(DuplexRead {
                                    bytes: vec![],
                                    eof: true,
                                })
                            },
                            Err,
                        );
                    }
                }
                notified.await;
            }
        }
    }
    async fn drain_stdout(
        mut reader: tokio::process::ChildStdout,
        output: Arc<Output>,
        stop: CancellationToken,
    ) -> Result<()> {
        let mut buffer = [0u8; 8192];
        loop {
            let notified = output.space.notified();
            let free = {
                let state = output
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                output.capacity - state.bytes.len()
            };
            if free == 0 {
                tokio::select! {biased;()=stop.cancelled()=>return Err(ProcessError::Io("duplex stdout cancelled before EOF".into())),()=notified=>{}};
                continue;
            }
            let size = free.min(buffer.len());
            let count = tokio::select! {biased;()=stop.cancelled()=>return Err(ProcessError::Io("duplex stdout cancelled before EOF".into())),result=reader.read(&mut buffer[..size])=>result.map_err(|error|ProcessError::Io(error.to_string()))?};
            if count == 0 {
                return Ok(());
            }
            output
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .bytes
                .extend(&buffer[..count]);
            output.data.notify_waiters();
        }
    }
    #[derive(Debug)]
    struct Control {
        child: Arc<ChildState>,
        input: Arc<Input>,
        output: Arc<Output>,
        stderr: Arc<Tail>,
    }
    #[async_trait]
    impl DuplexControl for Control {
        fn pid(&self) -> u32 {
            self.child.pid
        }
        fn stdin(&self) -> Arc<dyn DuplexInput> {
            self.input.clone()
        }
        fn stdout(&self) -> Arc<dyn DuplexOutput> {
            self.output.clone()
        }
        fn stderr(&self) -> Arc<dyn ProcessOutput> {
            self.stderr.clone()
        }
        fn terminate(&self) {
            self.child.terminate();
        }
        async fn wait(&self) -> Result<ProcessOutcome> {
            self.child.wait_outcome().await
        }
        async fn wait_settlement(&self) -> Result<()> {
            self.child.wait_settlement().await
        }
    }
    struct SupervisorResources {
        child: tokio::process::Child,
        state: Arc<ChildState>,
        input: Option<tokio::task::JoinHandle<()>>,
        input_stop: CancellationToken,
        drains: DrainTasks,
        output: Option<Arc<Output>>,
    }
    impl SupervisorResources {
        async fn settle(&mut self) -> (Result<ProcessOutcome>, Result<()>) {
            let outcome = reap_group(&mut self.child, &self.state).await;
            self.input_stop.cancel();
            if let Some(task) = &mut self.input {
                let _ = task.await;
                self.input = None;
            }
            let drain_error = self
                .drains
                .settle(self.state.grace)
                .await
                .map(ProcessError::Io);
            // Keep capture ownership through both joins, independently of stdout EOF.
            self.output.take();
            let reaped = outcome.as_ref().map(|_| ()).map_err(Clone::clone);
            (
                outcome.and_then(|outcome| drain_error.map_or(Ok(outcome), Err)),
                reaped,
            )
        }
    }
    /// Created before spawn, including the abort-before-first-poll case.
    struct SupervisorGuard(Option<SupervisorResources>);
    impl Drop for SupervisorGuard {
        fn drop(&mut self) {
            let Some(mut resources) = self.0.take() else {
                return;
            };
            resources.state.terminate();
            if let Some(task) = &resources.input {
                task.abort();
            }
            let runtime = resources.state.runtime.clone();
            let failure = RecoveryCompletion(resources.state.clone());
            runtime.spawn(async move {
                let _failure = failure;
                let (_, receipt) = resources.settle().await;
                resources.state.finish(
                    Err(ProcessError::Io(
                        "duplex supervisor ended before settlement".into(),
                    )),
                    receipt,
                );
            });
        }
    }
    struct RecoveryCompletion(Arc<ChildState>);
    impl Drop for RecoveryCompletion {
        fn drop(&mut self) {
            if self
                .0
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
            {
                self.0.recovery_interrupted();
            }
        }
    }
    impl Service {
        pub(super) fn spawn_duplex(
            &self,
            spec: DuplexProcessSpec,
            runtime: &tokio::runtime::Handle,
        ) -> Result<ManagedDuplexProcess> {
            self.spawn_duplex_tracked(spec, runtime)
                .map(|(process, _)| process)
        }
        fn spawn_duplex_tracked(
            &self,
            spec: DuplexProcessSpec,
            runtime: &tokio::runtime::Handle,
        ) -> Result<(ManagedDuplexProcess, tokio::task::AbortHandle)> {
            let spec = spec.into_process_spec();
            let (mut child, pid, capture_bytes) = self.admit_and_spawn(&spec, true)?;
            let reservation = Arc::new(CaptureReservation {
                service: Arc::downgrade(&self.state),
                bytes: capture_bytes,
            });
            let (sender, receiver) = mpsc::channel(1);
            let input = Arc::new(Input {
                sender,
                slot: Arc::new(Semaphore::new(1)),
                _reservation: reservation.clone(),
            });
            let output = Arc::new(Output {
                capacity: spec.stdout_max_bytes,
                state: Mutex::new(QueueState {
                    bytes: VecDeque::with_capacity(spec.stdout_max_bytes),
                    closed: false,
                    error: None,
                }),
                data: Notify::new(),
                space: Notify::new(),
                reading: AtomicBool::new(false),
                _reservation: reservation.clone(),
            });
            let stderr = Arc::new(Tail::new(spec.stderr_max_bytes, reservation));
            let stop = CancellationToken::new();
            let input_stop = CancellationToken::new();
            let (state, published) = self.publish_child(
                pid,
                spec.termination_grace_ms,
                runtime,
                Some(stop.clone()),
                spec.process.owner.clone(),
            );
            let stdin = child.stdin.take().expect("persistent piped stdin");
            let stdout = child.stdout.take().expect("piped stdout");
            let stderr_pipe = child.stderr.take().expect("piped stderr");
            let input_cancel = input_stop.clone();
            let terminate = stop.clone();
            let input_task=runtime.spawn(async move {tokio::select! {biased;()=terminate.cancelled()=>{},()=write_input(stdin,receiver,input_cancel)=>{}}});
            let completion = OutputCompletion(output.clone());
            let stdout_task = runtime.spawn(async move {
                let result = drain_stdout(stdout, completion.0.clone(), stop).await;
                completion.0.finish(result.clone());
                drop(completion);
                result
            });
            let stderr_task = runtime.spawn(drain(stderr_pipe, stderr.clone()));
            let guard = SupervisorGuard(Some(SupervisorResources {
                child,
                state: state.clone(),
                input: Some(input_task),
                input_stop,
                drains: DrainTasks::new(stdout_task, stderr_task),
                output: Some(output.clone()),
            }));
            let supervisor = runtime.spawn(async move {
                let mut guard = guard;
                let resources = guard.0.as_mut().expect("supervisor owns its resources");
                let (outcome, receipt) = resources.settle().await;
                resources.state.finish(outcome, receipt);
                guard.0 = None;
            });
            if !published {
                state.terminate();
                return Err(ProcessError::ShuttingDown);
            }
            Ok((
                ManagedDuplexProcess::new(Arc::new(Control {
                    child: state,
                    input,
                    output,
                    stderr,
                })),
                supervisor.abort_handle(),
            ))
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[derive(Debug, Default)]
        struct PanicOnceGroups(AtomicBool);
        impl ProcessGroups for PanicOnceGroups {
            fn signal(&self, pid: u32, tier: SignalTier) {
                SystemProcessGroups.signal(pid, tier);
            }
            fn is_alive(&self, pid: u32) -> bool {
                assert!(
                    self.0.swap(true, Ordering::AcqRel),
                    "injected supervisor panic after direct-child reap"
                );
                SystemProcessGroups.is_alive(pid)
            }
        }
        #[tokio::test]
        async fn panicking_supervisor_recovers_a_reaped_child_and_preserves_the_failure() {
            let service = Service::with_groups(
                ProcessLocalConfig::default(),
                Arc::new(PanicOnceGroups::default()),
            );
            let spec = crate::tests::immediate_process();
            let process = service
                .spawn_duplex(
                    DuplexProcessSpec {
                        process: spec.process,
                        environment: spec.environment,
                        stdout_buffer_bytes: 8,
                        stderr_max_bytes: 8,
                        termination_grace_ms: 50,
                    },
                    &tokio::runtime::Handle::current(),
                )
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), process.wait_settlement())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(process.wait().await, Err(ProcessError::Io(message)) if message.contains("supervisor"))
            );
            assert!(!SystemProcessGroups.is_alive(process.pid()));
            assert_eq!(lock_registry(&service.state).active, 0);
            service.shutdown().await.unwrap();
        }
        #[tokio::test]
        async fn interrupted_recovery_wakes_waiters_without_releasing_unreaped_ownership() {
            #[derive(Debug)]
            struct PlanDrop(Arc<AtomicBool>);
            impl Drop for PlanDrop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let service = Service::new(ProcessLocalConfig::default());
            let dropped = Arc::new(AtomicBool::new(false));
            let mut spec = crate::tests::immediate_process();
            spec.process.arguments[1] = "exec sleep 30".into();
            spec.process.owner = Some(rsi_sandbox::ProcessPlanOwner::new(PlanDrop(
                dropped.clone(),
            )));
            let (process, supervisor) = service
                .spawn_duplex_tracked(
                    DuplexProcessSpec {
                        process: spec.process,
                        environment: spec.environment,
                        stdout_buffer_bytes: 8,
                        stderr_max_bytes: 8,
                        termination_grace_ms: 50,
                    },
                    &tokio::runtime::Handle::current(),
                )
                .unwrap();
            let state = lock_registry(&service.state).managed[&process.pid()].clone();
            drop(RecoveryCompletion(state.clone()));
            assert!(matches!(
                process.wait_settlement().await,
                Err(ProcessError::Io(_))
            ));
            assert!(!dropped.load(Ordering::Acquire));
            assert!(!lock_registry(&service.state).accepting);
            assert_eq!(lock_registry(&service.state).active, 1);
            // The original supervisor is still available in this injected failure.
            // Await its actual reap/joins before explicitly releasing retained ownership.
            tokio::time::timeout(Duration::from_secs(2), async {
                while !supervisor.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the retained original supervisor must reap and join");
            process.wait_settlement().await.unwrap();
            assert!(dropped.load(Ordering::Acquire));
            assert_eq!(lock_registry(&service.state).active, 0);
            assert!(!lock_registry(&service.state).accepting);
            assert!(!SystemProcessGroups.is_alive(process.pid()));
        }
        #[tokio::test]
        async fn interrupted_supervisor_recovers_the_native_child_before_releasing_admission() {
            let service = Service::new(ProcessLocalConfig::default());
            let mut spec = crate::tests::immediate_process();
            spec.process.arguments[1] = "exec sleep 30".into();
            let (process, supervisor) = service
                .spawn_duplex_tracked(
                    DuplexProcessSpec {
                        process: spec.process,
                        environment: spec.environment,
                        stdout_buffer_bytes: 8,
                        stderr_max_bytes: 8,
                        termination_grace_ms: 50,
                    },
                    &tokio::runtime::Handle::current(),
                )
                .unwrap();
            let pid = process.pid();
            supervisor.abort();
            let receipt =
                tokio::time::timeout(Duration::from_secs(2), process.wait_settlement()).await;
            if receipt.is_err() {
                service.groups.signal(pid, SignalTier::Kill);
            }
            receipt
                .expect("supervisor loss must wake settlement waiters")
                .unwrap();
            assert!(
                matches!(process.wait().await, Err(ProcessError::Io(message)) if message.contains("supervisor"))
            );
            assert!(
                !service.groups.is_alive(pid),
                "native group must actually be gone"
            );
            assert_eq!(lock_registry(&service.state).active, 0);
            service.shutdown().await.unwrap();
        }
        fn output() -> Arc<Output> {
            Arc::new(Output {
                capacity: 8,
                state: Mutex::new(QueueState {
                    bytes: VecDeque::from(b"saved".to_vec()),
                    closed: false,
                    error: None,
                }),
                data: Notify::new(),
                space: Notify::new(),
                reading: AtomicBool::new(false),
                _reservation: Arc::new(CaptureReservation {
                    service: Weak::new(),
                    bytes: 8,
                }),
            })
        }
        #[tokio::test]
        async fn aborted_unpolled_stdout_guard_preserves_buffer_then_publishes_error() {
            let output = output();
            let guard = OutputCompletion(output.clone());
            let task = tokio::spawn(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            });
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            let read = output.read(8).await.unwrap();
            assert_eq!(read.bytes, b"saved");
            assert!(!read.eof);
            assert!(matches!(output.read(8).await, Err(ProcessError::Io(_))));
        }
        #[tokio::test]
        async fn panicking_stdout_guard_wakes_reader_and_first_result_wins() {
            let output = output();
            let guard = OutputCompletion(output.clone());
            assert!(
                tokio::spawn(async move {
                    let _guard = guard;
                    panic!("injected stdout panic");
                })
                .await
                .unwrap_err()
                .is_panic()
            );
            output.finish(Ok(()));
            assert!(!output.read(8).await.unwrap().eof);
            assert!(matches!(output.read(8).await, Err(ProcessError::Io(_))));
            let clean = self::output();
            let guard = OutputCompletion(clean.clone());
            clean.finish(Ok(()));
            drop(guard);
            assert!(clean.read(8).await.unwrap().eof);
        }
    }
}
