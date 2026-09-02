//! OpenAI-compatible Chat Completions adapter with strict DeepSeek-compatible streaming.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // AiError carries the public failure taxonomy.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use async_stream::stream;
use futures_util::StreamExt as _;
use http::{HeaderName, HeaderValue, Method};
use rsi_ai_protocol::{
    AiError, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase, FinishReason,
    HostedTool, ImageToolResultCapability, ImageToolResultMode, LanguageEvent, LanguageModelLimits,
    LanguageModelProfiles, LanguageProfile, LanguageRequest, Message, MessageContent, MessageRole,
    ResponseFormat, ToolCallKind, ToolChoice, ToolDialect,
};
use rsi_ai_provider::{
    AdapterFuture, LanguageAdapter, LanguageAdapterStream, LanguageRegistrarContract,
    PrepareContext, Prepared, ProviderPublication, ProviderRegistration,
};
use rsi_ai_transport::{
    ChatCompletionsChunk, HttpRequest, HttpTransport, JsonBase64Replacement, JsonRequestBody,
    MAX_PROVIDER_REQUEST_BODY_BYTES, SseTermination, decode_sse, invalid_request_error,
    json_base64_body, provider_error as ai_error, provider_http_error, reclassify_context_limit,
    transport_connect_error, transport_stream_error,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

/// Fixed endpoint policy for one compatible Chat Completions deployment.
#[derive(Clone, Debug)]
pub struct ChatCompletionsConfig {
    endpoint: String,
    path: String,
    allow_image_input: bool,
    language_models: LanguageModelProfiles,
}

impl ChatCompletionsConfig {
    /// Creates a configuration using `/v1/chat/completions` with image input enabled.
    pub fn new(endpoint: impl Into<String>) -> Result<Self, AiError> {
        let config = Self {
            endpoint: endpoint.into().trim_end_matches('/').to_owned(),
            path: "/v1/chat/completions".to_owned(),
            allow_image_input: true,
            language_models: LanguageModelProfiles::default(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Replaces the absolute ASCII request path after validating the full URL.
    pub fn with_path(mut self, path: impl Into<String>) -> Result<Self, AiError> {
        self.path = path.into();
        self.validate()?;
        Ok(self)
    }

    #[must_use]
    /// Enables or disables image input at request preparation time.
    pub const fn with_image_input(mut self, allow: bool) -> Self {
        self.allow_image_input = allow;
        self
    }

    /// Adds one exact model-capacity profile; duplicates and oversized maps fail.
    pub fn with_model_profile(
        mut self,
        model: impl Into<String>,
        limits: LanguageModelLimits,
    ) -> Result<Self, AiError> {
        self.language_models
            .insert(model, limits)
            .map_err(|error| invalid_profile(error.reason()))?;
        Ok(self)
    }

    fn model_limits(&self, model: &str) -> Result<LanguageModelLimits, AiError> {
        self.language_models
            .get(model)
            .ok_or_else(|| invalid_profile("language model has no configured capacity profile"))
    }

    fn url(&self) -> String {
        format!("{}{}", self.endpoint, self.path)
    }

    fn validate(&self) -> Result<(), AiError> {
        if !self.path.starts_with('/')
            || self.path.len() > 1_024
            || !self.path.is_ascii()
            || self.path.contains(['?', '#'])
        {
            return Err(ai_error(
                ErrorKind::InvalidRequest,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "Chat Completions path must be a bounded absolute ASCII path",
            ));
        }
        HttpRequest::new(Method::POST, self.url()).map_err(|error| {
            ai_error(
                ErrorKind::InvalidRequest,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                error.to_string(),
            )
        })?;
        Ok(())
    }
}

/// One no-retry Chat Completions adapter.
#[derive(Clone)]
pub struct ChatCompletionsAdapter {
    config: ChatCompletionsConfig,
    transport: Arc<dyn HttpTransport>,
}

impl ChatCompletionsAdapter {
    #[must_use]
    /// Binds validated endpoint policy to the transport that performs each request.
    pub fn new(config: ChatCompletionsConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self { config, transport }
    }
}

impl fmt::Debug for ChatCompletionsAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatCompletionsAdapter")
            .field("config", &self.config)
            .field("transport", &self.transport)
            .finish()
    }
}

impl LanguageAdapter for ChatCompletionsAdapter {
    fn describe(&self, model: &str) -> Result<LanguageProfile, AiError> {
        let limits = self.config.model_limits(model)?;
        Ok(LanguageProfile::new(
            limits.context_window_tokens(),
            limits.default_output_reserve_tokens(),
            limits.max_output_reserve_tokens(),
            ToolDialect::ChatCompletions,
            false,
            if self.config.allow_image_input {
                ImageToolResultCapability::Yes(ImageToolResultMode::AdjacentUserMessage)
            } else {
                ImageToolResultCapability::No
            },
            Vec::new(),
        )
        .expect("static Chat Completions profile is valid"))
    }

    fn validate_request(&self, model: &str, request: &LanguageRequest) -> Result<(), AiError> {
        self.config.model_limits(model)?;
        if request
            .hosted_tools()
            .iter()
            .any(|tool| matches!(tool, HostedTool::WebSearch { .. }))
        {
            return Err(ai_error(
                ErrorKind::Unsupported,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "Chat Completions does not support provider-hosted tools",
            ));
        }
        if request.tools().iter().any(|tool| tool.freeform().is_some()) {
            return Err(ai_error(
                ErrorKind::Unsupported,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "Chat Completions cannot preserve freeform custom-tool semantics",
            ));
        }
        if request.messages().iter().any(|message| {
            message.content().iter().any(|content| {
                matches!(
                    content,
                    MessageContent::ToolCall(call) if call.kind == ToolCallKind::Freeform
                )
            })
        }) {
            return Err(ai_error(
                ErrorKind::Unsupported,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "Chat Completions cannot preserve freeform custom-tool history",
            ));
        }
        validate_chat_tool_result_adjacency(request)
    }

    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, AiError>> {
        if let Err(error) = self.validate_request(&model, &request) {
            return Box::pin(async move { Err(error) });
        }
        let snapshot = context.snapshot().clone();
        let config = self.config.clone();
        let transport = Arc::clone(&self.transport);
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |abort| {
                Box::pin(async move {
                    let body = build_request_body(
                        &context,
                        &model,
                        &request,
                        abort.clone(),
                        config.allow_image_input,
                    )
                    .await?;
                    let credential = context.credential().ok_or_else(|| {
                        ai_error(
                            ErrorKind::Authentication,
                            ErrorPhase::Send,
                            DispatchStatus::NotDispatched,
                            "provider credential is unavailable",
                        )
                    })?;
                    let outgoing = HttpRequest::new(Method::POST, config.url())
                        .map_err(invalid_request_error)?
                        .header(
                            http::header::CONTENT_TYPE,
                            HeaderValue::from_static("application/json"),
                        )
                        .map_err(invalid_request_error)?
                        .header(
                            HeaderName::from_static("accept"),
                            HeaderValue::from_static("text/event-stream"),
                        )
                        .map_err(invalid_request_error)?
                        .bearer_auth(&credential.secret)
                        .map_err(invalid_request_error)?
                        .json_body(body);
                    let response = transport
                        .execute(outgoing, abort.cancellation_token())
                        .await
                        .map_err(transport_connect_error)?;
                    if !(200..300).contains(&response.status) {
                        return Err(http_failure(response.status, response.body).await);
                    }
                    Ok(translate_chat_stream(decode_sse(
                        response.body,
                        SseTermination::DoneSentinel,
                        rsi_ai_transport::DEFAULT_SSE_FRAME_BYTES,
                    )))
                })
            }))
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatiblePluginConfig {
    deployment: String,
    credential: rsi_credentials_protocol::CredentialRef,
    endpoint: String,
    path: String,
    allow_image_input: bool,
    language_models: BTreeMap<String, LanguageModelLimits>,
}

#[derive(Debug)]
struct PreparedCompatiblePlugin {
    config: CompatiblePluginConfig,
    adapter: ChatCompletionsConfig,
}

/// Ordinary plugin factory for one explicit OpenAI-compatible deployment.
#[derive(Clone, Default)]
pub struct OpenAiCompatibleFactory {
    transport: Option<Arc<dyn HttpTransport>>,
}

impl OpenAiCompatibleFactory {
    /// Uses an injected no-retry transport for deterministic embedders and tests.
    #[must_use]
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport: Some(transport),
        }
    }
}

