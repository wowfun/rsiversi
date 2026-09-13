use crate::{
    HttpConfig,
    access::Access,
    delivery::{Delivery, Flush, FlushIo},
    policy::{Policy, one},
};
use bytes::Bytes;
use futures_util::{StreamExt, stream::FuturesUnordered};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    body::{Body as _, Frame, Incoming},
    server::conn::{http1, http2},
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use rsi_api_protocol::{
    ApiDispatch, ApiError, ApiMessage, ApiOutput, DeviceAuthentication, EndpointId, HostEpoch,
    OperationEffect, OperationId, RequestEncoding, Result,
};
use rsi_meta::Execution;
use std::{convert::Infallible, io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
    sync::Semaphore,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

pub(crate) type Body = UnsyncBoxBody<Bytes, io::Error>;

/// Explicit capabilities and identities belonging to one served generation.
#[derive(Clone, Debug)]
pub struct HttpServices {
    /// Exact domain dispatcher.
    pub dispatch: Arc<dyn ApiDispatch>,
    /// Device verification without local administrative authority.
    pub authentication: Arc<dyn DeviceAuthentication>,
    /// Persisted deployment identity supplied by its exclusive owner.
    pub endpoint: EndpointId,
    /// Exact running generation shared with the local owner metadata.
    pub epoch: HostEpoch,
}

#[derive(Debug)]
pub(crate) struct State {
    pub assets: Option<Arc<dyn crate::HttpAssets>>,
    pub asset_deliveries: Arc<Semaphore>,
    pub diagnostics: crate::HttpDiagnostics,
    pub dispatch: Arc<dyn ApiDispatch>,
    pub endpoint: EndpointId,
    pub epoch: HostEpoch,
    pub access: Access,
    pub execution: Execution,
    pub unclassified: Arc<Semaphore>,
}

/// Native listener whose shutdown releases transports independently of admitted mutations.
pub struct HttpServer {
    listener: TcpListener,
    state: State,
    tls: Option<TlsAcceptor>,
}
impl std::fmt::Debug for HttpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpServer")
            .field("listener", &self.listener)
            .field("access", &self.state.access)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpServer {
    /// Validates policy and TLS assets before binding its configured listening address.
    pub async fn bind(
        execution: Execution,
        config: HttpConfig,
        services: HttpServices,
    ) -> Result<Self> {
        let policy = config.policy()?;
        let tls = match &config.tls {
            Some(files) => Some(crate::tls::acceptor(files).await?),
            None => None,
        };
        let listener = TcpListener::bind(config.bind)
            .await
            .map_err(|_| ApiError::Backend("HTTP bind failed".into()))?;
        Ok(Self::new(execution, listener, policy, tls, services))
    }

    /// Uses an already-bound listener, preserving all configured security validation.
    pub async fn from_listener(
        execution: Execution,
        listener: TcpListener,
        config: HttpConfig,
        services: HttpServices,
    ) -> Result<Self> {
        if listener
            .local_addr()
            .map_err(|_| ApiError::Invalid("listener address unavailable".into()))?
            != config.bind
        {
            return Err(ApiError::Invalid(
                "listener differs from configured bind address".into(),
            ));
        }
        let policy = config.policy()?;
        let tls = match &config.tls {
            Some(files) => Some(crate::tls::acceptor(files).await?),
            None => None,
        };
        Ok(Self::new(execution, listener, policy, tls, services))
    }

    fn new(
        execution: Execution,
        listener: TcpListener,
        policy: Policy,
        tls: Option<TlsAcceptor>,
        services: HttpServices,
    ) -> Self {
        Self {
            listener,
            state: State {
                assets: None,
                asset_deliveries: Arc::new(Semaphore::new(8)),
                diagnostics: crate::HttpDiagnostics::default(),
                dispatch: services.dispatch,
                endpoint: services.endpoint,
                epoch: services.epoch,
                access: Access::Remote {
                    policy,
                    authentication: services.authentication,
                },
                execution,
                unclassified: Arc::new(Semaphore::new(32)),
            },
            tls,
        }
    }

    /// Adds an explicitly owned immutable asset capability before this listener starts.
    #[must_use]
    pub fn with_assets(mut self, assets: Arc<dyn crate::HttpAssets>) -> Self {
        self.state.assets = Some(assets);
        self
    }

    /// Returns the actual listener address, including its selected ephemeral port.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener
            .local_addr()
            .map_err(|_| ApiError::Backend("listener address unavailable".into()))
    }

    /// Observes API failures without retaining the listener or any request payload.
    pub fn diagnostics(&self) -> crate::HttpDiagnostics {
        self.state.diagnostics.clone()
    }

    /// Serves at most 128 connections and drains transport jobs after cancellation.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let shared = Arc::new(self.state);
        let shutdown = shutdown.child_token();
        let _lifetime = CancelOnDrop(shutdown.clone());
        let connections = Arc::new(Semaphore::new(128));
        let handshakes = shared.unclassified.clone();
        let mut jobs = FuturesUnordered::new();
        let mut retry_at: Option<rsi_meta::Deadline> = None;
        let result = loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break Ok(()),
                _ = jobs.next(), if !jobs.is_empty() => {},
                accepted = async {
                    if let Some(deadline) = &retry_at { deadline.wait().await; }
                    self.listener.accept().await
                } => {
                    let Ok((socket, _)) = accepted else {
                        shared.diagnostics.connection_failure();
                        retry_at = Some(shared.execution.deadline_after(Duration::from_millis(100)));
                        continue;
                    };
                    retry_at = None;
                    let Ok(permit) = connections.clone().try_acquire_owned() else { shared.diagnostics.connection_failure(); continue; };
                    let Ok(handshake) = handshakes.clone().try_acquire_owned() else { shared.diagnostics.connection_failure(); continue; };

                    let state = shared.clone();
                    let tls = self.tls.clone();
                    let stop = shutdown.clone();
                    jobs.push(shared.execution.spawn(async move {
                        let _permit = permit;
                        if let Some(tls) = tls {
                            let deadline = state.execution.deadline_after(Duration::from_secs(10));
                            let accepted = tokio::select! { biased; () = stop.cancelled() => return, result = deadline.timeout(tls.accept(socket)) => result };
                            if let Ok(Ok(mut stream)) = accepted {
                                stream.get_mut().1.set_buffer_limit(Some(16 * 1024));
                                if stream.get_ref().1.alpn_protocol() == Some(b"h2") {
                                    serve_h2(stream, state, stop, handshake).await;
                                } else {
                                    serve_connection(stream, state, stop, Delivery::new(handshake, None)).await;
                                }
                            } else { state.diagnostics.tls_failure(); }
                        } else { serve_connection(socket, state, stop, Delivery::new(handshake, None)).await; }
                    }));
                }
            }
        };
        shutdown.cancel();
        drop(self.listener);
        while jobs.next().await.is_some() {}
        result
    }
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(crate) async fn serve_connection<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    socket: T,
    state: Arc<State>,
    stop: CancellationToken,
    connection: Arc<Delivery>,
) {
    let diagnostics = state.diagnostics.clone();
    let socket = crate::bounded_io::WriteBound::new(socket, state.execution.clone());
    // Hyper may drop the Body after queuing its last frame, before writing it.
    let delivery = connection.clone();
    let service = service_fn(move |request| {
        let state = state.clone();
        let connection = connection.clone();
        async move { Ok::<_, Infallible>(state.response(request, &connection).await) }
    });
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10))
        .max_buf_size(32 * 1024)
        .max_headers(64)
        .keep_alive(false)
        .writev(true);
    tokio::select! {
        biased;
        () = stop.cancelled() => {},
        result = builder.serve_connection(TokioIo::new(socket), service) => {
            if result.is_err() { diagnostics.connection_failure(); }
        },
    }
    drop(delivery);
}

