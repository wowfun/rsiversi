//! Bounded HTTP and SSE mechanics shared by concrete `rsi-ai` providers.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // TransportError owns the bounded failure contract.

mod json_extract;
mod json_projection;
mod sse;

pub use json_extract::{
    BoundedJsonExtractor, JsonExtractEvent, JsonExtractProgress, JsonExtraction,
    JsonExtractionLimits,
};
pub use json_projection::{JsonProjectionLimits, project_json_body};
pub use sse::{SseData, SseStream, decode_sse};

use std::{
    collections::{HashMap, HashSet},
    fmt,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use async_stream::stream;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use futures_util::{Stream, StreamExt as _};
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, TokenUsage, sanitize_error_summary,
    validate_identifier,
};
use rsi_credentials_protocol::SecretValue;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// Default ceiling for delta-oriented SSE provider frames.
pub const DEFAULT_SSE_FRAME_BYTES: usize = 256 * 1024;
/// Maximum bytes in one production HTTP response stream item.
pub const MAX_HTTP_RESPONSE_ITEM_BYTES: usize = 256 * 1024;
/// Absolute ceiling a concrete provider may select for one SSE frame.
pub const MAX_PROVIDER_SSE_FRAME_BYTES: usize = MAX_PROVIDER_REQUEST_BODY_BYTES;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_mins(5);
const REQUEST_BODY_RAW_CHUNK_BYTES: usize = 48 * 1024;
const _: () = assert!(REQUEST_BODY_RAW_CHUNK_BYTES.is_multiple_of(3));
/// Maximum base64 media slots admitted by one streamed JSON request body.
pub const MAX_JSON_BASE64_REPLACEMENTS: usize = 256;
/// Default concrete-provider ceiling for one projected JSON request body.
pub const MAX_PROVIDER_REQUEST_BODY_BYTES: usize = 384 * 1024 * 1024;

/// Formats one bearer credential in temporary zeroizing storage.
pub fn bearer_authorization_header(secret: &SecretValue) -> Result<HeaderValue, TransportError> {
    let encoded = Zeroizing::new(format!("Bearer {}", secret.expose_secret()));
    let mut value = HeaderValue::from_str(&encoded).map_err(|_| {
        TransportError::new(
            "http.invalid_credential",
            "credential cannot be encoded as an Authorization header",
        )
    })?;
    value.set_sensitive(true);
    Ok(value)
}

/// Pull-based HTTP body bytes. Each transport failure is terminal.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send + 'static>>;

enum RequestBodyPart {
    Bytes(Bytes),
    Base64(Arc<[u8]>),
}

/// A JSON request body that is buffered when it has no media and streamed
/// otherwise.
pub struct JsonRequestBody {
    body: RequestBody,
}

impl JsonRequestBody {
    /// Wraps already encoded JSON bytes as one buffered request body.
    pub fn buffered(bytes: impl Into<Bytes>) -> Self {
        Self {
            body: RequestBody::Buffered(bytes.into()),
        }
    }
}

impl fmt::Debug for JsonRequestBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonRequestBody")
            .field(
                "buffered_bytes",
                &match &self.body {
                    RequestBody::Buffered(bytes) => Some(bytes.len()),
                    RequestBody::Streaming(_) => None,
                },
            )
            .field("streaming", &matches!(self.body, RequestBody::Streaming(_)))
            .finish()
    }
}

/// One JSON Pointer to a `null` slot that will receive streamed base64 bytes.
#[derive(Clone)]
pub struct JsonBase64Replacement {
    pointer: String,
    prefix: String,
    bytes: Arc<[u8]>,
}

struct LocatedBase64Replacement {
    start: usize,
    end: usize,
    prefix: Bytes,
    bytes: Arc<[u8]>,
}

impl fmt::Debug for JsonBase64Replacement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonBase64Replacement")
            .field("pointer", &self.pointer)
            .field("prefix", &self.prefix)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

impl JsonBase64Replacement {
    /// Declares a JSON Pointer slot, string prefix, and raw bytes to encode there.
    ///
    /// The referenced slot must exist and contain `null` when passed to
    /// [`json_base64_body`].
    pub fn new(pointer: impl Into<String>, prefix: impl Into<String>, bytes: Arc<[u8]>) -> Self {
        Self {
            pointer: pointer.into(),
            prefix: prefix.into(),
            bytes,
        }
    }
}