impl fmt::Debug for OpenAiCompatibleFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatibleFactory")
            .field("injected_transport", &self.transport.is_some())
            .finish()
    }
}

#[async_trait::async_trait]
impl rsi_meta::PluginFactory for OpenAiCompatibleFactory {
    fn prepare(
        &self,
        desired: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        let config: CompatiblePluginConfig = serde_json::from_value(desired.clone())
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        rsi_ai_protocol::validate_identifier("deployment", &config.deployment)
            .map_err(rsi_meta::MetaError::InvalidInput)?;
        config
            .credential
            .validate()
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        if config.language_models.is_empty() {
            return Err(rsi_meta::MetaError::InvalidInput(
                "OpenAI-compatible deployment requires a nonempty exact model map".into(),
            ));
        }
        let mut adapter = ChatCompletionsConfig::new(&config.endpoint)
            .and_then(|adapter| adapter.with_path(&config.path))
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?
            .with_image_input(config.allow_image_input);
        for (model, limits) in &config.language_models {
            adapter = adapter
                .with_model_profile(model, *limits)
                .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        }
        let retained = serde_json::to_vec(desired)
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?
            .len();
        Ok(rsi_meta::PreparedActivation::with_state(
            desired.clone(),
            PreparedCompatiblePlugin { config, adapter },
            retained,
        )
        .requiring_local::<LanguageRegistrarContract>())
    }