async fn serve_h2<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    socket: T,
    state: Arc<State>,
    stop: CancellationToken,
    idle_permit: tokio::sync::OwnedSemaphorePermit,
) {
    let diagnostics = state.diagnostics.clone();
    let flush = Arc::new(Flush::default());
    let socket = FlushIo::new(
        crate::bounded_io::WriteBound::new(socket, state.execution.clone()),
        flush.clone(),
    );
    let tasks = crate::h2_tasks::Tasks::new(state.execution.clone());
    let activity = crate::delivery::Activity::new(state.unclassified.clone(), idle_permit);
    activity.track_flush(&flush);
    let requests = activity.clone();
    let service = service_fn(move |request: Request<Incoming>| {
        let state = state.clone();
        let delivery = Delivery::for_h2(&requests, flush.clone());
        async move {
            let response = if let Some(delivery) = delivery {
                let response = state.response(request, &delivery).await;
                delivery.begin_body(
                    response
                        .headers()
                        .get(http::header::CONTENT_TYPE)
                        .is_some_and(|value| value == "text/event-stream"),
                );
                response.map(|body| delivery.body(body))
            } else {
                state.decorate(failure(ApiError::Capacity), http::Version::HTTP_2)
            };
            Ok::<_, Infallible>(response)
        }
    });
    let mut builder = http2::Builder::new(tasks.clone());
    builder
        .timer(TokioTimer::new())
        .adaptive_window(false)
        .initial_connection_window_size(64 * 1024)
        .initial_stream_window_size(64 * 1024)
        .max_concurrent_streams(32)
        .max_frame_size(16 * 1024)
        .max_send_buf_size(64 * 1024)
        .max_header_list_size(32 * 1024)
        .header_table_size(4096)
        .max_pending_accept_reset_streams(32)
        .max_local_error_reset_streams(32);
    {
        let connection = builder.serve_connection(TokioIo::new(socket), service);
        tokio::pin!(connection);
        tokio::select! { biased;
            () = stop.cancelled() => {},
            result = &mut connection => { if result.is_err() { diagnostics.connection_failure(); } },
            () = activity.expired() => {},
        }
    }
    tasks.close().await;
}