fn stream_request_body(parts: Vec<RequestBodyPart>) -> ByteStream {
    Box::pin(stream! {
        for part in parts {
            match part {
                RequestBodyPart::Bytes(bytes) => {
                    if !bytes.is_empty() {
                        yield Ok(bytes);
                    }
                }
                RequestBodyPart::Base64(bytes) => {
                    for chunk in bytes.chunks(REQUEST_BODY_RAW_CHUNK_BYTES) {
                        yield Ok(Bytes::from(BASE64.encode(chunk)));
                    }
                }
            }
        }
    })
}

/// Fills declared `null` JSON slots with lazily base64-encoded media values.
///
/// The implementation owns the temporary wire markers and proves that they are
/// absent from the complete JSON template before serializing it, so caller data
/// cannot collide with a replacement marker.
pub fn json_base64_body(
    mut template: Value,
    replacements: Vec<JsonBase64Replacement>,
    maximum_body_bytes: usize,
) -> Result<JsonRequestBody, TransportError> {
    if replacements.len() > MAX_JSON_BASE64_REPLACEMENTS {
        return Err(TransportError::new(
            "http.too_many_media_replacements",
            format!(
                "JSON body contains more than {MAX_JSON_BASE64_REPLACEMENTS} media replacements"
            ),
        ));
    }
    if replacements.is_empty() {
        let body = serde_json::to_vec(&template).map_err(|error| {
            TransportError::new("http.invalid_body_template", error.to_string())
        })?;
        ensure_projected_body_size(body.len(), maximum_body_bytes)?;
        return Ok(JsonRequestBody::buffered(body));
    }

    let mut template_strings = HashSet::new();
    collect_json_strings(&template, &mut template_strings);
    let mut markers = Vec::with_capacity(replacements.len());
    let mut marker_nonce = 0usize;
    for (index, replacement) in replacements.iter().enumerate() {
        let marker = loop {
            let marker = format!("\0rsi-media-{index}-{marker_nonce}\0");
            marker_nonce = marker_nonce.checked_add(1).ok_or_else(|| {
                TransportError::new(
                    "http.invalid_body_template",
                    "JSON media marker identity overflowed",
                )
            })?;
            if template_strings.insert(marker.clone()) {
                break marker;
            }
        };
        let slot = template.pointer_mut(&replacement.pointer).ok_or_else(|| {
            TransportError::new("http.invalid_body_template", "JSON media slot is missing")
        })?;
        if !slot.is_null() {
            return Err(TransportError::new(
                "http.invalid_body_template",
                "JSON media slot is not empty",
            ));
        }
        *slot = Value::String(marker.clone());
        markers.push(marker);
    }
    let template =
        Bytes::from(serde_json::to_vec(&template).map_err(|error| {
            TransportError::new("http.invalid_body_template", error.to_string())
        })?);
    let marker_offsets = locate_marker_offsets(&template, &markers)?;
    let (mut located, projected_body_bytes) =
        locate_base64_replacements(template.len(), marker_offsets, replacements)?;
    ensure_projected_body_size(projected_body_bytes, maximum_body_bytes)?;
    located.sort_unstable_by_key(|replacement| replacement.start);
    debug_assert!(
        located.windows(2).all(|pair| pair[0].end <= pair[1].start),
        "distinct complete JSON string tokens cannot overlap"
    );

    let mut parts = Vec::with_capacity(located.len().saturating_mul(4).saturating_add(1));
    let mut offset = 0;
    for replacement in located {
        parts.push(RequestBodyPart::Bytes(
            template.slice(offset..replacement.start),
        ));
        parts.push(RequestBodyPart::Bytes(replacement.prefix));
        parts.push(RequestBodyPart::Base64(replacement.bytes));
        parts.push(RequestBodyPart::Bytes(Bytes::from_static(b"\"")));
        offset = replacement.end;
    }
    parts.push(RequestBodyPart::Bytes(template.slice(offset..)));
    Ok(JsonRequestBody {
        body: RequestBody::Streaming(stream_request_body(parts)),
    })
}

