use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt};
use rsi_api::ApiRegistry;
use rsi_api_http::{HttpConfig, HttpServer, HttpServices, TlsFiles};
use rsi_api_http_client::{HttpClient, HttpClientConfig};
use rsi_api_protocol::*;
use rsi_credentials_protocol::{CredentialRef, SecretValue};
use rsi_meta::Execution;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::Semaphore};
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
    registry: Arc<ApiRegistry>,
    connection: rsi_api::ConnectionApi,
    config: HttpClientConfig,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Result<()>>,
}
impl Harness {
    async fn start(tls: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let execution = Execution::native(tokio::runtime::Handle::current());
        let registry = Arc::new(ApiRegistry::new(execution.clone()));
        let authentication = Arc::new(Authentication(CancellationToken::new()));
        let endpoint = EndpointId::from_bytes([2; 16]);
        let epoch = HostEpoch::from_bytes([3; 16]);
        let connection = rsi_api::ConnectionApi::register(
            registry.clone(),
            registry.as_ref(),
            endpoint.clone(),
            epoch.clone(),
        )
        .unwrap();
        let origin = format!("{}://{address}", if tls { "https" } else { "http" });
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/rsi-api/tls");
        let server = HttpServer::from_listener(
            execution,
            listener,
            HttpConfig {
                bind: address,
                public_origin: origin.clone(),
                tls: tls.then(|| TlsFiles {
                    certificate: fixture.join("server-cert.pem"),
                    key: fixture.join("server-key.pem"),
                }),
                allow_loopback_http: !tls,
            },
            HttpServices {
                dispatch: registry.clone(),
                authentication,
                endpoint: endpoint.clone(),
                epoch,
            },
        )
        .await
        .unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(server.serve(stop.clone()));
        Self {
            registry,
            connection,
            config: HttpClientConfig {
                origin,
                endpoint_id: endpoint,
                credential: CredentialRef::new("test.client", "device").unwrap(),
                tls_ca: tls.then(|| fixture.join("server-cert.pem")),
                allow_loopback_http: !tls,
            },
            stop,
            task,
        }
    }
    async fn connect(&self) -> HttpClient {
        HttpClient::connect(
            Execution::native(tokio::runtime::Handle::current()),
            self.config.clone(),
            SecretValue::new(TOKEN).unwrap(),
        )
        .await
        .unwrap()
    }
    async fn close(self) {
        self.stop.cancel();
        self.task.await.unwrap().unwrap();
        self.connection.close().await;
        self.registry.close().await;
    }
}
fn spec(name: &str, class: OperationClass, effect: OperationEffect) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("test", name, 1).unwrap(),
        class,
        effect,
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 128,
        maximum_response_bytes: 128,
    }
}
fn input(client: &HttpClient, operation: &OperationSpec) -> RetainedBytes {
    client.input_budget(operation.class).copy(b"{}").unwrap()
}
#[derive(Debug)]
struct Echo(AtomicUsize);
#[async_trait]
impl ApiHandler for Echo {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        match output {
            ApiResponseCapacity::Finite(mut capacity) => Ok(ApiOutput::Reply(ApiMessage {
                json: capacity.split(20)?.copy(b"18446744073709551615")?,
                binary: Some(capacity.copy(&[0, 128, 255])?),
            })),
            ApiResponseCapacity::Subscription { budget, maximum } => {
                Ok(ApiOutput::Stream(Box::pin(futures_util::stream::iter([
                    Ok(ApiMessage {
                        json: budget.reserve(maximum)?.copy(b"18446744073709551615")?,
                        binary: None,
                    }),
                ]))))
            }
        }
    }
}

#[tokio::test]
async fn retained_binary_reply_does_not_block_the_next_maximum_sized_receive_reservation() {
    let harness = Harness::start(true).await;
    let echo = Arc::new(Echo(AtomicUsize::new(0)));
    let mut operation = spec("maximum", OperationClass::Data, OperationEffect::Read);
    operation.maximum_response_bytes = MAXIMUM_API_BYTES;
    let _registration = harness
        .registry
        .register(operation.clone(), echo.clone())
        .unwrap();
    let client = harness.connect().await;
    let ApiOutput::Reply(first) = client
        .call(&operation, input(&client, &operation))
        .await
        .unwrap()
    else {
        panic!("finite")
    };
    let ApiOutput::Reply(second) = client
        .call(&operation, input(&client, &operation))
        .await
        .unwrap()
    else {
        panic!("finite")
    };
    assert_eq!(first.binary.unwrap().as_bytes(), [0, 128, 255]);
    assert_eq!(second.binary.unwrap().as_bytes(), [0, 128, 255]);
    assert_eq!(echo.0.load(Ordering::SeqCst), 2);
    client.close().await;
    harness.close().await;
}

