//! Lossless bounded protocol pipes on the same process registry as batch jobs.
use super::*;
use rsi_process::{DuplexProcess, DuplexProcessSpec, ManagedDuplexProcess};
impl DuplexProcess for Service {
    fn spawn(&self, spec: DuplexProcessSpec) -> Result<ManagedDuplexProcess> {
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
                result=stdin.write(&bytes)=>result.map_err(|error|ProcessError::Io(error.to_string())).and_then(|size|if size==0 {Err(ProcessError::Io("duplex input closed during write".into()))} else {Ok(size)}),
            };
            let failed = result.is_err();
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
        settlement: tokio::sync::watch::Receiver<Option<Result<()>>>,
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
            let mut settled = self.settlement.clone();
            loop {
                if let Some(result) = settled.borrow_and_update().clone() {
                    return result;
                }
                settled.changed().await.map_err(|_| {
                    ProcessError::Io("duplex supervisor ended before settlement".into())
                })?;
            }
        }
    }
    impl Service {
        pub(super) fn spawn_duplex(
            &self,
            spec: DuplexProcessSpec,
            runtime: &tokio::runtime::Handle,
        ) -> Result<ManagedDuplexProcess> {
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
            let waiting = state.clone();
            let final_output = output.clone();
            let (settled, settlement) = tokio::sync::watch::channel(None);
            runtime.spawn(async move {
                let outcome = reap_group(&mut child, &waiting).await;
                input_stop.cancel();
                let _ = input_task.await;
                let drain_error = settle_drains(stdout_task, stderr_task, waiting.grace)
                    .await
                    .map(ProcessError::Io);
                // Keep capture ownership through both joins, independently of stdout EOF.
                drop(final_output);
                let reaped = outcome.as_ref().map(|_| ()).map_err(Clone::clone);
                waiting.finish(outcome.and_then(|outcome| drain_error.map_or(Ok(outcome), Err)));
                settled.send_replace(Some(reaped));
            });
            if !published {
                state.terminate();
                return Err(ProcessError::ShuttingDown);
            }
            Ok(ManagedDuplexProcess::new(Arc::new(Control {
                child: state,
                settlement,
                input,
                output,
                stderr,
            })))
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
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