fn locate_marker_offsets(
    template: &[u8],
    markers: &[String],
) -> Result<Vec<(usize, usize)>, TransportError> {
    let marker_tokens = markers
        .iter()
        .enumerate()
        .map(|(index, marker)| {
            serde_json::to_vec(marker)
                .map(|token| (token, index))
                .map_err(|error| {
                    TransportError::new("http.invalid_body_template", error.to_string())
                })
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    let mut offsets = vec![None; markers.len()];
    visit_json_string_tokens(template, |start, end| {
        if let Some(index) = marker_tokens.get(&template[start..end]) {
            debug_assert!(offsets[*index].is_none());
            offsets[*index] = Some((start, end));
        }
    });
    offsets
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            TransportError::new(
                "http.invalid_body_template",
                "generated JSON media marker is missing",
            )
        })
}

fn locate_base64_replacements(
    template_bytes: usize,
    marker_offsets: Vec<(usize, usize)>,
    replacements: Vec<JsonBase64Replacement>,
) -> Result<(Vec<LocatedBase64Replacement>, usize), TransportError> {
    let mut located = Vec::with_capacity(replacements.len());
    let mut projected_body_bytes = template_bytes;
    for ((start, end), replacement) in marker_offsets.into_iter().zip(replacements) {
        let mut prefix = serde_json::to_vec(&replacement.prefix).map_err(|error| {
            TransportError::new("http.invalid_body_template", error.to_string())
        })?;
        let Some(b'"') = prefix.pop() else {
            return Err(TransportError::new(
                "http.invalid_body_template",
                "JSON media prefix is not a string",
            ));
        };
        let base64_bytes = projected_base64_bytes(replacement.bytes.len())?;
        let replacement_bytes = prefix
            .len()
            .checked_add(base64_bytes)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(projected_body_overflow)?;
        projected_body_bytes = projected_body_bytes
            .checked_sub(end - start)
            .and_then(|value| value.checked_add(replacement_bytes))
            .ok_or_else(projected_body_overflow)?;
        located.push(LocatedBase64Replacement {
            start,
            end,
            prefix: Bytes::from(prefix),
            bytes: replacement.bytes,
        });
    }
    Ok((located, projected_body_bytes))
}

fn projected_base64_bytes(raw_bytes: usize) -> Result<usize, TransportError> {
    raw_bytes
        .checked_add(2)
        .and_then(|value| value.checked_div(3))
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(projected_body_overflow)
}

fn projected_body_overflow() -> TransportError {
    TransportError::new(
        "http.request_body_too_large",
        "projected JSON request body length overflowed",
    )
}

fn ensure_projected_body_size(
    projected_body_bytes: usize,
    maximum_body_bytes: usize,
) -> Result<(), TransportError> {
    if projected_body_bytes > maximum_body_bytes {
        return Err(TransportError::new(
            "http.request_body_too_large",
            format!("projected JSON request body exceeds {maximum_body_bytes} bytes"),
        ));
    }
    Ok(())
}

fn visit_json_string_tokens(json: &[u8], mut visit: impl FnMut(usize, usize)) {
    let mut offset = 0;
    while offset < json.len() {
        if json[offset] != b'"' {
            offset += 1;
            continue;
        }
        let start = offset;
        offset += 1;
        let mut escaped = false;
        while offset < json.len() {
            let byte = json[offset];
            offset += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                visit(start, offset);
                break;
            }
        }
    }
}

