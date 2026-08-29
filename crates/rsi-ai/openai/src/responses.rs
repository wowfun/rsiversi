use super::{
    shared::{authorized_control_request, authorized_json_request, http_failure, http_failure_at},
    *,
};
use std::collections::HashSet;

const DEFERRED_PARSER_NAMESPACE: &str = "openai.responses.deferred_parser";
const DEFERRED_PARSER_VERSION: u32 = 1;

impl LanguageAdapter for OpenAiResponsesAdapter {
    fn describe(&self, model: &str) -> Result<rsi_ai_protocol::LanguageProfile, AiError> {
        let limits = self.config.model_limits(model)?;
        Ok(rsi_ai_protocol::LanguageProfile::new(
            limits.context_window_tokens(),
            limits.default_output_reserve_tokens(),
            limits.max_output_reserve_tokens(),
            rsi_ai_protocol::ToolDialect::Responses,
            true,
            rsi_ai_protocol::ImageToolResultCapability::Yes(
                rsi_ai_protocol::ImageToolResultMode::FunctionOutput,
            ),
            vec![
                rsi_ai_protocol::ProviderExtensionFormat::new("openai.responses.replay", 0)
                    .expect("static OpenAI replay extension is valid"),
            ],
        )
        .expect("static OpenAI Responses profile is valid"))
    }

