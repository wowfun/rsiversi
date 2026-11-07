//! One admitted input command stays with the pipe owner until acknowledgement.
use rsi_process::{DuplexInput, MAXIMUM_DUPLEX_CHUNK_BYTES, ProcessError, Result};
use std::{fmt::Debug, sync::Arc};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt as _},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct Command {
    bytes: Option<Vec<u8>>,
    reply: oneshot::Sender<Result<usize>>,
    permit: OwnedSemaphorePermit,
}

#[derive(Debug)]
pub(crate) struct Input<R> {
    sender: mpsc::Sender<Command>,
    slot: Arc<Semaphore>,
    _reservation: Arc<R>,
}

impl<R> Input<R> {
    pub(crate) fn new(reservation: Arc<R>) -> (Arc<Self>, mpsc::Receiver<Command>) {
        let (sender, receiver) = mpsc::channel(1);
        (
            Arc::new(Self {
                sender,
                slot: Arc::new(Semaphore::new(1)),
                _reservation: reservation,
            }),
            receiver,
        )
    }

    async fn command(&self, bytes: Option<&[u8]>) -> Result<usize> {
        let permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProcessError::Capacity)?;
        let (reply, wait) = oneshot::channel();
        self.sender
            .try_send(Command {
                bytes: bytes.map(<[u8]>::to_vec),
                reply,
                permit,
            })
            .map_err(|_| ProcessError::ShuttingDown)?;
        wait.await.map_err(|_| interrupted())?
    }
}

#[async_trait::async_trait]
impl<R: Debug + Send + Sync + 'static> DuplexInput for Input<R> {
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

fn interrupted() -> ProcessError {
    ProcessError::Io("duplex write interrupted; accepted prefix is unknown".into())
}

pub(crate) async fn write_input<W: AsyncWrite + Unpin>(
    mut stdin: W,
    mut receiver: mpsc::Receiver<Command>,
    stop: CancellationToken,
) {
    loop {
        let command = tokio::select! {
            biased;
            () = stop.cancelled() => break,
            command = receiver.recv() => match command {
                Some(command) => command,
                None => break,
            },
        };
        let Command {
            bytes,
            reply,
            permit,
        } = command;
        let Some(bytes) = bytes else {
            receiver.close();
            // Windows Tokio pipes can still own an accepted native write.
            let result = tokio::select! {
                biased;
                () = stop.cancelled() => Err(interrupted()),
                result = stdin.flush() => result.map(|()| 0)
                    .map_err(|error| ProcessError::Io(error.to_string())),
            };
            drop(stdin);
            drop(permit);
            let _ = reply.send(result);
            return;
        };
        let result = tokio::select! {
            biased;
            () = stop.cancelled() => Err(interrupted()),
            result = async {
                // One write preserves the adapter's exact accepted prefix.
                let accepted = stdin.write(&bytes).await
                    .map_err(|error| ProcessError::Io(error.to_string()))?;
                if accepted == 0 {
                    return Err(ProcessError::Io("duplex input closed during write".into()));
                }
                Ok(accepted)
            } => result,
        };
        let failed = result.is_err();
        if failed {
            receiver.close();
        }
        drop(bytes);
        drop(permit);
        let _ = reply.send(result);
        if failed {
            break;
        }
    }
}

#[cfg(test)]
mod tests;