impl State {
    async fn response(&self, request: Request<Incoming>, connection: &Delivery) -> Response<Body> {
        let version = request.version();
        let response = match self.handle(request, connection).await {
            Ok(response) => response,
            Err(error) => failure(error),
        };
        self.decorate(response, version)
    }

    fn decorate(&self, mut response: Response<Body>, version: http::Version) -> Response<Body> {
        self.diagnostics.response(response.status());
        let headers = response.headers_mut();
        headers.insert(
            "x-rsi-http-version",
            http::HeaderValue::from_static(if version == http::Version::HTTP_2 {
                "2"
            } else {
                "1.1"
            }),
        );
        headers.insert("cache-control", http::HeaderValue::from_static("no-store"));
        headers.insert(
            "x-content-type-options",
            http::HeaderValue::from_static("nosniff"),
        );
        headers.insert("x-rsi-wire-version", http::HeaderValue::from_static("1"));
        headers.insert(
            "x-rsi-endpoint-id",
            self.endpoint.as_str().parse().expect("validated identity"),
        );
        headers.insert(
            "x-rsi-host-epoch",
            self.epoch.as_str().parse().expect("validated identity"),
        );
        response
    }

    async fn handle(
        &self,
        request: Request<Incoming>,
        connection: &Delivery,
    ) -> Result<Response<Body>> {
        self.access.fence(&request)?;
        if request.method() == http::Method::GET
            && let Some(assets) = &self.assets
        {
            if request.uri().query().is_some() || !request.body().is_end_stream() {
                return Err(ApiError::Invalid(
                    "asset GET requires no query or body".into(),
                ));
            }
            let permit = self
                .asset_deliveries
                .clone()
                .try_acquire_owned()
                .map_err(|_| ApiError::Capacity)?;
            connection.admit(rsi_api_protocol::ApiAdmission::new(Arc::new(permit)));
            return crate::assets::response(assets.get(request.uri().path())?, request.headers());
        }
        if request.method() != http::Method::POST || request.uri().query().is_some() {
            return Err(ApiError::Invalid(
                "API requires POST without query parameters".into(),
            ));
        }
        let (parts, body) = request.into_parts();
        let path = parts.uri.path();
        if matches!(path, "/api/v1/login" | "/api/v1/logout") {
            return self.access.cookie(path, &parts.headers, &body);
        }
        let caller = self.access.authenticate(&parts.headers)?;
        let operation = operation(path)?;
        if operation != rsi_api_protocol::describe_operation().id
            && one(&parts.headers, "x-rsi-host-epoch")? != Some(self.epoch.as_str())
        {
            return Err(ApiError::ShuttingDown);
        }
        if one(&parts.headers, "x-rsi-wire-version")? != Some("1") {
            return Err(ApiError::Unavailable);
        }
        let content_type = one(&parts.headers, "content-type")?;
        if !matches!(
            content_type,
            Some("application/json" | "application/octet-stream")
        ) || one(&parts.headers, "content-encoding")?.is_some()
        {
            return Err(ApiError::Invalid("unsupported request encoding".into()));
        }
        let invocation = self.dispatch.admit(&operation, caller.origin)?;
        let spec = invocation.spec().clone();
        let mime = match spec.encoding {
            RequestEncoding::Json => "application/json",
            RequestEncoding::Binary => "application/octet-stream",
        };
        if content_type != Some(mime) {
            return Err(ApiError::Invalid(
                "request encoding does not match operation".into(),
            ));
        }
        connection.admit(invocation.retain_admission());
        let retiring = invocation.retiring();
        let input = receive(
            body,
            &parts.headers,
            invocation.input_budget(),
            spec.maximum_request_bytes,
            &self.execution,
            &retiring,
            &caller.revoked,
        )
        .await?;
        if caller.revoked.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        let result = invocation.invoke(input);
        let output = tokio::select! { biased;
            () = caller.revoked.cancelled() => return Err(if spec.effect == OperationEffect::Mutation { ApiError::OutcomeUnknown } else { ApiError::Unauthorized }),
            output = result => output?,
        };
        Ok(match output {
            ApiOutput::Reply(message) => reply(message),
            ApiOutput::Stream(stream) => crate::sse::response(stream, caller.revoked),
        })
    }
}

