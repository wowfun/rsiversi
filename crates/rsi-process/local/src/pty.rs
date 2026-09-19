//! Linux controlling PTYs share the ordinary Process registry and cleanup lifetime.
use super::Service;
use rsi_process::{ManagedPtyProcess, ProcessError, PtyProcess, PtyProcessSpec, Result};
impl PtyProcess for Service {
    fn spawn(&self, spec: PtyProcessSpec) -> Result<ManagedPtyProcess> {
        spec.validate()?;
        #[cfg(target_os = "linux")]
        {
            self.spawn_pty(&spec)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = spec;
            Err(ProcessError::Unsupported)
        }
    }
}
#[cfg(target_os = "linux")]
mod native {
    use super::{ManagedPtyProcess, ProcessError, PtyProcessSpec, Result, Service};
    use crate::{
        CaptureReservation, ChildState, POST_KILL_GROUP_SETTLEMENT_TIMEOUT, rollback_admission,
        wait_for_group_disappearance,
    };
    use async_trait::async_trait;
    use portable_pty::{CommandBuilder, MasterPty};
    use rsi_process::{MAXIMUM_PTY_IO_BYTES, ProcessOutcome, PtyControl, PtyRead, PtySize};
    use std::fs::File;
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt as _;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::unix::AsyncFd;
    use tokio::sync::{Semaphore, mpsc, oneshot};
    use tokio_util::sync::CancellationToken;
    const CHUNK: usize = 8192;
    const OUTPUT_CHUNKS: usize = 8;
    const RESERVED: usize = CHUNK * (OUTPUT_CHUNKS + 1);
    const INPUT_DEADLINE: Duration = Duration::from_millis(500);

