//! Xiaomi `MiMo` V2.5 ASR and TTS adapters over the documented Chat Completions wire.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // AiError carries the public failure taxonomy.

use std::{fmt, sync::Arc};

use async_stream::stream;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::{StreamExt as _, stream as futures_stream};
use http::{HeaderValue, Method};
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, SpeechEvent, SpeechFormat, SpeechRequest,
    TokenUsage, TranscriptionEvent, TranscriptionRequest,
};
use rsi_ai_provider::{
    AdapterFuture, PrepareContext, Prepared, SpeechAdapter, SpeechAdapterStream,
    TranscriptionAdapter, TranscriptionAdapterStream,
};
use rsi_ai_transport::{
    ByteStream, ChatCompletionsChunk, HttpRequest, HttpTransport, SseTermination, collect_body,
    decode_sse, invalid_request_error, provider_error as ai_error, provider_http_error,
    transport_body_error, transport_connect_error, transport_stream_error,
};
use serde_json::{Value, json};

// One maximum-size (128 MiB) audio body requires about 171 MiB of base64; the
// remaining headroom covers the bounded Chat Completions JSON envelope. This
// is a per-call transient ceiling.
const MAX_AUDIO_JSON_BYTES: usize = 180 * 1024 * 1024;
const OUTPUT_CHUNK_BYTES: usize = 256 * 1024;

/// Fixed Xiaomi `MiMo` API origin.
#[derive(Clone, Debug)]
pub struct XiaomiConfig {
    endpoint: String,
}

impl Default for XiaomiConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.xiaomimimo.com".to_owned(),
        }
    }
}

impl XiaomiConfig {
    pub fn new(endpoint: impl Into<String>) -> Result<Self, AiError> {
        let config = Self {
            endpoint: endpoint.into().trim_end_matches('/').to_owned(),
        };
        HttpRequest::new(Method::POST, config.url()).map_err(invalid_request_error)?;
        Ok(config)
    }

    fn url(&self) -> String {
        format!("{}/v1/chat/completions", self.endpoint)
    }
}

macro_rules! xiaomi_adapter {
    ($name:ident) => {
        #[derive(Clone)]
        pub struct $name {
            config: XiaomiConfig,
            transport: Arc<dyn HttpTransport>,
        }

        impl $name {
            #[must_use]
            pub fn new(config: XiaomiConfig, transport: Arc<dyn HttpTransport>) -> Self {
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

xiaomi_adapter!(XiaomiTranscriptionAdapter);
xiaomi_adapter!(XiaomiSpeechAdapter);

impl TranscriptionAdapter for XiaomiTranscriptionAdapter {
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: TranscriptionRequest,
    ) -> AdapterFuture<Result<Prepared<TranscriptionAdapterStream>, AiError>> {
        if request.timestamps() {
            return Box::pin(async {
                Err(ai_error(
                    ErrorKind::Unsupported,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "Xiaomi MiMo ASR does not return timestamped segments",
                ))
            });
        }
        if request.prompt().is_some() {
            return Box::pin(async {
                Err(ai_error(
                    ErrorKind::Unsupported,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "Xiaomi MiMo ASR does not support a transcription prompt",
                ))
            });
        }
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let bytes = context
                        .resolve_media(request.audio(), abort.clone())
                        .await?;
                    let format = audio_format(request.audio().mime_type())?;
                    let language = request.language().unwrap_or("auto").to_owned();
                    if !matches!(language.as_str(), "auto" | "zh" | "en") {
                        return Err(ai_error(
                            ErrorKind::Unsupported,
                            ErrorPhase::Prepare,
                            DispatchStatus::NotStarted,
                            "Xiaomi MiMo ASR language must be auto, zh, or en",
                        ));
                    }
                    let body = serde_json::to_vec(&json!({
                    "model":model,
                    "messages":[{
                        "role":"user",
                        "content":[{
                            "type":"input_audio",
                            "input_audio":{
                                "data":format!("data:{};base64,{}", request.audio().mime_type(), BASE64.encode(bytes)),
                                "format":format,
                            }
                        }]
                    }],
                    "asr_options":{"language":language},
                    "stream":true,
                    "stream_options":{"include_usage":true},
                })).map_err(invalid_request_error)?;
                    let outgoing = authorized(&context, config.url(), body)?;
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(transport_connect_error)?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure(response.status, response.body).await);
                    }
                    Ok(asr_stream(
                        decode_sse(response.body, SseTermination::DoneSentinel),
                        if language == "auto" {
                            None
                        } else {
                            Some(language)
                        },
                    ))
                })
            }))
        })
    }
}

