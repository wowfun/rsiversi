#![cfg(unix)]

use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt};
use rsi_api::{ApiRegistry, ConnectionApi};
use rsi_api_http::LocalHttpService;
use rsi_api_protocol::*;
use rsi_api_uds_client::{UdsClient, UdsClientConfig, UdsClientFactory};
use rsi_meta::{Execution, ResolvedFactory, Runtime, UpdateMode};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::UnixListener, sync::Semaphore, task::JoinSet};
use tokio_util::sync::CancellationToken;

fn operation(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("local", name, 1).unwrap(),
        class: match name {
            "events" => OperationClass::Subscription,
            "echo" => OperationClass::Control,
            _ => OperationClass::Data,
        },
        effect: if name == "mutate" {
            OperationEffect::Mutation
        } else {
            OperationEffect::Read
        },
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: if name == "binary" {
            RequestEncoding::Binary
        } else {
            RequestEncoding::Json
        },
        maximum_request_bytes: if name == "binary" { 512 * 1024 } else { 128 },
        maximum_response_bytes: if name == "binary" {
            512 * 1024 + 128
        } else {
            128
        },
    }
}
#[derive(Debug)]
struct State {
    entered: Semaphore,
    release: Semaphore,
    dropped: Semaphore,
    completed: AtomicUsize,
}
struct Lifetime(Arc<State>);
impl Drop for Lifetime {
    fn drop(&mut self) {
        self.0.dropped.add_permits(1);
    }
}
#[derive(Debug)]
struct Handler {
    name: &'static str,
    state: Arc<State>,
}
#[async_trait]
impl ApiHandler for Handler {
    async fn invoke(
        &self,
        caller: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        assert!(
            matches!(caller.origin, CallOrigin::Local),
            "local origin is assigned by the Unix transport"
        );
        if self.name == "events" {
            let lifetime = Lifetime(self.state.clone());
            let state = self.state.clone();
            let ApiResponseCapacity::Subscription { budget, .. } = output else {
                panic!("stream budget")
            };
            return Ok(ApiOutput::Stream(Box::pin(async_stream::try_stream! {
                let _lifetime = lifetime;
                state.entered.add_permits(1);
                yield ApiMessage { json: budget.copy(b"true")?, binary: None };
                futures_util::future::pending::<()>().await;
            })));
        }
        let ApiResponseCapacity::Finite(mut capacity) = output else {
            panic!("finite budget")
        };
        if self.name == "binary" {
            let bytes = input.as_bytes();
            let json = capacity.split(16)?;
            let binary = capacity;
            return Ok(ApiOutput::Reply(ApiMessage {
                json: json.encode(&bytes.len())?,
                binary: Some(binary.copy(bytes)?),
            }));
        }
        if matches!(self.name, "mutate" | "read") {
            let _lifetime = Lifetime(self.state.clone());
            self.state.entered.add_permits(1);
            self.state.release.acquire().await.unwrap().forget();
            self.state.completed.fetch_add(1, Ordering::AcqRel);
        }
        Ok(ApiOutput::Reply(ApiMessage {
            json: capacity.copy(b"true")?,
            binary: None,
        }))
    }
}

