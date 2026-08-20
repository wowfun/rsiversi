//! Official `OpenAI` adapters for Responses, Images, transcription, speech, and Realtime.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // AiError carries the public failure taxonomy.

use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_stream::{stream, try_stream};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::{SinkExt as _, StreamExt as _, stream as futures_stream};
use http::{HeaderValue, Method};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rsi_ai_auth::SecretValue;
use rsi_ai_protocol::{
    AiError, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase, FinishReason,
    HostedTool, ImageEvent, ImageRequest, LanguageEvent, LanguageRequest, MessageContent,
    MessageRole, ProviderExtension, RealtimeAudioFormat, RealtimeCloseReason, RealtimeCommand,
    RealtimeEvent, RealtimeRequest, ResponseFormat, Source, SpeechEvent, SpeechFormat,
    SpeechRequest, TokenUsage, ToolChoice, TranscriptionEvent, TranscriptionRequest,
    TranscriptionSegment,
};
use rsi_ai_provider::{
    AbortSignal, AdapterFuture, DeferredLanguageAdapterHandle, DeferredLanguageAdapterStream,
    DeferredLanguageBatch, DeferredLanguageCheckpoint, DeferredLanguageOperation, DeferredStatus,
    ImageAdapter, ImageAdapterStream, LanguageAdapter, LanguageAdapterStream, PrepareContext,
    Prepared, RealtimeAdapter, RealtimeAdapterTransport, RealtimeConnection, SpeechAdapter,
    SpeechAdapterStream, TranscriptionAdapter, TranscriptionAdapterStream,
};
use rsi_ai_transport::{
    ByteStream, HttpRequest, HttpTransport, SseTermination, TransportError, collect_body,
    decode_sse, invalid_request_error, provider_error as ai_error, provider_http_error,
    transport_body_error, transport_connect_error, transport_stream_error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message as WebSocketMessage, client::IntoClientRequest as _},
};

// Ten maximum-size (32 MiB) decoded images require about 427 MiB of base64;
// the remaining headroom covers the bounded JSON envelope. This is a per-call
// transient ceiling, not a process-wide concurrency budget.
const MAX_JSON_BODY_BYTES: usize = 448 * 1024 * 1024;
const MAX_TRANSCRIPTION_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEFERRED_CONTROL_BODY_BYTES: usize = 1024 * 1024;
const OUTPUT_CHUNK_BYTES: usize = 256 * 1024;
const DEFAULT_REALTIME_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Fixed official `OpenAI` endpoint policy shared by all HTTP capabilities.
#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    endpoint: String,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com".to_owned(),
        }
    }
}

impl OpenAiConfig {
    pub fn new(endpoint: impl Into<String>) -> Result<Self, AiError> {
        let config = Self {
            endpoint: endpoint.into().trim_end_matches('/').to_owned(),
        };
        for path in [
            "/v1/responses",
            "/v1/images/generations",
            "/v1/audio/transcriptions",
            "/v1/audio/speech",
        ] {
            HttpRequest::new(Method::POST, config.url(path)).map_err(invalid_request_error)?;
        }
        Ok(config)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint)
    }

    fn realtime_url(&self, model: &str) -> Result<String, AiError> {
        let origin = self
            .endpoint
            .strip_prefix("https://")
            .map(|origin| format!("wss://{origin}"))
            .or_else(|| {
                self.endpoint
                    .strip_prefix("http://")
                    .map(|origin| format!("ws://{origin}"))
            })
            .ok_or_else(|| {
                ai_error(
                    ErrorKind::InvalidRequest,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "OpenAI Realtime endpoint must use HTTP(S)",
                )
            })?;
        Ok(format!(
            "{origin}/v1/realtime?model={}",
            utf8_percent_encode(model, NON_ALPHANUMERIC)
        ))
    }
}

macro_rules! http_adapter {
    ($name:ident) => {
        #[derive(Clone)]
        pub struct $name {
            config: OpenAiConfig,
            transport: Arc<dyn HttpTransport>,
        }

        impl $name {
            #[must_use]
            pub fn new(config: OpenAiConfig, transport: Arc<dyn HttpTransport>) -> Self {
                Self { config, transport }
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("config", &self.config)
                    .field("transport", &self.transport)
                    .finish()
            }
        }
    };
}

http_adapter!(OpenAiResponsesAdapter);
http_adapter!(OpenAiImageAdapter);
http_adapter!(OpenAiTranscriptionAdapter);
http_adapter!(OpenAiSpeechAdapter);

/// Minimal JSON WebSocket used by the `OpenAI` Realtime adapter.
#[async_trait]
pub trait RealtimeJsonSocket: fmt::Debug + Send {
    async fn send_json(&mut self, value: Value) -> Result<(), AiError>;
    async fn next_json(&mut self) -> Result<Option<Value>, AiError>;
    async fn close(&mut self) -> Result<(), AiError>;
}

/// Injectable Realtime WebSocket connect seam.
pub trait RealtimeDialer: fmt::Debug + Send + Sync {
    fn connect(
        &self,
        url: String,
        credential: SecretValue,
        abort: AbortSignal,
    ) -> AdapterFuture<Result<Box<dyn RealtimeJsonSocket>, AiError>>;
}

/// Rustls-backed production `OpenAI` Realtime WebSocket dialer.
#[derive(Clone, Copy, Debug)]
pub struct TokioRealtimeDialer {
    connect_timeout: Duration,
}

impl TokioRealtimeDialer {
    pub fn with_connect_timeout(connect_timeout: Duration) -> Result<Self, AiError> {
        if connect_timeout.is_zero() {
            return Err(ai_error(
                ErrorKind::InvalidRequest,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "OpenAI Realtime connect timeout must be nonzero",
            ));
        }
        Ok(Self { connect_timeout })
    }
}

impl Default for TokioRealtimeDialer {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_REALTIME_CONNECT_TIMEOUT,
        }
    }
}

impl RealtimeDialer for TokioRealtimeDialer {
    fn connect(
        &self,
        url: String,
        credential: SecretValue,
        abort: AbortSignal,
    ) -> AdapterFuture<Result<Box<dyn RealtimeJsonSocket>, AiError>> {
        let connect_timeout = self.connect_timeout;
        Box::pin(async move {
            let mut request = url.into_client_request().map_err(|_| {
                ai_error(
                    ErrorKind::InvalidRequest,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "OpenAI Realtime URL cannot form a WebSocket request",
                )
            })?;
            request.headers_mut().insert(
                http::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", credential.expose())).map_err(
                    |_| {
                        ai_error(
                            ErrorKind::Authentication,
                            ErrorPhase::Connect,
                            DispatchStatus::NotDispatched,
                            "OpenAI credential cannot form an Authorization header",
                        )
                    },
                )?,
            );
            request
                .headers_mut()
                .insert("openai-beta", HeaderValue::from_static("realtime=v1"));
            let (socket, _) = tokio::select! {
                () = abort.cancelled() => {
                    return Err(ai_error(ErrorKind::Cancelled, ErrorPhase::Connect, DispatchStatus::NotDispatched, "OpenAI Realtime connection was cancelled"));
                }
                result = tokio::time::timeout(connect_timeout, connect_async(request)) => {
                    result.map_err(|_| ai_error(
                        ErrorKind::Timeout,
                        ErrorPhase::Connect,
                        DispatchStatus::Unknown,
                        "OpenAI Realtime WebSocket connection timed out",
                    ))?.map_err(|error| ai_error(
                        ErrorKind::Transport,
                        ErrorPhase::Connect,
                        DispatchStatus::Unknown,
                        format!("OpenAI Realtime WebSocket connection failed: {error}"),
                    ))?
                },
            };
            Ok(Box::new(TokioJsonSocket { socket, abort }) as Box<dyn RealtimeJsonSocket>)
        })
    }
}

struct TokioJsonSocket {
    socket: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    abort: AbortSignal,
}

impl fmt::Debug for TokioJsonSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TokioJsonSocket(..)")
    }
}

#[async_trait]
impl RealtimeJsonSocket for TokioJsonSocket {
    async fn send_json(&mut self, value: Value) -> Result<(), AiError> {
        let text = serde_json::to_string(&value).map_err(invalid_request_error)?;
        tokio::select! {
            () = self.abort.cancelled() => Err(realtime_cancelled()),
            result = self.socket.send(WebSocketMessage::Text(text.into())) => {
                result.map_err(realtime_socket_error)
            }
        }
    }

