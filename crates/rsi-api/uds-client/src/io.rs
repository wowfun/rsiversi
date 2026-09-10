use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1::{Connection, SendRequest};
use hyper_util::rt::TokioIo;
use rsi_api_client::ResponseBytes;
use rsi_api_protocol::{ApiError, Result};
use std::{path::Path, pin::Pin};
use tokio::net::UnixStream;

pub(crate) struct Driver<T: hyper::rt::Read + hyper::rt::Write = TokioIo<UnixStream>>(
    Connection<T, Full<Bytes>>,
);
impl<T: hyper::rt::Read + hyper::rt::Write + Unpin> std::future::Future for Driver<T> {
    type Output = std::result::Result<(), hyper::Error>;
    fn poll(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // The retained response owns this socket and closes it by dropping it.
        // A completed peer may already be disconnected before shutdown(Write).
        self.0.poll_without_shutdown(context)
    }
}
fn lost() -> ApiError {
    ApiError::Backend("local API connection ended".into())
}

pub(crate) async fn connect(path: &Path) -> Result<(SendRequest<Full<Bytes>>, Driver)> {
    let socket = UnixStream::connect(path)
        .await
        .map_err(|_| ApiError::Backend("local API connection failed".into()))?;
    if socket
        .peer_cred()
        .map_err(|_| ApiError::Unauthorized)?
        .uid()
        != rustix::process::geteuid().as_raw()
    {
        return Err(ApiError::Unauthorized);
    }
    hyper::client::conn::http1::Builder::new()
        .max_buf_size(32 * 1024)
        .writev(true)
        .handshake::<_, Full<Bytes>>(TokioIo::new(socket))
        .await
        .map(|(sender, connection)| (sender, Driver(connection)))
        .map_err(|_| lost())
}

// The retained source is the only socket driver owner. Dropping it closes I/O
// synchronously; an escaped task cannot extend client retirement.
pub(crate) fn source(
    mut connection: Pin<Box<Driver>>,
    mut body: hyper::body::Incoming,
    mut ended: bool,
) -> ResponseBytes {
    Box::pin(async_stream::try_stream! {
        enum Next {
            Connection(std::result::Result<(), hyper::Error>),
            Body(Option<std::result::Result<hyper::body::Frame<Bytes>, hyper::Error>>),
        }
        loop {
            let next = tokio::select! { biased;
                result = &mut connection, if !ended => Next::Connection(result),
                frame = body.frame() => Next::Body(frame),
            };
            match next {
                Next::Connection(result) => { ended = true; result.map_err(|_| lost())?; }
                Next::Body(None) => break,
                Next::Body(Some(frame)) => yield frame.map_err(|_| lost())?.into_data()
                    .map_err(|_| ApiError::Invalid("local API trailers are not supported".into()))?,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf};

    struct PeerClosedIo {
        inner: tokio::io::DuplexStream,
        shutdowns: Arc<AtomicUsize>,
    }
    impl AsyncRead for PeerClosedIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, bytes)
        }
    }
    impl AsyncWrite for PeerClosedIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, bytes)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "peer has closed",
            )))
        }
    }

    #[tokio::test]
    async fn complete_response_needs_no_redundant_socket_shutdown() {
        let (client, mut server) = tokio::io::duplex(4096);
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let peer = tokio::spawn(async move {
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(server.read_u8().await.unwrap());
            }
            server
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}")
                .await
                .unwrap();
        });
        let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
            .handshake::<_, Full<Bytes>>(TokioIo::new(PeerClosedIo {
                inner: client,
                shutdowns: shutdowns.clone(),
            }))
            .await
            .unwrap();
        let driver = tokio::spawn(Driver(connection));
        let response = sender
            .send_request(http::Request::new(Full::new(Bytes::new())))
            .await
            .unwrap();
        assert_eq!(response.collect().await.unwrap().to_bytes(), "{}");
        assert!(driver.await.unwrap().is_ok());
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
        peer.await.unwrap();
    }
}
