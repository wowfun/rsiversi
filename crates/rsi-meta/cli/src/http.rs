use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::auth::AuthState;
use crate::framing::{MAX_WIRE_REQUEST_BYTES, MAX_WIRE_RESPONSE_BYTES};
use crate::host::{SharedHost, submit_with_rejection};
use crate::lifecycle::DaemonLifecycle;
use crate::protocol::{
    CONTROL_PROTOCOL, CONTROL_VERSION, CommandOutcome, StreamKind, rejected, validate_command,
};
use crate::streams::{
    StreamDataLimitExceeded, StreamRouter, WireEnvelope, cancel_envelope, decode_stream_data,
    decode_wire_envelope, encode_stream_data_bounded,
};

const WEBSOCKET_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP_CONNECTION_MAX_LIFETIME: Duration = Duration::from_secs(30);
const HTTP_CONNECTION_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const MAX_HTTP_CONNECTIONS: usize = 128;

type ConnectionPermit = std::sync::Arc<OwnedSemaphorePermit>;

#[derive(Debug)]
struct OutgoingMessageTooLarge;

impl std::fmt::Display for OutgoingMessageTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("outgoing WebSocket envelope exceeds the configured message limit")
    }
}

impl std::error::Error for OutgoingMessageTooLarge {}

#[derive(Clone, Debug, Default)]
pub struct OriginPolicy {
    allowed: BTreeSet<String>,
}

impl OriginPolicy {
    pub fn new(origins: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut allowed = BTreeSet::new();
        for origin in origins {
            let value = HeaderValue::from_str(&origin)
                .with_context(|| format!("invalid allowed Origin {origin:?}"))?;
            let normalized = value
                .to_str()
                .context("allowed Origin is not visible ASCII")?
                .to_owned();
            allowed.insert(normalized);
        }
        Ok(Self { allowed })
    }

    pub fn for_listener(
        address: SocketAddr,
        additional: impl IntoIterator<Item = String>,
    ) -> Result<Self> {
        let mut origins = Vec::new();
        match address.ip() {
            std::net::IpAddr::V4(ip) => {
                origins.push(format!("http://{ip}:{}", address.port()));
                origins.push(format!("http://localhost:{}", address.port()));
            }
            std::net::IpAddr::V6(ip) => {
                origins.push(format!("http://[{ip}]:{}", address.port()));
                origins.push(format!("http://localhost:{}", address.port()));
            }
        }
        origins.extend(additional);
        Self::new(origins)
    }

    pub fn allows(&self, headers: &HeaderMap) -> bool {
        let mut origins = headers.get_all(header::ORIGIN).iter();
        let Some(origin) = origins.next() else {
            // Non-browser clients do not send Origin. Authentication is still
            // mandatory and browsers always send Origin on a WS handshake.
            return true;
        };
        if origins.next().is_some() {
            return false;
        }
        origin
            .to_str()
            .is_ok_and(|origin| self.allowed.contains(origin))
    }
}

#[derive(Clone, Debug)]
struct HttpState {
    host: SharedHost,
    auth: AuthState,
    origins: OriginPolicy,
    lifecycle: DaemonLifecycle,
    session_cancel: CancellationToken,
}

#[derive(Debug)]
pub struct HttpServer {
    listener: TcpListener,
    address: SocketAddr,
    router: Router,
    lifecycle: DaemonLifecycle,
    session_cancel: CancellationToken,
}