    async fn next_json(&mut self) -> Result<Option<Value>, AiError> {
        loop {
            let message = tokio::select! {
                () = self.abort.cancelled() => return Err(realtime_cancelled()),
                message = self.socket.next() => message,
            };
            let Some(message) = message else {
                return Ok(None);
            };
            match message.map_err(realtime_socket_error)? {
                WebSocketMessage::Text(text) => {
                    return serde_json::from_str(&text).map(Some).map_err(|_| {
                        ai_error(
                            ErrorKind::Protocol,
                            ErrorPhase::Realtime,
                            DispatchStatus::Dispatched,
                            "OpenAI Realtime emitted malformed JSON",
                        )
                    });
                }
                WebSocketMessage::Close(_) => return Ok(None),
                WebSocketMessage::Ping(_)
                | WebSocketMessage::Pong(_)
                | WebSocketMessage::Frame(_) => {}
                WebSocketMessage::Binary(_) => {
                    return Err(ai_error(
                        ErrorKind::Protocol,
                        ErrorPhase::Realtime,
                        DispatchStatus::Dispatched,
                        "OpenAI Realtime unexpectedly emitted a binary WebSocket message",
                    ));
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), AiError> {
        tokio::select! {
            () = self.abort.cancelled() => Err(realtime_cancelled()),
            result = self.socket.close(None) => result.map_err(realtime_socket_error),
        }
    }
}

fn realtime_cancelled() -> AiError {
    ai_error(
        ErrorKind::Cancelled,
        ErrorPhase::Realtime,
        DispatchStatus::Dispatched,
        "OpenAI Realtime session was cancelled",
    )
}

/// Official `OpenAI` Realtime WebSocket adapter.
#[derive(Clone)]
pub struct OpenAiRealtimeAdapter {
    config: OpenAiConfig,
    dialer: Arc<dyn RealtimeDialer>,
}

impl OpenAiRealtimeAdapter {
    #[must_use]
    pub fn new(config: OpenAiConfig, dialer: Arc<dyn RealtimeDialer>) -> Self {
        Self { config, dialer }
    }

    #[must_use]
    pub fn production(config: OpenAiConfig) -> Self {
        Self::new(config, Arc::new(TokioRealtimeDialer::default()))
    }
}

impl fmt::Debug for OpenAiRealtimeAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiRealtimeAdapter")
            .field("config", &self.config)
            .field("dialer", &self.dialer)
            .finish()
    }
}

impl RealtimeAdapter for OpenAiRealtimeAdapter {
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: RealtimeRequest,
    ) -> AdapterFuture<Result<Prepared<RealtimeAdapterTransport>, AiError>> {
        let snapshot = context.snapshot().clone();
        let url = match self.config.realtime_url(&model) {
            Ok(url) => url,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let dialer = Arc::clone(&self.dialer);
        let (input_type, input_rate) = openai_realtime_format(request.input_format());
        let (output_type, output_rate) = openai_realtime_format(request.output_format());
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let credential = context.credential().ok_or_else(|| {
                        ai_error(
                            ErrorKind::Authentication,
                            ErrorPhase::Connect,
                            DispatchStatus::NotDispatched,
                            "OpenAI credential is unavailable",
                        )
                    })?;
                    let mut socket = dialer
                        .connect(url, credential.secret().clone(), abort.clone())
                        .await?;
                    socket
                        .send_json(json!({
                            "type":"session.update",
                            "session": {
                                "type":"realtime",
                                "output_modalities":["audio"],
                                "instructions":request.instructions(),
                                "audio": {
                                    "input": {
                                        "format": {"type":input_type, "rate":input_rate},
                                        "transcription": {"model":"gpt-4o-mini-transcribe"}
                                    },
                                    "output": {
                                        "format": {"type":output_type, "rate":output_rate},
                                        "voice":request.voice()
                                    }
                                }
                            }
                        }))
                        .await?;
                    Ok(Box::new(OpenAiRealtimeConnection {
                        socket,
                        closed: false,
                        audio_sequences: BTreeMap::new(),
                    }) as RealtimeAdapterTransport)
                })
            }))
        })
    }
}

const fn openai_realtime_format(format: RealtimeAudioFormat) -> (&'static str, u32) {
    match format {
        RealtimeAudioFormat::Pcm16 => ("audio/pcm", 24_000),
    }
}

struct OpenAiRealtimeConnection {
    socket: Box<dyn RealtimeJsonSocket>,
    closed: bool,
    audio_sequences: BTreeMap<String, u32>,
}

impl fmt::Debug for OpenAiRealtimeConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiRealtimeConnection")
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)] // One exhaustive mapping owns the live event state machine.
impl RealtimeConnection for OpenAiRealtimeConnection {
    async fn send(&mut self, command: RealtimeCommand) -> Result<(), AiError> {
        let value = match command {
            RealtimeCommand::AppendAudio { bytes, .. } => {
                json!({"type":"input_audio_buffer.append", "audio":BASE64.encode(bytes)})
            }
            RealtimeCommand::AppendText { text } => json!({
                "type":"conversation.item.create",
                "item": {"type":"message", "role":"user", "content":[{"type":"input_text", "text":text}]}
            }),
            RealtimeCommand::CommitInput { item_id } => {
                json!({"type":"input_audio_buffer.commit", "event_id":item_id})
            }
            RealtimeCommand::RequestResponse => json!({"type":"response.create"}),
            RealtimeCommand::CancelResponse { response_id } => {
                json!({"type":"response.cancel", "response_id":response_id})
            }
            RealtimeCommand::Close => {
                self.closed = true;
                return self.socket.close().await;
            }
        };
        self.socket.send_json(value).await
    }

    async fn next_event(&mut self) -> Result<Option<RealtimeEvent>, AiError> {
        if self.closed {
            return Ok(None);
        }
        loop {
            let Some(value) = self.socket.next_json().await? else {
                self.closed = true;
                return Ok(Some(RealtimeEvent::Closed {
                    reason: RealtimeCloseReason::Provider,
                }));
            };
            let kind = value.get("type").and_then(Value::as_str).ok_or_else(|| {
                ai_error(
                    ErrorKind::Protocol,
                    ErrorPhase::Realtime,
                    DispatchStatus::Dispatched,
                    "OpenAI Realtime event has no type",
                )
            })?;
            let mapped = match kind {
                "session.created" => Some(RealtimeEvent::SessionStarted {
                    session_id: value
                        .get("session")
                        .and_then(|session| session.get("id"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            ai_error(
                                ErrorKind::Protocol,
                                ErrorPhase::Realtime,
                                DispatchStatus::Dispatched,
                                "OpenAI Realtime session has no id",
                            )
                        })?
                        .to_owned(),
                }),
                "input_audio_buffer.speech_started" => Some(RealtimeEvent::InputSpeechStarted {
                    item_id: required_string(&value, "item_id")?,
                }),
                "conversation.item.input_audio_transcription.delta" => {
                    Some(RealtimeEvent::InputTranscriptDelta {
                        item_id: required_string(&value, "item_id")?,
                        text: required_string(&value, "delta")?,
                    })
                }
                "conversation.item.input_audio_transcription.completed" => {
                    Some(RealtimeEvent::InputTranscriptFinished {
                        item_id: required_string(&value, "item_id")?,
                        text: required_string(&value, "transcript")?,
                    })
                }
                "response.output_text.delta"
                | "response.output_audio_transcript.delta"
                | "response.audio_transcript.delta" => Some(RealtimeEvent::OutputTextDelta {
                    response_id: required_string(&value, "response_id")?,
                    text: required_string(&value, "delta")?,
                }),
                "response.output_audio.delta" | "response.audio.delta" => {
                    let encoded = required_string(&value, "delta")?;
                    if encoded.len() > 384 * 1024 {
                        return Err(ai_error(
                            ErrorKind::OutputValidation,
                            ErrorPhase::Realtime,
                            DispatchStatus::Dispatched,
                            "OpenAI Realtime audio delta exceeds its encoded bound",
                        ));
                    }
                    let bytes = BASE64.decode(encoded).map_err(|_| {
                        ai_error(
                            ErrorKind::Protocol,
                            ErrorPhase::Realtime,
                            DispatchStatus::Dispatched,
                            "OpenAI Realtime audio delta has invalid base64",
                        )
                    })?;
                    let response_id = required_string(&value, "response_id")?;
                    let sequence = self.audio_sequences.entry(response_id.clone()).or_insert(0);
                    *sequence = sequence.saturating_add(1);
                    Some(RealtimeEvent::OutputAudioChunk {
                        response_id,
                        sequence: *sequence,
                        bytes,
                    })
                }
                "response.output_audio.done" | "response.audio.done" => {
                    if let Some(response_id) = value.get("response_id").and_then(Value::as_str) {
                        self.audio_sequences.remove(response_id);
                    }
                    None
                }
                "error" => Some(RealtimeEvent::RecoverableError {
                    error: ai_error(
                        ErrorKind::Server,
                        ErrorPhase::Realtime,
                        DispatchStatus::Dispatched,
                        "OpenAI Realtime reported a recoverable error",
                    ),
                }),
                "session.updated"
                | "response.created"
                | "response.done"
                | "response.output_item.added"
                | "response.output_item.done"
                | "response.content_part.added"
                | "response.content_part.done"
                | "response.output_text.done"
                | "response.output_audio_transcript.done"
                | "response.audio_transcript.done"
                | "input_audio_buffer.committed"
                | "input_audio_buffer.speech_stopped"
                | "conversation.item.created"
                | "rate_limits.updated" => None,
                _ => {
                    return Err(ai_error(
                        ErrorKind::Protocol,
                        ErrorPhase::Realtime,
                        DispatchStatus::Dispatched,
                        format!("unsupported OpenAI Realtime event `{kind}`"),
                    ));
                }
            };
            if mapped.is_some() {
                return Ok(mapped);
            }
        }
    }