#[tokio::test]
async fn actual_tls_negotiation_binary_sse_and_client_retirement_preserve_the_remote_host() {
    let harness = Harness::start(true).await;
    let echo = Arc::new(Echo(AtomicUsize::new(0)));
    let binary = spec("binary", OperationClass::Data, OperationEffect::Read);
    let events = spec(
        "events",
        OperationClass::Subscription,
        OperationEffect::Read,
    );
    let _binary = harness
        .registry
        .register(binary.clone(), echo.clone())
        .unwrap();
    let _events = harness
        .registry
        .register(events.clone(), echo.clone())
        .unwrap();
    let client = harness.connect().await;
    assert_eq!(client.description().endpoint_id, harness.config.endpoint_id);
    let ApiOutput::Reply(reply) = client.call(&binary, input(&client, &binary)).await.unwrap()
    else {
        panic!("finite")
    };
    assert_eq!(reply.json.as_bytes(), b"18446744073709551615");
    assert_eq!(reply.binary.unwrap().as_bytes(), [0, 128, 255]);
    let ApiOutput::Stream(mut stream) =
        client.call(&events, input(&client, &events)).await.unwrap()
    else {
        panic!("stream")
    };
    assert_eq!(
        stream.next().await.unwrap().unwrap().json.as_bytes(),
        b"18446744073709551615"
    );
    assert!(stream.next().await.is_none());
    let mut false_metadata = binary.clone();
    false_metadata.effect = OperationEffect::Mutation;
    assert!(matches!(
        client.call(&false_metadata, input(&client, &binary)).await,
        Err(ApiError::Unavailable)
    ));
    assert_eq!(echo.0.load(Ordering::SeqCst), 2);
    client.close().await;
    assert!(matches!(
        client.call(&binary, input(&client, &binary)).await,
        Err(ApiError::ShuttingDown)
    ));
    let second = harness.connect().await;
    second.call(&binary, input(&second, &binary)).await.unwrap();
    second.close().await;
    assert!(!harness.stop.is_cancelled());
    harness.close().await;
}

#[derive(Debug)]
struct Gate {
    entered: Semaphore,
    release: Semaphore,
    dropped: Arc<Semaphore>,
}
struct Dropped(Arc<Semaphore>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.add_permits(1);
    }
}
#[async_trait]
impl ApiHandler for Gate {
    async fn invoke(
        &self,
        _: ApiContext,
        _: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        match output {
            ApiResponseCapacity::Finite(capacity) => {
                let _lifetime = Dropped(self.dropped.clone());
                self.entered.add_permits(1);
                self.release.acquire().await.unwrap().forget();
                Ok(ApiOutput::Reply(ApiMessage {
                    json: capacity.encode(&true)?,
                    binary: None,
                }))
            }
            ApiResponseCapacity::Subscription { .. } => {
                let lifetime = Dropped(self.dropped.clone());
                self.entered.add_permits(1);
                Ok(ApiOutput::Stream(Box::pin(async_stream::stream! {
                    let _lifetime = lifetime;
                    std::future::pending::<()>().await;
                    yield Err(ApiError::Unavailable);
                })))
            }
        }
    }
}
fn gate() -> Arc<Gate> {
    Arc::new(Gate {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        dropped: Arc::new(Semaphore::new(0)),
    })
}

#[tokio::test]
async fn unpolled_read_waiters_retire_while_accepted_mutations_keep_the_server_owner() {
    for effect in [OperationEffect::Read, OperationEffect::Mutation] {
        let harness = Harness::start(false).await;
        let gate = gate();
        let operation = spec("blocked", OperationClass::Data, effect);
        let registration = harness
            .registry
            .register(operation.clone(), gate.clone())
            .unwrap();
        let client = harness.connect().await;
        let mut waiter = Box::pin(client.call(&operation, input(&client, &operation)));
        assert!(waiter.as_mut().now_or_never().is_none());
        gate.entered.acquire().await.unwrap().forget();
        tokio::time::timeout(Duration::from_secs(2), client.close())
            .await
            .unwrap();
        let result = waiter.await;
        if effect == OperationEffect::Mutation {
            assert!(matches!(result, Err(ApiError::OutcomeUnknown)));
            assert!(gate.dropped.try_acquire().is_err());
            gate.release.add_permits(1);
        } else {
            assert!(matches!(result, Err(ApiError::ShuttingDown)));
        }
        tokio::time::timeout(Duration::from_secs(2), gate.dropped.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        registration.close().await;
        harness.close().await;
    }
}

#[tokio::test]
async fn an_unpolled_stream_cannot_retain_native_connection_work_after_plugin_retirement() {
    let harness = Harness::start(false).await;
    let gate = gate();
    let operation = spec("idle", OperationClass::Subscription, OperationEffect::Read);
    let registration = harness
        .registry
        .register(operation.clone(), gate.clone())
        .unwrap();
    let client = harness.connect().await;
    let ApiOutput::Stream(mut stream) = client
        .call(&operation, input(&client, &operation))
        .await
        .unwrap()
    else {
        panic!("stream")
    };
    gate.entered.acquire().await.unwrap().forget();
    tokio::time::timeout(Duration::from_secs(2), client.close())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), gate.dropped.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(matches!(
        stream.next().await,
        Some(Err(ApiError::ShuttingDown))
    ));
    assert!(stream.next().await.is_none());
    registration.close().await;
    harness.close().await;
}