    async fn activate(&self, mut plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let prepared = plan.take_state::<PreparedCompatiblePlugin>()?;
        let generation = plan
            .context()
            .owner()
            .ok_or_else(|| rsi_meta::MetaError::Activation("provider has no Fiber owner".into()))?
            .1
            .0;
        let transport: Arc<dyn HttpTransport> = match &self.transport {
            Some(transport) => Arc::clone(transport),
            None => Arc::new(
                rsi_ai_transport::ReqwestTransport::new()
                    .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?,
            ),
        };
        let endpoint_fingerprint = format!(
            "sha256-{}",
            hex::encode(Sha256::digest(prepared.config.endpoint.as_bytes()))
        );
        let registration =
            ProviderRegistration::builder(&prepared.config.deployment, "openai-compatible")
                .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
                .with_credential(prepared.config.credential)
                .with_protocol("chat-completions", "http", endpoint_fingerprint)
                .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
                .with_config_generation(generation)
                .with_language(ChatCompletionsAdapter::new(prepared.adapter, transport))
                .build()
                .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        let publication = ProviderPublication::publish(
            Arc::new(registration),
            Some(plan.local::<LanguageRegistrarContract>()?),
            None,
        )
        .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw OpenAI-compatible provider facet",
            Box::new(move || {
                Box::pin(async move {
                    drop(publication);
                    Ok(())
                })
            }),
        )
    }
}

fn invalid_profile(message: impl Into<String>) -> AiError {
    ai_error(
        ErrorKind::InvalidRequest,
        ErrorPhase::Prepare,
        DispatchStatus::NotStarted,
        message,
    )
}

fn validate_chat_tool_result_adjacency(request: &LanguageRequest) -> Result<(), AiError> {
    let mut adjacent_calls = BTreeSet::new();
    for message in request.messages() {
        match message.role() {
            MessageRole::Assistant => {
                adjacent_calls.clear();
                adjacent_calls.extend(message.content().iter().filter_map(|content| {
                    let MessageContent::ToolCall(call) = content else {
                        return None;
                    };
                    Some(call.id.as_str())
                }));
            }
            MessageRole::Tool => {
                let MessageContent::ToolResult { call_id, .. } = &message.content()[0] else {
                    unreachable!("tool message validation")
                };
                if !adjacent_calls.remove(call_id.as_str()) {
                    return Err(ai_error(
                        ErrorKind::InvalidRequest,
                        ErrorPhase::Prepare,
                        DispatchStatus::NotStarted,
                        "Chat Completions tool results must immediately follow their assistant tool-call group",
                    ));
                }
            }
            MessageRole::System | MessageRole::Developer | MessageRole::User => {
                adjacent_calls.clear();
            }
        }
    }
    Ok(())
}

