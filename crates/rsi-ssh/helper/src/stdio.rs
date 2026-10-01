//! Exclusive nonblocking byte ports; no buffered stdio may read or write them.
use std::{
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf, unix::AsyncFd};

pub(crate) struct Port(AsyncFd<OwnedFd>);
impl Port {
    fn duplicate(fd: BorrowedFd<'_>) -> io::Result<Self> {
        let owned = rustix::io::fcntl_dupfd_cloexec(fd, 3)?;
        let flags = rustix::fs::fcntl_getfl(&owned)?;
        // The launcher passes these pipe descriptions through and performs no
        // concurrent byte I/O. NONBLOCK therefore changes only the helper's port use.
        rustix::fs::fcntl_setfl(&owned, flags | rustix::fs::OFlags::NONBLOCK)?;
        Ok(Self(AsyncFd::new(owned)?))
    }
    pub(crate) fn input() -> io::Result<Self> {
        Self::duplicate(rustix::stdio::stdin())
    }
    pub(crate) fn output() -> io::Result<Self> {
        Self::duplicate(rustix::stdio::stdout())
    }
}
impl AsyncRead for Port {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            let mut ready = std::task::ready!(self.0.poll_read_ready(cx))?;
            match ready.try_io(|fd| {
                rustix::io::read(fd.get_ref().as_fd(), buffer.initialize_unfilled())
                    .map_err(Into::into)
            }) {
                Ok(Ok(length)) => {
                    buffer.advance(length);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_) => {}
            }
        }
    }
}
impl AsyncWrite for Port {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut ready = std::task::ready!(self.0.poll_write_ready(cx))?;
            match ready
                .try_io(|fd| rustix::io::write(fd.get_ref().as_fd(), bytes).map_err(Into::into))
            {
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(result) => return Poll::Ready(result),
                Err(_) => {}
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn blocked_native_pipe_reads_do_not_keep_transport_or_runtime_tasks_alive() {
        let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
        let mut input = Port::duplicate(read.as_fd()).unwrap();
        let mut output = Port::duplicate(write.as_fd()).unwrap();
        let read = tokio::spawn(async move {
            let mut bytes = [0; 4];
            input.read_exact(&mut bytes).await.unwrap();
            bytes
        });
        output.write_all(b"pipe").await.unwrap();
        assert_eq!(read.await.unwrap(), *b"pipe");
        let (read, _write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
        let mut idle = Port::duplicate(read.as_fd()).unwrap();
        let task = tokio::spawn(async move { idle.read_u8().await });
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
}