impl HttpServer {
    pub async fn bind(
        address: SocketAddr,
        host: SharedHost,
        auth: AuthState,
        lifecycle: DaemonLifecycle,
        additional_origins: impl IntoIterator<Item = String>,
    ) -> Result<Self> {
        require_loopback(address)?;
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("bind loopback HTTP server at {address}"))?;
        let address = listener.local_addr()?;
        let origins = OriginPolicy::for_listener(address, additional_origins)?;
        let session_cancel = CancellationToken::new();
        let router = router(HttpState {
            host,
            auth,
            origins,
            lifecycle: lifecycle.clone(),
            session_cancel: session_cancel.clone(),
        });
        Ok(Self {
            listener,
            address,
            router,
            lifecycle,
            session_cancel,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub async fn serve(self, cancellation: CancellationToken) -> Result<()> {
        self.serve_with_limits(
            cancellation,
            MAX_HTTP_CONNECTIONS,
            HTTP_HEADER_READ_TIMEOUT,
            HTTP_CONNECTION_MAX_LIFETIME,
        )
        .await
    }

    async fn serve_with_limits(
        self,
        cancellation: CancellationToken,
        max_connections: usize,
        header_read_timeout: Duration,
        connection_max_lifetime: Duration,
    ) -> Result<()> {
        let session_cancel = self.session_cancel;
        let lifecycle = self.lifecycle;
        let admission = std::sync::Arc::new(Semaphore::new(max_connections));
        let mut connections = JoinSet::new();

        loop {
            let permit = tokio::select! {
                biased;
                () = lifecycle.restarting() => break,
                () = cancellation.cancelled() => break,
                Some(joined) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = joined {
                        tracing::warn!(%error, "HTTP client task panicked");
                    }
                    continue;
                }
                permit = admission.clone().acquire_owned() => {
                    permit.context("HTTP connection admission semaphore closed")?
                }
            };
            let accepted = tokio::select! {
                biased;
                () = lifecycle.restarting() => break,
                () = cancellation.cancelled() => break,
                accepted = self.listener.accept() => accepted,
            };
            let (stream, _) = match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::warn!(%error, "retrying loopback HTTP accept after an error");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let router = self.router.clone();
            let permit = std::sync::Arc::new(permit);
            let request_permit = permit.clone();
            let lifecycle = lifecycle.clone();
            let cancellation = cancellation.clone();
            connections.spawn(async move {
                let service = router.map_request(move |mut request: hyper::Request<Incoming>| {
                    request.extensions_mut().insert(request_permit.clone());
                    request.map(Body::new)
                });
                let service = TowerToHyperService::new(service);
                let mut builder = http1::Builder::new();
                builder
                    .timer(TokioTimer::default())
                    .header_read_timeout(header_read_timeout);
                let connection = builder
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades();
                tokio::pin!(connection);
                tokio::select! {
                    result = &mut connection => {
                        if let Err(error) = result {
                            tracing::debug!(%error, "HTTP client disconnected with an error");
                        }
                    }
                    () = lifecycle.restarting() => {
                        connection.as_mut().graceful_shutdown();
                        let _ = tokio::time::timeout(
                            HTTP_CONNECTION_SHUTDOWN_GRACE,
                            &mut connection,
                        ).await;
                    }
                    () = cancellation.cancelled() => {
                        connection.as_mut().graceful_shutdown();
                        let _ = tokio::time::timeout(
                            HTTP_CONNECTION_SHUTDOWN_GRACE,
                            &mut connection,
                        ).await;
                    }
                    () = tokio::time::sleep(connection_max_lifetime) => {
                        connection.as_mut().graceful_shutdown();
                        let _ = tokio::time::timeout(
                            HTTP_CONNECTION_SHUTDOWN_GRACE,
                            &mut connection,
                        ).await;
                    }
                }
                drop(permit);
            });
        }

        session_cancel.cancel();
        while connections.join_next().await.is_some() {}
        Ok(())
    }
}

pub fn require_loopback(address: SocketAddr) -> Result<()> {
    if !address.ip().is_loopback() {
        bail!("refusing non-loopback HTTP bind address {address}");
    }
    Ok(())
}

fn router(state: HttpState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/ws", get(upgrade_websocket))
        .layer(middleware::from_fn(reject_request_body))
        .with_state(state)
}

async fn reject_request_body(request: axum::extract::Request, next: Next) -> Response {
    let content_length = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if request.headers().contains_key(header::TRANSFER_ENCODING)
        || content_length.is_some_and(|length| length != 0)
    {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "request bodies are not supported",
        )
            .into_response();
    }
    next.run(request).await
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn version() -> Json<serde_json::Value> {
    Json(json!({
        "name": "rsi-meta",
        "version": env!("CARGO_PKG_VERSION"),
        "control_protocol": CONTROL_PROTOCOL,
        "control_version": CONTROL_VERSION,
    }))
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct WebSocketQuery {
    #[serde(default)]
    after: u64,
}

async fn upgrade_websocket(
    State(state): State<HttpState>,
    Extension(connection_permit): Extension<ConnectionPermit>,
    Query(query): Query<WebSocketQuery>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Response {
    let Some(generation) = authorized_generation(&state.auth, &headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "invalid bearer token",
        )
            .into_response();
    };
    if !state.origins.allows(&headers) {
        return (StatusCode::FORBIDDEN, "Origin is not allowed").into_response();
    }
    websocket
        .max_frame_size(MAX_WIRE_REQUEST_BYTES)
        .max_message_size(MAX_WIRE_REQUEST_BYTES)
        .on_upgrade(move |socket| async move {
            let _connection_permit = connection_permit;
            serve_websocket(socket, state, generation, query.after).await;
        })
        .into_response()
}

#[cfg(test)]
fn authorize_headers(auth: &AuthState, headers: &HeaderMap) -> bool {
    authorized_generation(auth, headers).is_some()
}

fn authorized_generation(auth: &AuthState, headers: &HeaderMap) -> Option<u64> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value
        .to_str()
        .ok()
        .and_then(|value| auth.authorize_generation(value))
}

#[allow(clippy::too_many_lines)]
async fn serve_websocket(
    socket: WebSocket,
    state: HttpState,
    connection_generation: u64,
    after_cursor: u64,
) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = match state.host.subscribe(after_cursor).await {
        Ok(events) => events,
        Err(error) => {
            tracing::warn!(%error, "failed to subscribe WebSocket event stream");
            let _ = close(
                &mut sender,
                close_code::ERROR,
                "code=event_subscribe_failed",
            )
            .await;
            return;
        }
    };
    let mut generation = state.auth.subscribe_generation();
    let mut last_cursor = after_cursor;
    let mut streams = StreamRouter::new(state.host.clone());