    async fn close(&mut self) -> Result<(), AiError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.socket.close().await
    }
}

impl LanguageAdapter for OpenAiResponsesAdapter {
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, AiError>> {
        if let Err(error) = validate_responses_request(&request) {
            return Box::pin(async move { Err(error) });
        }
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let body =
                        responses_request(&context, &model, &request, true, false, abort.clone())
                            .await?;
                    let outgoing =
                        authorized_json_request(&context, config.url("/v1/responses"), body)?;
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(transport_connect_error)?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure(response.status, response.body).await);
                    }
                    Ok(translate_responses(decode_sse(
                        response.body,
                        SseTermination::Eof,
                    )))
                })
            }))
        })
    }

    fn prepare_deferred(
        &self,
        context: PrepareContext,
        model: String,
        request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<DeferredLanguageAdapterHandle>, AiError>> {
        if let Err(error) = validate_responses_request(&request) {
            return Box::pin(async move { Err(error) });
        }
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot.clone(), move |abort| {
                Box::pin(async move {
                    let body =
                        responses_request(&context, &model, &request, false, true, abort.clone())
                            .await?;
                    let outgoing =
                        authorized_json_request(&context, config.url("/v1/responses"), body)?;
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(|error| {
                            deferred_transport_error(error, ErrorPhase::DeferredSubmit)
                        })?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure_at(
                            response.status,
                            response.body,
                            ErrorPhase::DeferredSubmit,
                        )
                        .await);
                    }
                    let value =
                        collect_json_control(response.body, ErrorPhase::DeferredSubmit).await?;
                    let (operation_id, status) =
                        deferred_response_identity(&value, None, ErrorPhase::DeferredSubmit)?;
                    let checkpoint = DeferredLanguageCheckpoint::new(
                        snapshot,
                        operation_id,
                        status,
                        Some(ResponsesParser::default().provider_state()),
                    )
                    .map_err(|error| {
                        deferred_checkpoint_error(ErrorPhase::DeferredSubmit, error)
                    })?;
                    Ok(Box::new(OpenAiDeferredOperation {
                        context,
                        config,
                        transport,
                        checkpoint: Arc::new(Mutex::new(checkpoint)),
                    }) as DeferredLanguageAdapterHandle)
                })
            }))
        })
    }

    fn restore_deferred(
        &self,
        context: PrepareContext,
        checkpoint: DeferredLanguageCheckpoint,
    ) -> AdapterFuture<Result<DeferredLanguageAdapterHandle, AiError>> {
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            ResponsesParser::from_provider_state(checkpoint.provider_state())?;
            Ok(Box::new(OpenAiDeferredOperation {
                context,
                config,
                transport,
                checkpoint: Arc::new(Mutex::new(checkpoint)),
            }) as DeferredLanguageAdapterHandle)
        })
    }
}

fn validate_responses_request(request: &LanguageRequest) -> Result<(), AiError> {
    if request.settings().seed().is_some() || !request.settings().stop().is_empty() {
        return Err(ai_error(
            ErrorKind::Unsupported,
            ErrorPhase::Prepare,
            DispatchStatus::NotStarted,
            "OpenAI Responses does not support seed or stop controls",
        ));
    }
    if request
        .hosted_tools()
        .iter()
        .any(|tool| matches!(tool, HostedTool::WebSearch { max_uses: Some(_) }))
    {
        return Err(ai_error(
            ErrorKind::Unsupported,
            ErrorPhase::Prepare,
            DispatchStatus::NotStarted,
            "OpenAI Responses cannot enforce a client-specified hosted-tool use count",
        ));
    }
    Ok(())
}

struct OpenAiDeferredOperation {
    context: PrepareContext,
    config: OpenAiConfig,
    transport: Arc<dyn HttpTransport>,
    checkpoint: Arc<Mutex<DeferredLanguageCheckpoint>>,
}

impl fmt::Debug for OpenAiDeferredOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiDeferredOperation")
            .field("config", &self.config)
            .field("checkpoint", &self.checkpoint())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl DeferredLanguageOperation for OpenAiDeferredOperation {
    fn checkpoint(&self) -> DeferredLanguageCheckpoint {
        self.checkpoint
            .lock()
            .expect("deferred checkpoint lock")
            .clone()
    }

    async fn poll(&mut self, abort: AbortSignal) -> Result<DeferredStatus, AiError> {
        let operation_id = self.checkpoint().operation_id().to_owned();
        let url = self
            .config
            .url(&format!("/v1/responses/{}", encoded_path(&operation_id)));
        let outgoing = authorized_control_request(&self.context, Method::GET, url)?;
        let response = self
            .transport
            .execute(outgoing, abort.cancellation_token())
            .await
            .map_err(|error| deferred_transport_error(error, ErrorPhase::DeferredPoll))?;
        if !(200..300).contains(&response.status) {
            return Err(
                http_failure_at(response.status, response.body, ErrorPhase::DeferredPoll).await,
            );
        }
        let value = collect_json_control(response.body, ErrorPhase::DeferredPoll).await?;
        let (_, status) =
            deferred_response_identity(&value, Some(&operation_id), ErrorPhase::DeferredPoll)?;
        self.checkpoint
            .lock()
            .expect("deferred checkpoint lock")
            .observe_status(status)
            .map_err(|error| deferred_checkpoint_error(ErrorPhase::DeferredPoll, error))?;
        Ok(status)
    }

    async fn resume(
        &mut self,
        abort: AbortSignal,
    ) -> Result<DeferredLanguageAdapterStream, AiError> {
        let checkpoint = self.checkpoint();
        if checkpoint.status().is_terminal() {
            return Err(ai_error(
                ErrorKind::InvalidRequest,
                ErrorPhase::DeferredPoll,
                DispatchStatus::NotStarted,
                "terminal deferred response cannot open another stream",
            ));
        }
        let operation_id = checkpoint.operation_id().to_owned();
        let mut url = self.config.url(&format!(
            "/v1/responses/{}?stream=true",
            encoded_path(&operation_id)
        ));
        if let Some(sequence) = checkpoint.sequence_number() {
            write!(&mut url, "&starting_after={sequence}")
                .expect("writing to a String cannot fail");
        }
        let outgoing = authorized_control_request(&self.context, Method::GET, url)?;
        let response = self
            .transport
            .execute(outgoing, abort.cancellation_token())
            .await
            .map_err(|error| deferred_transport_error(error, ErrorPhase::DeferredPoll))?;
        if !(200..300).contains(&response.status) {
            return Err(
                http_failure_at(response.status, response.body, ErrorPhase::DeferredPoll).await,
            );
        }
        let parser = ResponsesParser::from_provider_state(checkpoint.provider_state())?;
        Ok(translate_deferred_responses(
            decode_sse(response.body, SseTermination::Eof),
            parser,
            Arc::clone(&self.checkpoint),
        ))
    }

    async fn cancel(&mut self, abort: AbortSignal) -> Result<DeferredStatus, AiError> {
        let operation_id = self.checkpoint().operation_id().to_owned();
        let url = self.config.url(&format!(
            "/v1/responses/{}/cancel",
            encoded_path(&operation_id)
        ));
        let outgoing = authorized_control_request(&self.context, Method::POST, url)?;
        let response = self
            .transport
            .execute(outgoing, abort.cancellation_token())
            .await
            .map_err(|error| deferred_transport_error(error, ErrorPhase::DeferredCancel))?;
        if !(200..300).contains(&response.status) {
            return Err(http_failure_at(
                response.status,
                response.body,
                ErrorPhase::DeferredCancel,
            )
            .await);
        }
        let value = collect_json_control(response.body, ErrorPhase::DeferredCancel).await?;
        let (_, status) =
            deferred_response_identity(&value, Some(&operation_id), ErrorPhase::DeferredCancel)?;
        if status != DeferredStatus::Cancelled {
            return Err(ai_error(
                ErrorKind::Protocol,
                ErrorPhase::DeferredCancel,
                DispatchStatus::Dispatched,
                "OpenAI cancel response did not become cancelled",
            ));
        }
        self.checkpoint
            .lock()
            .expect("deferred checkpoint lock")
            .observe_status(status)
            .map_err(|error| deferred_checkpoint_error(ErrorPhase::DeferredCancel, error))?;
        Ok(status)
    }
}

