use crate::server::Body;
use bytes::Bytes;
use futures_util::StreamExt;
use http::Response;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use rsi_api_protocol::{ApiError, ApiStream};
use std::io;
use tokio_util::sync::CancellationToken;

pub(crate) fn response(mut source: ApiStream, revoked: CancellationToken) -> Response<Body> {
    let frames = async_stream::stream! {
        // Firefox does not resolve an otherwise idle Fetch from headers alone.
        yield Ok::<_, io::Error>(Frame::data(Bytes::from_static(b": ready\n\n")));
        loop {
            let item = tokio::select! { biased;
                () = revoked.cancelled() => Some(Err(ApiError::Unauthorized)),
                item = source.next() => item,
            };
            let Some(item) = item else { break; };
            let item = item.and_then(|message| {
                if message.binary.is_some() { Err(ApiError::Backend("SSE cannot carry binary replies".into())) } else { Ok(message.json) }
            });
            match item {
                Ok(bytes) => {
                    yield Ok::<_, io::Error>(Frame::data(Bytes::from_static(b"event: item\ndata: ")));
                    yield Ok(Frame::data(bytes.into_bytes()));
                    yield Ok(Frame::data(Bytes::from_static(b"\n\n")));
                }
                Err(error) => {
                    let (prefix, bytes) = match error {
                        ApiError::Domain(bytes) => (b"event: domain-error\ndata: ".as_slice(), bytes.into_bytes()),
                        error => (b"event: error\ndata: ".as_slice(), error_json(&error)),
                    };
                    yield Ok(Frame::data(Bytes::from_static(prefix)));
                    yield Ok(Frame::data(bytes));
                    yield Ok(Frame::data(Bytes::from_static(b"\n\n")));
                    break;
                }
            }
        }
        drop(source);
        yield Ok(Frame::data(Bytes::from_static(b"event: end\ndata: {}\n\n")));
    };
    let mut response = Response::new(StreamBody::new(frames).boxed_unsync());
    response.headers_mut().insert(
        "content-type",
        http::HeaderValue::from_static("text/event-stream"),
    );
    response
}

fn error_json(error: &ApiError) -> Bytes {
    Bytes::from_static(match error {
        ApiError::OutcomeUnknown => b"{\"code\":\"outcome_unknown\"}",
        ApiError::Unauthorized => b"{\"code\":\"unauthorized\"}",
        ApiError::Capacity => b"{\"code\":\"capacity\"}",
        ApiError::ShuttingDown => b"{\"code\":\"generation_retired\"}",
        ApiError::Unavailable => b"{\"code\":\"unavailable\"}",
        ApiError::Invalid(_) => b"{\"code\":\"invalid\"}",
        ApiError::Backend(_) | ApiError::Domain(_) => b"{\"code\":\"backend\"}",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[tokio::test]
    async fn idle_subscription_writes_opening_without_waiting_for_a_domain_item() {
        let response = response(
            Box::pin(futures_util::stream::pending()),
            CancellationToken::new(),
        );
        let mut body = response.into_body();
        let opening = body
            .frame()
            .now_or_never()
            .expect("opening must be ready")
            .expect("opening frame")
            .unwrap()
            .into_data()
            .unwrap();
        assert_eq!(opening, b": ready\n\n".as_slice());
        assert!(body.frame().now_or_never().is_none());
    }
}
