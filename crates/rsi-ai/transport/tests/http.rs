use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::{StreamExt as _, stream};
use http::Method;
use rsi_ai_transport::{
    ByteStream, HttpRequest, HttpTransport, MAX_HTTP_RESPONSE_ITEM_BYTES, ReqwestTransport,
    bearer_authorization_header,
};
use rsi_credentials_protocol::SecretValue;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

async fn stalled_body_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
            )
            .await
            .expect("response headers");
        std::future::pending::<()>().await;
    });
    (format!("http://{address}/stream"), task)
}

async fn stalled_header_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    let task = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.expect("accept");
        std::future::pending::<()>().await;
    });
    (format!("http://{address}/headers"), task)
}

async fn request_capture_server() -> (
    String,
    tokio::sync::oneshot::Receiver<Vec<u8>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    let (captured_sender, captured) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let read = socket.read(&mut chunk).await.expect("read request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(5).any(|window| window == b"0\r\n\r\n") {
                break;
            }
        }
        captured_sender.send(request).expect("capture receiver");
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await
            .expect("response");
    });
    (format!("http://{address}/upload"), captured, task)
}

async fn large_body_server(body_bytes: usize) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let address = listener.local_addr().expect("address");
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let mut chunk = [0_u8; 1024];
            let read = socket.read(&mut chunk).await.expect("read request");
            assert_ne!(read, 0, "request ended before its headers");
            request.extend_from_slice(&chunk[..read]);
        }
        let headers = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {body_bytes}\r\n\r\n"
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("response headers");
        socket
            .write_all(&vec![b'x'; body_bytes])
            .await
            .expect("response body");
    });
    (format!("http://{address}/large"), task)
}

#[tokio::test]
async fn cancelling_after_headers_terminates_the_response_body() {
    let (url, server) = stalled_body_server().await;
    let transport: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::new().expect("transport"));
    let abort = CancellationToken::new();
    let mut response = transport
        .execute(
            HttpRequest::new(Method::GET, url).expect("request"),
            abort.clone(),
        )
        .await
        .expect("response headers");

    abort.cancel();
    let error = tokio::time::timeout(Duration::from_millis(250), response.body.next())
        .await
        .expect("body observes cancellation")
        .expect("terminal body error")
        .expect_err("cancelled body");
    assert_eq!(error.code(), "http.cancelled");
    server.abort();
}

#[tokio::test]
async fn configured_request_timeout_bounds_a_server_that_never_sends_headers() {
    let (url, server) = stalled_header_server().await;
    let transport =
        ReqwestTransport::with_timeouts(Duration::from_secs(1), Duration::from_millis(50))
            .expect("transport");
    let error = transport
        .execute(
            HttpRequest::new(Method::GET, url).expect("request"),
            CancellationToken::new(),
        )
        .await
        .expect_err("request timeout");
    assert_eq!(error.code(), "http.timeout");
    server.abort();
}

#[tokio::test]
async fn request_body_can_be_pulled_incrementally() {
    let (url, captured, server) = request_capture_server().await;
    let body: ByteStream = Box::pin(stream::iter([
        Ok(Bytes::from_static(b"alpha")),
        Ok(Bytes::from_static(b"beta")),
    ]));
    let response = ReqwestTransport::new()
        .expect("transport")
        .execute(
            HttpRequest::new(Method::POST, url)
                .expect("request")
                .body_stream(body),
            CancellationToken::new(),
        )
        .await
        .expect("response");
    drop(response);

    let request = captured.await.expect("captured request");
    assert!(request.windows(5).any(|window| window == b"alpha"));
    assert!(request.windows(4).any(|window| window == b"beta"));
    server.await.expect("server");
}

#[tokio::test]
async fn production_response_items_obey_the_decoder_retention_bound() {
    let body_bytes = MAX_HTTP_RESPONSE_ITEM_BYTES * 64;
    let (url, server) = large_body_server(body_bytes).await;
    let mut response = ReqwestTransport::new()
        .expect("transport")
        .execute(
            HttpRequest::new(Method::GET, url).expect("request"),
            CancellationToken::new(),
        )
        .await
        .expect("response");
    let mut observed = 0_usize;
    while let Some(item) = response.body.next().await {
        let item = item.expect("body item");
        assert!(
            item.len() <= MAX_HTTP_RESPONSE_ITEM_BYTES,
            "production transport yielded {} bytes",
            item.len()
        );
        observed += item.len();
    }
    assert_eq!(observed, body_bytes);
    server.await.expect("server");
}

#[test]
fn bearer_authorization_header_is_shared_and_rejects_invalid_header_text() {
    let valid = SecretValue::new("shared-secret").expect("secret");
    let header = bearer_authorization_header(&valid).expect("header");
    assert_eq!(
        header.to_str().expect("header text"),
        "Bearer shared-secret"
    );
    assert!(header.is_sensitive());
    assert_eq!(format!("{header:?}"), "Sensitive");

    let invalid = SecretValue::new("line\nbreak").expect("secret storage permits UTF-8 text");
    assert_eq!(
        bearer_authorization_header(&invalid)
            .expect_err("HTTP header rejects newline")
            .code(),
        "http.invalid_credential"
    );
}