fn encoded_path(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT_ENCODE_SET).to_string()
}

async fn collect_json_control(body: ByteStream, phase: ErrorPhase) -> Result<Value, AiError> {
    let bytes = collect_body(body, MAX_DEFERRED_CONTROL_BODY_BYTES)
        .await
        .map_err(|error| {
            ai_error(
                ErrorKind::Transport,
                phase,
                DispatchStatus::Dispatched,
                format!("OpenAI deferred response body failed: {error}"),
            )
        })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        ai_error(
            ErrorKind::Protocol,
            phase,
            DispatchStatus::Dispatched,
            "OpenAI deferred response body is malformed JSON",
        )
    })
}

fn deferred_response_identity(
    value: &Value,
    expected_id: Option<&str>,
    phase: ErrorPhase,
) -> Result<(String, DeferredStatus), AiError> {
    let operation_id = value.get("id").and_then(Value::as_str).ok_or_else(|| {
        ai_error(
            ErrorKind::Protocol,
            phase,
            DispatchStatus::Dispatched,
            "OpenAI deferred response has no id",
        )
    })?;
    if expected_id.is_some_and(|expected| expected != operation_id) {
        return Err(ai_error(
            ErrorKind::Protocol,
            phase,
            DispatchStatus::Dispatched,
            "OpenAI deferred response id changed",
        ));
    }
    let status_value = value.get("status").and_then(Value::as_str).ok_or_else(|| {
        ai_error(
            ErrorKind::Protocol,
            phase,
            DispatchStatus::Dispatched,
            "OpenAI deferred response has no status",
        )
    })?;
    let status = deferred_status_at(status_value, phase)?;
    Ok((operation_id.to_owned(), status))
}

fn deferred_status(value: &str) -> Result<DeferredStatus, AiError> {
    deferred_status_at(value, ErrorPhase::DeferredPoll)
}

fn deferred_status_at(value: &str, phase: ErrorPhase) -> Result<DeferredStatus, AiError> {
    match value {
        "queued" => Ok(DeferredStatus::Queued),
        "in_progress" => Ok(DeferredStatus::InProgress),
        "completed" => Ok(DeferredStatus::Completed),
        "failed" | "incomplete" => Ok(DeferredStatus::Failed),
        "cancelled" => Ok(DeferredStatus::Cancelled),
        _ => Err(ai_error(
            ErrorKind::Protocol,
            phase,
            DispatchStatus::Dispatched,
            "OpenAI deferred response has an unknown status",
        )),
    }
}

#[allow(clippy::needless_pass_by_value)] // Directly usable with Result::map_err.
fn deferred_checkpoint_error(
    phase: ErrorPhase,
    error: rsi_ai_provider::ProviderSdkError,
) -> AiError {
    ai_error(
        ErrorKind::Protocol,
        phase,
        DispatchStatus::Dispatched,
        error.to_string(),
    )
}

#[allow(clippy::too_many_lines)] // One role-exhaustive request mapping owns provider defaults.
async fn responses_request(
    context: &PrepareContext,
    model: &str,
    request: &LanguageRequest,
    stream: bool,
    background: bool,
    abort: AbortSignal,
) -> Result<Vec<u8>, AiError> {
    let mut input = Vec::new();
    for message in request.messages() {
        match message.role() {
            MessageRole::System | MessageRole::Developer | MessageRole::User => {
                let role = match message.role() {
                    MessageRole::System => "system",
                    MessageRole::Developer => "developer",
                    MessageRole::User => "user",
                    MessageRole::Assistant | MessageRole::Tool => unreachable!(),
                };
                let mut wire_blocks = Vec::new();
                for block in message.content() {
                    match block {
                        MessageContent::Text { text } => {
                            wire_blocks.push(json!({"type":"input_text", "text":text}));
                        }
                        MessageContent::Image(media) => {
                            let bytes = context.resolve_media(media, abort.clone()).await?;
                            wire_blocks.push(json!({
                                "type":"input_image",
                                "image_url":format!("data:{};base64,{}", media.mime_type(), BASE64.encode(bytes))
                            }));
                        }
                        MessageContent::Audio(media) => {
                            let bytes = context.resolve_media(media, abort.clone()).await?;
                            wire_blocks.push(json!({
                                "type":"input_audio",
                                "input_audio": {
                                    "data": BASE64.encode(bytes),
                                    "format": audio_format(media.mime_type())?,
                                }
                            }));
                        }
                        _ => unreachable!("message role validation"),
                    }
                }
                input.push(json!({"type":"message", "role":role, "content":wire_blocks}));
            }
            MessageRole::Assistant => {
                let mut text = Vec::new();
                for block in message.content() {
                    match block {
                        MessageContent::Text { text: value } => text.push(json!({
                            "type":"output_text", "text":value, "annotations":[]
                        })),
                        MessageContent::ToolCall(call) => {
                            push_responses_assistant_text(&mut input, &mut text);
                            input.push(json!({
                                "type":"function_call",
                                "call_id":call.id,
                                "name":call.name,
                                "arguments":call.arguments,
                            }));
                        }
                        MessageContent::Reasoning { .. } => {
                            return Err(ai_error(
                                ErrorKind::Unsupported,
                                ErrorPhase::Prepare,
                                DispatchStatus::NotStarted,
                                "OpenAI reasoning history requires a bounded Responses replay extension",
                            ));
                        }
                        MessageContent::Image(_) | MessageContent::Audio(_) => {
                            return Err(ai_error(
                                ErrorKind::Unsupported,
                                ErrorPhase::Prepare,
                                DispatchStatus::NotStarted,
                                "OpenAI Responses assistant history cannot contain media",
                            ));
                        }
                        MessageContent::ToolResult { .. } => {
                            unreachable!("message role validation")
                        }
                    }
                }
                push_responses_assistant_text(&mut input, &mut text);
            }
            MessageRole::Tool => {
                let MessageContent::ToolResult {
                    call_id, content, ..
                } = &message.content()[0]
                else {
                    unreachable!("message role validation")
                };
                let mut output = String::new();
                for block in content {
                    match block {
                        MessageContent::Text { text } => output.push_str(text),
                        MessageContent::Image(_) | MessageContent::Audio(_) => {
                            return Err(ai_error(
                                ErrorKind::Unsupported,
                                ErrorPhase::Prepare,
                                DispatchStatus::NotStarted,
                                "OpenAI function outputs require text in v1",
                            ));
                        }
                        _ => unreachable!("tool result validation"),
                    }
                }
                input.push(json!({
                    "type":"function_call_output", "call_id":call_id, "output":output
                }));
            }
        }
    }

    let mut body = Map::from_iter([
        ("model".to_owned(), Value::String(model.to_owned())),
        ("input".to_owned(), Value::Array(input)),
        ("stream".to_owned(), Value::Bool(stream)),
    ]);
    if background {
        body.insert("background".to_owned(), Value::Bool(true));
    }
    let settings = request.settings();
    if let Some(value) = settings.max_output_tokens() {
        body.insert("max_output_tokens".to_owned(), Value::from(value));
    }
    if let Some(value) = settings.temperature() {
        body.insert("temperature".to_owned(), json!(value));
    }
    if let Some(value) = settings.top_p() {
        body.insert("top_p".to_owned(), json!(value));
    }
    if let Some(value) = settings.reasoning_effort() {
        body.insert("reasoning".to_owned(), json!({"effort":value}));
    }
    if !request.tools().is_empty() {
        body.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools()
                    .iter()
                    .map(|tool| {
                        json!({
                            "type":"function",
                            "name":tool.name(),
                            "description":tool.description(),
                            "parameters":tool.input_schema(),
                            "strict":true,
                        })
                    })
                    .collect(),
            ),
        );
        body.insert(
            "tool_choice".to_owned(),
            responses_tool_choice(request.tool_choice()),
        );
    }
    for hosted in request.hosted_tools() {
        if matches!(hosted, HostedTool::WebSearch { max_uses: None }) {
            body.entry("tools".to_owned())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .expect("tools is an array")
                .push(json!({"type":"web_search"}));
        }
    }
    if let ResponseFormat::JsonSchema {
        name,
        description,
        schema,
        strict,
    } = request.response_format()
    {
        body.insert(
            "text".to_owned(),
            json!({"format": {
                "type":"json_schema", "name":name, "description":description,
                "schema":schema, "strict":strict
            }}),
        );
    }
    for extension in request.extensions() {
        if extension.namespace == "openai.responses.replay" && extension.version == 0 {
            let response_id = extension
                .value
                .get("response_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ai_error(
                        ErrorKind::InvalidRequest,
                        ErrorPhase::Prepare,
                        DispatchStatus::NotStarted,
                        "OpenAI replay extension has no response_id",
                    )
                })?;
            body.insert(
                "previous_response_id".to_owned(),
                Value::String(response_id.to_owned()),
            );
        } else {
            return Err(ai_error(
                ErrorKind::Unsupported,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "OpenAI Responses received an unsupported extension",
            ));
        }
    }
    serde_json::to_vec(&body).map_err(invalid_request_error)
}

