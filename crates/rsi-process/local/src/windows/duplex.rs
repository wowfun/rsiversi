//! Bounded ongoing pipes over the same Windows process/Job admission owner.
use super::*;
use rsi_process::{
    DuplexControl, DuplexInput, DuplexOutput, DuplexProcessSpec, DuplexRead,
    MAXIMUM_DUPLEX_CHUNK_BYTES, ManagedDuplexProcess,
};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
struct InputMessage {
    bytes: Option<Vec<u8>>,
    reply: oneshot::Sender<Result<usize>>,
    _slot: OwnedSemaphorePermit,
}
#[derive(Debug)]
struct Input {
    sender: mpsc::Sender<InputMessage>,
    slot: Arc<Semaphore>,
    _capture: Arc<OwnedSemaphorePermit>,
}
impl Input {
    async fn command(&self, bytes: Option<&[u8]>) -> Result<usize> {
        let slot = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProcessError::Capacity)?;
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(InputMessage {
                bytes: bytes.map(<[u8]>::to_vec),
                reply,
                _slot: slot,
            })
            .map_err(|_| ProcessError::ShuttingDown)?;
        result
            .await
            .map_err(|_| ProcessError::Io("Windows input acknowledgement unavailable".into()))?
    }
}
#[async_trait::async_trait]
impl DuplexInput for Input {
    async fn write(&self, bytes: &[u8]) -> Result<usize> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_DUPLEX_CHUNK_BYTES {
            return Err(ProcessError::InvalidInput(
                "duplex input chunk exceeds bound".into(),
            ));
        }
        self.command(Some(bytes)).await
    }
    async fn close(&self) -> Result<()> {
        self.command(None).await.map(|_| ())
    }
}
#[derive(Debug)]
struct Chunk {
    bytes: Vec<u8>,
    offset: usize,
    _reservation: OwnedSemaphorePermit,
}

