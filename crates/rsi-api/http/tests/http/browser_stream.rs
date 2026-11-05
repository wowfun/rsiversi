use super::*;
use futures_util::{SinkExt, StreamExt};
use rsi_api_protocol::browser_stream::{CHUNK_BYTES, Head, Preface};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

fn request(harness: &Harness, name: &str) -> http::Request<()> {
    let mut request = format!("ws://{}{}", harness.address, harness.path(name))
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", harness.origin.parse().unwrap());
    request.headers_mut().insert(
        "cookie",
        browser_cookie(&harness.origin, TOKEN).parse().unwrap(),
    );
    request.headers_mut().insert(
        "sec-websocket-protocol",
        Preface {
            device: DeviceId::from_bytes([1; 16]),
            epoch: harness.epoch.clone(),
            length: 2,
        }
        .encode()
        .parse()
        .unwrap(),
    );
    request
}
async fn connect(harness: &Harness, name: &str) -> WebSocketStream<TcpStream> {
    connect_request(harness, request(harness, name)).await
}
async fn connect_request(
    harness: &Harness,
    request: http::Request<()>,
) -> WebSocketStream<TcpStream> {
    let (mut socket, response) = tokio_tungstenite::client_async(
        request,
        TcpStream::connect(harness.address).await.unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 101);
    socket
        .send(Message::Binary(Bytes::from_static(b"{}")))
        .await
        .unwrap();
    let head = socket.next().await.unwrap().unwrap().into_data();
    let head: Head = serde_json::from_slice(&head).unwrap();
    assert_eq!(head.status, 200);
    assert_eq!(head.epoch, harness.epoch);
    assert_eq!(head.content_type, "text/event-stream");
    socket
}
use bytes::Bytes;

#[tokio::test]
async fn browser_subscriptions_share_the_domain_owner_and_revoke_only_the_addressed_socket() {
    #[derive(Debug)]
    struct ObserveOwner(PendingEvents, tokio::sync::mpsc::Sender<DeviceId>);
    #[async_trait]
    impl ApiHandler for ObserveOwner {
        async fn invoke(
            &self,
            ctx: ApiContext,
            input: RetainedBytes,
            output: ApiResponseCapacity,
        ) -> Result<ApiOutput> {
            let rsi_api_protocol::CallOrigin::Device(ref actor) = ctx.origin else {
                panic!("expected browser owner")
            };
            self.1.try_send(actor.id.clone()).unwrap();
            self.0.invoke(ctx, input, output).await
        }
    }
    let h = Harness::start().await;
    let (sender, mut owners) = tokio::sync::mpsc::channel(2);
    let closed = Arc::new(Semaphore::new(0));
    let registration = h
        .registry
        .register(
            spec(
                "owner_stream",
                OperationClass::Subscription,
                OperationEffect::Read,
            ),
            Arc::new(ObserveOwner(PendingEvents(closed.clone()), sender)),
        )
        .unwrap();
    let mut first = connect(&h, "test/owner_stream").await;
    let mut second = request(&h, "test/owner_stream");
    second.headers_mut().insert(
        "cookie",
        browser_cookie(&h.origin, SECOND_TOKEN).parse().unwrap(),
    );
    second.headers_mut().insert(
        "sec-websocket-protocol",
        Preface {
            device: DeviceId::from_bytes([2; 16]),
            epoch: h.epoch.clone(),
            length: 2,
        }
        .encode()
        .parse()
        .unwrap(),
    );
    let second = connect_request(&h, second).await;
    assert_eq!(owners.recv().await.unwrap(), owners.recv().await.unwrap());
    h.authentication.second.cancel();
    tokio::time::timeout(Duration::from_secs(2), closed.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    consume_ready(&mut first).await;
    consume_first_event(&mut first).await;
    drop((first, second));
    registration.close().await;
    h.close().await;
}

async fn consume_ready(socket: &mut WebSocketStream<TcpStream>) {
    socket
        .send(Message::Binary(Bytes::from_static(&[1])))
        .await
        .unwrap();
    assert_eq!(
        socket.next().await.unwrap().unwrap(),
        Message::Binary(Bytes::from_static(b": ready\n\n"))
    );
}

async fn consume_first_event(socket: &mut WebSocketStream<TcpStream>) {
    let mut event = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !event.ends_with(b"\n\n") {
            socket
                .send(Message::Binary(Bytes::from_static(&[1])))
                .await
                .unwrap();
            let Message::Binary(bytes) = socket.next().await.unwrap().unwrap() else {
                panic!("expected credited SSE bytes");
            };
            assert!(!bytes.is_empty() && event.len() + bytes.len() <= 64);
            event.extend_from_slice(&bytes);
        }
    })
    .await
    .unwrap();
    assert_eq!(event, b"event: item\ndata: true\n\n");
}