struct Harness {
    _directory: tempfile::TempDir,
    config: UdsClientConfig,
    registry: Arc<ApiRegistry>,
    connection: ConnectionApi,
    registrations: Vec<ApiRegistration>,
    state: Arc<State>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Harness {
    fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = UdsClientConfig {
            socket: directory.path().join("api.sock"),
            endpoint_id: EndpointId::from_bytes([2; 16]),
            host_epoch: HostEpoch::from_bytes([3; 16]),
            compatibility: LocalCompatibilityKey::from_bytes([4; 32]),
        };
        let execution = Execution::native(tokio::runtime::Handle::current());
        let registry = Arc::new(ApiRegistry::new(execution.clone()));
        let connection = ConnectionApi::register(
            registry.clone(),
            registry.as_ref(),
            config.endpoint_id.clone(),
            config.host_epoch.clone(),
        )
        .unwrap();
        let state = Arc::new(State {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            dropped: Semaphore::new(0),
            completed: AtomicUsize::new(0),
        });
        let registrations = ["echo", "binary", "events", "read", "mutate"]
            .map(|name| {
                registry
                    .register(
                        operation(name),
                        Arc::new(Handler {
                            name,
                            state: state.clone(),
                        }),
                    )
                    .unwrap()
            })
            .into();
        let service = LocalHttpService::new(
            execution,
            registry.clone(),
            &connection.description(),
            config.compatibility.clone(),
        )
        .unwrap();
        let listener = UnixListener::bind(&config.socket).unwrap();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! { biased;
                    () = stopping.cancelled() => break,
                    result = tasks.join_next(), if !tasks.is_empty() => { result.unwrap().unwrap(); },
                    result = listener.accept() => {
                        let service = service.clone(); let stop = stopping.clone();
                        let (socket, _) = result.unwrap();
                        tasks.spawn(async move { service.serve(socket, stop).await.unwrap(); });
                    }
                }
            }
            while let Some(task) = tasks.join_next().await {
                task.unwrap();
            }
        });
        Self {
            _directory: directory,
            config,
            registry,
            connection,
            registrations,
            state,
            stop,
            task,
        }
    }
    async fn client(&self) -> UdsClient {
        UdsClient::connect(
            Execution::native(tokio::runtime::Handle::current()),
            self.config.clone(),
        )
        .await
        .unwrap()
    }
    async fn close(self) {
        self.stop.cancel();
        self.task.await.unwrap();
        for registration in self.registrations {
            registration.close().await;
        }
        self.connection.close().await;
        self.registry.close().await;
    }
}
async fn signal(semaphore: &Semaphore) {
    tokio::time::timeout(Duration::from_secs(3), semaphore.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}
fn input(client: &dyn ApiClient, operation: &OperationSpec) -> RetainedBytes {
    client.input_budget(operation.class).copy(b"{}").unwrap()
}

#[tokio::test]
async fn shared_negotiation_binary_stream_and_plugin_retirement_over_actual_unix_sockets() {
    let harness = Harness::start();
    let runtime = Runtime::default();
    let plugin = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "local",
                "test",
                UpdateMode::Replayable,
                Arc::new(UdsClientFactory),
            ),
            serde_json::to_value(&harness.config).unwrap(),
        )
        .await
        .unwrap();
    let client = runtime.root().lookup_local::<ApiClientContract>().unwrap();
    let binary = operation("binary");
    let bytes: Vec<u8> = (0_u8..251).cycle().take(512 * 1024).collect();
    let reply = client
        .call(
            &binary,
            client.input_budget(binary.class).copy(&bytes).unwrap(),
        )
        .await
        .unwrap();
    let ApiOutput::Reply(reply) = reply else {
        panic!("binary reply")
    };
    assert_eq!(reply.binary.unwrap().as_bytes(), bytes);
    let stream = operation("events");
    let ApiOutput::Stream(mut events) = client
        .call(&stream, input(client.as_ref(), &stream))
        .await
        .unwrap()
    else {
        panic!("stream")
    };
    assert_eq!(
        events.next().await.unwrap().unwrap().json.as_bytes(),
        b"true"
    );
    signal(&harness.state.entered).await;
    // Keep the application stream unpolled while its connection owner retires.
    tokio::time::timeout(Duration::from_secs(3), plugin.dispose())
        .await
        .unwrap();
    assert!(runtime.root().lookup_local::<ApiClientContract>().is_none());
    signal(&harness.state.dropped).await;
    assert!(matches!(
        client.call(&binary, input(client.as_ref(), &binary)).await,
        Err(ApiError::ShuttingDown)
    ));
    drop(events);
    let second = harness.client().await;
    let echo = operation("echo");
    second.call(&echo, input(&second, &echo)).await.unwrap();
    second.close().await;
    assert!(runtime.shutdown().await.is_clean());
    harness.close().await;
}

