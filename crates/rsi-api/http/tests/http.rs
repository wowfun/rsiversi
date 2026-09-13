use async_trait::async_trait;
use rsi_api::ApiRegistry;
use rsi_api_http::{HttpConfig, HttpServer, HttpServices};
use rsi_api_protocol::{
    ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput, ApiRegistrar, ApiRegistration,
    ApiResponseCapacity, AuthenticatedDevice, DeviceAuthentication, DeviceId, EndpointId,
    HostEpoch, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
    Result, RetainedBytes,
};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[derive(Debug)]
struct Authentication(CancellationToken);
impl DeviceAuthentication for Authentication {
    fn authenticate(&self, token: &SecretValue) -> Result<AuthenticatedDevice> {
        if token.expose_secret() != TOKEN || self.0.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        Ok(AuthenticatedDevice {
            id: DeviceId::from_bytes([1; 16]),
            revoked: self.0.clone(),
        })
    }
}

struct Harness {
    address: std::net::SocketAddr,
    origin: String,
    epoch: HostEpoch,
    registry: Arc<ApiRegistry>,
    connection: rsi_api::ConnectionApi,
    authentication: Arc<Authentication>,
    diagnostics: rsi_api_http::HttpDiagnostics,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Result<()>>,
}
impl Harness {
    async fn start() -> Self {
        Self::start_with_tls(false).await
    }
    async fn start_with_tls(tls: bool) -> Self {
        Self::start_with_assets(tls, None).await
    }
    async fn start_with_assets(
        tls: bool,
        assets: Option<Arc<dyn rsi_api_http::HttpAssets>>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("{}://{address}", if tls { "https" } else { "http" });
        let execution = Execution::native(tokio::runtime::Handle::current());
        let registry = Arc::new(ApiRegistry::new(execution.clone()));
        let authentication = Arc::new(Authentication(CancellationToken::new()));
        let epoch = HostEpoch::from_bytes([3; 16]);
        let connection = rsi_api::ConnectionApi::register(
            registry.clone(),
            registry.as_ref(),
            EndpointId::from_bytes([2; 16]),
            epoch.clone(),
        )
        .unwrap();
        let services = HttpServices {
            dispatch: registry.clone(),
            authentication: authentication.clone(),
            endpoint: EndpointId::from_bytes([2; 16]),
            epoch: epoch.clone(),
        };
        let config = HttpConfig {
            bind: address,
            public_origin: origin.clone(),
            tls: tls.then(tls_files),
            allow_loopback_http: !tls,
        };
        let server = HttpServer::from_listener(execution, listener, config, services)
            .await
            .unwrap();
        let server = if let Some(assets) = assets {
            server.with_assets(assets)
        } else {
            server
        };
        let stop = CancellationToken::new();
        let diagnostics = server.diagnostics();
        let task = tokio::spawn(server.serve(stop.clone()));
        Self {
            address,
            origin,
            epoch,
            registry,
            connection,
            authentication,
            diagnostics,
            stop,
            task,
        }
    }
    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }
    fn request(&self, operation: &str) -> reqwest::RequestBuilder {
        Self::client()
            .post(format!("{}/api/v1/{operation}/1", self.origin))
            .bearer_auth(TOKEN)
            .header("content-type", "application/json")
            .header("x-rsi-wire-version", "1")
            .header("x-rsi-host-epoch", self.epoch.as_str())
    }
    async fn raw(&self, operation: &str) -> TcpStream {
        let mut socket = TcpStream::connect(self.address).await.unwrap();
        let request = format!(
            "POST /api/v1/{operation}/1 HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {TOKEN}\r\nX-Rsi-Wire-Version: 1\r\nX-Rsi-Host-Epoch: {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}",
            self.address,
            self.epoch.as_str()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        socket
    }
    async fn close(self) {
        self.stop.cancel();
        self.task.await.unwrap().unwrap();
        self.connection.close().await;
        self.registry.close().await;
    }
}

#[derive(Debug)]
struct Assets(rsi_api_protocol::RetainedBytes);

