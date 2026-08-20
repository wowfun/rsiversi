use std::{sync::Arc, time::Duration};

use futures_util::StreamExt as _;
use http::Method;
use rsi_ai_transport::{HttpRequest, HttpTransport, ReqwestTransport};
use tokio::io::AsyncWriteExt as _;
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