fn push_responses_assistant_text(input: &mut Vec<Value>, text: &mut Vec<Value>) {
    if !text.is_empty() {
        input.push(json!({
            "type":"message",
            "role":"assistant",
            "content":std::mem::take(text),
        }));
    }
}

fn responses_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".to_owned()),
        ToolChoice::None => Value::String("none".to_owned()),
        ToolChoice::Required => Value::String("required".to_owned()),
        ToolChoice::Specific(name) => json!({"type":"function", "name":name}),
    }
}

#[derive(Clone, Debug)]
struct OpenBlock {
    index: u32,
    kind: OpenBlockKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OpenBlockKind {
    Text,
    Reasoning,
    Tool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredOpenBlock {
    key: String,
    index: u32,
    kind: OpenBlockKind,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredResponsesParser {
    next_index: u32,
    open: Vec<StoredOpenBlock>,
    saw_tool: bool,
}

#[derive(Debug, Default)]
struct ResponsesParser {
    next_index: u32,
    open: BTreeMap<String, OpenBlock>,
    saw_tool: bool,
}

impl ResponsesParser {
    fn from_provider_state(state: Option<&ProviderExtension>) -> Result<Self, AiError> {
        let Some(state) = state else {
            return Ok(Self::default());
        };
        if state.namespace != "openai.responses.deferred_parser" || state.version != 0 {
            return Err(ai_error(
                ErrorKind::Protocol,
                ErrorPhase::DeferredPoll,
                DispatchStatus::NotStarted,
                "OpenAI deferred checkpoint has incompatible parser state",
            ));
        }
        let stored: StoredResponsesParser =
            serde_json::from_value(state.value.clone()).map_err(|_| {
                ai_error(
                    ErrorKind::Protocol,
                    ErrorPhase::DeferredPoll,
                    DispatchStatus::NotStarted,
                    "OpenAI deferred checkpoint parser state is malformed",
                )
            })?;
        if usize::try_from(stored.next_index)
            .map_or(true, |value| value > rsi_ai_protocol::MAX_CONTENT_BLOCKS)
            || stored.open.len() > rsi_ai_protocol::MAX_CONTENT_BLOCKS
        {
            return Err(ai_error(
                ErrorKind::Protocol,
                ErrorPhase::DeferredPoll,
                DispatchStatus::NotStarted,
                "OpenAI deferred checkpoint parser state exceeds content bounds",
            ));
        }
        let mut open = BTreeMap::new();
        for block in stored.open {
            validate_provider_item_id(&block.key)?;
            if block.index >= stored.next_index
                || open
                    .insert(
                        block.key,
                        OpenBlock {
                            index: block.index,
                            kind: block.kind,
                        },
                    )
                    .is_some()
            {
                return Err(ai_error(
                    ErrorKind::Protocol,
                    ErrorPhase::DeferredPoll,
                    DispatchStatus::NotStarted,
                    "OpenAI deferred checkpoint parser state is inconsistent",
                ));
            }
        }
        Ok(Self {
            next_index: stored.next_index,
            open,
            saw_tool: stored.saw_tool,
        })
    }

    fn provider_state(&self) -> ProviderExtension {
        let stored = StoredResponsesParser {
            next_index: self.next_index,
            open: self
                .open
                .iter()
                .map(|(key, block)| StoredOpenBlock {
                    key: key.clone(),
                    index: block.index,
                    kind: block.kind,
                })
                .collect(),
            saw_tool: self.saw_tool,
        };
        ProviderExtension {
            namespace: "openai.responses.deferred_parser".to_owned(),
            version: 0,
            value: serde_json::to_value(stored).expect("parser state is serializable"),
        }
    }

    #[allow(clippy::too_many_lines)] // One exhaustive transition owns the Responses stream grammar.
    fn apply(&mut self, event: &Value) -> Result<Vec<LanguageEvent>, AiError> {
        let kind = event.get("type").and_then(Value::as_str).ok_or_else(|| {
            ai_error(
                ErrorKind::Protocol,
                ErrorPhase::Stream,
                DispatchStatus::Dispatched,
                "OpenAI Responses event has no type",
            )
        })?;
        let mut output = Vec::new();
        match kind {
            "response.output_text.delta" | "response.reasoning_summary_text.delta" => {
                let item_id =
                    required_response_string(event, "item_id", "OpenAI text delta has no item_id")?;
                validate_provider_item_id(item_id)?;
                let block_kind = if kind.contains("reasoning") {
                    OpenBlockKind::Reasoning
                } else {
                    OpenBlockKind::Text
                };
                let key = format!(
                    "{item_id}:{block_kind:?}:{}",
                    event
                        .get("content_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                );
                if !self.open.contains_key(&key) {
                    if usize::try_from(self.next_index)
                        .map_or(true, |value| value >= rsi_ai_protocol::MAX_CONTENT_BLOCKS)
                    {
                        return Err(ai_error(
                            ErrorKind::OutputValidation,
                            ErrorPhase::Stream,
                            DispatchStatus::Dispatched,
                            "OpenAI Responses emitted too many content blocks",
                        ));
                    }
                    let index = self.next_index;
                    self.next_index = self.next_index.saturating_add(1);
                    let content = if block_kind == OpenBlockKind::Reasoning {
                        ContentStart::Reasoning
                    } else {
                        ContentStart::Text
                    };
                    output.push(LanguageEvent::ContentStarted { index, content });
                    self.open.insert(
                        key.clone(),
                        OpenBlock {
                            index,
                            kind: block_kind,
                        },
                    );
                }
                let block = self.open.get(&key).expect("block inserted");
                let delta =
                    required_response_string(event, "delta", "OpenAI text delta has no text")?;
                output.push(LanguageEvent::ContentDelta {
                    index: block.index,
                    delta: if block.kind == OpenBlockKind::Reasoning {
                        ContentDelta::Reasoning(delta.to_owned())
                    } else {
                        ContentDelta::Text(delta.to_owned())
                    },
                });
            }
            "response.output_item.added" => {
                let item = event.get("item").unwrap_or(&Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let item_id = required_response_string(
                        item,
                        "id",
                        "OpenAI function call has no item id",
                    )?;
                    validate_provider_item_id(item_id)?;
                    let call_id = required_response_string(
                        item,
                        "call_id",
                        "OpenAI function call has no call_id",
                    )?;
                    let name =
                        required_response_string(item, "name", "OpenAI function call has no name")?;
                    if usize::try_from(self.next_index)
                        .map_or(true, |value| value >= rsi_ai_protocol::MAX_CONTENT_BLOCKS)
                    {
                        return Err(ai_error(
                            ErrorKind::OutputValidation,
                            ErrorPhase::Stream,
                            DispatchStatus::Dispatched,
                            "OpenAI Responses emitted too many content blocks",
                        ));
                    }
                    let index = self.next_index;
                    self.next_index = self.next_index.saturating_add(1);
                    self.saw_tool = true;
                    output.push(LanguageEvent::ContentStarted {
                        index,
                        content: ContentStart::ToolCall {
                            id: call_id.to_owned(),
                            name: name.to_owned(),
                        },
                    });
                    if self
                        .open
                        .insert(
                            item_id.to_owned(),
                            OpenBlock {
                                index,
                                kind: OpenBlockKind::Tool,
                            },
                        )
                        .is_some()
                    {
                        return Err(ai_error(
                            ErrorKind::Protocol,
                            ErrorPhase::Stream,
                            DispatchStatus::Dispatched,
                            "OpenAI Responses repeated a function item id",
                        ));
                    }
                    if let Some(arguments) = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        output.push(LanguageEvent::ContentDelta {
                            index,
                            delta: ContentDelta::ToolArguments(arguments.to_owned()),
                        });
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = required_response_string(
                    event,
                    "item_id",
                    "OpenAI function delta has no item_id",
                )?;
                validate_provider_item_id(item_id)?;
                let block = self.open.get(item_id).ok_or_else(|| {
                    ai_error(
                        ErrorKind::Protocol,
                        ErrorPhase::Stream,
                        DispatchStatus::Dispatched,
                        "OpenAI function delta arrived before its item",
                    )
                })?;
                let delta = required_response_string(
                    event,
                    "delta",
                    "OpenAI function delta has no arguments",
                )?;
                output.push(LanguageEvent::ContentDelta {
                    index: block.index,
                    delta: ContentDelta::ToolArguments(delta.to_owned()),
                });
            }
            "response.completed" => {
                let response = event.get("response").unwrap_or(&Value::Null);
                let reason = if self.saw_tool {
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
                };
                self.finish_response(response, reason, &mut output);
            }
            "response.output_text.annotation.added" => {
                let item_id =
                    required_response_string(event, "item_id", "OpenAI citation has no item_id")?;
                validate_provider_item_id(item_id)?;
                let annotation = event.get("annotation").unwrap_or(&Value::Null);
                if annotation.get("type").and_then(Value::as_str) == Some("url_citation") {
                    let url = required_response_string(
                        annotation,
                        "url",
                        "OpenAI URL citation has no URL",
                    )?;
                    let annotation_index = event
                        .get("annotation_index")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| {
                            ai_error(
                                ErrorKind::Protocol,
                                ErrorPhase::Stream,
                                DispatchStatus::Dispatched,
                                "OpenAI citation has no annotation_index",
                            )
                        })?;
                    output.push(LanguageEvent::Source {
                        source: Source {
                            id: citation_source_id(item_id, annotation_index),
                            title: annotation
                                .get("title")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            url: Some(url.to_owned()),
                        },
                    });
                }
            }
            "response.incomplete" => {
                let response = event.get("response").unwrap_or(&Value::Null);
                if response
                    .get("incomplete_details")
                    .and_then(|details| details.get("reason"))
                    .and_then(Value::as_str)
                    == Some("max_output_tokens")
                {
                    self.finish_response(response, FinishReason::MaxTokens, &mut output);
                } else {
                    output.push(language_failed(ai_error(
                        ErrorKind::Server,
                        ErrorPhase::Stream,
                        DispatchStatus::Dispatched,
                        "OpenAI Responses did not complete successfully",
                    )));
                }
            }
            "response.failed" | "error" => {
                output.push(language_failed(ai_error(
                    ErrorKind::Server,
                    ErrorPhase::Stream,
                    DispatchStatus::Dispatched,
                    "OpenAI Responses did not complete successfully",
                )));
            }
            "response.created"
            | "response.in_progress"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.done"
            | "response.function_call_arguments.done"
            | "response.web_search_call.in_progress"
            | "response.web_search_call.searching"
            | "response.web_search_call.completed" => {}
            _ => {
                return Err(ai_error(
                    ErrorKind::Protocol,
                    ErrorPhase::Stream,
                    DispatchStatus::Dispatched,
                    format!("unsupported OpenAI Responses event `{kind}`"),
                ));
            }
        }
        Ok(output)
    }

    fn finish_response(
        &mut self,
        response: &Value,
        reason: FinishReason,
        output: &mut Vec<LanguageEvent>,
    ) {
        for block in std::mem::take(&mut self.open).into_values() {
            output.push(LanguageEvent::ContentFinished { index: block.index });
        }
        if let Some(usage) = response.get("usage") {
            output.push(LanguageEvent::Usage {
                usage: responses_usage(usage),
            });
        }
        let replay = response
            .get("id")
            .and_then(Value::as_str)
            .map(|id| ProviderExtension {
                namespace: "openai.responses.replay".to_owned(),
                version: 0,
                value: json!({"response_id":id}),
            });
        output.push(LanguageEvent::Finished { reason, replay });
    }
}

fn required_response_string<'a>(
    value: &'a Value,
    field: &str,
    summary: &'static str,
) -> Result<&'a str, AiError> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        ai_error(
            ErrorKind::Protocol,
            ErrorPhase::Stream,
            DispatchStatus::Dispatched,
            summary,
        )
    })
}