fn asr_stream(
    mut input: rsi_ai_transport::SseStream,
    language: Option<String>,
) -> TranscriptionAdapterStream {
    Box::pin(stream! {
        let mut finished = false;
        let mut usage = None;
        while let Some(payload) = input.next().await {
            let payload = match payload {
                Ok(payload) => payload,
                Err(error) => {
                    yield Err(transport_stream_error(error));
                    return;
                }
            };
            let chunk = match serde_json::from_str::<ChatCompletionsChunk>(&payload) {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(ai_error(
                        ErrorKind::Protocol,
                        ErrorPhase::Stream,
                        DispatchStatus::Dispatched,
                        format!("Xiaomi MiMo ASR emitted malformed JSON: {error}"),
                    ));
                    return;
                }
            };
            if chunk.choices.len() > 1 {
                yield Err(ai_error(ErrorKind::OutputValidation, ErrorPhase::Stream, DispatchStatus::Dispatched, "Xiaomi MiMo ASR emitted multiple choices"));
                return;
            }
            if let Some(choice) = chunk.choices.into_iter().next() {
                if let Some(text) = choice.delta.content.filter(|value| !value.is_empty()) {
                    yield Ok(TranscriptionEvent::TextDelta { text });
                }
                if choice.finish_reason.is_some() {
                    finished = true;
                }
            }
            if let Some(wire_usage) = chunk.usage {
                usage = Some(wire_usage.normalized());
            }
        }
        if !finished {
            yield Err(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "Xiaomi MiMo ASR ended without finish_reason"));
            return;
        }
        if let Some(usage) = usage {
            yield Ok(TranscriptionEvent::Usage { usage });
        }
        yield Ok(TranscriptionEvent::Finished { language });
    })
}

impl SpeechAdapter for XiaomiSpeechAdapter {
    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: SpeechRequest,
    ) -> AdapterFuture<Result<Prepared<SpeechAdapterStream>, AiError>> {
        if request.speed().is_some() {
            return Box::pin(async {
                Err(ai_error(
                    ErrorKind::Unsupported,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "Xiaomi MiMo TTS uses natural-language style control instead of numeric speed",
                ))
            });
        }
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let wire_format = match request.format() {
                        SpeechFormat::Pcm16 => "pcm16",
                        SpeechFormat::Wav => "wav",
                        SpeechFormat::Mp3 => "mp3",
                    };
                    let streaming = request.format() == SpeechFormat::Pcm16;
                    let body = serde_json::to_vec(&json!({
                        "model":model,
                        "messages":[{"role":"assistant", "content":request.text()}],
                        "audio":{"format":wire_format, "voice":request.voice()},
                        "stream":streaming,
                    }))
                    .map_err(invalid_request_error)?;
                    let outgoing = authorized(&context, config.url(), body)?;
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(transport_connect_error)?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure(response.status, response.body).await);
                    }
                    if streaming {
                        Ok(tts_stream(
                            decode_sse(response.body, SseTermination::DoneSentinel),
                            request.format(),
                        ))
                    } else {
                        let body = collect_body(response.body, MAX_AUDIO_JSON_BYTES)
                            .await
                            .map_err(transport_body_error)?;
                        let value: Value = serde_json::from_slice(&body).map_err(|_| {
                            ai_error(
                                ErrorKind::Protocol,
                                ErrorPhase::Assemble,
                                DispatchStatus::Dispatched,
                                "Xiaomi MiMo TTS returned malformed JSON",
                            )
                        })?;
                        let encoded = value
                            .pointer("/choices/0/message/audio/data")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                ai_error(
                                    ErrorKind::Protocol,
                                    ErrorPhase::Assemble,
                                    DispatchStatus::Dispatched,
                                    "Xiaomi MiMo TTS response has no audio data",
                                )
                            })?;
                        let bytes = BASE64.decode(encoded).map_err(|_| {
                            ai_error(
                                ErrorKind::Protocol,
                                ErrorPhase::Assemble,
                                DispatchStatus::Dispatched,
                                "Xiaomi MiMo TTS audio has invalid base64",
                            )
                        })?;
                        Ok(completed_speech(&bytes, request.format()))
                    }
                })
            }))
        })
    }
}