async fn build_request_body(
    context: &PrepareContext,
    model: &str,
    request: &LanguageRequest,
    abort: rsi_ai_provider::AbortSignal,
    allow_image_input: bool,
) -> Result<JsonRequestBody, AiError> {
    let (messages, media) = serialize_messages(context, request, abort, allow_image_input).await?;
    let tools = request
        .tools()
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.input_schema(),
                }
            })
        })
        .collect::<Vec<_>>();
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(model.to_owned()));
    body.insert("messages".to_owned(), Value::Array(messages));
    body.insert("stream".to_owned(), Value::Bool(true));
    body.insert("stream_options".to_owned(), json!({"include_usage": true}));
    let settings = request.settings();
    if let Some(value) = settings.max_output_tokens() {
        body.insert("max_tokens".to_owned(), Value::from(value));
    }
    if let Some(value) = settings.temperature() {
        body.insert("temperature".to_owned(), json!(value));
    }
    if let Some(value) = settings.top_p() {
        body.insert("top_p".to_owned(), json!(value));
    }
    if let Some(value) = settings.seed() {
        body.insert("seed".to_owned(), Value::from(value));
    }
    if !settings.stop().is_empty() {
        body.insert("stop".to_owned(), json!(settings.stop()));
    }
    if let Some(value) = settings.reasoning_effort() {
        body.insert("reasoning_effort".to_owned(), json!(value));
    }
    if !tools.is_empty() {
        body.insert("tools".to_owned(), Value::Array(tools));
        body.insert(
            "tool_choice".to_owned(),
            serialize_tool_choice(request.tool_choice()),
        );
    }
    match request.response_format() {
        ResponseFormat::Text => {}
        ResponseFormat::JsonSchema {
            name,
            description,
            schema,
            strict,
        } => {
            body.insert(
                "response_format".to_owned(),
                json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": name,
                        "description": description,
                        "schema": schema,
                        "strict": strict,
                    }
                }),
            );
        }
    }
    json_base64_body(Value::Object(body), media, MAX_PROVIDER_REQUEST_BODY_BYTES)
        .map_err(invalid_request_error)
}

async fn serialize_messages(
    context: &PrepareContext,
    request: &LanguageRequest,
    abort: rsi_ai_provider::AbortSignal,
    allow_image_input: bool,
) -> Result<(Vec<Value>, Vec<JsonBase64Replacement>), AiError> {
    let mut messages = Vec::with_capacity(request.messages().len());
    let mut media = Vec::new();
    let mut request_index = 0;
    while request_index < request.messages().len() {
        let message = &request.messages()[request_index];
        if message.role() == MessageRole::Tool {
            let group_end = request.messages()[request_index..]
                .iter()
                .position(|candidate| candidate.role() != MessageRole::Tool)
                .map_or(request.messages().len(), |offset| request_index + offset);
            let group_len = group_end - request_index;
            let mut tool_messages = Vec::with_capacity(group_len);
            let mut image_messages = Vec::new();
            for tool_message in &request.messages()[request_index..group_end] {
                let image_index = messages.len() + group_len + image_messages.len();
                let (tool, images) = serialize_tool_result(
                    context,
                    tool_message,
                    abort.clone(),
                    allow_image_input,
                    image_index,
                    &mut media,
                )
                .await?;
                tool_messages.push(tool);
                if let Some(images) = images {
                    image_messages.push(images);
                }
            }
            messages.extend(tool_messages);
            messages.extend(image_messages);
            request_index = group_end;
            continue;
        }
        let message_index = messages.len();
        messages.extend(
            serialize_message(
                context,
                message,
                abort.clone(),
                allow_image_input,
                message_index,
                &mut media,
            )
            .await?,
        );
        request_index += 1;
    }
    Ok((messages, media))
}