fn validate_provider_item_id(value: &str) -> Result<(), AiError> {
    if rsi_ai_protocol::validate_identifier("OpenAI Responses item id", value).is_err() {
        return Err(ai_error(
            ErrorKind::OutputValidation,
            ErrorPhase::Stream,
            DispatchStatus::Dispatched,
            "OpenAI Responses item id is outside provider-state bounds",
        ));
    }
    Ok(())
}

fn citation_source_id(item_id: &str, annotation_index: u64) -> String {
    // FNV-1a is sufficient here: this is a stable, bounded event identifier rather than a
    // security digest. Keeping the raw provider id would exceed MAX_ID_BYTES at its valid limit.
    let hash = item_id
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("openai-source-{hash:016x}-{annotation_index}")
}

fn translate_responses(mut input: rsi_ai_transport::SseStream) -> LanguageAdapterStream {
    Box::pin(stream! {
        let mut parser = ResponsesParser::default();
        while let Some(payload) = input.next().await {
            let payload = match payload {
                Ok(payload) => payload,
                Err(error) => {
                    yield Ok(language_failed(transport_stream_error(error)));
                    return;
                }
            };
            let Ok(event) = serde_json::from_str::<Value>(&payload) else {
                yield Ok(language_failed(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "OpenAI Responses emitted malformed JSON")));
                return;
            };
            let events = match parser.apply(&event) {
                Ok(events) => events,
                Err(error) => {
                    yield Ok(language_failed(error));
                    return;
                }
            };
            let terminal = events.iter().any(is_language_terminal_event);
            for event in events {
                yield Ok(event);
            }
            if terminal {
                return;
            }
        }
        yield Ok(language_failed(ai_error(
            ErrorKind::Protocol,
            ErrorPhase::Stream,
            DispatchStatus::Dispatched,
            "OpenAI Responses stream ended without response.completed",
        )));
    })
}

fn translate_deferred_responses(
    mut input: rsi_ai_transport::SseStream,
    mut parser: ResponsesParser,
    checkpoint: Arc<Mutex<DeferredLanguageCheckpoint>>,
) -> DeferredLanguageAdapterStream {
    Box::pin(try_stream! {
        while let Some(payload) = input.next().await {
            let payload = payload.map_err(transport_stream_error)?;
            let event: Value = serde_json::from_str(&payload).map_err(|_| {
                ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "OpenAI Responses emitted malformed JSON")
            })?;
            let sequence = event.get("sequence_number").and_then(Value::as_u64).ok_or_else(|| {
                ai_error(ErrorKind::Protocol, ErrorPhase::DeferredPoll, DispatchStatus::Dispatched, "OpenAI background stream event has no sequence_number")
            })?;
            let kind = event.get("type").and_then(Value::as_str).ok_or_else(|| {
                ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "OpenAI Responses event has no type")
            })?;
            let events = parser.apply(&event)?;
            let terminal = events.iter().any(is_language_terminal_event);
            let next = advance_deferred_checkpoint(
                &checkpoint,
                kind,
                &event,
                sequence,
                parser.provider_state(),
            )?;
            let batch = DeferredLanguageBatch::new(events, next)
                .map_err(|error| deferred_checkpoint_error(ErrorPhase::DeferredPoll, error))?;
            yield batch;
            if terminal {
                return;
            }
        }
    })
}