#[tokio::test]
async fn unauthenticated_stale_epoch_is_unauthorized() {
    let harness = Harness::start().await;
    for cookie in [None, Some(browser_cookie(&harness.origin, "wrong"))] {
        let mut request = request(&harness, "connection/caller");
        request.headers_mut().remove("cookie");
        if let Some(cookie) = cookie {
            request
                .headers_mut()
                .insert("cookie", cookie.parse().unwrap());
        }
        request.headers_mut().insert(
            "sec-websocket-protocol",
            Preface {
                device: DeviceId::from_bytes([1; 16]),
                epoch: HostEpoch::from_bytes([4; 16]),
                length: 2,
            }
            .encode()
            .parse()
            .unwrap(),
        );
        let error = tokio_tungstenite::client_async(
            request,
            TcpStream::connect(harness.address).await.unwrap(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401)
        );
    }
    harness.close().await;
}

#[tokio::test]
async fn control_frames_preserve_input_credit_and_idle_subscription() {
    let harness = Harness::start().await;
    let dropped = Arc::new(Semaphore::new(0));
    let registration = harness
        .registry
        .register(
            spec("idle", OperationClass::Subscription, OperationEffect::Read),
            Arc::new(PendingEvents(dropped.clone())),
        )
        .unwrap();
    let (mut socket, _) = tokio_tungstenite::client_async(
        request(&harness, "test/idle"),
        TcpStream::connect(harness.address).await.unwrap(),
    )
    .await
    .unwrap();
    let ping = Bytes::from(vec![7; 125]);
    socket.send(Message::Ping(ping.clone())).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Message::Pong(ping.clone())
    );
    socket
        .send(Message::Binary(Bytes::from_static(b"{}")))
        .await
        .unwrap();
    assert!(matches!(
        socket.next().await.unwrap().unwrap(),
        Message::Binary(_)
    ));
    socket.send(Message::Pong(ping.clone())).await.unwrap();
    socket.send(Message::Ping(ping.clone())).await.unwrap();
    assert_eq!(
        socket.next().await.unwrap().unwrap(),
        Message::Pong(ping.clone())
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), socket.next())
            .await
            .is_err(),
        "control frames must not grant credit"
    );
    consume_ready(&mut socket).await;
    // PendingEvents emits one complete domain item before it becomes pending.
    // Pong may interleave with available body bytes; drain them before testing
    // control progress during an actually idle body read.
    consume_first_event(&mut socket).await;
    socket
        .send(Message::Binary(Bytes::from_static(&[1])))
        .await
        .unwrap();
    socket.send(Message::Ping(ping.clone())).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Message::Pong(ping)
    );
    assert_eq!(dropped.available_permits(), 0);
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), dropped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    registration.close().await;
    harness.close().await;
}

#[tokio::test]
async fn independent_idle_sockets_preserve_control_progress_and_release_on_disconnect() {
    let harness = Harness::start().await;
    let dropped = Arc::new(Semaphore::new(0));
    let registration = harness
        .registry
        .register(
            spec("idle", OperationClass::Subscription, OperationEffect::Read),
            Arc::new(PendingEvents(dropped.clone())),
        )
        .unwrap();
    let mut sockets = Vec::new();
    for _ in 0..8 {
        sockets.push(connect(&harness, "test/idle").await);
    }
    assert_eq!(
        harness
            .request("connection/caller")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // No pull has been sent. Each stream is idle and still occupies admission.
    drop(sockets);
    tokio::time::timeout(Duration::from_secs(2), dropped.acquire_many(8))
        .await
        .unwrap()
        .unwrap()
        .forget();
    let mut replacement = connect(&harness, "test/idle").await;
    replacement.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), dropped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    registration.close().await;
    harness.close().await;
}

