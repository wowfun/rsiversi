use super::*;
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiContext, ApiHandler, ApiRegistrar, ApiResponseCapacity, AuthenticatedDevice, DeviceId,
    OperationAccess, OperationClass, OperationSpec, RetainedBytes,
};
use rsi_credentials_protocol::SecretValue;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[derive(Debug)]
struct Auth;
impl DeviceAuthentication for Auth {
    fn authenticate(&self, token: &SecretValue) -> Result<AuthenticatedDevice> {
        if token.expose_secret() != TOKEN {
            return Err(ApiError::Unauthorized);
        }
        Ok(AuthenticatedDevice {
            id: DeviceId::from_bytes([1; 16]),
            revoked: CancellationToken::new(),
        })
    }
}
#[derive(Debug)]
struct LargeReply;
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
        Ok(ApiOutput::Reply(ApiMessage {
            json: receiver.finish(),
            binary: None,
        }))
    }
}

async fn request(
    state: &Arc<State>,
    path: &str,
    body: &str,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
) -> DuplexStream {
    // Fixed capacity establishes actual write backpressure on every platform;
    // Windows loopback TCP can buffer the complete large response in the kernel.
    let (mut peer, socket) = tokio::io::duplex(1024);
    let delivery = Delivery::new(
        state.unclassified.clone().try_acquire_owned().unwrap(),
        None,
    );
    tasks.push(tokio::spawn(serve_connection(
        socket,
        state.clone(),
        CancellationToken::new(),
        delivery,
    )));
    let request = format!(
        "POST /api/v1/{path}/1 HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nX-Rsi-Wire-Version: 1\r\nX-Rsi-Host-Epoch: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        state.epoch.as_str(),
        body.len()
    );
    peer.write_all(request.as_bytes()).await.unwrap();
    peer
}
async fn finite(
    state: &Arc<State>,
    path: &str,
    body: &str,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    status: u16,
) {
    let mut peer = request(state, path, body, tasks).await;
    let mut response = Vec::new();
    peer.read_to_end(&mut response).await.unwrap();
    assert!(response.starts_with(format!("HTTP/1.1 {status}").as_bytes()));
}

#[tokio::test]
async fn blocked_delivery_keeps_device_quota_but_releases_domain_work() {
    tokio::time::timeout(Duration::from_secs(10), verify())
        .await
        .unwrap();
}
async fn verify() {
    let execution = Execution::native(tokio::runtime::Handle::current());
    let registry = Arc::new(rsi_api::ApiRegistry::new(execution.clone()));
    let endpoint = EndpointId::from_bytes([2; 16]);
    let epoch = HostEpoch::from_bytes([3; 16]);
    let connection = rsi_api::ConnectionApi::register(
        registry.clone(),
        registry.as_ref(),
        endpoint.clone(),
        epoch.clone(),
    )
    .unwrap();
    let registration = registry
        .register(
            OperationSpec {
                protocol: rsi_api_protocol::connection_protocol(),
                id: OperationId::new("test", "large", 1).unwrap(),
                class: OperationClass::Data,
                effect: OperationEffect::Read,
                access: OperationAccess::Authenticated,
                encoding: RequestEncoding::Json,
                maximum_request_bytes: 2,
                maximum_response_bytes: 8 * 1024 * 1024,
            },
            Arc::new(LargeReply),
        )
        .unwrap();
    let state = Arc::new(State {
        assets: None,
        bootstrap: None,
        asset_deliveries: Arc::new(Semaphore::new(8)),
        diagnostics: crate::HttpDiagnostics::default(),
        dispatch: registry.clone(),
        endpoint,
        epoch,
        access: Access::Remote {
            policy: Policy {
                origin: "http://127.0.0.1".into(),
                authority: "127.0.0.1".into(),
                secure: false,
            },
            authentication: Arc::new(Auth),
        },
        execution,
        unclassified: Arc::new(Semaphore::new(32)),
        socket_scratch: rsi_api_protocol::ByteBudget::default(),
    });
    let mut tasks = Vec::new();
    let mut peers = Vec::new();
    for _ in 0..4 {
        let mut peer = request(&state, "test/large", "{}", &mut tasks).await;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(peer.read_u8().await.unwrap());
            assert!(head.len() < 8192);
        }
        assert!(head.starts_with(b"HTTP/1.1 200"));
        peers.push(peer);
    }
    registration.close().await;
    finite(&state, "connection/operations", "{}", &mut tasks, 429).await;
    finite(
        &state,
        "connection/describe",
        r#"{"wire_version":1}"#,
        &mut tasks,
        200,
    )
    .await;
    drop(peers);
    for task in tasks.drain(..) {
        task.await.unwrap();
    }
    finite(&state, "connection/operations", "{}", &mut tasks, 200).await;
    for task in tasks {
        task.await.unwrap();
    }
    connection.close().await;
    registry.close().await;
}