fn serialize_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".to_owned()),
        ToolChoice::None => Value::String("none".to_owned()),
        ToolChoice::Required => Value::String("required".to_owned()),
        ToolChoice::Specific(name) => {
            json!({"type":"function", "function":{"name":name}})
        }
    }
}

#[allow(clippy::too_many_lines)] // One role-exhaustive serializer keeps wire mapping visible.
async fn serialize_message(
    context: &PrepareContext,
    message: &Message,
    abort: rsi_ai_provider::AbortSignal,
    allow_image_input: bool,
    message_index: usize,
    media_replacements: &mut Vec<JsonBase64Replacement>,
) -> Result<Vec<Value>, AiError> {
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
                        wire_blocks.push(json!({"type":"text", "text":text}));
                    }
                    MessageContent::Image(media) if allow_image_input && role == "user" => {
                        let bytes = context.resolve_media(media, abort.clone()).await?;
                        media_replacements.push(JsonBase64Replacement::new(
                            format!(
                                "/messages/{message_index}/content/{}/image_url/url",
                                wire_blocks.len()
                            ),
                            format!("data:{};base64,", media.mime_type()),
                            bytes,
                        ));
                        wire_blocks.push(json!({
                            "type":"image_url",
                            "image_url": {
                                "url": null
                            }
                        }));
                    }
                    MessageContent::Image(_) | MessageContent::Audio(_) => {
                        return Err(ai_error(
                            ErrorKind::Unsupported,
                            ErrorPhase::Prepare,
                            DispatchStatus::NotStarted,
                            "this Chat Completions deployment does not support the requested media input",
                        ));
                    }
                    MessageContent::Reasoning { .. }
                    | MessageContent::ToolCall(_)
                    | MessageContent::ToolResult { .. } => unreachable!("role validation"),
                }
            }
            Ok(vec![json!({"role":role, "content":wire_blocks})])
        }
        MessageRole::Assistant => {
            let mut text = String::new();
            let mut reasoning = String::new();
            let mut calls = Vec::new();
            for block in message.content() {
                match block {
                    MessageContent::Text { text: value } => text.push_str(value),
                    MessageContent::Reasoning { text: value, .. } => reasoning.push_str(value),
                    MessageContent::ToolCall(call) => calls.push(json!({
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.arguments},
                    })),
                    MessageContent::Image(_) | MessageContent::Audio(_) => {
                        return Err(ai_error(
                            ErrorKind::Unsupported,
                            ErrorPhase::Prepare,
                            DispatchStatus::NotStarted,
                            "Chat Completions assistant history cannot contain media",
                        ));
                    }
                    MessageContent::ToolResult { .. } => unreachable!("role validation"),
                }
            }
            let mut value = Map::new();
            value.insert("role".to_owned(), Value::String("assistant".to_owned()));
            value.insert(
                "content".to_owned(),
                if text.is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                },
            );
            if !calls.is_empty() {
                value.insert("tool_calls".to_owned(), Value::Array(calls));
                if !reasoning.is_empty() {
                    value.insert("reasoning_content".to_owned(), Value::String(reasoning));
                }
            }
            Ok(vec![Value::Object(value)])
        }
        MessageRole::Tool => {
            let (tool, images) = serialize_tool_result(
                context,
                message,
                abort,
                allow_image_input,
                message_index + 1,
                media_replacements,
            )
            .await?;
            match images {
                Some(images) => Ok(vec![tool, images]),
                None => Ok(vec![tool]),
            }
        }
    }
}