fn advance_deferred_checkpoint(
    checkpoint: &Mutex<DeferredLanguageCheckpoint>,
    kind: &str,
    event: &Value,
    sequence: u64,
    provider_state: ProviderExtension,
) -> Result<DeferredLanguageCheckpoint, AiError> {
    let mut current = checkpoint.lock().expect("deferred checkpoint lock");
    let status = deferred_event_status(kind, event, current.status())?;
    let stream_created = current.stream_created() || kind == "response.created";
    current
        .advance(status, stream_created, sequence, Some(provider_state))
        .map_err(|error| deferred_checkpoint_error(ErrorPhase::DeferredPoll, error))?;
    Ok(current.clone())
}

fn deferred_event_status(
    kind: &str,
    event: &Value,
    current: DeferredStatus,
) -> Result<DeferredStatus, AiError> {
    match kind {
        "response.created" => event
            .get("response")
            .and_then(|response| response.get("status"))
            .and_then(Value::as_str)
            .map(deferred_status)
            .transpose()
            .map(|status| status.unwrap_or(DeferredStatus::InProgress)),
        "response.completed" => Ok(DeferredStatus::Completed),
        "response.failed" | "response.incomplete" | "error" => Ok(DeferredStatus::Failed),
        _ if current == DeferredStatus::Queued => Ok(DeferredStatus::InProgress),
        _ => Ok(current),
    }
}

fn is_language_terminal_event(event: &LanguageEvent) -> bool {
    matches!(
        event,
        LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
    )
}

impl ImageAdapter for OpenAiImageAdapter {
    #[allow(clippy::too_many_lines)] // Generation and edit wire forms share one prepared effect.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: ImageRequest,
    ) -> AdapterFuture<Result<Prepared<ImageAdapterStream>, AiError>> {
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let (path, content_type, body) = if request.inputs().is_empty() {
                        (
                            "/v1/images/generations",
                            "application/json".to_owned(),
                            serde_json::to_vec(&json!({
                                "model":model, "prompt":request.prompt(), "n":request.count()
                            }))
                            .map_err(invalid_request_error)?,
                        )
                    } else {
                        let mut parts = vec![
                            ("model".to_owned(), None, None, model.into_bytes()),
                            (
                                "prompt".to_owned(),
                                None,
                                None,
                                request.prompt().as_bytes().to_vec(),
                            ),
                            (
                                "n".to_owned(),
                                None,
                                None,
                                request.count().to_string().into_bytes(),
                            ),
                        ];
                        for (index, media) in request.inputs().iter().enumerate() {
                            let bytes = context.resolve_media(media, abort.clone()).await?;
                            parts.push((
                                "image[]".to_owned(),
                                Some(format!("image-{index}.bin")),
                                Some(media.mime_type().to_owned()),
                                bytes,
                            ));
                        }
                        if let Some(mask) = request.mask() {
                            let bytes = context.resolve_media(mask, abort.clone()).await?;
                            parts.push((
                                "mask".to_owned(),
                                Some("mask.bin".to_owned()),
                                Some(mask.mime_type().to_owned()),
                                bytes,
                            ));
                        }
                        let boundary =
                            format!("rsi-ai-{}", &context.snapshot().request_sha256[..24]);
                        let body = multipart(&boundary, parts)?;
                        (
                            "/v1/images/edits",
                            format!("multipart/form-data; boundary={boundary}"),
                            body,
                        )
                    };
                    let outgoing =
                        authorized_request(&context, config.url(path), &content_type, body)?;
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(transport_connect_error)?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure(response.status, response.body).await);
                    }
                    let body = collect_body(response.body, MAX_JSON_BODY_BYTES)
                        .await
                        .map_err(transport_body_error)?;
                    let response: OpenAiImagesResponse<'_> = serde_json::from_slice(&body)
                        .map_err(|_| {
                            ai_error(
                                ErrorKind::Protocol,
                                ErrorPhase::Assemble,
                                DispatchStatus::Dispatched,
                                "OpenAI Images returned malformed JSON",
                            )
                        })?;
                    let data = response.data;
                    if data.len() != usize::from(request.count()) {
                        return Err(ai_error(
                            ErrorKind::OutputValidation,
                            ErrorPhase::Assemble,
                            DispatchStatus::Dispatched,
                            "OpenAI Images output count differs from the request",
                        ));
                    }
                    let mut events = Vec::new();
                    for (index, item) in data.iter().enumerate() {
                        let bytes = BASE64.decode(item.b64_json).map_err(|_| {
                            ai_error(
                                ErrorKind::Protocol,
                                ErrorPhase::Assemble,
                                DispatchStatus::Dispatched,
                                "OpenAI Images item has invalid base64",
                            )
                        })?;
                        let index = u32::try_from(index).expect("image count is bounded");
                        events.push(ImageEvent::OutputStarted {
                            index,
                            mime_type: "image/png".to_owned(),
                        });
                        for (offset, chunk) in bytes.chunks(OUTPUT_CHUNK_BYTES).enumerate() {
                            events.push(ImageEvent::OutputChunk {
                                index,
                                sequence: u32::try_from(offset + 1).expect("image is bounded"),
                                bytes: chunk.to_vec(),
                            });
                        }
                        events.push(ImageEvent::OutputFinished { index });
                    }
                    events.push(ImageEvent::Finished);
                    Ok(Box::pin(futures_stream::iter(events.into_iter().map(Ok)))
                        as ImageAdapterStream)
                })
            }))
        })
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiImagesResponse<'a> {
    #[serde(borrow)]
    data: Vec<OpenAiImageData<'a>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiImageData<'a> {
    #[serde(borrow)]
    b64_json: &'a str,
}

impl TranscriptionAdapter for OpenAiTranscriptionAdapter {
    #[allow(clippy::too_many_lines)] // Multipart request and timestamp mapping share one effect.
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: TranscriptionRequest,
    ) -> AdapterFuture<Result<Prepared<TranscriptionAdapterStream>, AiError>> {
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let audio = context
                        .resolve_media(request.audio(), abort.clone())
                        .await?;
                    let mut parts = vec![
                        ("model".to_owned(), None, None, model.into_bytes()),
                        (
                            "response_format".to_owned(),
                            None,
                            None,
                            b"verbose_json".to_vec(),
                        ),
                        (
                            "file".to_owned(),
                            Some("audio.bin".to_owned()),
                            Some(request.audio().mime_type().to_owned()),
                            audio,
                        ),
                    ];
                    if let Some(language) = request.language() {
                        parts.push((
                            "language".to_owned(),
                            None,
                            None,
                            language.as_bytes().to_vec(),
                        ));
                    }
                    if let Some(prompt) = request.prompt() {
                        parts.push(("prompt".to_owned(), None, None, prompt.as_bytes().to_vec()));
                    }
                    let boundary = format!("rsi-ai-{}", &context.snapshot().request_sha256[..24]);
                    let body = multipart(&boundary, parts)?;
                    let outgoing = authorized_request(
                        &context,
                        config.url("/v1/audio/transcriptions"),
                        &format!("multipart/form-data; boundary={boundary}"),
                        body,
                    )?;
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(transport_connect_error)?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure(response.status, response.body).await);
                    }
                    let body = collect_body(response.body, MAX_TRANSCRIPTION_BODY_BYTES)
                        .await
                        .map_err(transport_body_error)?;
                    let value: Value = serde_json::from_slice(&body).map_err(|_| {
                        ai_error(
                            ErrorKind::Protocol,
                            ErrorPhase::Assemble,
                            DispatchStatus::Dispatched,
                            "OpenAI transcription returned malformed JSON",
                        )
                    })?;
                    let text = value.get("text").and_then(Value::as_str).ok_or_else(|| {
                        ai_error(
                            ErrorKind::Protocol,
                            ErrorPhase::Assemble,
                            DispatchStatus::Dispatched,
                            "OpenAI transcription has no text",
                        )
                    })?;
                    let mut events = Vec::new();
                    let mut offset = 0;
                    while offset < text.len() {
                        let mut end = offset.saturating_add(64 * 1024).min(text.len());
                        while end > offset && !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        debug_assert!(end > offset, "chunk bound exceeds one UTF-8 scalar");
                        events.push(TranscriptionEvent::TextDelta {
                            text: text[offset..end].to_owned(),
                        });
                        offset = end;
                    }
                    if request.timestamps() {
                        for (index, segment) in value
                            .get("segments")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .enumerate()
                        {
                            let start =
                                seconds_to_ms(segment.get("start").and_then(Value::as_f64))?;
                            let end = seconds_to_ms(segment.get("end").and_then(Value::as_f64))?;
                            let segment_text =
                                segment.get("text").and_then(Value::as_str).ok_or_else(|| {
                                    ai_error(
                                        ErrorKind::Protocol,
                                        ErrorPhase::Assemble,
                                        DispatchStatus::Dispatched,
                                        "transcription segment has no text",
                                    )
                                })?;
                            events.push(TranscriptionEvent::Segment {
                                segment: TranscriptionSegment {
                                    id: u32::try_from(index).map_err(|_| {
                                        ai_error(
                                            ErrorKind::OutputValidation,
                                            ErrorPhase::Assemble,
                                            DispatchStatus::Dispatched,
                                            "too many transcription segments",
                                        )
                                    })?,
                                    start_ms: start,
                                    end_ms: end,
                                    text: segment_text.to_owned(),
                                },
                            });
                        }
                    }
                    events.push(TranscriptionEvent::Finished {
                        language: value
                            .get("language")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    });
                    Ok(Box::pin(futures_stream::iter(events.into_iter().map(Ok)))
                        as TranscriptionAdapterStream)
                })
            }))
        })
    }
}