    let current_generation = *generation.borrow();
    if current_generation != connection_generation {
        let _ = close(&mut sender, close_code::POLICY, "code=bearer_token_rotated").await;
        return;
    }

    loop {
        tokio::select! {
            () = state.lifecycle.restarting() => {
                let (code, reason) = if state.lifecycle.is_restarting() {
                    (close_code::RESTART, "code=daemon_restarting")
                } else {
                    (close_code::AWAY, "code=daemon_shutdown")
                };
                let _ = close(&mut sender, code, reason).await;
                break;
            }
            () = state.session_cancel.cancelled() => {
                let _ = close(&mut sender, close_code::AWAY, "code=daemon_shutdown").await;
                break;
            }
            changed = generation.changed() => {
                if changed.is_err() || *generation.borrow_and_update() != connection_generation {
                    let _ = close(&mut sender, close_code::POLICY, "code=bearer_token_rotated").await;
                    break;
                }
            }
            event = events.next() => {
                let envelope = match event {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => {
                        tracing::warn!(%error, last_cursor, "WebSocket event stream interrupted");
                        let (code, reason) = match error.downcast_ref::<rsi_meta::HostError>() {
                            Some(rsi_meta::HostError::EventCursorExpired {
                                requested,
                                minimum_available,
                            }) => (
                                close_code::POLICY,
                                format!(
                                    "code=cursor_expired requested={requested} minimum_available={minimum_available} resync_cursor={}",
                                    state.host.event_cursor(),
                                ),
                            ),
                            _ => (
                                close_code::AGAIN,
                                format!(
                                    "code=event_stream_interrupted last_cursor={last_cursor}"
                                ),
                            ),
                        };
                        let _ = close(&mut sender, code, reason).await;
                        break;
                    }
                    None => {
                        let reason =
                            format!("code=event_stream_interrupted last_cursor={last_cursor}");
                        let _ = close(&mut sender, close_code::AGAIN, reason).await;
                        break;
                    }
                };
                if let Err(error) = send_text(&mut sender, &envelope).await {
                    // `last_cursor` means successfully handed to the socket,
                    // never merely dequeued from the durable event stream.
                    // Advancing it early would make a reconnect skip the event
                    // whose write just failed.
                    tracing::debug!(%error, last_cursor, "WebSocket event delivery interrupted");
                    let reason =
                        format!("code=event_delivery_interrupted last_cursor={last_cursor}");
                    let _ = close(&mut sender, close_code::AGAIN, reason).await;
                    break;
                }
                last_cursor = envelope.cursor;
            }
            frame = streams.recv() => {
                let Some(frame) = frame else { continue };
                if let Err(error) = send_stream(&mut sender, &frame).await {
                    close_after_send_error(&mut sender, &error).await;
                    break;
                }
            }
            message = receiver.next() => {
                let Some(message) = message else { break };
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::debug!(%error, "rejected WebSocket input frame");
                        let _ = close(&mut sender, close_code::SIZE, "code=message_too_large").await;
                        break;
                    }
                };
                match message {
                    Message::Text(text) => {
                        let envelope = match decode_wire_envelope(text.as_str()) {
                            Ok(envelope) => envelope,
                            Err(error) => {
                                tracing::debug!(%error, "rejected invalid WebSocket envelope");
                                let _ = close(&mut sender, close_code::INVALID, "code=invalid_envelope").await;
                                break;
                            }
                        };
                        let WireEnvelope::Control(command) = envelope else {
                            let WireEnvelope::Stream(frame) = envelope else { unreachable!() };
                            let stream_id = frame.stream_id.clone();
                            if let Err(error) = streams.route(frame) {
                                let response = cancel_envelope(
                                    &stream_id,
                                    "invalid_stream_frame",
                                    &format!("{error:#}"),
                                );
                                if let Err(error) = send_text(&mut sender, &response).await {
                                    close_after_send_error(&mut sender, &error).await;
                                    break;
                                }
                            }
                            continue;
                        };
                        let command_id = command.command_id.clone();
                        if let Err(error) = validate_command(&command) {
                            let response = rejected(
                                command_id,
                                state.host.graph_revision(),
                                "invalid_command",
                                error.to_string(),
                            );
                            if let Err(error) = send_text(&mut sender, &response).await {
                                close_after_send_error(&mut sender, &error).await;
                                break;
                            }
                            continue;
                        }
                        let response = submit_with_rejection(state.host.as_ref(), command).await;
                        let rotated = if let CommandOutcome::TokenRotated { generation } = &response.payload {
                            match state.auth.rotate_to(*generation) {
                                Ok(rotated) => rotated,
                                Err(error) => {
                                    tracing::error!(%error, "failed to publish durable token rotation");
                                    let _ = close(
                                        &mut sender,
                                        close_code::RESTART,
                                        "code=daemon_restarting",
                                    ).await;
                                    state.lifecycle.request_restart();
                                    break;
                                }
                            }
                        } else {
                            false
                        };
                        let shutting_down = matches!(&response.payload, CommandOutcome::ShuttingDown);
                        let send_error = send_text(&mut sender, &response).await.err();
                        if shutting_down {
                            // The durable core outcome, not delivery of its
                            // acknowledgement, owns process termination.
                            state.lifecycle.request_shutdown();
                        }
                        if let Some(error) = send_error {
                            close_after_send_error(&mut sender, &error).await;
                            break;
                        }
                        if rotated {
                            let _ = close(
                                &mut sender,
                                close_code::POLICY,
                                "code=bearer_token_rotated",
                            ).await;
                            break;
                        }
                        if shutting_down {
                            break;
                        }
                    }
                    Message::Ping(bytes) => {
                        let pong = tokio::time::timeout(
                            WEBSOCKET_SEND_TIMEOUT,
                            sender.send(Message::Pong(bytes)),
                        )
                        .await
                        .context("WebSocket Pong send timed out")
                        .and_then(|result| result.context("send WebSocket Pong"));
                        if let Err(error) = pong {
                            close_after_send_error(&mut sender, &error).await;
                            break;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                    Message::Binary(bytes) => {
                        let frame = match decode_stream_data(&bytes) {
                            Ok(frame) => frame,
                            Err(error) => {
                                tracing::debug!(%error, "rejected invalid WebSocket DATA frame");
                                let _ = close(&mut sender, close_code::INVALID, "code=invalid_binary_data").await;
                                break;
                            }
                        };
                        let stream_id = frame.stream_id.clone();
                        if let Err(error) = streams.route(frame) {
                            let response = cancel_envelope(
                                &stream_id,
                                "invalid_stream_frame",
                                &format!("{error:#}"),
                            );
                            if let Err(error) = send_stream(&mut sender, &response).await {
                                close_after_send_error(&mut sender, &error).await;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    streams.disconnect().await;
}

async fn send_stream(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    frame: &crate::protocol::StreamEnvelope,
) -> Result<()> {
    if frame.kind != StreamKind::Data {
        frame.validate().context("validate outgoing stream frame")?;
        return send_text(sender, frame).await;
    }
    let encoded = match encode_stream_data_bounded(frame, MAX_WIRE_RESPONSE_BYTES) {
        Ok(encoded) => encoded,
        Err(error) if error.downcast_ref::<StreamDataLimitExceeded>().is_some() => {
            return Err(OutgoingMessageTooLarge.into());
        }
        Err(error) => return Err(error),
    };
    tokio::time::timeout(
        WEBSOCKET_SEND_TIMEOUT,
        sender.send(Message::Binary(encoded.into())),
    )
    .await
    .context("WebSocket DATA send timed out")??;
    Ok(())
}

async fn send_text(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    envelope: &impl Serialize,
) -> Result<()> {
    let encoded = serde_json::to_string(envelope)?;
    if encoded.len() > MAX_WIRE_RESPONSE_BYTES {
        return Err(OutgoingMessageTooLarge.into());
    }
    tokio::time::timeout(
        WEBSOCKET_SEND_TIMEOUT,
        sender.send(Message::Text(encoded.into())),
    )
    .await
    .context("WebSocket send timed out")??;
    Ok(())
}

async fn close_after_send_error(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    error: &anyhow::Error,
) {
    let (code, reason) = if error.downcast_ref::<OutgoingMessageTooLarge>().is_some() {
        (
            close_code::SIZE,
            "code=outgoing_message_too_large".to_owned(),
        )
    } else {
        tracing::debug!(%error, "WebSocket outgoing delivery failed");
        (
            close_code::ERROR,
            "code=outgoing_delivery_failed".to_owned(),
        )
    };
    let _ = close(sender, code, reason).await;
}

async fn close(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    code: u16,
    reason: impl Into<String>,
) -> Result<()> {
    let mut reason = reason.into();
    // RFC 6455 permits at most 123 UTF-8 bytes after the two-byte close code.
    if reason.len() > 123 {
        let mut boundary = 123;
        while !reason.is_char_boundary(boundary) {
            boundary -= 1;
        }
        reason.truncate(boundary);
    }
    tokio::time::timeout(
        WEBSOCKET_SEND_TIMEOUT,
        sender.send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }))),
    )
    .await
    .context("WebSocket close timed out")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use anyhow::Result;
    use async_trait::async_trait;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::Notify;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    use rsi_meta::{
        InstanceSnapshot, InstanceStatus, PackageId, PackageSource, PluginInspection, ScopeId,
    };
    use rsi_meta_loader::ContentHash;

    use super::*;
    use crate::auth::read_token_file;
    use crate::host::{HostApi, HostEventStream};
    use crate::protocol::{
        CONTROL_PROTOCOL, CONTROL_VERSION, Command, CommandEnvelope, CommandOutcome,
        CommandOutcomeEnvelope, ControlEnvelopeKind, EventEnvelope, GraphRevision, outcome,
    };

    #[derive(Debug)]
    struct ReplayHost {
        subscribed_after: AtomicU64,
    }

    #[derive(Debug)]
    struct LagThenReplayHost {
        subscribed_after: AtomicU64,
        lag_gate: Arc<Notify>,
    }

    #[derive(Debug)]
    struct OversizedOutcomeHost;

    #[derive(Debug)]
    struct ExhaustedEventHost;

    #[derive(Debug)]
    struct ExpiredEventCursorHost;

    #[derive(Debug)]
    struct InspectOutcomeHost {
        schema_bytes: usize,
    }

    #[derive(Debug)]
    struct TokenRotationHost;

    fn event(cursor: u64) -> EventEnvelope {
        EventEnvelope {
            protocol: CONTROL_PROTOCOL.to_owned(),
            version: CONTROL_VERSION,
            kind: ControlEnvelopeKind::Event,
            cursor,
            operation_id: Some(format!("event-{cursor}")),
            graph_revision: GraphRevision(3),
            payload: crate::protocol::Event::HostShuttingDown,
            extensions: BTreeMap::default(),
        }
    }

    #[async_trait]
    impl HostApi for ReplayHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            Ok(outcome(
                command.command_id,
                GraphRevision(3),
                CommandOutcome::RestartRequired {
                    current: None,
                    candidate: rsi_meta::CompositionDigest {
                        composition_id: "demo".to_owned(),
                        manifest_sha256: "manifest".to_owned(),
                        lock_sha256: "lock".to_owned(),
                    },
                    packages: Vec::new(),
                },
            ))
        }

        async fn subscribe(&self, after_cursor: u64) -> Result<HostEventStream> {
            self.subscribed_after.store(after_cursor, Ordering::Release);
            Ok(Box::pin(
                futures_util::stream::iter([Ok(event(after_cursor + 1))])
                    .chain(futures_util::stream::pending()),
            ))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(3)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostApi for LagThenReplayHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            Ok(outcome(
                command.command_id,
                GraphRevision(3),
                CommandOutcome::ShuttingDown,
            ))
        }

        async fn subscribe(&self, after_cursor: u64) -> Result<HostEventStream> {
            self.subscribed_after.store(after_cursor, Ordering::Release);
            if after_cursor == 40 {
                let lag_gate = self.lag_gate.clone();
                let lagged = futures_util::stream::once(async move {
                    lag_gate.notified().await;
                    Err(anyhow::anyhow!(
                        "event subscriber lagged by 256 events; resubscribe from the last cursor"
                    ))
                });
                return Ok(Box::pin(
                    futures_util::stream::iter([Ok(event(41))]).chain(lagged),
                ));
            }
            Ok(Box::pin(
                futures_util::stream::iter([Ok(event(after_cursor + 1))])
                    .chain(futures_util::stream::pending()),
            ))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(3)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostApi for OversizedOutcomeHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            Ok(outcome(
                command.command_id,
                GraphRevision(0),
                CommandOutcome::Rejected {
                    code: "oversized_test_outcome".to_owned(),
                    message: "x".repeat(6 * 1024 * 1024),
                    details: BTreeMap::new(),
                },
            ))
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::pending()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostApi for ExpiredEventCursorHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            Ok(outcome(
                command.command_id,
                GraphRevision(3),
                CommandOutcome::ShuttingDown,
            ))
        }

        async fn subscribe(&self, after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::iter([Err(
                rsi_meta::HostError::EventCursorExpired {
                    requested: after_cursor,
                    minimum_available: 42,
                }
                .into(),
            )])))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(3)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        fn event_cursor(&self) -> u64 {
            99
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostApi for ExhaustedEventHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            Ok(outcome(
                command.command_id,
                GraphRevision(0),
                CommandOutcome::ShuttingDown,
            ))
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::empty()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostApi for InspectOutcomeHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            assert!(matches!(command.payload, Command::InspectPlugin { .. }));
            let hash = ContentHash::digest([]);
            Ok(outcome(
                command.command_id,
                GraphRevision(0),
                CommandOutcome::Plugin {
                    instance: Some(PluginInspection {
                        instance: InstanceSnapshot {
                            id: rsi_meta::InstanceId::new("probe"),
                            package: PackageSource {
                                package_id: PackageId::new("fixture.probe"),
                                version: "0.0.1".to_owned(),
                                manifest_path: PathBuf::from("/fixture/plugin.toml"),
                                target: "test-target".to_owned(),
                                manifest_sha256: hash,
                                artifact_sha256: hash,
                                config_schema_sha256: Some(hash),
                            },
                            scope: ScopeId::new("root"),
                            status: InstanceStatus::Active,
                            provides: Vec::new(),
                            requires: Vec::new(),
                        },
                        process_fixed: false,
                        capabilities: Vec::new(),
                        config_schema_path: Some(PathBuf::from("/fixture/config.schema.json")),
                        config_schema: Some(json!({
                            "description": "x".repeat(self.schema_bytes),
                        })),
                    }),
                },
            ))
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::pending()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostApi for TokenRotationHost {
        async fn submit(&self, command: CommandEnvelope) -> Result<CommandOutcomeEnvelope> {
            assert!(matches!(command.payload, Command::RotateToken));
            Ok(outcome(
                command.command_id,
                GraphRevision(0),
                CommandOutcome::TokenRotated { generation: 1 },
            ))
        }

        async fn subscribe(&self, _after_cursor: u64) -> Result<HostEventStream> {
            Ok(Box::pin(futures_util::stream::pending()))
        }

        fn graph_revision(&self) -> GraphRevision {
            GraphRevision(0)
        }

        fn token_generation(&self) -> u64 {
            0
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    fn authenticated_request(
        address: SocketAddr,
        after: u64,
        token: &crate::auth::BearerToken,
    ) -> http::Request<()> {
        let mut request = format!("ws://{address}/ws?after={after}")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token.expose())).unwrap(),
        );
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_str(&format!("http://{address}")).unwrap(),
        );
        request
    }