#[derive(Debug)]
struct Credentials;
#[async_trait]
impl rsi_credentials_protocol::CredentialsResolve for Credentials {
    async fn resolve(
        &self,
        _: &CredentialRef,
    ) -> rsi_credentials_protocol::Result<rsi_credentials_protocol::ResolvedCredential> {
        Ok(rsi_credentials_protocol::ResolvedCredential {
            secret: SecretValue::new(TOKEN).unwrap(),
            source: rsi_credentials_protocol::CredentialSource::Keyring,
        })
    }
}

#[async_trait]
impl rsi_meta::PluginFactory for Credentials {
    fn prepare(&self, _: &serde_json::Value) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(serde_json::Value::Null))
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<rsi_credentials_protocol::CredentialsResolveContract>(Arc::new(
                Credentials,
            ))?;
        plan.defer(
            "withdraw fixture credentials",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn ordinary_client_plugin_withdraws_and_fences_escaped_capability_without_stopping_server() {
    let harness = Harness::start(false).await;
    let runtime = rsi_meta::Runtime::with_execution(
        rsi_meta::RuntimeLimits::default(),
        Execution::native(tokio::runtime::Handle::current()),
    )
    .unwrap();
    let _credentials = runtime
        .root()
        .apply(
            rsi_meta::ResolvedFactory::linked(
                "fixture-credentials",
                "test",
                rsi_meta::UpdateMode::Replayable,
                Arc::new(Credentials),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let config = serde_json::to_value(&harness.config).unwrap();
    assert!(!config.to_string().contains(TOKEN));
    let factory = rsi_meta::ResolvedFactory::linked(
        "api-client",
        "test",
        rsi_meta::UpdateMode::Replayable,
        Arc::new(rsi_api_http_client::HttpClientFactory),
    );
    let fiber = runtime.root().apply(factory, config).await.unwrap();
    let client = runtime.root().lookup_local::<ApiClientContract>().unwrap();
    assert!(!format!("{client:?}").contains(TOKEN));
    let operation = describe_operation();
    let input = client
        .input_budget(operation.class)
        .copy(br#"{"wire_version":1}"#)
        .unwrap();
    client.call(&operation, input).await.unwrap();
    fiber.dispose().await;
    assert!(runtime.root().lookup_local::<ApiClientContract>().is_none());
    let input = client
        .input_budget(operation.class)
        .copy(br#"{"wire_version":1}"#)
        .unwrap();
    assert!(matches!(
        client.call(&operation, input).await,
        Err(ApiError::ShuttingDown)
    ));
    assert!(runtime.shutdown().await.is_clean());
    let fresh = harness.connect().await;
    fresh.close().await;
    harness.close().await;
}

#[tokio::test]
async fn client_configuration_rejects_implicit_http_foreign_origins_and_empty_explicit_ca() {
    let harness = Harness::start(true).await;
    for origin in [
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com/path",
        "https://example.com?token=secret",
        "https://EXAMPLE.com",
        "https://example.com/",
    ] {
        let mut config = harness.config.clone();
        config.origin = origin.into();
        assert!(config.validate().is_err(), "accepted {origin}");
    }
    let empty = tempfile::NamedTempFile::new().unwrap();
    let mut config = harness.config.clone();
    config.tls_ca = Some(empty.path().into());
    let result = HttpClient::connect(
        Execution::native(tokio::runtime::Handle::current()),
        config,
        SecretValue::new(TOKEN).unwrap(),
    )
    .await;
    assert!(matches!(result, Err(ApiError::Invalid(_))));
    harness.close().await;
}