fn tts_stream(mut input: rsi_ai_transport::SseStream, format: SpeechFormat) -> SpeechAdapterStream {
    Box::pin(stream! {
        yield Ok(SpeechEvent::OutputStarted { mime_type: mime_type(format).to_owned() });
        let mut sequence = 1_u32;
        let mut finished = false;
        let mut saw_audio = false;
        let mut usage = None;
        while let Some(payload) = input.next().await {
            let payload = match payload {
                Ok(payload) => payload,
                Err(error) => {
                    yield Err(transport_stream_error(error));
                    return;
                }
            };
            let Ok(value) = serde_json::from_str::<Value>(&payload) else {
                yield Err(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "Xiaomi MiMo TTS emitted malformed JSON"));
                return;
            };
            let choices: &[Value] = value
                .get("choices")
                .and_then(Value::as_array)
                .map_or(&[], Vec::as_slice);
            if choices.len() > 1 {
                yield Err(ai_error(ErrorKind::OutputValidation, ErrorPhase::Stream, DispatchStatus::Dispatched, "Xiaomi MiMo TTS emitted multiple choices"));
                return;
            }
            if let Some(choice) = choices.first() {
                if let Some(encoded) = choice.pointer("/delta/audio/data").and_then(Value::as_str) {
                    if encoded.len() > 384 * 1024 {
                        yield Err(ai_error(ErrorKind::OutputValidation, ErrorPhase::Stream, DispatchStatus::Dispatched, "Xiaomi MiMo TTS audio delta exceeds its encoded bound"));
                        return;
                    }
                    let Ok(bytes) = BASE64.decode(encoded) else {
                        yield Err(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "Xiaomi MiMo TTS audio delta has invalid base64"));
                        return;
                    };
                    for chunk in bytes.chunks(OUTPUT_CHUNK_BYTES) {
                        if chunk.is_empty() { continue; }
                        saw_audio = true;
                        yield Ok(SpeechEvent::AudioChunk { sequence, bytes: chunk.to_vec() });
                        sequence = sequence.saturating_add(1);
                    }
                }
                if choice.get("finish_reason").and_then(Value::as_str).is_some() {
                    finished = true;
                }
            }
            if let Some(wire_usage) = value.get("usage") {
                usage = Some(token_usage(wire_usage));
            }
        }
        if !finished || !saw_audio {
            yield Err(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "Xiaomi MiMo TTS ended without audio and finish_reason"));
            return;
        }
        if let Some(usage) = usage {
            yield Ok(SpeechEvent::Usage { usage });
        }
        yield Ok(SpeechEvent::OutputFinished);
        yield Ok(SpeechEvent::Finished);
    })
}

fn completed_speech(bytes: &[u8], format: SpeechFormat) -> SpeechAdapterStream {
    let mut events = vec![SpeechEvent::OutputStarted {
        mime_type: mime_type(format).to_owned(),
    }];
    for (index, chunk) in bytes.chunks(OUTPUT_CHUNK_BYTES).enumerate() {
        events.push(SpeechEvent::AudioChunk {
            sequence: u32::try_from(index + 1).expect("speech output is bounded"),
            bytes: chunk.to_vec(),
        });
    }
    events.push(SpeechEvent::OutputFinished);
    events.push(SpeechEvent::Finished);
    Box::pin(futures_stream::iter(events.into_iter().map(Ok)))
}

fn authorized(
    context: &PrepareContext,
    url: String,
    body: Vec<u8>,
) -> Result<HttpRequest, AiError> {
    let credential = context.credential().ok_or_else(|| {
        ai_error(
            ErrorKind::Authentication,
            ErrorPhase::Send,
            DispatchStatus::NotDispatched,
            "Xiaomi MiMo credential is unavailable",
        )
    })?;
    HttpRequest::new(Method::POST, url)
        .map_err(invalid_request_error)?
        .header(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .map_err(invalid_request_error)?
        .bearer_auth(credential.secret())
        .map_err(invalid_request_error)
        .map(|request| request.body(body))
}

fn token_usage(value: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: value
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        cache_write_tokens: None,
        reasoning_tokens: value
            .pointer("/completion_tokens_details/reasoning_tokens")
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
            "Xiaomi MiMo ASR accepts MP3 or WAV audio",
        )),
    }
}

const fn mime_type(format: SpeechFormat) -> &'static str {
    match format {
        SpeechFormat::Pcm16 => "audio/pcm",
        SpeechFormat::Wav => "audio/wav",
        SpeechFormat::Mp3 => "audio/mpeg",
    }
}

async fn http_failure(status: u16, body: ByteStream) -> AiError {
    provider_http_error(
        status,
        body,
        ErrorPhase::FirstEvent,
        "Xiaomi MiMo rejected the request",
    )
    .await
}