#[tokio::test]
async fn local_request_drop_cancels_reads_and_preserves_accepted_mutations_without_replay() {
    let harness = Harness::start();
    let client = Arc::new(harness.client().await);
    for (name, completed) in [("read", 0), ("mutate", 1)] {
        let operation = operation(name);
        let mut call = Box::pin(client.call(&operation, input(client.as_ref(), &operation)));
        tokio::select! { result = &mut call => panic!("ungated result {result:?}"), () = signal(&harness.state.entered) => {} }
        drop(call);
        if name == "mutate" {
            assert_eq!(harness.state.completed.load(Ordering::Acquire), 0);
            harness.state.release.add_permits(1);
        }
        signal(&harness.state.dropped).await;
        assert_eq!(harness.state.completed.load(Ordering::Acquire), completed);
    }
    client.close().await;
    harness.close().await;
}

#[tokio::test]
async fn local_configuration_and_generation_fences_reject_before_domain_dispatch() {
    let harness = Harness::start();
    for field in ["key", "epoch", "endpoint", "relative", "nul"] {
        let mut config = harness.config.clone();
        match field {
            "key" => config.compatibility = LocalCompatibilityKey::from_bytes([5; 32]),
            "epoch" => config.host_epoch = HostEpoch::from_bytes([5; 16]),
            "endpoint" => config.endpoint_id = EndpointId::from_bytes([5; 16]),
            "relative" => config.socket = "relative.sock".into(),
            "nul" => config.socket = "/tmp/invalid\0sock".into(),
            _ => unreachable!(),
        }
        assert!(
            UdsClient::connect(Execution::native(tokio::runtime::Handle::current()), config)
                .await
                .is_err(),
            "accepted {field}"
        );
    }
    assert!(harness.state.entered.acquire().now_or_never().is_none());
    harness.close().await;
}

#[tokio::test]
async fn local_http_rejects_remote_credentials_and_foreign_request_authority() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let harness = Harness::start();
    for (host, extra) in [
        ("foreign.invalid", ""),
        ("rsi.local", "Authorization: Bearer remote\r\n"),
        ("rsi.local", "Cookie: rsi-device=remote\r\n"),
        ("rsi.local", "Origin: http://rsi.local\r\n"),
        ("rsi.local", "X-Rsi-Local-Key: duplicate\r\n"),
    ] {
        let mut stream = tokio::net::UnixStream::connect(&harness.config.socket)
            .await
            .unwrap();
        let request = format!(
            "POST /api/v1/local/echo/1 HTTP/1.1\r\nHost: {host}\r\nX-Rsi-Wire-Version: 1\r\nX-Rsi-Host-Epoch: {}\r\nX-Rsi-Local-Key: {}\r\n{extra}Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}",
            harness.config.host_epoch.as_str(),
            harness.config.compatibility.as_str()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            stream.take(4096).read_to_end(&mut reply),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            reply.starts_with(b"HTTP/1.1 401 "),
            "accepted {host} {extra}"
        );
    }
    harness.close().await;
}

#[tokio::test]
async fn pipelined_bytes_cannot_dispatch_a_second_mutation_or_undo_the_first() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let harness = Harness::start();
    let mut stream = tokio::net::UnixStream::connect(&harness.config.socket)
        .await
        .unwrap();
    let request = format!(
        "POST /api/v1/local/mutate/1 HTTP/1.1\r\nHost: rsi.local\r\nX-Rsi-Wire-Version: 1\r\nX-Rsi-Host-Epoch: {}\r\nX-Rsi-Local-Key: {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}",
        harness.config.host_epoch.as_str(),
        harness.config.compatibility.as_str(),
    );
    stream
        .write_all(request.repeat(2).as_bytes())
        .await
        .unwrap();
    signal(&harness.state.entered).await;
    harness.state.release.add_permits(2);
    let mut reply = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(3),
        stream.take(4096).read_to_end(&mut reply),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(reply.starts_with(b"HTTP/1.1 200 "));
    assert_eq!(harness.state.completed.load(Ordering::Acquire), 1);
    assert_eq!(harness.state.release.available_permits(), 1);
    assert_eq!(harness.state.entered.available_permits(), 0);
    harness.close().await;
}
