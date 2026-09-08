use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1::{Connection, SendRequest};
use hyper_util::rt::TokioIo;
use rsi_api_client::ResponseBytes;
use rsi_api_protocol::{ApiError, Result};
use std::{path::Path, pin::Pin};
use tokio::net::UnixStream;

type Driver = Connection<TokioIo<UnixStream>, Full<Bytes>>;
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