struct OutputReceipt {
    sender: Option<mpsc::Sender<Chunk>>,
    error: Arc<Mutex<Option<ProcessError>>>,
    acknowledged: bool,
}
impl Drop for OutputReceipt {
    fn drop(&mut self) {
        if !self.acknowledged {
            *self.error.lock().expect("Windows duplex output") = Some(ProcessError::Io(
                "Windows duplex stdout worker interrupted".into(),
            ));
        }
        // The sender drops only after publishing any interruption error.
    }
}
#[derive(Debug)]
struct Reader {
    receiver: mpsc::Receiver<Chunk>,
    chunk: Option<Chunk>,
    ended: bool,
}
#[derive(Debug)]
struct Output {
    reader: tokio::sync::Mutex<Reader>,
    slot: Arc<Semaphore>,
    error: Arc<Mutex<Option<ProcessError>>>,
    _capture: Arc<OwnedSemaphorePermit>,
}
#[async_trait::async_trait]
impl DuplexOutput for Output {
    async fn read(&self, maximum: usize) -> Result<DuplexRead> {
        if !(1..=MAXIMUM_DUPLEX_CHUNK_BYTES).contains(&maximum) {
            return Err(ProcessError::InvalidInput(
                "duplex output chunk exceeds bound".into(),
            ));
        }
        let _slot = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProcessError::Capacity)?;
        let mut reader = self.reader.lock().await;
        if reader.chunk.is_none() && !reader.ended {
            reader.chunk = reader.receiver.recv().await;
            reader.ended = reader.chunk.is_none();
        }
        if let Some(chunk) = reader.chunk.as_mut() {
            let end = (chunk.offset + maximum).min(chunk.bytes.len());
            let bytes = chunk.bytes[chunk.offset..end].to_vec();
            chunk.offset = end;
            if end == chunk.bytes.len() {
                reader.chunk = None;
            }
            return Ok(DuplexRead { bytes, eof: false });
        }
        if let Some(error) = self.error.lock().expect("Windows duplex output").clone() {
            return Err(error);
        }
        Ok(DuplexRead {
            bytes: vec![],
            eof: true,
        })
    }
}
#[derive(Debug)]
struct Control {
    child: Arc<Child>,
    input: Arc<Input>,
    output: Arc<Output>,
}
#[async_trait::async_trait]
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
        self.child.stderr.clone()
    }
    fn terminate(&self) {
        self.child.terminate();
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        ProcessControl::wait(self.child.as_ref()).await
    }
    async fn wait_settlement(&self) -> Result<()> {
        loop {
            let notified = self.child.settled.notified();
            if let Some(result) = self
                .child
                .settlement
                .lock()
                .expect("Windows duplex settlement")
                .clone()
            {
                return result;
            }
            notified.await;
        }
    }
}
impl Service {
    #[expect(
        clippy::too_many_lines,
        reason = "Duplex tasks and Job settlement retain the same child and capture permits."
    )]
    pub(crate) async fn spawn_duplex(
        self: &Arc<Self>,
        spec: DuplexProcessSpec,
    ) -> Result<ManagedDuplexProcess> {
        spec.validate()?;
        let budget = spec.stdout_buffer_bytes;
        let (mut process, child, _, grace) = self.start(spec.into_process_spec(), false).await?;
        let mut stdin = process
            .stdin
            .take()
            .expect("successful piped spawn retains stdin");
        let mut stdout = process
            .stdout
            .take()
            .expect("successful piped spawn retains stdout");
        let stderr = process
            .stderr
            .take()
            .expect("successful piped spawn retains stderr");
        let (send, mut messages) = mpsc::channel::<InputMessage>(1);
        let input = Arc::new(Input {
            sender: send,
            slot: Arc::new(Semaphore::new(1)),
            _capture: child.stderr.reservation.clone(),
        });
        let (chunks, receiver) = mpsc::channel::<Chunk>(32);
        let error = Arc::new(Mutex::new(None));
        let output = Arc::new(Output {
            reader: tokio::sync::Mutex::new(Reader {
                receiver,
                chunk: None,
                ended: false,
            }),
            slot: Arc::new(Semaphore::new(1)),
            error: error.clone(),
            _capture: child.stderr.reservation.clone(),
        });
        let input_stop = child.stop.clone();
        let input_task = tokio::spawn(async move {
            loop {
                let message = tokio::select! { () = input_stop.cancelled() => break, message = messages.recv() => message };
                let Some(message) = message else {
                    break;
                };
                let result = if let Some(bytes) = message.bytes {
                    match tokio::time::timeout(Duration::from_millis(500), stdin.write(&bytes))
                        .await
                    {
                        Ok(Ok(0)) => Err(ProcessError::Io("Windows duplex stdin closed".into())),
                        Ok(Ok(count)) => Ok(count),
                        Ok(Err(error)) => Err(io_error(error)),
                        Err(_) => Err(ProcessError::Capacity),
                    }
                } else {
                    let result =
                        match tokio::time::timeout(Duration::from_millis(500), stdin.shutdown())
                            .await
                        {
                            Ok(result) => result.map(|()| 0).map_err(io_error),
                            Err(_) => Err(ProcessError::Capacity),
                        };
                    let _ = message.reply.send(result);
                    break;
                };
                let _ = message.reply.send(result);
            }
        });
        let stop = child.stop.clone();
        let state = child.clone();
        let receipt = OutputReceipt {
            sender: Some(chunks),
            error,
            acknowledged: false,
        };
        let out_task = tokio::spawn(async move {
            let mut receipt = receipt;
            let permits = Arc::new(Semaphore::new(budget));
            let size = budget.min(MAXIMUM_DUPLEX_CHUNK_BYTES);
            let permits_per_read = u32::try_from(size).expect("validated bounded duplex window");
            let result = async { loop {
                let reservation = tokio::select! { () = stop.cancelled() => return Err(ProcessError::Io("Windows duplex stdout cancelled".into())),
                    permit = permits.clone().acquire_many_owned(permits_per_read) => permit.map_err(|_| ProcessError::ShuttingDown)? };
                let mut bytes = vec![0; size];
                let read = tokio::select! { () = stop.cancelled() => return Err(ProcessError::Io("Windows duplex stdout cancelled".into())),
                    read = stdout.read(&mut bytes) => read.map_err(io_error)? };
                if read == 0 { return Ok(()); }
                bytes.truncate(read);
                tokio::select! { () = stop.cancelled() => return Err(ProcessError::Io("Windows duplex stdout cancelled".into())),
                    delivery_result = receipt.sender.as_ref().expect("stdout sender is retained").send(Chunk { bytes, offset: 0, _reservation: reservation }) => delivery_result.map_err(|_| ProcessError::Io("Windows duplex reader closed".into()))? };
            }}.await;
            if let Err(output_error) = &result {
                *receipt.error.lock().expect("Windows duplex output") = Some(output_error.clone());
            }
            receipt.acknowledged = true;
            receipt.sender.take();
            result
        });
        let err_task = tokio::spawn(drain(stderr, child.stderr.clone()));
        tokio::spawn(async move {
            let status = process.wait().await.map_err(io_error);
            let settled = settle_job(&state.job).await;
            input_task.abort();
            let _ = input_task.await;
            let pipes = settle_pipes(out_task, err_task, grace).await;
            // Explicit output cancellation may be a protocol error while the Job
            // and every pipe task have independently confirmed resource settlement.
            let resources_settled = status.is_ok() && settled.is_ok();
            let outcome = status.and_then(|status| {
                settled.and(pipes).map(|()| ProcessOutcome {
                    exit_code: status.code(),
                    signal: None,
                })
            });
            state.finish(outcome, resources_settled);
        });
        Ok(ManagedDuplexProcess::new(Arc::new(Control {
            child,
            input,
            output,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_sandbox::{
        ConfinedProcess, EnforcementStamp, ProcessStdio, SandboxBackend, SandboxFileSystem,
        SandboxMode, SandboxNetwork, SandboxScratch,
    };
    #[tokio::test]
    async fn native_duplex_round_trip_preserves_bytes_under_small_backpressure_window() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().to_owned();
        let service = Service::new(ProcessLocalConfig::default());
        let process = service
            .spawn_duplex(DuplexProcessSpec {
                process: ConfinedProcess {
                    owner: None,
                    stdio: ProcessStdio::Pipes,
                    program: r"C:\Windows\System32\cmd.exe".into(),
                    arguments: ["/d", "/v:on", "/c", "set /p line=&echo !line!"]
                        .map(std::ffi::OsString::from)
                        .into(),
                    cwd: workspace.clone(),
                    stamp: EnforcementStamp {
                        requested: SandboxMode::DangerFullAccess,
                        backend: SandboxBackend::Unconfined,
                        workspace,
                        filesystem: SandboxFileSystem::Unconfined,
                        scratch: SandboxScratch::Host,
                        network: SandboxNetwork::Host,
                    },
                },
                environment: vec![],
                stdout_buffer_bytes: 4,
                stderr_max_bytes: 8,
                termination_grace_ms: 1000,
            })
            .await
            .unwrap();
        assert_eq!(process.stdin().write(b"abcdef\r\n").await.unwrap(), 8);
        process.stdin().close().await.unwrap();
        let mut output = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let read = process.stdout().read(2).await.unwrap();
                output.extend(read.bytes);
                if read.eof {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(output, b"abcdef\r\n");
        assert_eq!(process.wait().await.unwrap().exit_code, Some(0));
        process.wait_settlement().await.unwrap();
        service.shutdown().await.unwrap();
    }
}
