use super::{
    Arc, Entry, Failure, Plan, ProcessError, Reply, Result, Server, StreamId, lock, output,
    transport_failure,
};
use rsi_process::{
    DuplexProcessSpec, ManagedDuplexProcess, ManagedProcess, ManagedPtyProcess, ProcessSpec,
    PtyProcessSpec, PtySize,
};
use rsi_ssh_protocol::execution::StartOptions;

#[derive(Clone)]
pub(super) enum Native {
    Batch(ManagedProcess),
    Duplex(ManagedDuplexProcess),
    Pty(ManagedPtyProcess),
}
impl Native {
    pub fn pid(&self) -> u32 {
        match self {
            Self::Batch(p) => p.pid(),
            Self::Duplex(p) => p.pid(),
            Self::Pty(p) => p.pid(),
        }
    }
    pub fn terminate(&self) {
        match self {
            Self::Batch(p) => p.terminate(),
            Self::Duplex(p) => p.terminate(),
            Self::Pty(p) => p.terminate(),
        }
    }
    pub async fn wait(&self) -> Result<rsi_ssh_protocol::rpc::Outcome> {
        match self {
            Self::Batch(p) => p.wait().await,
            Self::Duplex(p) => p.wait().await,
            Self::Pty(p) => p.wait().await,
        }
        .map(Into::into)
        .map_err(Into::into)
    }
    pub async fn settlement(&self, outcome: &Result<rsi_ssh_protocol::rpc::Outcome>) -> Result<()> {
        match self {
            Self::Duplex(process) => process.wait_settlement().await.map_err(Into::into),
            Self::Batch(_) | Self::Pty(_) => outcome.as_ref().map(|_| ()).map_err(|error| *error),
        }
    }
    pub async fn write(&self, bytes: &[u8]) -> Result<usize> {
        match self {
            Self::Batch(_) => Err(Failure::Invalid),
            Self::Duplex(p) => p.stdin().write(bytes).await.map_err(write_failure),
            Self::Pty(p) => p.write(bytes).await.map_err(write_failure),
        }
    }
    pub async fn close(&self) -> Result<()> {
        match self {
            Self::Duplex(p) => p.stdin().close().await.map_err(Into::into),
            Self::Batch(_) | Self::Pty(_) => Err(Failure::Invalid),
        }
    }
    pub async fn resize(&self, size: PtySize) -> Result<()> {
        match self {
            Self::Pty(p) => p.resize(size).await.map_err(Into::into),
            Self::Batch(_) | Self::Duplex(_) => Err(Failure::Invalid),
        }
    }
}
fn write_failure(error: ProcessError) -> Failure {
    match error {
        ProcessError::Io(_) => Failure::OutcomeUnknown,
        other => other.into(),
    }
}
impl Server {
    pub async fn start(self: &Arc<Self>, handle: u64, options: StartOptions) -> Result<Reply> {
        let entry = self.entry(handle)?;
        options
            .validate(entry.prepared.stderr.is_none())
            .map_err(|_| Failure::Invalid)?;
        let plan = lock(&entry.plan).take().ok_or(Failure::Invalid)?;
        if entry.is_terminated() {
            entry.outcome.send_replace(Some(Err(Failure::Closed)));
            entry.settlement.send_replace(Some(Ok(())));
            return Err(Failure::Closed);
        }
        let Plan {
            process,
            environment,
            output,
            error,
        } = plan;
        let result = self
            .spawn_native(&entry, options, process, environment)
            .await;
        let native = match result {
            Ok(native) => native,
            Err(failure) => {
                entry.outcome.send_replace(Some(Err(failure)));
                entry.settlement.send_replace(Some(
                    if matches!(
                        failure,
                        Failure::OutcomeUnknown | Failure::Io | Failure::SettlementTimeout
                    ) {
                        Err(failure)
                    } else {
                        Ok(())
                    },
                ));
                return Err(failure);
            }
        };
        let pid = native.pid();
        *lock(&entry.native) = Some(native.clone());
        if entry.is_terminated() || entry.cancel.is_cancelled() {
            native.terminate();
        }
        let task_entry = entry.clone();
        let connection = self.connection.clone();
        let mut task = lock(&entry.task);
        *task = Some(tokio::spawn(async move {
            output::run(task_entry, native, output, error, connection).await;
        }));
        Ok(Reply::Started { pid })
    }
    async fn spawn_native(
        &self,
        entry: &Entry,
        options: StartOptions,
        process: rsi_sandbox::ConfinedProcess,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> Result<Native> {
        match options {
            StartOptions::Batch {
                stdin_bytes,
                stdout_max_bytes,
                stderr_max_bytes,
                termination_grace_ms,
            } => {
                let stdin = self.upload(entry, stdin_bytes).await;
                match stdin {
                    Ok(stdin) => self
                        .owner
                        .capabilities
                        .process
                        .spawn(ProcessSpec {
                            process,
                            environment,
                            stdin,
                            stdout_max_bytes,
                            stderr_max_bytes,
                            termination_grace_ms,
                        })
                        .await
                        .map(Native::Batch)
                        .map_err(Into::into),
                    Err(failure) => Err(failure),
                }
            }
            StartOptions::Duplex {
                stdout_buffer_bytes,
                stderr_max_bytes,
                termination_grace_ms,
            } => self
                .owner
                .capabilities
                .duplex
                .spawn(DuplexProcessSpec {
                    process,
                    environment,
                    stdout_buffer_bytes,
                    stderr_max_bytes,
                    termination_grace_ms,
                })
                .await
                .map(Native::Duplex)
                .map_err(Into::into),
            StartOptions::Pty {
                columns,
                rows,
                termination_grace_ms,
            } => self
                .owner
                .capabilities
                .pty
                .spawn(PtyProcessSpec {
                    process,
                    environment,
                    size: PtySize { columns, rows },
                    termination_grace_ms,
                })
                .await
                .map(Native::Pty)
                .map_err(Into::into),
        }
    }
    async fn upload(&self, entry: &Entry, length: usize) -> Result<Vec<u8>> {
        let mut input = self
            .connection
            .open_receiver(StreamId::from_raw(entry.prepared.stdin).map_err(transport_failure)?)
            .map_err(transport_failure)?;
        let mut bytes = Vec::with_capacity(length);
        loop {
            let chunk = tokio::select! {
                () = entry.cancel.cancelled() => return Err(Failure::Closed),
                () = self.connection.closed() => return Err(Failure::Closed),
                chunk = input.next() => chunk.map_err(transport_failure)?.ok_or(Failure::Invalid)?,
            };
            if entry.is_terminated() || bytes.len().saturating_add(chunk.bytes.len()) > length {
                return Err(Failure::Invalid);
            }
            bytes.extend(chunk.bytes);
            if chunk.eof {
                break;
            }
        }
        if bytes.len() != length {
            return Err(Failure::Invalid);
        }
        Ok(bytes)
    }
}