    fn validate_request(&self, model: &str, request: &LanguageRequest) -> Result<(), AiError> {
        self.config.model_limits(model)?;
        validate_responses_request(request)
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
                        MAX_DEFERRED_CONTROL_BODY_BYTES,
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
        if let Err(error) = self.validate_request(&model, &request) {
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
                    context.release_resolved_media();
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
            checkpoint.validate().map_err(|error| {
                ai_error(
                    ErrorKind::InvalidRequest,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    error.to_string(),
                )
            })?;
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
    for message in request.messages() {
        match message.role() {
            MessageRole::Assistant => {
                for content in message.content() {
                    match content {
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
                        MessageContent::Text { .. } | MessageContent::ToolCall(_) => {}
                        MessageContent::ToolResult { .. } => {
                            unreachable!("validated assistant message")
                        }
                    }
                }
            }
            MessageRole::User => {
                for content in message.content() {
                    if let MessageContent::Audio(media) = content {
                        audio_format(media.mime_type())?;
                    }
                }
            }
            MessageRole::Tool => {
                let MessageContent::ToolResult { content, .. } = &message.content()[0] else {
                    unreachable!("validated tool message")
                };
                if content
                    .iter()
                    .any(|content| matches!(content, MessageContent::Audio(_)))
                {
                    return Err(ai_error(
                        ErrorKind::Unsupported,
                        ErrorPhase::Prepare,
                        DispatchStatus::NotStarted,
                        "OpenAI function outputs do not accept audio in v1",
                    ));
                }
            }
            MessageRole::System | MessageRole::Developer => {}
        }
    }
    for extension in request.extensions() {
        responses_replay_id(extension)?;
    }
    Ok(())
}

fn responses_replay_id(extension: &ProviderExtension) -> Result<&str, AiError> {
    if extension.namespace != "openai.responses.replay" || extension.version != 0 {
        return Err(ai_error(
            ErrorKind::Unsupported,
            ErrorPhase::Prepare,
            DispatchStatus::NotStarted,
            "OpenAI Responses received an unsupported extension",
        ));
    }
    extension
        .value
        .get("response_id")
        .and_then(Value::as_str)
        .filter(|response_id| !response_id.is_empty())
        .ok_or_else(|| {
            ai_error(
                ErrorKind::InvalidRequest,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "OpenAI replay extension has no nonempty response_id",
            )
        })
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn poll(&mut self, abort: AbortSignal) -> Result<DeferredStatus, AiError> {
        let operation_id = self
            .checkpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .operation_id()
            .to_owned();
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe_status(status)
            .map_err(|error| deferred_checkpoint_error(ErrorPhase::DeferredPoll, error))?;
        Ok(status)
    }

    async fn resume(
        &mut self,
        abort: AbortSignal,
    ) -> Result<DeferredLanguageAdapterStream, AiError> {
        let checkpoint = self.checkpoint();
        if checkpoint.event_stream_terminal() {
            return Err(ai_error(
                ErrorKind::InvalidRequest,
                ErrorPhase::DeferredPoll,
                DispatchStatus::NotStarted,
                "terminal deferred event cursor cannot open another stream",
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
            decode_sse(
                response.body,
                SseTermination::Eof,
                MAX_DEFERRED_CONTROL_BODY_BYTES,
            ),
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe_status(status)
            .map_err(|error| deferred_checkpoint_error(ErrorPhase::DeferredCancel, error))?;
        Ok(status)
    }
}

fn encoded_path(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT_ENCODE_SET).to_string()
}

#[derive(Deserialize)]
struct DeferredControlProjection {
    id: Option<String>,
    status: Option<String>,
}

async fn collect_json_control(
    body: ByteStream,
    phase: ErrorPhase,
) -> Result<DeferredControlProjection, AiError> {
    let limits = JsonProjectionLimits::new(MAX_DEFERRED_CONTROL_BODY_BYTES)
        .and_then(|limits| limits.with_top_level_string("id", rsi_ai_protocol::MAX_ID_BYTES))
        .and_then(|limits| limits.with_top_level_string("status", 16))
        .expect("OpenAI deferred-control projection limits are valid");
    project_json_body(body, limits).await.map_err(|error| {
        let kind = match error.code() {
            "json.project_limit" => ErrorKind::OutputValidation,
            "json.project" => ErrorKind::Protocol,
            "http.cancelled" => ErrorKind::Cancelled,
            "http.timeout" => ErrorKind::Timeout,
            _ => ErrorKind::Transport,
        };
        ai_error(
            kind,
            phase,
            DispatchStatus::Dispatched,
            format!("OpenAI deferred response body failed: {error}"),
        )
    })
}

fn parse_provider_json(payload: &str, phase: ErrorPhase) -> Result<Value, AiError> {
    let value = serde_json::from_str(payload).map_err(|_| {
        ai_error(
            ErrorKind::Protocol,
            phase,
            DispatchStatus::Dispatched,
            "OpenAI Responses emitted malformed JSON",
        )
    })?;
    validate_provider_json(value, phase)
}

fn validate_provider_json(value: Value, phase: ErrorPhase) -> Result<Value, AiError> {
    validate_json_structure(&value).map_err(|_| {
        ai_error(
            ErrorKind::Protocol,
            phase,
            DispatchStatus::Dispatched,
            "OpenAI response exceeds the JSON structure limits",
        )
    })?;
    Ok(value)
}

fn deferred_response_identity(
    value: &DeferredControlProjection,
    expected_id: Option<&str>,
    phase: ErrorPhase,
) -> Result<(String, DeferredStatus), AiError> {
    let operation_id = value.id.as_deref().ok_or_else(|| {
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
    let status_value = value.status.as_deref().ok_or_else(|| {
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
) -> Result<JsonRequestBody, AiError> {
    let mut input = Vec::new();
    let mut media_replacements = Vec::new();
    let custom_calls = request
        .messages()
        .iter()
        .flat_map(rsi_ai_protocol::Message::content)
        .filter_map(|content| match content {
            MessageContent::ToolCall(call) if call.kind == ToolCallKind::Freeform => {
                Some(call.id.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for message in request.messages() {
        match message.role() {
            MessageRole::System | MessageRole::Developer | MessageRole::User => {
                let input_index = input.len();
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
                            media_replacements.push(JsonBase64Replacement::new(
                                format!(
                                    "/input/{input_index}/content/{}/image_url",
                                    wire_blocks.len()
                                ),
                                format!("data:{};base64,", media.mime_type()),
                                bytes,
                            ));
                            wire_blocks.push(json!({
                                "type":"input_image",
                                "image_url":null
                            }));
                        }
                        MessageContent::Audio(media) => {
                            let bytes = context.resolve_media(media, abort.clone()).await?;
                            media_replacements.push(JsonBase64Replacement::new(
                                format!(
                                    "/input/{input_index}/content/{}/input_audio/data",
                                    wire_blocks.len()
                                ),
                                "",
                                bytes,
                            ));
                            wire_blocks.push(json!({
                                "type":"input_audio",
                                "input_audio": {
                                    "data": null,
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
                            if call.kind == ToolCallKind::Freeform {
                                input.push(json!({
                                    "type":"custom_tool_call",
                                    "call_id":call.id,
                                    "name":call.name,
                                    "input":call.arguments,
                                }));
                            } else {
                                input.push(json!({
                                    "type":"function_call",
                                    "call_id":call.id,
                                    "name":call.name,
                                    "arguments":call.arguments,
                                }));
                            }
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
                let input_index = input.len();
                let mut output = String::new();
                let mut rich_output = Vec::new();
                let mut has_media = false;
                for block in content {
                    match block {
                        MessageContent::Text { text } => {
                            output.push_str(text);
                            rich_output.push(json!({"type":"input_text", "text":text}));
                        }
                        MessageContent::Image(media) => {
                            has_media = true;
                            let bytes = context.resolve_media(media, abort.clone()).await?;
                            media_replacements.push(JsonBase64Replacement::new(
                                format!(
                                    "/input/{input_index}/output/{}/image_url",
                                    rich_output.len()
                                ),
                                format!("data:{};base64,", media.mime_type()),
                                bytes,
                            ));
                            rich_output.push(json!({
                                "type":"input_image",
                                "image_url":null
                            }));
                        }
                        MessageContent::Audio(_) => {
                            return Err(ai_error(
                                ErrorKind::Unsupported,
                                ErrorPhase::Prepare,
                                DispatchStatus::NotStarted,
                                "OpenAI function outputs do not accept audio in v1",
                            ));
                        }
                        _ => unreachable!("tool result validation"),
                    }
                }
                let output = if has_media {
                    Value::Array(rich_output)
                } else {
                    Value::String(output)
                };
                if custom_calls.contains(call_id) {
                    input.push(json!({
                        "type":"custom_tool_call_output", "call_id":call_id, "output":output
                    }));
                } else {
                    input.push(json!({
                        "type":"function_call_output", "call_id":call_id, "output":output
                    }));
                }
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
                        if let Some(freeform) = tool.freeform() {
                            let syntax = match freeform.format() {
                                rsi_ai_protocol::FreeformFormat::Lark => "lark",
                            };
                            json!({
                                "type":"custom",
                                "name":tool.name(),
                                "description":tool.description(),
                                "format": {
                                    "type":"grammar",
                                    "syntax":syntax,
                                    "definition":freeform.grammar(),
                                },
                            })
                        } else {
                            json!({
                                "type":"function",
                                "name":tool.name(),
                                "description":tool.description(),
                                "parameters":tool.input_schema(),
                                "strict":true,
                            })
                        }
                    })
                    .collect(),
            ),
        );
        body.insert(
            "tool_choice".to_owned(),
            responses_tool_choice(request.tool_choice(), request.tools()),
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
        let response_id = responses_replay_id(extension)?;
        body.insert(
            "previous_response_id".to_owned(),
            Value::String(response_id.to_owned()),
        );
    }
    json_base64_body(
        Value::Object(body),
        media_replacements,
        MAX_PROVIDER_REQUEST_BODY_BYTES,
    )
    .map_err(invalid_request_error)
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

fn responses_tool_choice(choice: &ToolChoice, tools: &[ToolDefinition]) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".to_owned()),
        ToolChoice::None => Value::String("none".to_owned()),
        ToolChoice::Required => Value::String("required".to_owned()),
        ToolChoice::Specific(name) => {
            let kind = if tools
                .iter()
                .any(|tool| tool.name() == name && tool.freeform().is_some())
            {
                "custom"
            } else {
                "function"
            };
            json!({"type":kind, "name":name})
        }
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
        if state.namespace != DEFERRED_PARSER_NAMESPACE || state.version != DEFERRED_PARSER_VERSION
        {
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
        let mut indexes = HashSet::new();
        for block in stored.open {
            validate_parser_block_key(&block.key)?;
            if block.index >= stored.next_index
                || !indexes.insert(block.index)
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
            namespace: DEFERRED_PARSER_NAMESPACE.to_owned(),
            version: DEFERRED_PARSER_VERSION,
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
            "response.output_text.delta"
            | "response.reasoning_summary_text.delta"
            | "response.refusal.delta" => {
                let item_id =
                    required_response_string(event, "item_id", "OpenAI text delta has no item_id")?;
                validate_provider_item_id(item_id)?;
                let block_kind = if kind.contains("reasoning") {
                    OpenBlockKind::Reasoning
                } else {
                    OpenBlockKind::Text
                };
                let key = parser_block_key(
                    item_id,
                    block_kind,
                    Some(
                        event
                            .get("content_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    ),
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
                if delta.is_empty() {
                    return Ok(output);
                }
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
                let item_type = item.get("type").and_then(Value::as_str);
                if matches!(item_type, Some("function_call" | "custom_tool_call")) {
                    let item_id =
                        required_response_string(item, "id", "OpenAI tool call has no item id")?;
                    validate_provider_item_id(item_id)?;
                    let call_id = required_response_string(
                        item,
                        "call_id",
                        "OpenAI tool call has no call_id",
                    )?;
                    let name =
                        required_response_string(item, "name", "OpenAI tool call has no name")?;
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
                            kind: if item_type == Some("custom_tool_call") {
                                ToolCallKind::Freeform
                            } else {
                                ToolCallKind::Function
                            },
                        },
                    });
                    let key = parser_block_key(item_id, OpenBlockKind::Tool, None);
                    if self
                        .open
                        .insert(
                            key,
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
                    let arguments_field = if item_type == Some("custom_tool_call") {
                        "input"
                    } else {
                        "arguments"
                    };
                    if let Some(arguments) = item
                        .get(arguments_field)
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
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let item_id = required_response_string(
                    event,
                    "item_id",
                    "OpenAI function delta has no item_id",
                )?;
                validate_provider_item_id(item_id)?;
                let key = parser_block_key(item_id, OpenBlockKind::Tool, None);
                let block = self.open.get(&key).ok_or_else(|| {
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
                if delta.is_empty() {
                    return Ok(output);
                }
                output.push(LanguageEvent::ContentDelta {
                    index: block.index,
                    delta: ContentDelta::ToolArguments(delta.to_owned()),
                });
            }
            "response.completed" => {
                let response = event.get("response").unwrap_or(&Value::Null);
                if response
                    .get("status")
                    .is_some_and(|status| status.as_str() != Some("completed"))
                {
                    return Err(ai_error(
                        ErrorKind::Protocol,
                        ErrorPhase::Stream,
                        DispatchStatus::Dispatched,
                        "OpenAI Responses response.completed conflicts with response.status",
                    ));
                }
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
                if is_max_output_incomplete(event) {
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
                output.push(language_failed(responses_failure(event)));
            }
            "response.created"
            | "response.queued"
            | "response.in_progress"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.done"
            | "response.function_call_arguments.done"
            | "response.custom_tool_call_input.done"
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

fn responses_failure(event: &Value) -> AiError {
    let details = if event.get("type").and_then(Value::as_str) == Some("error") {
        Some(event)
    } else {
        event
            .get("response")
            .and_then(|response| response.get("error"))
            .filter(|error| !error.is_null())
            .or_else(|| event.get("error"))
            .filter(|error| !error.is_null())
    };
    let code = details
        .and_then(|details| details.get("code"))
        .and_then(Value::as_str);
    let summary = details
        .and_then(|details| details.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("OpenAI Responses did not complete successfully");
    let kind = match code {
        Some("context_length_exceeded") => ErrorKind::ContextLimit,
        Some("invalid_api_key") => ErrorKind::Authentication,
        _ => ErrorKind::Server,
    };
    let error = ai_error(
        kind,
        ErrorPhase::Stream,
        DispatchStatus::Dispatched,
        summary,
    );
    match code {
        Some(code) => error.with_provider_code(code).unwrap_or_else(|_| {
            ai_error(
                ErrorKind::Protocol,
                ErrorPhase::Stream,
                DispatchStatus::Dispatched,
                "OpenAI Responses returned an invalid error code",
            )
        }),
        None => error,
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

fn validate_parser_block_key(value: &str) -> Result<(), AiError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ai_error(
            ErrorKind::Protocol,
            ErrorPhase::DeferredPoll,
            DispatchStatus::NotStarted,
            "OpenAI deferred checkpoint parser state has an invalid block key",
        ));
    }
    Ok(())
}

fn parser_block_key(item_id: &str, kind: OpenBlockKind, content_index: Option<u64>) -> String {
    let mut digest = Sha256::new();
    digest.update(match kind {
        OpenBlockKind::Text => [0],
        OpenBlockKind::Reasoning => [1],
        OpenBlockKind::Tool => [2],
    });
    digest.update(
        u64::try_from(item_id.len())
            .expect("validated provider item id length fits u64")
            .to_le_bytes(),
    );
    digest.update(item_id.as_bytes());
    if let Some(content_index) = content_index {
        digest.update(content_index.to_le_bytes());
    }
    hex::encode(digest.finalize())
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
            let event = match parse_provider_json(&payload, ErrorPhase::Stream) {
                Ok(event) => event,
                Err(error) => {
                    yield Ok(language_failed(error));
                    return;
                }
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
            let event = parse_provider_json(&payload, ErrorPhase::Stream)?;
            let sequence = event.get("sequence_number").and_then(Value::as_u64).ok_or_else(|| {
                ai_error(ErrorKind::Protocol, ErrorPhase::DeferredPoll, DispatchStatus::Dispatched, "OpenAI background stream event has no sequence_number")
            })?;
            let kind = event.get("type").and_then(Value::as_str).ok_or_else(|| {
                ai_error(ErrorKind::Protocol, ErrorPhase::Stream, DispatchStatus::Dispatched, "OpenAI Responses event has no type")
            })?;
            let events = parser.apply(&event)?;
            let batch = commit_deferred_batch(
                &checkpoint,
                events,
                kind,
                &event,
                sequence,
                parser.provider_state(),
            )?;
            let event_stream_terminal = batch.checkpoint().event_stream_terminal();
            yield batch;
            if event_stream_terminal {
                return;
            }
        }
    })
}

fn commit_deferred_batch(
    checkpoint: &Mutex<DeferredLanguageCheckpoint>,
    events: Vec<LanguageEvent>,
    kind: &str,
    event: &Value,
    sequence: u64,
    provider_state: ProviderExtension,
) -> Result<DeferredLanguageBatch, AiError> {
    let mut current = checkpoint
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let event_status = deferred_event_status(kind, event, current.status())?;
    let status = if current.status().is_terminal() && !event_status.is_terminal() {
        current.status()
    } else {
        event_status
    };
    let event_stream_terminal =
        current.event_stream_terminal() || events.iter().any(is_language_terminal_event);
    let mut next = current.clone();
    next.advance(
        status,
        event_stream_terminal,
        sequence,
        Some(provider_state),
    )
    .map_err(|error| deferred_checkpoint_error(ErrorPhase::DeferredPoll, error))?;
    let batch = DeferredLanguageBatch::new(events, next)
        .map_err(|error| deferred_checkpoint_error(ErrorPhase::DeferredPoll, error))?;
    *current = batch.checkpoint().clone();
    Ok(batch)
}

fn deferred_event_status(
    kind: &str,
    event: &Value,
    current: DeferredStatus,
) -> Result<DeferredStatus, AiError> {
    match kind {
        "response.queued" => event
            .get("response")
            .and_then(|response| response.get("status"))
            .and_then(Value::as_str)
            .map(deferred_status)
            .transpose()
            .map(|status| status.unwrap_or(DeferredStatus::Queued)),
        "response.created" => event
            .get("response")
            .and_then(|response| response.get("status"))
            .and_then(Value::as_str)
            .map(deferred_status)
            .transpose()
            .map(|status| status.unwrap_or(DeferredStatus::InProgress)),
        "response.completed" => Ok(DeferredStatus::Completed),
        "response.incomplete" if is_max_output_incomplete(event) => Ok(DeferredStatus::Completed),
        "response.failed" | "response.incomplete" | "error" => Ok(DeferredStatus::Failed),
        _ if current == DeferredStatus::Queued => Ok(DeferredStatus::InProgress),
        _ => Ok(current),
    }
}

fn is_max_output_incomplete(event: &Value) -> bool {
    event
        .get("response")
        .and_then(|response| response.get("incomplete_details"))
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
        == Some("max_output_tokens")
}

fn is_language_terminal_event(event: &LanguageEvent) -> bool {
    matches!(
        event,
        LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_queued_event_preserves_deferred_status() {
        let event = json!({
            "type": "response.queued",
            "response": {"id": "resp-1", "status": "queued"}
        });
        assert_eq!(
            deferred_event_status("response.queued", &event, DeferredStatus::Queued,)
                .expect("documented queued event"),
            DeferredStatus::Queued
        );
    }
}