impl SpeechAdapter for OpenAiSpeechAdapter {
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: SpeechRequest,
    ) -> AdapterFuture<Result<Prepared<SpeechAdapterStream>, AiError>> {
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let format = match request.format() {
                        SpeechFormat::Pcm16 => "pcm",
                        SpeechFormat::Wav => "wav",
                        SpeechFormat::Mp3 => "mp3",
                    };
                    let mut payload = json!({
                        "model":model,
                        "input":request.text(),
                        "voice":request.voice(),
                        "response_format":format,
                    });
                    if let Some(speed) = request.speed() {
                        payload
                            .as_object_mut()
                            .expect("speech payload is an object")
                            .insert("speed".to_owned(), json!(speed));
                    }
                    let body = serde_json::to_vec(&payload).map_err(invalid_request_error)?;
                    let outgoing =
                        authorized_json_request(&context, config.url("/v1/audio/speech"), body)?;
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(transport_connect_error)?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure(response.status, response.body).await);
                    }
                    Ok(speech_stream(response.body, request.format()))
                })
            }))
        })
    }
}

fn speech_stream(mut body: ByteStream, format: SpeechFormat) -> SpeechAdapterStream {
    Box::pin(stream! {
        let mime_type = match format {
            SpeechFormat::Pcm16 => "audio/pcm",
            SpeechFormat::Wav => "audio/wav",
            SpeechFormat::Mp3 => "audio/mpeg",
        };
        yield Ok(SpeechEvent::OutputStarted { mime_type: mime_type.to_owned() });
        let mut sequence = 1_u32;
        let mut saw_bytes = false;
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(transport_body_error(error));
                    return;
                }
            };
            for piece in chunk.chunks(OUTPUT_CHUNK_BYTES) {
                if piece.is_empty() { continue; }
                saw_bytes = true;
                yield Ok(SpeechEvent::AudioChunk { sequence, bytes: piece.to_vec() });
                sequence = sequence.saturating_add(1);
            }
        }
        if !saw_bytes {
            yield Err(ai_error(ErrorKind::OutputValidation, ErrorPhase::Assemble, DispatchStatus::Dispatched, "OpenAI speech returned an empty body"));
            return;
        }
        yield Ok(SpeechEvent::OutputFinished);
        yield Ok(SpeechEvent::Finished);
    })
}

fn authorized_json_request(
    context: &PrepareContext,
    url: String,
    body: Vec<u8>,
) -> Result<HttpRequest, AiError> {
    authorized_request(context, url, "application/json", body)
}

fn authorized_control_request(
    context: &PrepareContext,
    method: Method,
    url: String,
) -> Result<HttpRequest, AiError> {
    let credential = context.credential().ok_or_else(|| {
        ai_error(
            ErrorKind::Authentication,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            "OpenAI credential is unavailable",
        )
    })?;
    HttpRequest::new(method, url)
        .map_err(invalid_request_error)?
        .bearer_auth(credential.secret())
        .map_err(invalid_request_error)
}

fn authorized_request(
    context: &PrepareContext,
    url: String,
    content_type: &str,
    body: Vec<u8>,
) -> Result<HttpRequest, AiError> {
    let credential = context.credential().ok_or_else(|| {
        ai_error(
            ErrorKind::Authentication,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            "OpenAI credential is unavailable",
        )
    })?;
    HttpRequest::new(Method::POST, url)
        .map_err(invalid_request_error)?
        .header(
            http::header::CONTENT_TYPE,
            HeaderValue::from_str(content_type).map_err(|_| {
                ai_error(
                    ErrorKind::InvalidRequest,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "invalid content type",
                )
            })?,
        )
        .map_err(invalid_request_error)?
        .bearer_auth(credential.secret())
        .map_err(invalid_request_error)
        .map(|request| request.body(body))
}

type MultipartPart = (String, Option<String>, Option<String>, Vec<u8>);

fn multipart(boundary: &str, parts: Vec<MultipartPart>) -> Result<Vec<u8>, AiError> {
    if parts.iter().any(|(_, _, _, body)| {
        body.windows(boundary.len())
            .any(|window| window == boundary.as_bytes())
    }) {
        return Err(ai_error(
            ErrorKind::InvalidRequest,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            "media collides with the multipart boundary",
        ));
    }
    let mut output = Vec::new();
    for (name, filename, mime, body) in parts {
        output.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes(),
        );
        if let Some(filename) = filename {
            output.extend_from_slice(format!("; filename=\"{filename}\"").as_bytes());
        }
        output.extend_from_slice(b"\r\n");
        if let Some(mime) = mime {
            output.extend_from_slice(format!("Content-Type: {mime}\r\n").as_bytes());
        }
        output.extend_from_slice(b"\r\n");
        output.extend_from_slice(&body);
        output.extend_from_slice(b"\r\n");
    }
    output.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok(output)
}

fn responses_usage(value: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: value
            .get("input_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64),
        cache_write_tokens: None,
        reasoning_tokens: value
            .get("output_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64),
    }
}

fn audio_format(mime: &str) -> Result<&'static str, AiError> {
    match mime {
        "audio/wav" | "audio/x-wav" => Ok("wav"),
        "audio/mpeg" | "audio/mp3" => Ok("mp3"),
        _ => Err(ai_error(
            ErrorKind::Unsupported,
            ErrorPhase::Prepare,
            DispatchStatus::NotStarted,
            "OpenAI Responses supports only WAV or MP3 audio input",
        )),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)] // Range and finiteness are checked before the rounded millisecond conversion.
fn seconds_to_ms(value: Option<f64>) -> Result<u64, AiError> {
    let value = value.ok_or_else(|| {
        ai_error(
            ErrorKind::Protocol,
            ErrorPhase::Assemble,
            DispatchStatus::Dispatched,
            "transcription timestamp is absent",
        )
    })?;
    if !value.is_finite() || value < 0.0 || value > (u64::MAX as f64) / 1_000.0 {
        return Err(ai_error(
            ErrorKind::OutputValidation,
            ErrorPhase::Assemble,
            DispatchStatus::Dispatched,
            "transcription timestamp is outside bounds",
        ));
    }
    Ok((value * 1_000.0).round() as u64)
}

async fn http_failure(status: u16, body: ByteStream) -> AiError {
    http_failure_at(status, body, ErrorPhase::FirstEvent).await
}

async fn http_failure_at(status: u16, body: ByteStream, phase: ErrorPhase) -> AiError {
    provider_http_error(status, body, phase, "OpenAI rejected the request").await
}

fn language_failed(error: AiError) -> LanguageEvent {
    LanguageEvent::Failed {
        error,
        replay: None,
    }
}

#[allow(clippy::needless_pass_by_value)] // Preserves ownership at the external error seam.
fn deferred_transport_error(error: TransportError, phase: ErrorPhase) -> AiError {
    ai_error(
        ErrorKind::Transport,
        phase,
        DispatchStatus::Unknown,
        error.to_string(),
    )
}

fn required_string(value: &Value, field: &str) -> Result<String, AiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            ai_error(
                ErrorKind::Protocol,
                ErrorPhase::Realtime,
                DispatchStatus::Dispatched,
                format!("OpenAI Realtime event has no `{field}`"),
            )
        })
}

#[allow(clippy::needless_pass_by_value)] // Directly usable with Result::map_err.
fn realtime_socket_error(error: tokio_tungstenite::tungstenite::Error) -> AiError {
    ai_error(
        ErrorKind::Transport,
        ErrorPhase::Realtime,
        DispatchStatus::Unknown,
        format!("OpenAI Realtime WebSocket failed: {error}"),
    )
}
