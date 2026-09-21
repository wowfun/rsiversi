use crate::Error;
use async_trait::async_trait;
use rsi_process::ManagedDuplexProcess;
use std::{fmt, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::Mutex;

/// Explicit transport with one ordered reader and one serialized writer.
#[async_trait]
pub trait Transport: fmt::Debug + Send + Sync + 'static {
    /// Reads at most 64 KiB; empty means EOF. Cancellation must release the read.
    async fn read(&self) -> Result<Vec<u8>, Error>;
    /// Writes the complete bounded frame; partial failure retires this transport.
    async fn write(&self, bytes: &[u8]) -> Result<(), Error>;
    /// Confirms all accepted bytes crossed any buffering layer.
    async fn flush(&self) -> Result<(), Error>;
    /// Idempotently closes and settles owned resources, with bounded completion.
    async fn close(&self) -> Result<(), Error>;
}

/// Adapter for explicitly supplied asynchronous streams, including stdio.
pub struct StreamTransport {
    reader: Mutex<Box<dyn AsyncRead + Unpin + Send>>,
    writer: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
}
impl fmt::Debug for StreamTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamTransport").finish_non_exhaustive()
    }
}
impl StreamTransport {
    /// Wraps caller-owned streams; it does not open sockets or ambient stdio.
    pub fn new(
        reader: impl AsyncRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            reader: Mutex::new(Box::new(reader)),
            writer: Mutex::new(Box::new(writer)),
        })
    }
}
#[async_trait]
impl Transport for StreamTransport {
    async fn read(&self) -> Result<Vec<u8>, Error> {
        let mut bytes = vec![0; 64 * 1024];
        let count = self
            .reader
            .lock()
            .await
            .read(&mut bytes)
            .await
            .map_err(|_| Error::Closed)?;
        bytes.truncate(count);
        Ok(bytes)
    }
    async fn write(&self, bytes: &[u8]) -> Result<(), Error> {
        self.writer
            .lock()
            .await
            .write_all(bytes)
            .await
            .map_err(|_| Error::Closed)
    }
    async fn flush(&self) -> Result<(), Error> {
        self.writer
            .lock()
            .await
            .flush()
            .await
            .map_err(|_| Error::Closed)
    }
    async fn close(&self) -> Result<(), Error> {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            self.writer.lock().await.shutdown().await
        })
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed)
    }
}

/// Process-owned protocol pipe adapter; retains the provider's exact child handle.
#[derive(Debug)]
pub struct ProcessTransport {
    process: ManagedDuplexProcess,
}
impl ProcessTransport {
    /// Consumes an explicitly confined, already admitted process handle.
    pub fn new(process: ManagedDuplexProcess) -> Arc<Self> {
        Arc::new(Self { process })
    }
}
#[async_trait]
impl Transport for ProcessTransport {
    async fn read(&self) -> Result<Vec<u8>, Error> {
        self.process
            .stdout()
            .read(64 * 1024)
            .await
            .map(|read| read.bytes)
            .map_err(|_| Error::Closed)
    }
    async fn write(&self, mut bytes: &[u8]) -> Result<(), Error> {
        let stdin = self.process.stdin();
        while !bytes.is_empty() {
            let length = bytes.len().min(64 * 1024);
            let written = stdin
                .write(&bytes[..length])
                .await
                .map_err(|_| Error::Closed)?;
            if written == 0 || written > length {
                return Err(Error::Protocol);
            }
            bytes = &bytes[written..];
        }
        Ok(())
    }
    async fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
    async fn close(&self) -> Result<(), Error> {
        self.process.terminate();
        self.process
            .wait_settlement()
            .await
            .map_err(|_| Error::Closed)
    }
}