fn collect_json_strings(value: &Value, strings: &mut HashSet<String>) {
    match value {
        Value::String(value) => {
            strings.insert(value.clone());
        }
        Value::Array(values) => {
            for value in values {
                collect_json_strings(value, strings);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                strings.insert(key.clone());
                collect_json_strings(value, strings);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Whether a provider uses `[DONE]` or clean EOF to terminate SSE.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SseTermination {
    /// A `data: [DONE]` event terminates the stream.
    DoneSentinel,
    /// Clean end-of-file terminates the stream.
    Eof,
}

/// Shared provider wire grammar for one Chat Completions SSE chunk.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionsChunk {
    /// Incremental choices emitted by the provider.
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub choices: Vec<ChatCompletionsChoice>,
    /// Usage totals, when the provider includes them in this chunk.
    pub usage: Option<ChatCompletionsUsage>,
}

/// One choice from a Chat Completions streaming chunk.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionsChoice {
    /// Incremental content for the choice.
    #[serde(default)]
    pub delta: ChatCompletionsDelta,
    /// Provider finish reason when this choice has terminated.
    pub finish_reason: Option<String>,
}

/// Incremental assistant content in a Chat Completions choice.
#[derive(Debug, Default, Deserialize)]
pub struct ChatCompletionsDelta {
    /// User-visible text appended by this chunk.
    pub content: Option<String>,
    /// Provider reasoning text appended by this chunk, when exposed.
    pub reasoning_content: Option<String>,
    /// Incremental tool-call fragments indexed by the provider.
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

/// One indexed tool-call fragment from a Chat Completions chunk.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionsToolDelta {
    /// Stable position of the tool call within the assistant turn.
    pub index: u32,
    /// Provider-assigned call identifier, normally present in the first fragment.
    pub id: Option<String>,
    /// Incremental function name and argument payload.
    pub function: Option<ChatCompletionsFunctionDelta>,
}

/// Incremental function payload for a streamed tool call.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionsFunctionDelta {
    /// Function name fragment, when supplied by the provider.
    pub name: Option<String>,
    /// JSON argument text fragment, to be concatenated in stream order.
    pub arguments: Option<String>,
}

/// Provider token counters from a Chat Completions response.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionsUsage {
    /// Total prompt tokens charged by the provider.
    pub prompt_tokens: u64,
    /// Total completion tokens charged by the provider.
    pub completion_tokens: u64,
    /// Provider-specific prompt cache-hit counter.
    pub prompt_cache_hit_tokens: Option<u64>,
    /// OpenAI-compatible cache-read input counter.
    pub cache_read_input_tokens: Option<u64>,
    /// OpenAI-compatible cache-creation input counter.
    pub cache_creation_input_tokens: Option<u64>,
    /// Optional detailed completion-token counters.
    pub completion_tokens_details: Option<ChatCompletionTokenDetails>,
}

impl ChatCompletionsUsage {
    #[must_use]
    /// Converts provider-specific counters into the shared usage contract.
    ///
    /// `prompt_cache_hit_tokens` takes precedence over
    /// `cache_read_input_tokens` when both are present.
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

/// Detailed completion-token counters returned by a provider.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionTokenDetails {
    /// Completion tokens the provider classifies as reasoning.
    pub reasoning_tokens: Option<u64>,
}

/// One fully configured request at the true external HTTP seam.
pub struct HttpRequest {
    method: Method,
    url: reqwest::Url,
    headers: HeaderMap,
    body: RequestBody,
}

enum RequestBody {
    Buffered(Bytes),
    Streaming(ByteStream),
}

impl HttpRequest {
    /// Creates an empty request after validating the endpoint URL.
    pub fn new(method: Method, url: impl AsRef<str>) -> Result<Self, TransportError> {
        let url = reqwest::Url::parse(url.as_ref()).map_err(|error| {
            TransportError::new("http.invalid_url", format!("invalid endpoint URL: {error}"))
        })?;
        validate_url(&url)?;
        Ok(Self {
            method,
            url,
            headers: HeaderMap::new(),
            body: RequestBody::Buffered(Bytes::new()),
        })
    }

    /// Adds a header, rejecting transport-owned `Host` and `Content-Length`.
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

    /// Sets a bearer credential as a sensitive authorization header.
    pub fn bearer_auth(mut self, secret: &SecretValue) -> Result<Self, TransportError> {
        let value = bearer_authorization_header(secret)?;
        self.headers.insert(http::header::AUTHORIZATION, value);
        Ok(self)
    }