async fn serialize_tool_result(
    context: &PrepareContext,
    message: &Message,
    abort: rsi_ai_provider::AbortSignal,
    allow_image_input: bool,
    image_message_index: usize,
    media_replacements: &mut Vec<JsonBase64Replacement>,
) -> Result<(Value, Option<Value>), AiError> {
    let MessageContent::ToolResult {
        call_id, content, ..
    } = &message.content()[0]
    else {
        unreachable!("role validation")
    };
    let mut text = String::new();
    let mut images = Vec::new();
    for block in content {
        match block {
            MessageContent::Text { text: value } => text.push_str(value),
            MessageContent::Image(image) if allow_image_input => images.push(image),
            MessageContent::Image(_) | MessageContent::Audio(_) => {
                return Err(ai_error(
                    ErrorKind::Unsupported,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "Chat Completions tool results require text",
                ));
            }
            _ => unreachable!("tool result validation"),
        }
    }
    let tool = json!({"role":"tool", "tool_call_id":call_id, "content":text});
    if images.is_empty() {
        return Ok((tool, None));
    }
    let mut image_content = Vec::with_capacity(images.len());
    for image in images {
        let bytes = context.resolve_media(image, abort.clone()).await?;
        media_replacements.push(JsonBase64Replacement::new(
            format!(
                "/messages/{image_message_index}/content/{}/image_url/url",
                image_content.len()
            ),
            format!("data:{};base64,", image.mime_type()),
            bytes,
        ));
        image_content.push(json!({
            "type":"image_url",
            "image_url":{"url":null}
        }));
    }
    Ok((tool, Some(json!({"role":"user", "content":image_content}))))
}

#[derive(Debug)]
struct ToolState {
    output_index: u32,
    id: String,
    name: String,
}