#[tokio::test]
async fn pull_frames_keep_existing_sse_bytes_and_normal_eof() {
    let harness = Harness::start().await;
    let registration = harness
        .registry
        .register(
            spec(
                "events",
                OperationClass::Subscription,
                OperationEffect::Read,
            ),
            Arc::new(Payloads { subscription: true }),
        )
        .unwrap();
    let mut socket = connect(&harness, "test/events").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), socket.next())
            .await
            .is_err(),
        "body must wait for credit"
    );
    let mut body = Vec::new();
    loop {
        socket
            .send(Message::Binary(Bytes::from_static(&[1])))
            .await
            .unwrap();
        let Message::Binary(chunk) = socket.next().await.unwrap().unwrap() else {
            panic!("binary chunk");
        };
        assert!(chunk.len() <= CHUNK_BYTES);
        if chunk.is_empty() {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    assert_eq!(
        String::from_utf8(body).unwrap(),
        ": ready\n\nevent: item\ndata: 18446744073709551615\n\nevent: item\ndata: 18446744073709551615\n\nevent: end\ndata: {}\n\n"
    );
    assert!(matches!(
        socket.next().await.unwrap().unwrap(),
        Message::Close(_)
    ));
    registration.close().await;
    harness.close().await;
}

#[tokio::test]
async fn socket_handshake_fences_origin_cookie_device_generation_and_operation_class() {
    let harness = Harness::start().await;
    let registration = harness
        .registry
        .register(
            spec("idle", OperationClass::Subscription, OperationEffect::Read),
            Arc::new(PendingEvents(Arc::new(Semaphore::new(0)))),
        )
        .unwrap();
    for (field, value, status) in [
        ("origin", "http://other.invalid".to_owned(), 401),
        ("cookie", browser_cookie(&harness.origin, "wrong"), 401),
        (
            "cookie",
            format!("{0}; {0}", browser_cookie(&harness.origin, TOKEN)),
            401,
        ),
        ("authorization", format!("Bearer {TOKEN}"), 401),
        ("x-rsi-csrf", "1".to_owned(), 401),
        (
            "x-rsi-expected-device",
            DeviceId::from_bytes([1; 16]).as_str().to_owned(),
            401,
        ),
        (
            "sec-websocket-protocol",
            Preface {
                device: DeviceId::from_bytes([2; 16]),
                epoch: harness.epoch.clone(),
                length: 2,
            }
            .encode(),
            401,
        ),
        (
            "sec-websocket-protocol",
            Preface {
                device: DeviceId::from_bytes([1; 16]),
                epoch: HostEpoch::from_bytes([4; 16]),
                length: 2,
            }
            .encode(),
            409,
        ),
        (
            "sec-websocket-protocol",
            Preface {
                device: DeviceId::from_bytes([1; 16]),
                epoch: harness.epoch.clone(),
                length: usize::MAX,
            }
            .encode(),
            400,
        ),
    ] {
        let mut request = request(&harness, "test/idle");
        request.headers_mut().insert(
            http::header::HeaderName::from_bytes(field.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        let error = tokio_tungstenite::client_async(
            request,
            TcpStream::connect(harness.address).await.unwrap(),
        )
        .await
        .unwrap_err();
        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("HTTP rejection: {error}");
        };
        assert_eq!(response.status(), status);
    }
    for missing in ["origin", "cookie"] {
        let mut request = request(&harness, "test/idle");
        request.headers_mut().remove(missing);
        let error = tokio_tungstenite::client_async(
            request,
            TcpStream::connect(harness.address).await.unwrap(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&error, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401),
            "{missing}: {error}"
        );
    }
    let error = tokio_tungstenite::client_async(
        request(&harness, "connection/caller"),
        TcpStream::connect(harness.address).await.unwrap(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 400)
    );
    registration.close().await;
    harness.close().await;
}

#[tokio::test]
async fn socket_revocation_retirement_shutdown_and_invalid_credit_release_idle_streams() {
    for action in ["revoke", "retire", "shutdown", "credit", "extra-credit"] {
        let harness = Harness::start().await;
        let dropped = Arc::new(Semaphore::new(0));
        let registration = harness
            .registry
            .register(
                spec("idle", OperationClass::Subscription, OperationEffect::Read),
                Arc::new(PendingEvents(dropped.clone())),
            )
            .unwrap();
        let mut socket = connect(&harness, "test/idle").await;
        let mut registration = Some(registration);
        match action {
            "revoke" => harness.authentication.first.cancel(),
            "retire" => registration.take().unwrap().close().await,
            "shutdown" => harness.stop.cancel(),
            "extra-credit" => {
                socket
                    .send(Message::Binary(Bytes::from_static(&[1])))
                    .await
                    .unwrap();
                assert!(matches!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Binary(_)
                ));
                consume_first_event(&mut socket).await;
                socket
                    .send(Message::Binary(Bytes::from_static(&[1])))
                    .await
                    .unwrap();
                socket
                    .send(Message::Binary(Bytes::from_static(&[1])))
                    .await
                    .unwrap();
            }
            _ => socket
                .send(Message::Binary(Bytes::from_static(&[2])))
                .await
                .unwrap(),
        }
        tokio::time::timeout(Duration::from_secs(2), dropped.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        drop(socket);
        if let Some(registration) = registration {
            registration.close().await;
        }
        harness.close().await;
    }
}