#[tokio::test]
async fn expected_device_is_checked_before_domain_dispatch_and_cookie_removal() {
    let harness = Harness::start().await;
    for (identity, status) in [
        (DeviceId::from_bytes([1; 16]), 200),
        (DeviceId::from_bytes([2; 16]), 401),
    ] {
        let response = harness
            .request("connection/caller")
            .header("x-rsi-expected-device", identity.as_str())
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        response.bytes().await.unwrap();
        let response = Harness::client()
            .post(format!("{}/api/v1/logout", harness.origin))
            .header("origin", &harness.origin)
            .header("x-rsi-csrf", "1")
            .header("cookie", format!("rsi-device={TOKEN}"))
            .header("x-rsi-expected-device", identity.as_str())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(response.headers().contains_key("set-cookie"), status == 200);
        response.bytes().await.unwrap();
    }
    let response = Harness::client()
        .post(format!("{}/api/v1/logout", harness.origin))
        .header("origin", &harness.origin)
        .header("x-rsi-csrf", "1")
        .header(
            "x-rsi-expected-device",
            DeviceId::from_bytes([1; 16]).as_str(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "absent device cookie is already logged out"
    );
    response.bytes().await.unwrap();
    let response = Harness::client()
        .post(format!("{}/api/v1/logout", harness.origin))
        .header("origin", &harness.origin)
        .header("x-rsi-csrf", "1")
        .header("cookie", format!("rsi-device={TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        401,
        "clearing a present cookie requires an explicit device pin"
    );
    assert!(!response.headers().contains_key("set-cookie"));
    response.bytes().await.unwrap();
    let response = Harness::client()
        .post(format!("{}/api/v1/logout", harness.origin))
        .header("origin", &harness.origin)
        .header("x-rsi-csrf", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "an absent cookie needs no pin");
    response.bytes().await.unwrap();
    for values in [
        vec!["invalid".to_owned()],
        vec!["01".repeat(16), "01".repeat(16)],
    ] {
        let mut request = harness.request("connection/caller");
        for value in values {
            request = request.header("x-rsi-expected-device", value);
        }
        let response = request.body("{}").send().await.unwrap();
        assert!(response.status().is_client_error());
        response.bytes().await.unwrap();
    }
    harness.close().await;
}
impl rsi_api_http::HttpAssets for Assets {
    fn get(&self, path: &str) -> Result<Option<rsi_api_http::HttpAsset>> {
        Ok((path == "/app.wasm").then(|| rsi_api_http::HttpAsset {
            kind: rsi_api_http::AssetType::Wasm,
            bytes: self.0.clone(),
        }))
    }
}

#[tokio::test]
async fn immutable_assets_are_public_exact_and_bounded_without_blocking_api_controls() {
    let budget = rsi_api_protocol::ByteBudget::new(8 * 1024 * 1024).unwrap();
    let assets = Arc::new(Assets(
        budget
            .reserve(budget.limit())
            .unwrap()
            .retain_vec(vec![0; budget.limit()])
            .unwrap(),
    ));
    let harness = Harness::start_with_assets(true, Some(assets)).await;
    let client = slow_http2_client();
    let get = || client.get(format!("{}/app.wasm", harness.origin));
    for (path, status) in [("/missing.js", 404), ("/app.wasm?query=1", 400)] {
        assert_eq!(
            client
                .get(format!("{}{path}", harness.origin))
                .send()
                .await
                .unwrap()
                .status(),
            status
        );
    }
    assert_eq!(
        get()
            .header("origin", "https://foreign.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        get()
            .header("range", "bytes=0-9")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(get().body("unexpected").send().await.unwrap().status(), 400);
    let mut pending = Vec::new();
    for _ in 0..8 {
        let response = get().send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.version(), http::Version::HTTP_2);
        assert_eq!(response.headers()["content-type"], "application/wasm");
        assert_eq!(response.headers()["referrer-policy"], "same-origin");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("'wasm-unsafe-eval'")
        );
        pending.push(response);
    }
    assert_eq!(get().send().await.unwrap().status(), 429);
    let control = client
        .post(format!("{}/api/v1/connection/describe/1", harness.origin))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .header("x-rsi-wire-version", "1")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(control.status(), 200);
    control.bytes().await.unwrap();
    assert_eq!(
        budget.used(),
        budget.limit(),
        "all deliveries share one immutable file owner"
    );
    drop(pending);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = get().send().await.unwrap();
            if response.status() == 200 {
                break;
            }
            assert_eq!(response.status(), 429);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(client);
    harness.close().await;
    assert_eq!(budget.used(), 0);
}

fn tls_files() -> rsi_api_http::TlsFiles {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/rsi-api/tls");
    rsi_api_http::TlsFiles {
        certificate: directory.join("server-cert.pem"),
        key: directory.join("server-key.pem"),
    }
}

#[tokio::test]
async fn tls_requires_explicit_certificate_trust_and_browser_cookies_are_secure() {
    let harness = Harness::start_with_tls(true).await;
    assert!(
        harness
            .request("connection/describe")
            .body(r#"{"wire_version":1}"#)
            .send()
            .await
            .is_err()
    );
    let certificate = reqwest::Certificate::from_pem(include_bytes!(
        "../../../../fixtures/rsi-api/tls/server-cert.pem"
    ))
    .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(certificate)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let login = client
        .post(format!("{}/api/v1/login", harness.origin))
        .bearer_auth(TOKEN)
        .header("origin", &harness.origin)
        .header("x-rsi-csrf", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    assert!(
        login.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
    let reply = client
        .post(format!("{}/api/v1/connection/describe/1", harness.origin))
        .bearer_auth(TOKEN)
        .header("x-rsi-wire-version", "1")
        .header("content-type", "application/json")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    harness.close().await;
}

#[derive(Debug)]
struct Payloads {
    subscription: bool,
}
#[async_trait]
impl ApiHandler for Payloads {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        if self.subscription {
            let ApiResponseCapacity::Subscription { budget, maximum } = output else {
                panic!("subscription")
            };
            let source =
                futures_util::stream::unfold((0, budget), move |(index, budget)| async move {
                    if index == 2 {
                        return None;
                    }
                    let reservation = budget.reserve(maximum).unwrap();
                    let value: serde_json::Value =
                        serde_json::from_str("18446744073709551615").unwrap();
                    Some((
                        Ok(ApiMessage {
                            json: reservation.encode(&value).unwrap(),
                            binary: None,
                        }),
                        (index + 1, budget),
                    ))
                });
            Ok(ApiOutput::Stream(Box::pin(source)))
        } else {
            let ApiResponseCapacity::Finite(mut reservation) = output else {
                panic!("finite")
            };
            let binary = reservation.split(4)?.copy(&[0, 127, 128, 255])?;
            let json = reservation.encode(&serde_json::json!({"length":4}))?;
            Ok(ApiOutput::Reply(ApiMessage {
                json,
                binary: Some(binary),
            }))
        }
    }
}

#[tokio::test]
async fn finite_wire_lengths_survive_http2_delivery_and_http1_binary_framing() {
    for tls in [true, false] {
        let harness = Harness::start_with_tls(tls).await;
        let client = if tls {
            slow_http2_client()
        } else {
            Harness::client()
        };
        let binary = harness
            .registry
            .register(
                spec("binary", OperationClass::Data, OperationEffect::Read),
                Arc::new(Payloads {
                    subscription: false,
                }),
            )
            .unwrap();
        for (operation, input, status) in [
            ("connection/describe", r#"{"wire_version":1}"#, 200),
            ("test/binary", "{}", 200),
            ("connection/describe", r#"{"wire_version":2}"#, 404),
        ] {
            let reply = client
                .post(format!("{}/api/v1/{operation}/1", harness.origin))
                .bearer_auth(TOKEN)
                .header("content-type", "application/json")
                .header("x-rsi-wire-version", "1")
                .header("x-rsi-host-epoch", harness.epoch.as_str())
                .body(input)
                .send()
                .await
                .unwrap();
            assert_eq!(reply.status(), status);
            assert_eq!(
                reply.version(),
                if tls {
                    http::Version::HTTP_2
                } else {
                    http::Version::HTTP_11
                }
            );
            let length = reply
                .headers()
                .get("content-length")
                .unwrap_or_else(|| {
                    panic!(
                        "{operation} under {:?} must declare its finite wire length: {:?}",
                        reply.version(),
                        reply.headers()
                    )
                })
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let bytes = reply.bytes().await.unwrap();
            assert_eq!(length, bytes.len());
            if operation == "test/binary" {
                assert_eq!(length, 16 + br#"{"length":4}"#.len() + 4);
                assert_eq!(&bytes[16..length - 4], br#"{"length":4}"#);
                assert_eq!(&bytes[length - 4..], &[0, 127, 128, 255]);
            } else if status == 200 {
                serde_json::from_slice::<rsi_api_protocol::ConnectionDescription>(&bytes).unwrap();
            } else {
                assert_eq!(bytes, br#"{"code":"unavailable"}"#.as_slice());
            }
        }
        binary.close().await;
        drop(client);
        harness.close().await;
    }
}

#[tokio::test]
async fn binary_payloads_have_exact_lengths_and_sse_preserves_numbers_and_explicit_end() {
    let harness = Harness::start().await;
    let binary = harness
        .registry
        .register(
            spec("binary", OperationClass::Data, OperationEffect::Read),
            Arc::new(Payloads {
                subscription: false,
            }),
        )
        .unwrap();
    let stream = harness
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
    let reply = harness
        .request("test/binary")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(
        reply.headers()["content-type"],
        "application/vnd.rsi.binary"
    );
    let bytes = reply.bytes().await.unwrap();
    let metadata_len = usize::try_from(u64::from_be_bytes(bytes[..8].try_into().unwrap())).unwrap();
    assert_eq!(u64::from_be_bytes(bytes[8..16].try_into().unwrap()), 4);
    assert_eq!(&bytes[16..16 + metadata_len], br#"{"length":4}"#);
    assert_eq!(&bytes[16 + metadata_len..], [0, 127, 128, 255]);
    let reply = harness
        .request("test/events")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(reply.headers()["content-type"], "text/event-stream");
    assert_eq!(
        reply.text().await.unwrap(),
        ": ready\n\nevent: item\ndata: 18446744073709551615\n\nevent: item\ndata: 18446744073709551615\n\nevent: end\ndata: {}\n\n"
    );
    binary.close().await;
    stream.close().await;
    harness.close().await;
}

#[tokio::test]
async fn dropping_the_listener_task_cancels_read_transports_without_cancelling_its_parent() {
    let harness = Harness::start().await;
    let (gate, registration) = blocked(&harness, OperationEffect::Read);
    let socket = harness.raw("test/blocked").await;
    gate.entered.acquire().await.unwrap().forget();
    harness.task.abort();
    assert!(harness.task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), gate.dropped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(!harness.stop.is_cancelled());
    drop(socket);
    registration.close().await;
    harness.registry.close().await;
}

#[derive(Debug)]
struct Blocked {
    entered: Semaphore,
    release: Semaphore,
    completed: Semaphore,
    dropped: Arc<Semaphore>,
}
struct OnDrop(Arc<Semaphore>);
impl Drop for OnDrop {
    fn drop(&mut self) {
        self.0.add_permits(1);
    }
}
#[async_trait]
impl ApiHandler for Blocked {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let _lifetime = OnDrop(self.dropped.clone());
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        self.completed.add_permits(1);
        let ApiResponseCapacity::Finite(reservation) = output else {
            panic!("finite")
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: reservation.encode(&true)?,
            binary: None,
        }))
    }
}
fn blocked(harness: &Harness, effect: OperationEffect) -> (Arc<Blocked>, ApiRegistration) {
    let gate = Arc::new(Blocked {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        completed: Semaphore::new(0),
        dropped: Arc::new(Semaphore::new(0)),
    });
    let registration = harness
        .registry
        .register(spec("blocked", OperationClass::Data, effect), gate.clone())
        .unwrap();
    (gate, registration)
}
fn spec(name: &str, class: OperationClass, effect: OperationEffect) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("test", name, 1).unwrap(),
        class,
        effect,
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 1024,
        maximum_response_bytes: 4096,
    }
}

#[tokio::test]
async fn malformed_encoding_is_rejected_before_exhausted_mutation_admission() {
    use rsi_api_protocol::{ApiDispatch as _, CallOrigin};
    let harness = Harness::start().await;
    let (gate, registration) = blocked(&harness, OperationEffect::Mutation);
    let origin = CallOrigin::Device(
        harness
            .authentication
            .authenticate(&SecretValue::new(TOKEN).unwrap())
            .unwrap(),
    );
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(
            harness
                .registry
                .admit(
                    &spec("blocked", OperationClass::Data, OperationEffect::Mutation).id,
                    origin.clone(),
                )
                .unwrap(),
        );
    }
    for (header, value) in [("content-encoding", "gzip"), ("content-type", "text/plain")] {
        let mut request = harness.request("test/blocked").body("{}").build().unwrap();
        request.headers_mut().insert(header, value.parse().unwrap());
        let response = Harness::client().execute(request).await.unwrap();
        assert_eq!(response.status(), 400);
        response.bytes().await.unwrap();
    }
    assert_eq!(
        harness
            .request("test/blocked")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    assert_eq!(gate.entered.available_permits(), 0);
    drop(held);
    registration.close().await;
    harness.close().await;
}

#[tokio::test]
async fn chunked_requests_retain_received_bytes_instead_of_each_operation_maximum() {
    use rsi_api_protocol::{ApiDispatch, CallOrigin};
    let harness = Harness::start().await;
    let gate = Arc::new(Blocked {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        completed: Semaphore::new(0),
        dropped: Arc::new(Semaphore::new(0)),
    });
    let mut operation = spec("chunked", OperationClass::Data, OperationEffect::Read);
    operation.maximum_request_bytes = rsi_api_protocol::MAXIMUM_API_BYTES;
    let registration = harness
        .registry
        .register(operation.clone(), gate.clone())
        .unwrap();
    let admission = harness
        .registry
        .admit(&operation.id, CallOrigin::Local)
        .unwrap();
    let budget = admission.input_budget();
    drop(admission);
    let mut peers = Vec::new();
    for expected in 1..=2 {
        let mut peer = TcpStream::connect(harness.address).await.unwrap();
        let headers = format!(
            "POST /api/v1/test/chunked/1 HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {TOKEN}\r\nX-Rsi-Wire-Version: 1\r\nX-Rsi-Host-Epoch: {}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n",
            harness.address,
            harness.epoch.as_str()
        );
        peer.write_all(headers.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while budget.used() < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            budget.used(),
            expected,
            "partial body reserved its unused maximum"
        );
        peers.push(peer);
    }
    for peer in &mut peers {
        peer.write_all(b"0\r\n\r\n").await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), gate.entered.acquire_many(2))
        .await
        .unwrap()
        .unwrap()
        .forget();
    gate.release.add_permits(2);
    for mut peer in peers {
        let mut response = Vec::new();
        peer.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
    }
    assert_eq!(budget.used(), 0);
    registration.close().await;
    harness.close().await;
}

#[derive(Debug)]
struct LargeReply(Semaphore);
#[async_trait]
impl ApiHandler for LargeReply {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let ApiResponseCapacity::Finite(reservation) = output else {
            panic!("finite")
        };
        let mut receiver = reservation.reserve()?.receive();
        receiver.append(b"\"")?;
        for _ in 0..2047 {
            receiver.append(&[b' '; 4096])?;
        }
        receiver.append(b"\"")?;
        self.0.add_permits(1);
        Ok(ApiOutput::Reply(ApiMessage {
            json: receiver.finish(),
            binary: None,
        }))
    }
}

#[tokio::test]
async fn blocked_response_delivery_keeps_device_quota_but_releases_domain_work() {
    let harness = Harness::start().await;
    let producer = Arc::new(LargeReply(Semaphore::new(0)));
    let mut large = spec("large", OperationClass::Data, OperationEffect::Read);
    large.maximum_response_bytes = 8 * 1024 * 1024;
    let registration = harness.registry.register(large, producer.clone()).unwrap();
    let mut sockets = Vec::new();
    for _ in 0..4 {
        sockets.push(harness.raw("test/large").await);
        tokio::time::timeout(Duration::from_secs(5), producer.0.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
    // Domain work has finished; the client deliberately reads no response bytes.
    tokio::time::timeout(Duration::from_secs(2), registration.close())
        .await
        .unwrap();
    let response = harness
        .request("connection/operations")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        429,
        "delivery still owns four device Data slots"
    );
    let control = harness
        .request("connection/describe")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(control.status(), 200);
    drop(sockets);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = harness
                .request("connection/operations")
                .body("{}")
                .send()
                .await
                .unwrap();
            if response.status() == 200 {
                break;
            }
            assert_eq!(response.status(), 429);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    harness.close().await;
}

#[tokio::test]
async fn incomplete_headers_cannot_exceed_thirty_two_unclassified_connections() {
    let harness = Harness::start().await;
    let mut sockets = Vec::new();
    for _ in 0..32 {
        let mut socket = TcpStream::connect(harness.address).await.unwrap();
        socket
            .write_all(b"POST /api/v1/connection/describe/1 HTTP/1.1\r\n")
            .await
            .unwrap();
        sockets.push(socket);
    }
    let mut overflow = TcpStream::connect(harness.address).await.unwrap();
    let mut byte = [0];
    let result = tokio::time::timeout(Duration::from_secs(2), overflow.read(&mut byte))
        .await
        .unwrap();
    assert!(
        matches!(result, Ok(0)) || result.is_err(),
        "overflow was served"
    );
    assert_eq!(harness.diagnostics.snapshot().connection_failures, 1);
    drop(sockets);
    drop(overflow);
    harness.close().await;
}

#[tokio::test]
async fn http2_stream_delivery_retains_device_quota_through_flow_control_and_reset() {
    let harness = Harness::start_with_tls(true).await;
    let certificate = reqwest::Certificate::from_pem(include_bytes!(
        "../../../../fixtures/rsi-api/tls/server-cert.pem"
    ))
    .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(certificate)
        .http2_prior_knowledge()
        .http2_initial_stream_window_size(1024)
        .http2_initial_connection_window_size(64 * 1024)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let request = |name: &str| {
        client
            .post(format!("{}/api/v1/{name}/1", harness.origin))
            .bearer_auth(TOKEN)
            .header("content-type", "application/json")
            .header("x-rsi-wire-version", "1")
            .header("x-rsi-host-epoch", harness.epoch.as_str())
    };
    let producer = Arc::new(LargeReply(Semaphore::new(0)));
    let mut large = spec("large", OperationClass::Data, OperationEffect::Read);
    large.maximum_response_bytes = 8 * 1024 * 1024;
    let registration = harness.registry.register(large, producer.clone()).unwrap();
    let mut responses = Vec::new();
    for _ in 0..4 {
        let response = request("test/large").body("{}").send().await.unwrap();
        assert_eq!(response.version(), http::Version::HTTP_2);
        assert_eq!(response.headers()["x-rsi-http-version"], "2");
        assert_eq!(response.status(), 200);
        responses.push(response);
    }
    tokio::time::timeout(Duration::from_secs(2), registration.close())
        .await
        .unwrap();
    let full = request("connection/operations")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(
        full.status(),
        429,
        "each blocked stream retains its own Data slot"
    );
    full.bytes().await.unwrap();
    let control = request("connection/describe")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(control.version(), http::Version::HTTP_2);
    assert_eq!(
        control.status(),
        200,
        "data flow control preserves control progress"
    );
    control.bytes().await.unwrap();
    let foreign = request("connection/describe")
        .header("host", "foreign.invalid")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(
        foreign.status(),
        401,
        "HTTP/2 authority and Host must agree"
    );
    foreign.bytes().await.unwrap();
    drop(responses);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = request("connection/operations")
                .body("{}")
                .send()
                .await
                .unwrap();
            let status = response.status();
            response.bytes().await.unwrap();
            if status == 200 {
                break;
            }
            assert_eq!(status, 429);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    harness.close().await;
}

#[tokio::test]
async fn handshake_and_cookie_calls_enforce_origin_epoch_and_closed_requests() {
    let harness = Harness::start().await;
    let response = harness
        .request("connection/describe")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: serde_json::Value =
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(value["host_epoch"], harness.epoch.as_str());
    for request in [
        harness
            .request("connection/describe")
            .header("origin", "https://foreign.invalid")
            .body(r#"{"wire_version":1}"#),
        harness
            .request("connection/describe")
            .header("x-rsi-local-key", "4".repeat(64))
            .body(r#"{"wire_version":1}"#),
        harness
            .request("connection/describe")
            .header("host", "foreign.invalid")
            .body(r#"{"wire_version":1}"#),
        harness
            .request("connection/describe")
            .body(r#"{"wire_version":1,"undeclared":true}"#),
        harness
            .request("connection/operations")
            .header("x-rsi-host-epoch", "0".repeat(32))
            .body("{}"),
    ] {
        assert!(!request.send().await.unwrap().status().is_success());
    }
    let login = Harness::client()
        .post(format!("{}/api/v1/login", harness.origin))
        .bearer_auth(TOKEN)
        .header("origin", &harness.origin)
        .header("x-rsi-csrf", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookie = login.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let credential = cookie.split(';').next().unwrap();
    let browser = || {
        Harness::client()
            .post(format!("{}/api/v1/connection/operations/1", harness.origin))
            .header("cookie", credential)
            .header("x-rsi-wire-version", "1")
            .header("x-rsi-host-epoch", harness.epoch.as_str())
            .header("content-type", "application/json")
            .body("{}")
    };
    assert_eq!(browser().send().await.unwrap().status(), 401);
    assert_eq!(
        browser()
            .header("origin", &harness.origin)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        browser()
            .header("origin", &harness.origin)
            .header("x-rsi-csrf", "1")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(harness.diagnostics.snapshot().rejected_requests, 7);
    assert_eq!(harness.diagnostics.snapshot().failed_requests, 0);
    harness.close().await;
}

#[derive(Debug)]
struct Failure;
#[async_trait]
impl ApiHandler for Failure {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        _: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        Err(ApiError::Backend("isolated diagnostic failure".into()))
    }
}

#[tokio::test]
async fn diagnostics_observe_protocol_backend_and_tls_failures_but_not_retirement() {
    let harness = Harness::start().await;
    let _operation = harness
        .registry
        .register(
            spec("failure", OperationClass::Control, OperationEffect::Read),
            Arc::new(Failure),
        )
        .unwrap();
    assert_eq!(
        harness
            .request("test/failure")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        500
    );
    let mut malformed = TcpStream::connect(harness.address).await.unwrap();
    malformed
        .write_all(b"INVALID HEADER\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    malformed.read_to_end(&mut response).await.unwrap();
    assert_eq!(harness.diagnostics.snapshot().failed_requests, 1);
    assert_eq!(harness.diagnostics.snapshot().connection_failures, 1);
    let before = harness.diagnostics.snapshot();
    let mut idle = TcpStream::connect(harness.address).await.unwrap();
    idle.write_all(b"POST /api/").await.unwrap();
    let diagnostics = harness.diagnostics.clone();
    harness.close().await;
    assert!(
        !diagnostics
            .snapshot()
            .saturating_delta_since(before)
            .has_anomaly()
    );

    let harness = Harness::start_with_tls(true).await;
    let mut plaintext = TcpStream::connect(harness.address).await.unwrap();
    plaintext
        .write_all(b"GET / HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    let _ = plaintext.read_to_end(&mut Vec::new()).await;
    assert_eq!(harness.diagnostics.snapshot().tls_failures, 1);
    harness.close().await;
}

#[tokio::test]
async fn real_disconnect_preserves_mutation_jobs_and_device_quota_until_the_write_finishes() {
    let harness = Harness::start().await;
    let (gate, registration) = blocked(&harness, OperationEffect::Mutation);
    for _ in 0..4 {
        let socket = harness.raw("test/blocked").await;
        tokio::time::timeout(Duration::from_secs(3), gate.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        drop(socket);
    }
    assert_eq!(
        harness
            .request("test/blocked")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    assert_eq!(
        harness
            .request("connection/describe")
            .body(r#"{"wire_version":1}"#)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    gate.release.add_permits(4);
    gate.completed.acquire_many(4).await.unwrap().forget();
    registration.close().await;
    harness.close().await;
}

#[tokio::test]
async fn real_disconnect_cancels_a_blocked_read_without_a_timer() {
    let harness = Harness::start().await;
    let (gate, registration) = blocked(&harness, OperationEffect::Read);
    let socket = harness.raw("test/blocked").await;
    gate.entered.acquire().await.unwrap().forget();
    drop(socket);
    tokio::time::timeout(Duration::from_secs(2), gate.dropped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert_eq!(gate.completed.available_permits(), 0);
    registration.close().await;
    harness.close().await;
}

#[tokio::test]
async fn revocation_releases_read_work_but_preserves_already_admitted_writes() {
    for effect in [OperationEffect::Read, OperationEffect::Mutation] {
        let harness = Harness::start().await;
        let (gate, registration) = blocked(&harness, effect);
        let socket = harness.raw("test/blocked").await;
        gate.entered.acquire().await.unwrap().forget();
        harness.authentication.0.cancel();
        if effect == OperationEffect::Mutation {
            assert_eq!(gate.dropped.available_permits(), 0);
            gate.release.add_permits(1);
            gate.completed.acquire().await.unwrap().forget();
        }
        tokio::time::timeout(Duration::from_secs(2), gate.dropped.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        drop(socket);
        registration.close().await;
        harness.close().await;
    }
}

#[test]
fn insecure_listeners_require_explicit_loopback_policy() {
    let mut config = HttpConfig {
        bind: "127.0.0.1:8080".parse().unwrap(),
        public_origin: "http://127.0.0.1:8080".into(),
        tls: None,
        allow_loopback_http: false,
    };
    assert!(config.validate().is_err());
    config.allow_loopback_http = true;
    config.validate().unwrap();
    config.bind = "0.0.0.0:8080".parse().unwrap();
    assert!(config.validate().is_err());
    config.bind = "127.0.0.1:8080".parse().unwrap();
    for origin in [
        "http://foreign.test",
        "http://user@127.0.0.1:8080",
        "http://127.0.0.1:8080/path",
        "http://127.0.0.1:8080?token=forbidden",
    ] {
        config.public_origin = origin.into();
        assert!(config.validate().is_err());
    }
}

#[test]
fn noncanonical_origins_are_rejected_before_transport_startup() {
    let mut config = HttpConfig {
        bind: "127.0.0.1:8080".parse().unwrap(),
        public_origin: "https://localhost:8080".into(),
        tls: Some(tls_files()),
        allow_loopback_http: false,
    };
    config.validate().unwrap();
    for origin in [
        "https://LOCALHOST:8080",
        "https://localhost:443",
        "https://localhost/",
        "https://127.1:8080",
        "https://0x7f000001:8080",
        "https://[0:0:0:0:0:0:0:1]:8080",
        "https://localhost:65536",
        "https://localhost:08080",
    ] {
        config.public_origin = origin.into();
        assert!(
            matches!(config.validate(), Err(ApiError::Invalid(_))),
            "accepted {origin}"
        );
    }
}

#[derive(Debug)]
struct EndpointDependencies;
#[async_trait]
impl rsi_meta::PluginFactory for EndpointDependencies {
    fn prepare(&self, _: &serde_json::Value) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(serde_json::Value::Null))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let authentication = plan
            .context()
            .provide_local::<rsi_api_protocol::DeviceAuthenticationContract>(Arc::new(
                Authentication(CancellationToken::new()),
            ))?;
        let endpoint = plan
            .context()
            .provide_local::<rsi_api_protocol::EndpointIdentityContract>(Arc::new(
                EndpointId::from_bytes([2; 16]),
            ))?;
        let generation = plan
            .context()
            .provide_local::<rsi_api_protocol::HostGenerationContract>(Arc::new(
                HostEpoch::from_bytes([3; 16]),
            ))?;
        plan.defer(
            "withdraw fixture identities",
            Box::new(move || {
                Box::pin(async move {
                    drop(generation);
                    drop(endpoint);
                    drop(authentication);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn ordinary_http_plugin_retires_listener_without_withdrawing_shared_connection_operations() {
    use futures_util::FutureExt;
    use rsi_api_http::{HttpFactory, HttpListenerContract};
    use rsi_meta::{ResolvedFactory, Runtime, RuntimeLimits, UpdateMode};
    let runtime = Runtime::new(RuntimeLimits::default()).unwrap();
    let linked =
        |name, factory| ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory);
    let registry = runtime
        .root()
        .apply(
            linked("api", Arc::new(rsi_api::ApiFactory)),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let identities = runtime
        .root()
        .apply(
            linked("identities", Arc::new(EndpointDependencies)),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let connection = runtime
        .root()
        .apply(
            linked("connection-api", Arc::new(rsi_api::ConnectionApiFactory)),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let http = runtime.root().apply(linked("http", Arc::new(HttpFactory)), serde_json::json!({"bind":"127.0.0.1:0", "public_origin":"http://127.0.0.1", "tls":null, "allow_loopback_http":true})).await.unwrap();
    let listener = runtime
        .root()
        .lookup_local::<HttpListenerContract>()
        .unwrap();
    let dispatcher = runtime
        .root()
        .lookup_local::<rsi_api_protocol::ApiDispatchContract>()
        .unwrap();
    assert_eq!(dispatcher.operations().len(), 3);
    assert_ne!(listener.address().port(), 0);
    let socket = TcpStream::connect(listener.address()).await.unwrap();
    let mut stopped = Box::pin(listener.stopped());
    assert!(stopped.as_mut().now_or_never().is_none());
    http.dispose().await;
    stopped.await.unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<HttpListenerContract>()
            .is_none()
    );
    assert_eq!(dispatcher.operations().len(), 3);
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_api_protocol::ConnectionDescriptionContract>()
            .is_some()
    );
    connection.dispose().await;
    assert!(dispatcher.operations().is_empty());
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_api_protocol::ConnectionDescriptionContract>()
            .is_none()
    );
    drop(socket);
    identities.dispose().await;
    registry.dispose().await;
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct PendingEvents(Arc<Semaphore>);
struct PendingStream {
    first: Option<ApiMessage>,
    _lifetime: OnDrop,
}
impl futures_util::Stream for PendingStream {
    type Item = Result<ApiMessage>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.first.take().map_or(std::task::Poll::Pending, |item| {
            std::task::Poll::Ready(Some(Ok(item)))
        })
    }
}
#[async_trait]
impl ApiHandler for PendingEvents {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let ApiResponseCapacity::Subscription { budget, maximum } = output else {
            panic!("subscription")
        };
        let first = ApiMessage {
            json: budget.reserve(maximum)?.encode(&true)?,
            binary: None,
        };
        Ok(ApiOutput::Stream(Box::pin(PendingStream {
            first: Some(first),
            _lifetime: OnDrop(self.0.clone()),
        })))
    }
}

#[tokio::test]
async fn revocation_releases_an_idle_domain_subscription_without_polling_the_http_response() {
    let harness = Harness::start().await;
    let dropped = Arc::new(Semaphore::new(0));
    let registration = harness
        .registry
        .register(
            spec("idle", OperationClass::Subscription, OperationEffect::Read),
            Arc::new(PendingEvents(dropped.clone())),
        )
        .unwrap();
    let response = harness
        .request("test/idle")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    harness.authentication.0.cancel();
    tokio::time::timeout(Duration::from_secs(2), dropped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let body = response.text().await.unwrap();
    assert!(body.contains("event: error\ndata: {\"code\":\"unauthorized\"}"));
    assert!(body.ends_with("event: end\ndata: {}\n\n"));
    registration.close().await;
    harness.close().await;
}

#[tokio::test]
async fn two_listeners_share_connection_operations_after_one_transport_stops() {
    let first = Harness::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let server = HttpServer::from_listener(
        Execution::native(tokio::runtime::Handle::current()),
        listener,
        HttpConfig {
            bind: address,
            public_origin: origin.clone(),
            tls: None,
            allow_loopback_http: true,
        },
        HttpServices {
            dispatch: first.registry.clone(),
            authentication: first.authentication.clone(),
            endpoint: EndpointId::from_bytes([2; 16]),
            epoch: first.epoch.clone(),
        },
    )
    .await
    .unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(server.serve(stop.clone()));
    first.stop.cancel();
    first.task.await.unwrap().unwrap();
    let response = Harness::client()
        .post(format!("{origin}/api/v1/connection/describe/1"))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .header("x-rsi-wire-version", "1")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let description: rsi_api_protocol::ConnectionDescription =
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(&description, first.connection.description().as_ref());
    first.connection.close().await;
    let response = Harness::client()
        .post(format!("{origin}/api/v1/connection/describe/1"))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .header("x-rsi-wire-version", "1")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    stop.cancel();
    task.await.unwrap().unwrap();
    first.registry.close().await;
}

fn slow_http2_client() -> reqwest::Client {
    let certificate = reqwest::Certificate::from_pem(include_bytes!(
        "../../../../fixtures/rsi-api/tls/server-cert.pem"
    ))
    .unwrap();
    reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(certificate)
        .http2_prior_knowledge()
        .http2_initial_stream_window_size(1024)
        .http2_initial_connection_window_size(64 * 1024)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn idle_h2_connections_retain_unclassified_admission_and_expire() {
    let harness = Harness::start_with_tls(true).await;
    let mut clients = Vec::new();
    for _ in 0..32 {
        let client = slow_http2_client();
        let response = client
            .post(format!("{}/api/v1/connection/describe/1", harness.origin))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        response.bytes().await.unwrap();
        clients.push(client);
    }
    let fresh = slow_http2_client();
    assert!(
        fresh
            .post(format!("{}/api/v1/connection/describe/1", harness.origin))
            .body("{}")
            .send()
            .await
            .is_err(),
        "idle rejected connections must still consume the 32-slot unclassified bound"
    );
    // Keep every client pool alive. Server retirement must not depend on Drop at the peer.
    tokio::time::sleep(Duration::from_secs(11)).await;
    let response = fresh
        .post(format!("{}/api/v1/connection/describe/1", harness.origin))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .header("x-rsi-wire-version", "1")
        .body(r#"{"wire_version":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.bytes().await.unwrap();
    drop(clients);
    harness.close().await;
}

#[tokio::test]
async fn flow_control_stalled_assets_release_delivery_capacity_after_deadline() {
    let budget = rsi_api_protocol::ByteBudget::new(8 * 1024 * 1024).unwrap();
    let assets = Arc::new(Assets(
        budget
            .reserve(budget.limit())
            .unwrap()
            .retain_vec(vec![0; budget.limit()])
            .unwrap(),
    ));
    let harness = Harness::start_with_assets(true, Some(assets)).await;
    let client = slow_http2_client();
    let mut pending = Vec::new();
    for _ in 0..8 {
        let response = client
            .get(format!("{}/app.wasm", harness.origin))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        pending.push(response);
    }
    let other = slow_http2_client();
    assert_eq!(
        other
            .get(format!("{}/app.wasm", harness.origin))
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    tokio::time::sleep(Duration::from_secs(33)).await;
    let response = other
        .get(format!("{}/app.wasm", harness.origin))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "codec flow control retained every asset delivery beyond its deadline"
    );
    drop(response);
    drop(pending);
    harness.close().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn listener_recovers_from_descriptor_pressure_and_can_stop_during_backoff() {
    const CHILD: &str = "RSI_TEST_HTTP_ACCEPT_PRESSURE";
    if std::env::var_os(CHILD).is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "listener_recovers_from_descriptor_pressure_and_can_stop_during_backoff",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            if std::time::Instant::now() >= until {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("isolated accept-pressure test timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    for stop_under_pressure in [false, true] {
        let harness = Harness::start().await;
        // Queue a real connection without yielding to accept, then exhaust this
        // isolated subprocess's descriptors before its next scheduler turn.
        let mut peer = std::net::TcpStream::connect(harness.address).unwrap();
        std::io::Write::write_all(&mut peer, format!("POST /api/v1/connection/describe/1 HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {TOKEN}\r\nX-Rsi-Wire-Version: 1\r\nContent-Type: application/json\r\nContent-Length: 18\r\n\r\n{{\"wire_version\":1}}", harness.address).as_bytes()).unwrap();
        peer.set_nonblocking(true).unwrap();
        let mut peer = TcpStream::from_std(peer).unwrap();
        let limits = rustix::process::getrlimit(rustix::process::Resource::Nofile);
        rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(64),
                maximum: limits.maximum,
            },
        )
        .unwrap();
        let mut descriptors = Vec::new();
        loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => descriptors.push(file),
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        let alive = !harness.task.is_finished();
        let failures = harness.diagnostics.snapshot().connection_failures;
        if stop_under_pressure {
            harness.stop.cancel();
            tokio::time::timeout(Duration::from_millis(50), async {
                while !harness.task.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("shutdown must not wait for descriptor pressure or retry backoff");
        }
        drop(descriptors);
        rustix::process::setrlimit(rustix::process::Resource::Nofile, limits).unwrap();
        assert!(alive, "transient accept failure terminated the listener");
        assert!(
            (1..=3).contains(&failures),
            "accept retries must be observable and paced: {failures}"
        );
        if !stop_under_pressure {
            let mut bytes = Vec::new();
            tokio::time::timeout(Duration::from_secs(3), peer.read_to_end(&mut bytes))
                .await
                .unwrap()
                .unwrap();
            assert!(
                bytes.starts_with(b"HTTP/1.1 200"),
                "listener did not resume service: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        tokio::time::timeout(Duration::from_secs(1), harness.close())
            .await
            .unwrap();
    }
}
