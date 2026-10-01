use super::{Arc, Control, Owner, Reply, Request, Result, State, invalid, output};
use async_trait::async_trait;
use rsi_process::{
    DuplexControl, DuplexInput, DuplexOutput, DuplexRead, ProcessControl, ProcessOutcome,
    ProcessOutput, PtyControl, PtyRead, PtySize,
};
#[async_trait]
impl ProcessControl for Owner {
    fn pid(&self) -> u32 {
        self.0.pid
    }
    fn stdout(&self) -> Arc<dyn ProcessOutput> {
        self.0.stdout.clone().expect("batch stdout")
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        self.0.stderr.clone().expect("batch stderr")
    }
    fn terminate(&self) {
        self.0.terminate();
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        self.0.captured().await
    }
}
#[async_trait]
impl DuplexControl for Owner {
    fn pid(&self) -> u32 {
        self.0.pid
    }
    fn stdin(&self) -> Arc<dyn DuplexInput> {
        self.0.clone()
    }
    fn stdout(&self) -> Arc<dyn DuplexOutput> {
        self.0.lossless.clone().expect("duplex stdout")
    }
    fn stderr(&self) -> Arc<dyn ProcessOutput> {
        self.0.stderr.clone().expect("duplex stderr")
    }
    fn terminate(&self) {
        self.0.terminate();
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        self.0.captured().await
    }
    async fn wait_settlement(&self) -> Result<()> {
        self.0.wait_settlement().await
    }
}
#[async_trait]
impl DuplexInput for State {
    async fn write(&self, bytes: &[u8]) -> Result<usize> {
        if bytes.is_empty() || bytes.len() > rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES {
            return Err(invalid());
        }
        match self
            .client
            .call(Request::Write {
                handle: self.prepared.handle,
                bytes: bytes.to_vec(),
            })
            .await?
        {
            Reply::Written { bytes: written } if written > 0 && written <= bytes.len() => {
                Ok(written)
            }
            _ => Err(self.client.malformed()),
        }
    }
    async fn close(&self) -> Result<()> {
        match self
            .client
            .call(Request::CloseInput {
                handle: self.prepared.handle,
            })
            .await?
        {
            Reply::Done => Ok(()),
            _ => Err(self.client.malformed()),
        }
    }
}
#[async_trait]
impl DuplexOutput for output::Lossless {
    async fn read(&self, maximum: usize) -> Result<DuplexRead> {
        self.read(maximum).await
    }
}
#[async_trait]
impl PtyControl for Owner {
    fn pid(&self) -> u32 {
        self.0.pid
    }
    async fn read(&self) -> Result<PtyRead> {
        self.0
            .lossless
            .as_ref()
            .expect("PTY output")
            .read(rsi_process::MAXIMUM_PTY_IO_BYTES)
            .await
            .map(|read| PtyRead {
                bytes: read.bytes,
                eof: read.eof,
            })
    }
    async fn write(&self, bytes: &[u8]) -> Result<usize> {
        self.0.write(bytes).await
    }
    async fn resize(&self, size: PtySize) -> Result<()> {
        size.validate()?;
        self.0
            .client
            .control(Control::Resize {
                process: self.0.prepared.handle,
                columns: size.columns,
                rows: size.rows,
            })
            .await
    }
    fn terminate(&self) {
        self.0.terminate();
    }
    async fn wait(&self) -> Result<ProcessOutcome> {
        self.0.settled().await
    }
}