    #[must_use]
    /// Replaces the request body with buffered bytes.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = RequestBody::Buffered(body.into());
        self
    }

    #[must_use]
    /// Replaces the request body with a pull-based stream.
    pub fn body_stream(mut self, body: ByteStream) -> Self {
        self.body = RequestBody::Streaming(body);
        self
    }

    #[must_use]
    /// Replaces the request body with a prepared buffered-or-streaming JSON body.
    pub fn json_body(mut self, body: JsonRequestBody) -> Self {
        self.body = body.body;
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
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field(
                "body_bytes",
                &match &self.body {
                    RequestBody::Buffered(bytes) => Some(bytes.len()),
                    RequestBody::Streaming(_) => None,
                },
            )
            .field(
                "streaming_body",
                &matches!(&self.body, RequestBody::Streaming(_)),
            )
            .finish()
    }
}

/// Headers and streaming body returned by an HTTP transport.
pub struct HttpResponse {
    pub status: u16,
    pub headers: HeaderMap,
    /// Pull-based response body; production transport items are bounded by
    /// [`MAX_HTTP_RESPONSE_ITEM_BYTES`] and each transport failure is terminal.
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
    /// Executes one request until headers arrive, cancellation wins, or it fails.
    ///
    /// Implementations must not retry because retry ownership belongs above the
    /// true external-effect seam.
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
    /// Constructs a transport with the crate's finite default timeouts.
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
        let outgoing = self
            .client
            .request(request.method, request.url)
            .headers(request.headers);
        let mut outgoing = match request.body {
            RequestBody::Buffered(bytes) => outgoing.body(bytes),
            RequestBody::Streaming(stream) => outgoing.body(reqwest::Body::wrap_stream(stream)),
        };
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
                        Some(Ok(bytes)) if bytes.len() <= MAX_HTTP_RESPONSE_ITEM_BYTES => {
                            yield Ok(bytes);
                        }
                        Some(Ok(bytes)) => {
                            // Copy before yielding so no bounded slice retains the complete
                            // oversized upstream backing allocation across a consumer yield.
                            let items = bytes
                                .chunks(MAX_HTTP_RESPONSE_ITEM_BYTES)
                                .map(Bytes::copy_from_slice)
                                .collect::<Vec<_>>();
                            drop(bytes);
                            for item in items {
                                yield Ok(item);
                            }
                        }
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
    let kind = if error.code() == "http.body_too_large" {
        ErrorKind::OutputValidation
    } else {
        ErrorKind::Transport
    };
    provider_error(
        kind,
        ErrorPhase::Assemble,
        DispatchStatus::Dispatched,
        error.to_string(),
    )
}

/// Maps bounded incremental JSON extraction from a successful provider response.
#[allow(clippy::needless_pass_by_value)] // Intended for direct use with Result::map_err.
pub fn transport_json_response_error(error: TransportError) -> AiError {
    let kind = if error.code() == "json.extract_limit" {
        ErrorKind::OutputValidation
    } else {
        ErrorKind::Protocol
    };
    provider_error(
        kind,
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
    let mut error = provider_error(kind, phase, DispatchStatus::Dispatched, summary);
    if (100..=599).contains(&status) {
        error = match error.with_status(status) {
            Ok(error) => error,
            Err(invalid) => provider_error(
                ErrorKind::Protocol,
                phase,
                DispatchStatus::Dispatched,
                invalid.to_string(),
            ),
        };
    }
    if let Some(code) = provider_code
        && validate_identifier("provider_code", code).is_ok()
    {
        error = match error.with_provider_code(code) {
            Ok(error) => error,
            Err(invalid) => provider_error(
                ErrorKind::Protocol,
                phase,
                DispatchStatus::Dispatched,
                invalid.to_string(),
            ),
        };
    }
    error
}

/// Reclassifies the shared OpenAI-compatible context overflow response.
#[must_use]
pub fn reclassify_context_limit(error: AiError) -> AiError {
    if matches!(error.status(), Some(400 | 422))
        && error.provider_code() == Some("context_length_exceeded")
    {
        error.with_kind(ErrorKind::ContextLimit)
    } else {
        error
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
    /// Creates a failure with a stable code and sanitized, bounded message.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        let message = rsi_ai_protocol::sanitize_error_summary(&message.into());
        Self {
            code,
            message: Arc::from(message),
        }
    }

    /// Returns the stable machine-readable failure code.
    pub const fn code(&self) -> &'static str {
        self.code
    }
}
