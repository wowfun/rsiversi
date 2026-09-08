use futures_util::future::BoxFuture;
use rsi_meta::Execution;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// One absolute write deadline through the next completed flush. Partial progress
/// cannot renew it; idle subscriptions have no running write deadline.
pub(crate) struct WriteBound<T> {
    inner: T,
    execution: Execution,
    deadline: Option<BoxFuture<'static, ()>>,
}
impl<T> WriteBound<T> {
    pub fn new(inner: T, execution: Execution) -> Self {
        Self {
            inner,
            execution,
            deadline: None,
        }
    }
    fn check(&mut self, context: &mut Context<'_>, writing: bool) -> io::Result<()> {
        if writing && self.deadline.is_none() {
            self.deadline = Some(
                self.execution
                    .deadline_after(Duration::from_secs(30))
                    .wait(),
            );
        }
        if self
            .deadline
            .as_mut()
            .is_some_and(|deadline| deadline.as_mut().poll(context).is_ready())
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP write deadline elapsed",
            ));
        }
        Ok(())
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for WriteBound<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for WriteBound<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check(context, !bytes.is_empty())?;
        Pin::new(&mut self.inner).poll_write(context, bytes)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.check(context, buffers.iter().any(|buffer| !buffer.is_empty()))?;
        Pin::new(&mut self.inner).poll_write_vectored(context, buffers)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(context, false)?;
        let result = Pin::new(&mut self.inner).poll_flush(context);
        if result.is_ready() {
            self.deadline = None;
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(context, true)?;
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn write_progress_does_not_renew_the_flush_deadline() {
        let (writer, mut reader) = tokio::io::duplex(8);
        let execution = Execution::native(tokio::runtime::Handle::current());
        let mut writer = WriteBound::new(writer, execution);
        let mut write = Box::pin(writer.write_all(&[1; 32]));
        assert!(write.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_secs(20)).await;
        reader.read_exact(&mut [0; 4]).await.unwrap();
        assert!(write.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_secs(11)).await;
        assert_eq!(write.await.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn completed_flush_allows_an_idle_subscription_to_wait_for_another_event() {
        let (writer, _reader) = tokio::io::duplex(8);
        let execution = Execution::native(tokio::runtime::Handle::current());
        let mut writer = WriteBound::new(writer, execution);
        writer.write_all(b"a").await.unwrap();
        writer.flush().await.unwrap();
        tokio::time::advance(Duration::from_secs(35)).await;
        writer.write_all(b"b").await.unwrap();
        writer.flush().await.unwrap();
    }
}
