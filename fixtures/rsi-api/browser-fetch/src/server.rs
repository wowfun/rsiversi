use async_trait::async_trait;
use rsi_api::ApiRegistry;
use rsi_api_browser_fetch::{NAMES, TOKEN, spec};
use rsi_api_http::{HttpConfig, HttpServer, HttpServices, TlsFiles};
use rsi_api_protocol::*;
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::AsyncReadExt,
    net::TcpListener,
    sync::{Barrier, Semaphore},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Auth;
impl DeviceAuthentication for Auth {
    fn authenticate(&self, secret: &SecretValue) -> Result<AuthenticatedDevice> {
        if secret.expose_secret() != TOKEN {
            return Err(ApiError::Unauthorized);
        }
        Ok(AuthenticatedDevice {
            id: DeviceId::from_bytes([1; 16]),
            revoked: CancellationToken::new(),
        })
    }
}
#[derive(Debug)]
struct State {
    started: AtomicUsize,
    completed: AtomicUsize,
    reads: AtomicUsize,
    dropped: AtomicUsize,
    idle: AtomicUsize,
    release: Semaphore,
    barrier: Barrier,
    population: AtomicUsize,
}
#[derive(Debug)]
struct Handler {
    name: &'static str,
    state: Arc<State>,
}
struct ReadGuard(Arc<State>);
struct Idle(Arc<State>);
impl futures_util::Stream for Idle {
    type Item = Result<ApiMessage>;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Pending
    }
}
impl Drop for Idle {
    fn drop(&mut self) {
        self.0.idle.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ApiHandler for Handler {
    async fn invoke(
        &self,
        _: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        if let ApiResponseCapacity::Subscription { budget, maximum } = output {
            return if self.name == "idle" {
                self.state.idle.fetch_add(1, Ordering::SeqCst);
                Ok(ApiOutput::Stream(Box::pin(Idle(self.state.clone()))))
            } else {
                Ok(ApiOutput::Stream(Box::pin(
                    futures_util::stream::iter([0, 1]).map(move |_| {
                        Ok(ApiMessage {
                            json: budget.reserve(maximum)?.copy(b"18446744073709551615")?,
                            binary: None,
                        })
                    }),
                )))
            };
        }
        let ApiResponseCapacity::Finite(mut capacity) = output else {
            unreachable!()
        };
        let json = match self.name {
            "binary" => {
                let json = capacity.split(20)?.copy(b"18446744073709551615")?;
                return Ok(ApiOutput::Reply(ApiMessage {
                    json,
                    binary: Some(capacity.copy(input.as_bytes())?),
                }));
            }
            "mutate" => {
                self.state.started.fetch_add(1, Ordering::SeqCst);
                self.state.release.acquire().await.unwrap().forget();
                self.state.completed.fetch_add(1, Ordering::SeqCst);
                serde_json::json!(true)
            }
            "read-gate" => {
                let _guard = ReadGuard(self.state.clone());
                self.state.reads.fetch_add(1, Ordering::SeqCst);
                futures_util::future::pending::<()>().await;
                unreachable!()
            }
            "release" => {
                self.state.release.add_permits(4);
                serde_json::json!(true)
            }
            "stats" => serde_json::json!({
                "started": self.state.started.load(Ordering::SeqCst), "completed": self.state.completed.load(Ordering::SeqCst),
                "reads": self.state.reads.load(Ordering::SeqCst), "dropped": self.state.dropped.load(Ordering::SeqCst),
                "idle": self.state.idle.load(Ordering::SeqCst),
            }),
            "pool-barrier" => {
                if self.state.barrier.wait().await.is_leader() {
                    self.state
                        .population
                        .store(self.state.idle.load(Ordering::SeqCst), Ordering::SeqCst);
                }
                self.state.barrier.wait().await;
                serde_json::json!({"concurrent_subscriptions": self.state.population.load(Ordering::SeqCst)})
            }
            "reject" => return Err(ApiError::Capacity),
            "domain" => return Err(ApiError::Domain(capacity.copy(b"{\"kind\":\"fixture\"}")?)),
            _ => unreachable!(),
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: capacity.encode(&json)?,
            binary: None,
        }))
    }
}
use futures_util::StreamExt;

pub async fn run() {
    if std::env::args().nth(1).as_deref() == Some("--malformed-catalog") {
        println!(
            "{}",
            serde_json::to_string(&rsi_api_browser_fetch::malformed_specs()).unwrap()
        );
        return;
    }
    let secure = std::env::args().nth(1).as_deref() == Some("--tls");
    let execution = Execution::native(tokio::runtime::Handle::current());
    let registry = Arc::new(ApiRegistry::new(execution.clone()));
    let connection = rsi_api::ConnectionApi::register(
        registry.clone(),
        registry.as_ref(),
        EndpointId::from_bytes([2; 16]),
        HostEpoch::from_bytes([3; 16]),
    )
    .unwrap();
    let state = Arc::new(State {
        started: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
        reads: AtomicUsize::new(0),
        dropped: AtomicUsize::new(0),
        idle: AtomicUsize::new(0),
        release: Semaphore::new(0),
        barrier: Barrier::new(2),
        population: AtomicUsize::new(0),
    });
    let registrations: Vec<_> = NAMES
        .iter()
        .map(|name| {
            registry
                .register(
                    spec(name),
                    Arc::new(Handler {
                        name,
                        state: state.clone(),
                    }),
                )
                .unwrap()
        })
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("{}://{address}", if secure { "https" } else { "http" });
    let server = HttpServer::from_listener(
        execution,
        listener,
        HttpConfig {
            bind: address,
            public_origin: origin.clone(),
            tls: secure.then(|| {
                let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tls");
                TlsFiles {
                    certificate: root.join("server-cert.pem"),
                    key: root.join("server-key.pem"),
                }
            }),
            allow_loopback_http: !secure,
        },
        HttpServices {
            dispatch: registry.clone(),
            authentication: Arc::new(Auth),
            endpoint: EndpointId::from_bytes([2; 16]),
            epoch: HostEpoch::from_bytes([3; 16]),
        },
    )
    .await
    .unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(server.serve(stop.clone()));
    println!("{}", serde_json::json!({"origin": origin}));
    std::io::stdout().flush().unwrap();
    let mut command = [0];
    tokio::io::stdin().read_exact(&mut command).await.unwrap();
    stop.cancel();
    state.release.add_permits(16);
    task.await.unwrap().unwrap();
    connection.close().await;
    for registration in registrations {
        registration.close().await;
    }
    registry.close().await;
}
