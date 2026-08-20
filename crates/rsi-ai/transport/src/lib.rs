//! Bounded HTTP and SSE mechanics shared by concrete `rsi-ai` providers.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // TransportError owns the bounded failure contract.

use std::{fmt, pin::Pin, sync::Arc, time::Duration};

use async_stream::stream;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt as _};
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use memchr::memchr2;
use rsi_ai_auth::SecretValue;
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, TokenUsage, sanitize_error_summary,
};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const MAX_SSE_FRAME_BYTES: usize = 256 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_mins(5);

/// Pull-based HTTP body bytes. Each transport failure is terminal.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send + 'static>>;
/// Pull-based decoded SSE `data` fields.
pub type SseStream = Pin<Box<dyn Stream<Item = Result<String, TransportError>> + Send + 'static>>;

/// Whether a provider uses `[DONE]` or clean EOF to terminate SSE.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SseTermination {
    DoneSentinel,
    Eof,
}

/// Shared provider wire grammar for one Chat Completions SSE chunk.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionsChunk {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub choices: Vec<ChatCompletionsChoice>,
    pub usage: Option<ChatCompletionsUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsChoice {
    #[serde(default)]
    pub delta: ChatCompletionsDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ChatCompletionsDelta {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub tool_calls: Vec<ChatCompletionsToolDelta>,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsToolDelta {
    pub index: u32,
    pub id: Option<String>,
    pub function: Option<ChatCompletionsFunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsFunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub prompt_cache_hit_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub completion_tokens_details: Option<ChatCompletionTokenDetails>,
}

impl ChatCompletionsUsage {
    #[must_use]
    pub fn normalized(self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.prompt_tokens,
            output_tokens: self.completion_tokens,
            cache_read_tokens: self
                .prompt_cache_hit_tokens
                .or(self.cache_read_input_tokens),
            cache_write_tokens: self.cache_creation_input_tokens,
            reasoning_tokens: self
                .completion_tokens_details
                .and_then(|details| details.reasoning_tokens),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionTokenDetails {
    pub reasoning_tokens: Option<u64>,
}

/// One fully configured request at the true external HTTP seam.
pub struct HttpRequest {
    method: Method,
    url: reqwest::Url,
    headers: HeaderMap,
    body: Bytes,
}

impl HttpRequest {
    pub fn new(method: Method, url: impl AsRef<str>) -> Result<Self, TransportError> {
        let url = reqwest::Url::parse(url.as_ref()).map_err(|error| {
            TransportError::new("http.invalid_url", format!("invalid endpoint URL: {error}"))
        })?;
        validate_url(&url)?;
        Ok(Self {
            method,
            url,
            headers: HeaderMap::new(),
            body: Bytes::new(),
        })
    }

    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Result<Self, TransportError> {
        if matches!(name.as_str(), "host" | "content-length") {
            return Err(TransportError::new(
                "http.forbidden_header",
                "Host and Content-Length are owned by the transport",
            ));
        }
        self.headers.insert(name, value);
        Ok(self)
    }

    pub fn bearer_auth(mut self, secret: &SecretValue) -> Result<Self, TransportError> {
        let encoded = Zeroizing::new(format!("Bearer {}", secret.expose()));
        let value = HeaderValue::from_str(&encoded).map_err(|_| {
            TransportError::new(
                "http.invalid_credential",
                "credential cannot be encoded as an Authorization header",
            )
        })?;
        self.headers.insert(http::header::AUTHORIZATION, value);
        Ok(self)
    }

    #[must_use]
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn url(&self) -> &reqwest::Url {
        &self.url
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn body_bytes(&self) -> &Bytes {
        &self.body
    }
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// Headers and streaming body returned by an HTTP transport.
pub struct HttpResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: ByteStream,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Injectable true-external HTTP seam.
#[async_trait]
pub trait HttpTransport: fmt::Debug + Send + Sync {
    async fn execute(
        &self,
        request: HttpRequest,
        abort: CancellationToken,
    ) -> Result<HttpResponse, TransportError>;
}

/// Rustls-backed production transport. It performs one request and no retry.
#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, TransportError> {
        Self::with_timeouts(DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT)
    }

    /// Constructs a transport with finite connect and whole-request deadlines.
    pub fn with_timeouts(
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, TransportError> {
        if connect_timeout.is_zero() || request_timeout.is_zero() {
            return Err(TransportError::new(
                "http.invalid_timeout",
                "HTTP timeouts must be nonzero",
            ));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(|error| TransportError::new("http.client", error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn execute(
        &self,
        request: HttpRequest,
        abort: CancellationToken,
    ) -> Result<HttpResponse, TransportError> {
        let mut outgoing = self
            .client
            .request(request.method, request.url)
            .headers(request.headers)
            .body(request.body);
        outgoing = outgoing.header(http::header::ACCEPT_ENCODING, "identity");
        let response = tokio::select! {
            () = abort.cancelled() => {
                return Err(TransportError::new("http.cancelled", "HTTP request was cancelled"));
            }
            response = outgoing.send() => response.map_err(|error| reqwest_error(&error))?,
        };
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response.bytes_stream();
        let body = stream! {
            futures_util::pin_mut!(body);
            loop {
                tokio::select! {
                    biased;
                    () = abort.cancelled() => {
                        yield Err(TransportError::new(
                            "http.cancelled",
                            "HTTP response body was cancelled",
                        ));
                        return;
                    }
                    next = body.next() => match next {
                        Some(Ok(bytes)) => yield Ok(bytes),
                        Some(Err(error)) => {
                            yield Err(reqwest_error(&error));
                            return;
                        }
                        None => return,
                    },
                }
            }
        };
        Ok(HttpResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}

fn reqwest_error(error: &reqwest::Error) -> TransportError {
    if error.is_timeout() {
        TransportError::new("http.timeout", error.to_string())
    } else {
        TransportError::new("http.transport", error.to_string())
    }
}

/// Collects a non-streaming response under an explicit byte limit.
pub async fn collect_body(mut body: ByteStream, maximum: usize) -> Result<Bytes, TransportError> {
    let mut output = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        let projected = output
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| TransportError::new("http.body_too_large", "body length overflowed"))?;
        if projected > maximum {
            return Err(TransportError::new(
                "http.body_too_large",
                format!("HTTP response exceeds {maximum} bytes"),
            ));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(output))
}

/// Constructs one bounded provider error without exposing an untrusted raw summary.
///
/// # Panics
///
/// Panics only if the protocol rejects the output of its own error-summary
/// sanitizer, which would violate the shared protocol invariant.
pub fn provider_error(
    kind: ErrorKind,
    phase: ErrorPhase,
    dispatch: DispatchStatus,
    summary: impl Into<String>,
) -> AiError {
    let summary = sanitize_error_summary(&summary.into());
    AiError::new(kind, phase, dispatch, summary).expect("sanitized provider errors are bounded")
}

/// Maps local request construction failures before provider I/O.
pub fn invalid_request_error(error: impl fmt::Display) -> AiError {
    provider_error(
        ErrorKind::InvalidRequest,
        ErrorPhase::Prepare,
        DispatchStatus::NotStarted,
        error.to_string(),
    )
}

/// Maps a transport connection failure while preserving cancellation.
#[allow(clippy::needless_pass_by_value)] // Intended for direct use with Result::map_err.
pub fn transport_connect_error(error: TransportError) -> AiError {
    let kind = if error.code() == "http.cancelled" {
        ErrorKind::Cancelled
    } else {
        ErrorKind::Transport
    };
    provider_error(
        kind,
        ErrorPhase::Connect,
        DispatchStatus::Unknown,
        error.to_string(),
    )
}

/// Maps malformed or failed incremental transport input.
#[allow(clippy::needless_pass_by_value)] // Intended for direct use with Result::map_err.
pub fn transport_stream_error(error: TransportError) -> AiError {
    let kind = match error.code() {
        "http.cancelled" => ErrorKind::Cancelled,
        "http.timeout" => ErrorKind::Timeout,
        code if code.starts_with("http.") => ErrorKind::Transport,
        _ => ErrorKind::Protocol,
    };
    provider_error(
        kind,
        ErrorPhase::Stream,
        DispatchStatus::Dispatched,
        error.to_string(),
    )
}

/// Maps a failure while assembling a dispatched response body.
#[allow(clippy::needless_pass_by_value)] // Intended for direct use with Result::map_err.
pub fn transport_body_error(error: TransportError) -> AiError {
    provider_error(
        ErrorKind::Transport,
        ErrorPhase::Assemble,
        DispatchStatus::Dispatched,
        error.to_string(),
    )
}

/// Maps one bounded JSON provider error body and HTTP status through the shared taxonomy.
pub async fn provider_http_error(
    status: u16,
    body: ByteStream,
    phase: ErrorPhase,
    fallback: &'static str,
) -> AiError {
    let value = collect_body(body, MAX_ERROR_BODY_BYTES)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let provider = value
        .as_ref()
        .map(|value| value.get("error").unwrap_or(value));
    let summary = provider
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    let provider_code = provider
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str);
    let kind = match (status, phase) {
        (402, _) => ErrorKind::Quota,
        (404, ErrorPhase::DeferredPoll | ErrorPhase::DeferredCancel) => ErrorKind::RemoteExpired,
        (400 | 422, _) => ErrorKind::InvalidRequest,
        (401, _) => ErrorKind::Authentication,
        (403, _) => ErrorKind::Permission,
        (404, _) => ErrorKind::NotFound,
        (408, _) => ErrorKind::Timeout,
        (429, _) => ErrorKind::RateLimited,
        (500..=599, _) => ErrorKind::Server,
        _ => ErrorKind::Transport,
    };
    let mut error =
        provider_error(kind, phase, DispatchStatus::Dispatched, summary).with_status(status);
    if let Some(code) = provider_code
        && let Ok(with_code) = error.clone().with_provider_code(code)
    {
        error = with_code;
    }
    error
}

/// Decodes SSE framing incrementally without buffering an unbounded body.
pub fn decode_sse(mut body: ByteStream, termination: SseTermination) -> SseStream {
    Box::pin(stream! {
        let mut frame = Vec::new();
        let mut line = Vec::new();
        let mut previous_was_cr = false;
        let mut done = false;
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };
            let mut offset = 0;
            if previous_was_cr && chunk.first() == Some(&b'\n') {
                offset = 1;
            }
            previous_was_cr = false;
            while offset < chunk.len() {
                let Some(relative) = memchr2(b'\r', b'\n', &chunk[offset..]) else {
                    line.extend_from_slice(&chunk[offset..]);
                    if frame.len().saturating_add(line.len()) > MAX_SSE_FRAME_BYTES {
                        yield Err(TransportError::new(
                            "sse.frame_too_large",
                            format!("SSE frame exceeds {MAX_SSE_FRAME_BYTES} bytes"),
                        ));
                        return;
                    }
                    break;
                };
                let terminator = offset + relative;
                line.extend_from_slice(&chunk[offset..terminator]);
                if frame.len().saturating_add(line.len()) > MAX_SSE_FRAME_BYTES {
                    yield Err(TransportError::new(
                        "sse.frame_too_large",
                        format!("SSE frame exceeds {MAX_SSE_FRAME_BYTES} bytes"),
                    ));
                    return;
                }
                if line.is_empty() {
                    let complete = std::mem::take(&mut frame);
                    match decode_sse_frame(&complete) {
                        Ok(Some(data)) if data == "[DONE]" => {
                            done = true;
                            break;
                        }
                        Ok(Some(data)) => yield Ok(data),
                        Ok(None) => {}
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                } else {
                    frame.append(&mut line);
                    frame.push(b'\n');
                }
                previous_was_cr = chunk[terminator] == b'\r';
                offset = terminator + 1;
                if previous_was_cr && chunk.get(offset) == Some(&b'\n') {
                    offset += 1;
                    previous_was_cr = false;
                }
                if done { break; }
            }
            if done {
                break;
            }
        }
        if done {
            return;
        }
        if !frame.is_empty() || !line.is_empty() {
            yield Err(TransportError::new(
                "sse.incomplete_frame",
                "SSE stream ended inside a frame",
            ));
            return;
        }
        if termination == SseTermination::DoneSentinel {
            yield Err(TransportError::new(
                "sse.missing_done",
                "SSE stream ended without [DONE]",
            ));
        }
    })
}

fn decode_sse_frame(frame: &[u8]) -> Result<Option<String>, TransportError> {
    let text = std::str::from_utf8(frame)
        .map_err(|_| TransportError::new("sse.invalid_utf8", "SSE frame is not valid UTF-8"))?;
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    if data.is_empty() {
        Ok(None)
    } else {
        Ok(Some(data.join("\n")))
    }
}

fn validate_url(url: &reqwest::Url) -> Result<(), TransportError> {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(TransportError::new(
            "http.invalid_url",
            "endpoint URL cannot contain credentials or a fragment",
        ));
    }
    let host = url.host_str().unwrap_or_default();
    let local = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return Err(TransportError::new(
            "http.insecure_url",
            "endpoint URL must use HTTPS except on loopback",
        ));
    }
    Ok(())
}

/// Bounded, secret-free transport failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct TransportError {
    code: &'static str,
    message: Arc<str>,
}

impl TransportError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        let message = rsi_ai_protocol::sanitize_error_summary(&message.into());
        Self {
            code,
            message: Arc::from(message),
        }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}