fn operation(path: &str) -> Result<OperationId> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 6 || parts[..3] != ["", "api", "v1"] {
        return Err(ApiError::Unavailable);
    }
    let version = parts[5].parse::<u16>().map_err(|_| ApiError::Unavailable)?;
    if parts[5] != version.to_string() {
        return Err(ApiError::Unavailable);
    }
    OperationId::new(parts[3], parts[4], version)
}

async fn receive(
    mut body: Incoming,
    headers: &http::HeaderMap,
    budget: rsi_api_protocol::ByteBudget,
    maximum: usize,
    execution: &Execution,
    retiring: &CancellationToken,
    revoked: &CancellationToken,
) -> Result<rsi_api_protocol::RetainedBytes> {
    let declared = one(headers, "content-length")?
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| ApiError::Invalid("invalid content length".into()))
        })
        .transpose()?;
    if declared.is_some_and(|bytes| bytes > maximum) {
        return Err(ApiError::Invalid("body exceeds registered bound".into()));
    }
    let mut receiver =
        rsi_api_protocol::ByteAccumulator::new(&budget, declared.unwrap_or(maximum))?;
    let deadline = execution.deadline_after(Duration::from_mins(1));
    loop {
        let frame = tokio::select! { biased;
            () = retiring.cancelled() => return Err(ApiError::ShuttingDown),
            () = revoked.cancelled() => return Err(ApiError::Unauthorized),
            frame = deadline.timeout(body.frame()) => frame.map_err(|_| ApiError::Invalid("body receive deadline elapsed".into()))?,
        };
        match frame {
            None => break,
            Some(Err(_)) => return Err(ApiError::Invalid("incomplete request body".into())),
            Some(Ok(frame)) => {
                let bytes = frame
                    .into_data()
                    .map_err(|_| ApiError::Invalid("request trailers are not supported".into()))?;
                receiver.append(&bytes)?;
            }
        }
    }
    let input = receiver.finish_compact()?;
    if declared.is_some_and(|bytes| bytes != input.len()) {
        return Err(ApiError::Invalid("incomplete request body".into()));
    }
    Ok(input)
}

pub(crate) fn full(bytes: Bytes) -> Body {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}
fn reply(message: ApiMessage) -> Response<Body> {
    let length = message.json.len()
        + message
            .binary
            .as_ref()
            .map_or(0, |binary| 16 + binary.len());
    let (body, mime) = if let Some(binary) = message.binary {
        let mut prefix = [0; 16];
        prefix[..8].copy_from_slice(&(message.json.len() as u64).to_be_bytes());
        prefix[8..].copy_from_slice(&(binary.len() as u64).to_be_bytes());
        let chunks = [
            Bytes::copy_from_slice(&prefix),
            message.json.into_bytes(),
            binary.into_bytes(),
        ];
        (
            StreamBody::new(futures_util::stream::iter(
                chunks.map(|bytes| Ok::<_, io::Error>(Frame::data(bytes))),
            ))
            .boxed_unsync(),
            "application/vnd.rsi.binary",
        )
    } else {
        (full(message.json.into_bytes()), "application/json")
    };
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert("content-type", http::HeaderValue::from_static(mime));
    response
        .headers_mut()
        .insert("content-length", http::HeaderValue::from(length));
    response
}

pub(crate) fn failure(error: ApiError) -> Response<Body> {
    let (status, bytes, mime) = match error {
        ApiError::OutcomeUnknown => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(b"{\"code\":\"outcome_unknown\"}"),
            "application/json",
        ),
        ApiError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            Bytes::from_static(b"{\"code\":\"unauthorized\"}"),
            "application/json",
        ),
        ApiError::Capacity => (
            StatusCode::TOO_MANY_REQUESTS,
            Bytes::from_static(b"{\"code\":\"capacity\"}"),
            "application/json",
        ),
        ApiError::ShuttingDown => (
            StatusCode::CONFLICT,
            Bytes::from_static(b"{\"code\":\"generation_retired\"}"),
            "application/json",
        ),
        ApiError::Unavailable => (
            StatusCode::NOT_FOUND,
            Bytes::from_static(b"{\"code\":\"unavailable\"}"),
            "application/json",
        ),
        ApiError::Invalid(_) => (
            StatusCode::BAD_REQUEST,
            Bytes::from_static(b"{\"code\":\"invalid\"}"),
            "application/json",
        ),
        ApiError::Backend(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(b"{\"code\":\"backend\"}"),
            "application/json",
        ),
        ApiError::Domain(bytes) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            bytes.into_bytes(),
            "application/vnd.rsi.domain-error+json",
        ),
    };
    let length = bytes.len();
    let mut response = Response::new(full(bytes));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert("content-type", http::HeaderValue::from_static(mime));
    response
        .headers_mut()
        .insert("content-length", http::HeaderValue::from(length));
    response
}