#[allow(clippy::too_many_lines)] // One exhaustive stream grammar owns provider ordering.
fn translate_chat_stream(mut input: rsi_ai_transport::SseStream) -> LanguageAdapterStream {
    Box::pin(stream! {
        let mut next_output = 0_u32;
        let mut reasoning = None;
        let mut reasoning_started = false;
        let mut text = None;
        let mut text_started = false;
        let mut tools = BTreeMap::<u32, ToolState>::new();
        let mut finish = None;
        let mut usage = None;
        while let Some(payload) = input.next().await {
            let payload = match payload {
                Ok(payload) => payload,
                Err(error) => {
                    yield Ok(failed(transport_stream_error(error)));
                    return;
                }
            };
            let Ok(chunk) = serde_json::from_str::<ChatCompletionsChunk>(payload.as_str()) else {
                yield Ok(failed(ai_error(
                    ErrorKind::Protocol,
                    ErrorPhase::Stream,
                    DispatchStatus::Dispatched,
                    "provider emitted malformed Chat Completions JSON",
                )));
                return;
            };
            drop(payload);
            if chunk.choices.len() > 1 {
                yield Ok(failed(ai_error(
                    ErrorKind::OutputValidation,
                    ErrorPhase::Stream,
                    DispatchStatus::Dispatched,
                    "provider emitted more than one Chat Completions choice",
                )));
                return;
            }
            if let Some(choice) = chunk.choices.into_iter().next() {
                if let Some(delta) = choice.delta.reasoning_content.filter(|value| !value.is_empty()) {
                    let index = *reasoning.get_or_insert_with(|| {
                        let index = next_output;
                        next_output = next_output.saturating_add(1);
                        index
                    });
                    if !reasoning_started {
                        yield Ok(LanguageEvent::ContentStarted { index, content: ContentStart::Reasoning });
                        reasoning_started = true;
                    }
                    yield Ok(LanguageEvent::ContentDelta { index, delta: ContentDelta::Reasoning(delta) });
                }
                if let Some(delta) = choice.delta.content.filter(|value| !value.is_empty()) {
                    let index = *text.get_or_insert_with(|| {
                        let index = next_output;
                        next_output = next_output.saturating_add(1);
                        index
                    });
                    if !text_started {
                        yield Ok(LanguageEvent::ContentStarted { index, content: ContentStart::Text });
                        text_started = true;
                    }
                    yield Ok(LanguageEvent::ContentDelta { index, delta: ContentDelta::Text(delta) });
                }
                for call in choice.delta.tool_calls {
                    if !tools.contains_key(&call.index) {
                        if usize::try_from(call.index).ok() != Some(tools.len()) {
                            yield Ok(failed(ai_error(
                                ErrorKind::Protocol,
                                ErrorPhase::Stream,
                                DispatchStatus::Dispatched,
                                "tool call indexes are not contiguous",
                            )));
                            return;
                        }
                        if usize::try_from(next_output)
                            .map_or(true, |value| value >= rsi_ai_protocol::MAX_CONTENT_BLOCKS)
                        {
                            yield Ok(failed(ai_error(
                                ErrorKind::OutputValidation,
                                ErrorPhase::Stream,
                                DispatchStatus::Dispatched,
                                "Chat Completions emitted too many content blocks",
                            )));
                            return;
                        }
                        let Some(function) = call.function.as_ref() else {
                            yield Ok(failed(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "new tool call has no function")));
                            return;
                        };
                        let Some(id) = call.id.clone() else {
                            yield Ok(failed(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "new tool call has no id")));
                            return;
                        };
                        let Some(name) = function.name.clone() else {
                            yield Ok(failed(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "new tool call has no name")));
                            return;
                        };
                        let output_index = next_output;
                        next_output = next_output.saturating_add(1);
                        yield Ok(LanguageEvent::ContentStarted {
                            index: output_index,
                            content: ContentStart::ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                kind: rsi_ai_protocol::ToolCallKind::Function,
                            },
                        });
                        tools.insert(call.index, ToolState { output_index, id, name });
                    }
                    let state = tools.get(&call.index).expect("tool inserted");
                    if call.id.as_ref().is_some_and(|id| id != &state.id)
                        || call.function.as_ref().and_then(|f| f.name.as_ref()).is_some_and(|name| name != &state.name)
                    {
                        yield Ok(failed(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "tool call identity changed while streaming")));
                        return;
                    }
                    if let Some(arguments) = call.function.and_then(|function| function.arguments).filter(|value| !value.is_empty()) {
                        yield Ok(LanguageEvent::ContentDelta {
                            index: state.output_index,
                            delta: ContentDelta::ToolArguments(arguments),
                        });
                    }
                }
                if let Some(reason) = choice.finish_reason {
                    if finish.is_some() {
                        yield Ok(failed(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "provider emitted finish_reason more than once")));
                        return;
                    }
                    finish = match map_finish_reason(&reason) {
                        Ok(reason) => Some(reason),
                        Err(error) => {
                            yield Ok(failed(error));
                            return;
                        }
                    };
                }
            }
            if let Some(wire) = chunk.usage {
                if usage.is_some() {
                    yield Ok(failed(ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "provider emitted usage more than once")));
                    return;
                }
                usage = Some(wire.normalized());
            }
        }
        let Some(finish) = finish else {
            yield Ok(failed(ai_error(
                ErrorKind::Protocol,
                ErrorPhase::Stream,
                DispatchStatus::Dispatched,
                "Chat Completions stream ended without finish_reason",
            )));
            return;
        };
        if let Some(index) = reasoning {
            yield Ok(LanguageEvent::ContentFinished { index });
        }
        if let Some(index) = text {
            yield Ok(LanguageEvent::ContentFinished { index });
        }
        for state in tools.into_values() {
            yield Ok(LanguageEvent::ContentFinished { index: state.output_index });
        }
        if let Some(usage) = usage {
            yield Ok(LanguageEvent::Usage { usage });
        }
        yield Ok(LanguageEvent::Finished { reason: finish, replay: None });
    })
}

fn map_finish_reason(reason: &str) -> Result<FinishReason, AiError> {
    match reason {
        "stop" => Ok(FinishReason::Stop),
        "tool_calls" | "function_call" => Ok(FinishReason::ToolCalls),
        "length" | "max_tokens" => Ok(FinishReason::MaxTokens),
        "content_filter" => Ok(FinishReason::ContentFilter),
        _ => Err(ai_error(
            ErrorKind::OutputValidation,
            ErrorPhase::Stream,
            DispatchStatus::Dispatched,
            format!("unsupported provider finish reason `{reason}`"),
        )),
    }
}

async fn http_failure(status: u16, body: rsi_ai_transport::ByteStream) -> AiError {
    let error = provider_http_error(
        status,
        body,
        ErrorPhase::FirstEvent,
        "provider rejected the request",
    )
    .await;
    reclassify_context_limit(error)
}

fn failed(error: AiError) -> LanguageEvent {
    LanguageEvent::Failed {
        error,
        replay: None,
    }
}