    // The pinned Linux portable-pty implementation returns std::process::Child.
    // Retain that native status so Unix signal numbers are not reduced to text.
    struct Reaper(Option<Box<dyn portable_pty::Child + Send + Sync>>);
    impl Reaper {
        fn wait(&mut self) -> Result<ProcessOutcome> {
            let child: &mut dyn portable_pty::Child =
                self.0.as_mut().expect("owned child").as_mut();
            let child = child
                .downcast_mut::<std::process::Child>()
                .ok_or(ProcessError::Unsupported)?;
            let status = child
                .wait()
                .map_err(|error| ProcessError::Io(error.to_string()))?;
            self.0.take();
            Ok(ProcessOutcome {
                exit_code: status.code(),
                signal: status.signal(),
            })
        }
    }
    impl Drop for Reaper {
        fn drop(&mut self) {
            // Thread creation failure or panic must still kill and reap the admitted child.
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    fn record_failure(outcome: &mut Result<ProcessOutcome>, error: ProcessError) {
        if outcome.is_ok() {
            *outcome = Err(error);
        }
    }
    #[test]
    fn later_io_failure_preserves_the_original_settlement_error() {
        let mut outcome = Err(ProcessError::SettlementTimeout);
        record_failure(&mut outcome, ProcessError::Io("input".into()));
        record_failure(&mut outcome, ProcessError::Io("output".into()));
        assert!(matches!(outcome, Err(ProcessError::SettlementTimeout)));
    }
    struct Control {
        child: Arc<ChildState>,
        master: Mutex<Box<dyn MasterPty + Send>>,
        writer: AsyncFd<File>,
        writing: Arc<Semaphore>,
        output: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
        _reservation: Arc<CaptureReservation>,
    }
    impl std::fmt::Debug for Control {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PtyControl")
                .field("pid", &self.child.pid)
                .finish_non_exhaustive()
        }
    }
    #[async_trait]
    impl PtyControl for Control {
        fn pid(&self) -> u32 {
            self.child.pid
        }
        async fn read(&self) -> Result<PtyRead> {
            let mut output = self.output.try_lock().map_err(|_| ProcessError::Capacity)?;
            if let Some(bytes) = output.recv().await {
                Ok(PtyRead { bytes, eof: false })
            } else {
                // A failing native drain remains a failure after its buffered prefix.
                self.child.wait_outcome().await?;
                Ok(PtyRead {
                    bytes: vec![],
                    eof: true,
                })
            }
        }
        async fn write(&self, bytes: &[u8]) -> Result<usize> {
            if bytes.is_empty() || bytes.len() > MAXIMUM_PTY_IO_BYTES {
                return Err(ProcessError::InvalidInput(
                    "PTY writes require 1..=64 KiB".into(),
                ));
            }
            let permit = self
                .writing
                .clone()
                .try_acquire_owned()
                .map_err(|_| ProcessError::Capacity)?;
            let _permit = permit;
            tokio::time::timeout(INPUT_DEADLINE, async {
                loop {
                    if self.child.termination_started.load(Ordering::Acquire) {
                        return Err(ProcessError::ShuttingDown);
                    }
                    let mut ready = self
                        .writer
                        .writable()
                        .await
                        .map_err(|error| ProcessError::Io(error.to_string()))?;
                    match ready.try_io(|fd| {
                        rustix::io::write(fd.get_ref(), bytes).map_err(std::io::Error::from)
                    }) {
                        Ok(Ok(0)) => return Err(ProcessError::Io("PTY input closed".into())),
                        Ok(Ok(count)) => return Ok(count),
                        Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Ok(Err(error)) => return Err(ProcessError::Io(error.to_string())),
                        Err(_) => {}
                    }
                }
            })
            .await
            .map_err(|_| ProcessError::Io("PTY input capacity deadline exceeded".into()))?
        }
        fn resize(&self, size: PtySize) -> Result<()> {
            size.validate()?;
            if self.child.termination_started.load(Ordering::Acquire) {
                return Err(ProcessError::ShuttingDown);
            }
            self.master
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .resize(native_size(size))
                .map_err(|error| ProcessError::Io(error.to_string()))
        }
        fn terminate(&self) {
            self.child.terminate();
        }
        async fn wait(&self) -> Result<ProcessOutcome> {
            self.child.wait_outcome().await
        }
    }
    // portable-pty exposes the native master only as a raw fd. Duplicate it while
    // its owner is borrowed, then use owned descriptors for all readiness and I/O.
    #[allow(unsafe_code)]
    fn master_file(master: &dyn MasterPty) -> Result<File> {
        let raw = master.as_raw_fd().ok_or(ProcessError::Unsupported)?;
        // SAFETY: the pinned Linux portable-pty master owns this descriptor and
        // cannot be dropped while borrowed here. The borrowed fd never escapes;
        // fcntl creates an independently owned CLOEXEC descriptor before return.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) };
        let owned = rustix::io::fcntl_dupfd_cloexec(borrowed, 0).map_err(spawn_error)?;
        let flags = rustix::fs::fcntl_getfl(&owned).map_err(spawn_error)?;
        rustix::fs::fcntl_setfl(&owned, flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(spawn_error)?;
        Ok(owned.into())
    }
    fn native_size(size: PtySize) -> portable_pty::PtySize {
        portable_pty::PtySize {
            rows: size.rows,
            cols: size.columns,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
    fn spawn_error(error: impl std::fmt::Display) -> ProcessError {
        ProcessError::Spawn(error.to_string())
    }
    #[allow(clippy::needless_pass_by_value)] // The reader thread owns and closes its sender, cancellation and runtime handles.
    fn drain(
        mut reader: File,
        output: mpsc::Sender<Vec<u8>>,
        stop: CancellationToken,
        runtime: tokio::runtime::Handle,
    ) -> Result<()> {
        let mut buffer = [0; CHUNK];
        loop {
            if stop.is_cancelled() {
                return Ok(());
            }
            let count = match reader.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(count) => count,
                // Linux PTY masters report EIO after the last slave closes.
                Err(error) if error.raw_os_error() == Some(5) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut fds = [rustix::event::PollFd::new(
                        &reader,
                        rustix::event::PollFlags::IN,
                    )];
                    match rustix::event::poll(
                        &mut fds,
                        Some(&rustix::event::Timespec {
                            tv_sec: 0,
                            tv_nsec: 100_000_000,
                        }),
                    ) {
                        Ok(_) | Err(rustix::io::Errno::INTR) => continue,
                        Err(error) => return Err(ProcessError::Io(error.to_string())),
                    }
                }
                Err(error) => return Err(ProcessError::Io(error.to_string())),
            };
            let sent = runtime.block_on(async {
                tokio::select! { biased;
                    () = stop.cancelled() => false,
                    result = output.send(buffer[..count].to_vec()) => result.is_ok(),
                }
            });
            if !sent {
                return Ok(());
            }
        }
    }
    impl Service {
        #[allow(clippy::too_many_lines)] // Admission, native handles and both worker rollback paths form one lifecycle transition.
        pub(super) fn spawn_pty(&self, spec: &PtyProcessSpec) -> Result<ManagedPtyProcess> {
            let runtime = tokio::runtime::Handle::try_current()
                .map_err(|_| ProcessError::Spawn("Tokio runtime is unavailable".into()))?;
            self.reserve_capture(RESERVED)?;
            let spawned = (|| {
                let pair = portable_pty::native_pty_system()
                    .openpty(native_size(spec.size))
                    .map_err(spawn_error)?;
                let writer = master_file(pair.master.as_ref())?;
                let reader = writer.try_clone().map_err(spawn_error)?;
                let writer = AsyncFd::new(writer).map_err(spawn_error)?;
                let mut command = CommandBuilder::new(&spec.process.program);
                command.args(&spec.process.arguments);
                command.cwd(&spec.process.cwd);
                command.env_clear();
                for (name, value) in &spec.environment {
                    command.env(name, value);
                }
                let child = Reaper(Some(
                    pair.slave.spawn_command(command).map_err(spawn_error)?,
                ));
                drop(pair.slave);
                Ok::<_, ProcessError>((pair.master, reader, writer, child))
            })();
            let (master, reader, writer, mut child) = match spawned {
                Ok(spawned) => spawned,
                Err(error) => {
                    rollback_admission(&self.state, RESERVED);
                    return Err(error);
                }
            };
            let Some(pid) = child.0.as_ref().and_then(|child| child.process_id()) else {
                rollback_admission(&self.state, RESERVED);
                return Err(ProcessError::Unsupported);
            };
            let reservation = Arc::new(CaptureReservation {
                service: Arc::downgrade(&self.state),
                bytes: RESERVED,
            });
            let stop = CancellationToken::new();
            let (state, published) = self.publish_child(
                pid,
                spec.termination_grace_ms,
                &runtime,
                Some(stop.clone()),
                spec.process.owner.clone(),
            );
            let (output, receiver) = mpsc::channel(OUTPUT_CHUNKS);
            let (read_done, read_wait) = oneshot::channel();
            let read_runtime = runtime.clone();
            let read_retained = Arc::clone(&reservation);
            let read_state = Arc::clone(&state);
            let reader_thread = std::thread::Builder::new()
                .name("rsi-pty-read".into())
                .spawn(move || {
                    let _retained = read_retained;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        drain(reader, output, stop, read_runtime)
                    }))
                    .unwrap_or_else(|_| Err(ProcessError::Io("PTY reader panicked".into())));
                    if result.is_err() {
                        read_state.terminate();
                    }
                    let _ = read_done.send(result);
                });
            let (child_done, child_result) = oneshot::channel();
            let waiter_thread = std::thread::Builder::new()
                .name("rsi-pty-reap".into())
                .spawn(move || {
                    let _ = child_done.send(child.wait());
                });
            let failed = reader_thread.is_err() || waiter_thread.is_err();
            let settling = state.clone();
            let writing = Arc::new(Semaphore::new(1));
            let finishing_writes = Arc::clone(&writing);
            runtime.spawn(async move {
                let mut outcome = child_result
                    .await
                    .unwrap_or_else(|_| Err(ProcessError::Io("PTY reaper failed".into())));
                if settling.group_is_alive() {
                    settling.terminate();
                    if !wait_for_group_disappearance(
                        &settling,
                        settling
                            .grace
                            .saturating_add(POST_KILL_GROUP_SETTLEMENT_TIMEOUT),
                    )
                    .await
                    {
                        record_failure(&mut outcome, ProcessError::SettlementTimeout);
                    }
                }
                // Stop new input after reaping, while normal output still drains in order.
                settling.termination_started.store(true, Ordering::Release);
                if tokio::time::timeout(
                    settling.grace.max(INPUT_DEADLINE),
                    finishing_writes.acquire_owned(),
                )
                .await
                .is_err()
                {
                    record_failure(
                        &mut outcome,
                        ProcessError::Io("PTY input did not settle after reaping".into()),
                    );
                }
                match tokio::time::timeout(settling.grace, read_wait).await {
                    Ok(Ok(Ok(()))) => {}
                    Ok(Ok(Err(error))) => record_failure(&mut outcome, error),
                    _ => {
                        if let Some(stop) = &settling.duplex_stop {
                            stop.cancel();
                        }
                        record_failure(
                            &mut outcome,
                            ProcessError::Io("PTY output did not settle after reaping".into()),
                        );
                    }
                }
                settling.finish(outcome);
            });
            if failed || !published {
                state.terminate();
                return Err(if published {
                    ProcessError::Spawn("PTY worker creation failed".into())
                } else {
                    ProcessError::ShuttingDown
                });
            }
            Ok(ManagedPtyProcess::new(Arc::new(Control {
                child: state,
                master: Mutex::new(master),
                writer,
                writing,
                output: tokio::sync::Mutex::new(receiver),
                _reservation: reservation,
            })))
        }
    }
}