    #[test]
    fn refuses_non_loopback_addresses() {
        assert!(require_loopback(SocketAddr::from(([127, 0, 0, 1], 0))).is_ok());
        assert!(require_loopback(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0)).is_ok());
        assert!(require_loopback(SocketAddr::from(([0, 0, 0, 0], 0))).is_err());
        assert!(
            require_loopback(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4)),
                0
            ))
            .is_err()
        );
    }

    #[test]
    fn origin_policy_is_exact_and_rejects_duplicates() {
        let policy = OriginPolicy::new(["http://localhost:9000".to_owned()]).unwrap();
        let mut headers = HeaderMap::new();
        assert!(policy.allows(&headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:9000"),
        );
        assert!(policy.allows(&headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.invalid"),
        );
        assert!(!policy.allows(&headers));
        headers.append(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:9000"),
        );
        assert!(!policy.allows(&headers));
    }

    #[test]
    fn http_authorization_requires_the_current_bearer() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let mut headers = HeaderMap::new();

        assert!(!authorize_headers(&auth, &headers));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer invalid"),
        );
        assert!(!authorize_headers(&auth, &headers));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token.expose())).unwrap(),
        );
        assert!(authorize_headers(&auth, &headers));
        headers.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer duplicate"),
        );
        assert!(!authorize_headers(&auth, &headers));
        headers.remove(header::AUTHORIZATION);
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token.expose())).unwrap(),
        );

        assert!(auth.rotate_to(1).unwrap());
        assert!(!authorize_headers(&auth, &headers));
    }

    #[tokio::test]
    async fn slow_http_headers_are_timed_out_and_connection_admission_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let auth =
            AuthState::initialize(directory.path().join("run").join("daemon.token")).unwrap();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(ReplayHost {
                subscribed_after: AtomicU64::new(0),
            }),
            auth,
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve_with_limits(
            cancellation.clone(),
            1,
            Duration::from_millis(250),
            Duration::from_secs(30),
        ));

        let mut slow = TcpStream::connect(address).await.unwrap();
        slow.write_all(b"GET /health HTTP/1.1\r\nHost:")
            .await
            .unwrap();
        let mut queued = TcpStream::connect(address).await.unwrap();
        queued
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                queued.read_to_end(&mut response)
            )
            .await
            .is_err(),
            "the second connection must remain outside the one-connection admission bound"
        );

        tokio::time::timeout(Duration::from_secs(1), slow.read_to_end(&mut Vec::new()))
            .await
            .expect("slow header deadline")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), queued.read_to_end(&mut response))
            .await
            .expect("queued request admitted after the slow connection closes")
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn completed_http_request_releases_connection_admission_without_client_close() {
        let directory = tempfile::tempdir().unwrap();
        let auth =
            AuthState::initialize(directory.path().join("run").join("daemon.token")).unwrap();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(ReplayHost {
                subscribed_after: AtomicU64::new(0),
            }),
            auth,
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve_with_limits(
            cancellation.clone(),
            1,
            Duration::from_secs(1),
            Duration::from_millis(50),
        ));

        let mut first = TcpStream::connect(address).await.unwrap();
        first
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut first_response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            first.read_to_end(&mut first_response),
        )
        .await
        .expect("server closes the idle connection at its maximum lifetime")
        .unwrap();
        assert!(first_response.starts_with(b"HTTP/1.1 200 OK"));

        let mut second = TcpStream::connect(address).await.unwrap();
        second
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut second_response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            second.read_to_end(&mut second_response),
        )
        .await
        .expect("connection permit is released after the first response")
        .unwrap();
        assert!(second_response.starts_with(b"HTTP/1.1 200 OK"));

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn http_rejects_a_trickled_body_without_waiting_for_it() {
        let directory = tempfile::tempdir().unwrap();
        let auth =
            AuthState::initialize(directory.path().join("run").join("daemon.token")).unwrap();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(ReplayHost {
                subscribed_after: AtomicU64::new(0),
            }),
            auth,
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));

        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"POST /health HTTP/1.1\r\nHost: localhost\r\nContent-Length: 999999\r\nConnection: close\r\n\r\nx",
            )
            .await
            .unwrap();
        let mut response = [0_u8; 256];
        let bytes = tokio::time::timeout(Duration::from_secs(1), client.read(&mut response))
            .await
            .expect("body rejection deadline")
            .unwrap();
        assert!(response[..bytes].starts_with(b"HTTP/1.1 413"));

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn authenticated_websocket_replays_after_cursor_and_rotation_closes_it() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let host = Arc::new(ReplayHost {
            subscribed_after: AtomicU64::new(0),
        });
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            host.clone(),
            auth.clone(),
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));

        let unauthorized = connect_async(format!("ws://{address}/ws?after=41"))
            .await
            .unwrap_err();
        let response = match unauthorized {
            tokio_tungstenite::tungstenite::Error::Http(response) => response,
            other => panic!("expected an HTTP authentication rejection, got {other:?}"),
        };
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let request = authenticated_request(address, 41, &token);
        let (mut websocket, response) = connect_async(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

        let message = websocket.next().await.unwrap().unwrap();
        let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
            panic!("expected replay event text frame, got {message:?}");
        };
        let event: EventEnvelope = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(event.cursor, 42);
        assert_eq!(host.subscribed_after.load(Ordering::Acquire), 41);

        assert!(auth.rotate_to(1).unwrap());
        let close = websocket.next().await.unwrap().unwrap();
        let tokio_tungstenite::tungstenite::Message::Close(Some(frame)) = close else {
            panic!("expected token-rotation close frame, got {close:?}");
        };
        assert_eq!(frame.code, CloseCode::Policy);
        assert_eq!(frame.reason, "code=bearer_token_rotated");

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn failed_token_publication_sends_a_restart_close_frame() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let lifecycle = DaemonLifecycle::default();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(TokenRotationHost),
            auth,
            lifecycle.clone(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));
        let (mut websocket, _) = connect_async(authenticated_request(address, 0, &token))
            .await
            .unwrap();

        std::fs::remove_file(&token_file).unwrap();
        std::fs::create_dir(&token_file).unwrap();
        let command = CommandEnvelope::new("rotate-fails-to-publish", Command::RotateToken);
        websocket
            .send(ClientMessage::Text(
                serde_json::to_string(&command).unwrap().into(),
            ))
            .await
            .unwrap();
        let closed = websocket.next().await.unwrap().unwrap();
        let ClientMessage::Close(Some(frame)) = closed else {
            panic!("expected restart close frame, got {closed:?}");
        };
        assert_eq!(frame.code, CloseCode::Restart);
        assert_eq!(frame.reason, "code=daemon_restarting");
        assert!(lifecycle.is_restarting());

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn lagged_control_client_is_closed_with_a_cursor_it_can_resume() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let lag_gate = Arc::new(Notify::new());
        let host = Arc::new(LagThenReplayHost {
            subscribed_after: AtomicU64::new(0),
            lag_gate: lag_gate.clone(),
        });
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            host.clone(),
            auth,
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));

        let (mut stalled, _) = connect_async(authenticated_request(address, 40, &token))
            .await
            .unwrap();
        // Do not drain the connection before releasing the exact core-lag seam.
        // Notify retains a permit, so this is scheduling-independent.
        lag_gate.notify_one();
        let first = stalled.next().await.unwrap().unwrap();
        let ClientMessage::Text(first) = first else {
            panic!("expected last deliverable event, got {first:?}");
        };
        let first: EventEnvelope = serde_json::from_str(first.as_str()).unwrap();
        assert_eq!(first.cursor, 41);
        let closed = stalled.next().await.unwrap().unwrap();
        let ClientMessage::Close(Some(frame)) = closed else {
            panic!("expected lag close frame, got {closed:?}");
        };
        assert_eq!(frame.code, CloseCode::Again);
        assert!(frame.reason.contains("code=event_stream_interrupted"));
        assert!(frame.reason.contains("last_cursor=41"));

        let (mut resumed, _) = connect_async(authenticated_request(address, 41, &token))
            .await
            .unwrap();
        let replay = resumed.next().await.unwrap().unwrap();
        let ClientMessage::Text(replay) = replay else {
            panic!("expected resumed event, got {replay:?}");
        };
        let replay: EventEnvelope = serde_json::from_str(replay.as_str()).unwrap();
        assert_eq!(replay.cursor, 42);
        assert_eq!(host.subscribed_after.load(Ordering::Acquire), 41);
        resumed.close(None).await.unwrap();

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unexpected_event_stream_exhaustion_has_a_resumable_close_reason() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(ExhaustedEventHost),
            auth,
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));

        let (mut websocket, _) = connect_async(authenticated_request(address, 73, &token))
            .await
            .unwrap();
        let closed = websocket.next().await.unwrap().unwrap();
        let ClientMessage::Close(Some(frame)) = closed else {
            panic!("expected an event-stream close frame, got {closed:?}");
        };
        assert_eq!(frame.code, CloseCode::Again);
        assert!(frame.reason.contains("code=event_stream_interrupted"));
        assert!(frame.reason.contains("last_cursor=73"));

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn expired_websocket_cursor_returns_the_resync_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run/daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(ExpiredEventCursorHost),
            auth,
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));

        let (mut websocket, _) = connect_async(authenticated_request(address, 7, &token))
            .await
            .unwrap();
        let closed = websocket.next().await.unwrap().unwrap();
        let ClientMessage::Close(Some(frame)) = closed else {
            panic!("expected a cursor-expired close frame, got {closed:?}");
        };
        assert_eq!(frame.code, CloseCode::Policy);
        assert!(frame.reason.contains("code=cursor_expired"));
        assert!(frame.reason.contains("requested=7"));
        assert!(frame.reason.contains("minimum_available=42"));
        assert!(frame.reason.contains("resync_cursor=99"));

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn restart_required_outcome_keeps_websocket_admission_open() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let host = Arc::new(ReplayHost {
            subscribed_after: AtomicU64::new(0),
        });
        let lifecycle = DaemonLifecycle::default();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            host,
            auth,
            lifecycle.clone(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));
        let (mut websocket, _) = connect_async(authenticated_request(address, 0, &token))
            .await
            .unwrap();
        assert!(matches!(
            websocket.next().await.unwrap().unwrap(),
            ClientMessage::Text(_)
        ));

        for command_id in ["cached-restart", "admission-still-open"] {
            let command = CommandEnvelope::new(command_id, Command::QueryGraph);
            websocket
                .send(ClientMessage::Text(
                    serde_json::to_string(&command).unwrap().into(),
                ))
                .await
                .unwrap();
            let ClientMessage::Text(result) = websocket.next().await.unwrap().unwrap() else {
                panic!("cached restart outcome closed WebSocket admission");
            };
            let result: CommandOutcomeEnvelope = serde_json::from_str(result.as_str()).unwrap();
            assert!(matches!(
                result.payload,
                CommandOutcome::RestartRequired { .. }
            ));
            assert!(!lifecycle.is_restarting());
        }

        drop(websocket);
        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn oversized_host_outcome_is_replaced_by_a_bounded_rejection() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(OversizedOutcomeHost),
            auth,
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));
        let (mut websocket, _) = connect_async(authenticated_request(address, 0, &token))
            .await
            .unwrap();

        let command = CommandEnvelope::new("oversized", Command::QueryGraph);
        websocket
            .send(ClientMessage::Text(
                serde_json::to_string(&command).unwrap().into(),
            ))
            .await
            .unwrap();
        let response = websocket.next().await.unwrap().unwrap();
        let ClientMessage::Text(response) = response else {
            panic!("expected a bounded rejection, got {response:?}");
        };
        let response: CommandOutcomeEnvelope = serde_json::from_str(response.as_str()).unwrap();
        assert!(matches!(
            response.payload,
            CommandOutcome::Rejected { ref code, .. } if code == "outcome_too_large"
        ));

        cancellation.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn websocket_inspect_delivers_a_legal_large_config_schema() {
        let directory = tempfile::tempdir().unwrap();
        let token_file = directory.path().join("run").join("daemon.token");
        let auth = AuthState::initialize(&token_file).unwrap();
        let token = read_token_file(&token_file).unwrap();
        let server = HttpServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(InspectOutcomeHost {
                schema_bytes: 2 * 1024 * 1024,
            }),
            auth,
            DaemonLifecycle::default(),
            [],
        )
        .await
        .unwrap();
        let address = server.local_addr();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(server.serve(cancellation.clone()));
        let (mut websocket, _) = connect_async(authenticated_request(address, 0, &token))
            .await
            .unwrap();

        let command = CommandEnvelope::new(
            "inspect-large-schema",
            Command::InspectPlugin {
                instance_id: rsi_meta::InstanceId::new("probe"),
            },
        );
        websocket
            .send(ClientMessage::Text(
                serde_json::to_string(&command).unwrap().into(),
            ))
            .await
            .unwrap();
        let response = websocket.next().await.unwrap().unwrap();
        let ClientMessage::Text(response) = response else {
            panic!("expected a large inspect response, got {response:?}");
        };
        let response: CommandOutcomeEnvelope = serde_json::from_str(response.as_str()).unwrap();
        let CommandOutcome::Plugin {
            instance: Some(instance),
        } = response.payload
        else {
            panic!("expected plugin inspection");
        };
        assert_eq!(
            instance.config_schema.unwrap()["description"]
                .as_str()
                .unwrap()
                .len(),
            2 * 1024 * 1024
        );

        websocket.close(None).await.unwrap();
        cancellation.cancel();
        task.await.unwrap().unwrap();
    }
}
