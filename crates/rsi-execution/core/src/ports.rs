use crate::{ExecutionPin, retained};
use async_trait::async_trait;
use rsi_process::{
    DuplexControl, DuplexInput, DuplexOutput, DuplexRead, ManagedDuplexProcess, ManagedProcess,
    ManagedPtyProcess, ProcessControl, ProcessError, ProcessOutcome, ProcessOutput, ProcessRead,
    PtyControl, PtyRead, PtySize, Result,
};
use std::sync::Arc;

#[derive(Debug)]
struct Bound<T> {
    inner: T,
    pin: ExecutionPin,
}

pub(crate) fn batch(inner: ManagedProcess, pin: ExecutionPin) -> ManagedProcess {
    ManagedProcess::new(Arc::new(Bound { inner, pin }))
}
pub(crate) fn duplex(inner: ManagedDuplexProcess, pin: ExecutionPin) -> ManagedDuplexProcess {
    ManagedDuplexProcess::new(Arc::new(Bound { inner, pin }))
}
pub(crate) fn pty(inner: ManagedPtyProcess, pin: ExecutionPin) -> ManagedPtyProcess {
    ManagedPtyProcess::new(Arc::new(Bound { inner, pin }))
}
fn tail(inner: Arc<dyn ProcessOutput>, pin: ExecutionPin) -> Arc<dyn ProcessOutput> {
    Arc::new(Bound { inner, pin })
}
impl ProcessOutput for Bound<Arc<dyn ProcessOutput>> {
    fn read_from(&self, offset: u64) -> Result<ProcessRead> {
        let _permit = self.pin.admit()?;
        self.inner.read_from(offset)
    }
    fn peek_tail(&self, maximum: usize) -> Result<ProcessRead> {
        let _permit = self.pin.admit()?;
        self.inner.peek_tail(maximum)
    }
}
#[async_trait]
impl ProcessControl for Bound<ManagedProcess> {
    fn pid(&self) -> u32 {
        self.inner.pid()
    }
    fn stdout(&self) -> Arc<dyn ProcessOutput> {
        tail(self.inner.stdout(), self.pin.clone())
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        tail(self.inner.stderr(), self.pin.clone())
    }
    fn terminate(&self) {
        self.inner.terminate();
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        self.inner.wait().await
    }
}
#[async_trait]
impl DuplexControl for Bound<ManagedDuplexProcess> {
    fn pid(&self) -> u32 {
        self.inner.pid()
    }
    fn stdin(&self) -> Arc<dyn DuplexInput> {
        Arc::new(Bound {
            inner: self.inner.stdin(),
            pin: self.pin.clone(),
        })
    }
    fn stdout(&self) -> Arc<dyn DuplexOutput> {
        Arc::new(Bound {
            inner: self.inner.stdout(),
            pin: self.pin.clone(),
        })
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        tail(self.inner.stderr(), self.pin.clone())
    }
    fn terminate(&self) {
        self.inner.terminate();
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        self.inner.wait().await
    }
    async fn wait_settlement(&self) -> Result<()> {
        self.inner.wait_settlement().await
    }
}
#[async_trait]
impl DuplexInput for Bound<Arc<dyn DuplexInput>> {
    async fn write(&self, bytes: &[u8]) -> Result<usize> {
        check_chunk(bytes.len())?;
        let permit = self.pin.admit()?;
        let (inner, bytes) = (self.inner.clone(), bytes.to_vec());
        retained::operation(self.pin.clone(), permit, async move {
            inner.write(&bytes).await
        })
        .await
    }
    async fn close(&self) -> Result<()> {
        // Closing a byte port relinquishes authority and remains available after revocation.
        self.inner.close().await
    }
}
#[async_trait]
impl DuplexOutput for Bound<Arc<dyn DuplexOutput>> {
    async fn read(&self, maximum: usize) -> Result<DuplexRead> {
        check_chunk(maximum)?;
        // Receiving an already-running stream must not hold revocation open
        // indefinitely while idle. The byte owner owns cancellation of its read.
        drop(self.pin.publication()?);
        let read = self.inner.read(maximum).await?;
        let _publication = self.pin.publication()?;
        Ok(read)
    }
}
#[async_trait]
impl PtyControl for Bound<ManagedPtyProcess> {
    fn pid(&self) -> u32 {
        self.inner.pid()
    }
    async fn read(&self) -> Result<PtyRead> {
        drop(self.pin.publication()?);
        let read = self.inner.read().await?;
        let _publication = self.pin.publication()?;
        Ok(read)
    }
    async fn write(&self, bytes: &[u8]) -> Result<usize> {
        check_chunk(bytes.len())?;
        let permit = self.pin.admit()?;
        let (inner, bytes) = (self.inner.clone(), bytes.to_vec());
        retained::operation(self.pin.clone(), permit, async move {
            inner.write(&bytes).await
        })
        .await
    }
    async fn resize(&self, size: PtySize) -> Result<()> {
        size.validate()?;
        let permit = self.pin.admit()?;
        let inner = self.inner.clone();
        retained::operation(
            self.pin.clone(),
            permit,
            async move { inner.resize(size).await },
        )
        .await
    }
    fn terminate(&self) {
        self.inner.terminate();
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        self.inner.wait().await
    }
}
fn check_chunk(length: usize) -> Result<()> {
    if !(1..=rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES).contains(&length) {
        return Err(ProcessError::InvalidInput(
            "execution byte chunk exceeds its bound".into(),
        ));
    }
    Ok(())
}
