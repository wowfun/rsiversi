//! `DeepSeek` endpoint policy with default stateless Responses and explicit Chat support.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // AiError carries the public failure taxonomy.

use std::{collections::BTreeMap, fmt, sync::Arc};

mod fields;
pub use fields::{
    AcceptedFields, DeepSeekFieldSource, DeepSeekFieldSourceContract, FieldPreparation,
    PreparedFields,
};

use rsi_ai_openai::{OpenAiConfig, OpenAiResponsesAdapter, ResponsesState};
use rsi_ai_openai_compatible::{
    ChatCompletionsAdapter, ChatCompletionsConfig, DeveloperMessageRole,
};
use rsi_ai_protocol::{
    AiError, LanguageModelLimits, LanguageRequest, MessageContent, MessageRole, ToolCallKind,
};
use rsi_ai_provider::{
    AdapterFuture, LanguageAdapter, LanguageAdapterStream, LanguageRegistrarContract,
    PrepareContext, Prepared, ProviderPublication, ProviderRegistration,
};
use rsi_ai_transport::HttpTransport;
use serde::Deserialize;
/// Lists candidates at this provider's endpoint without registering a route.
pub async fn discover_models(
    transport: &dyn HttpTransport,
    endpoint: &str,
    secret: &rsi_credentials_protocol::SecretValue,
) -> Result<Vec<rsi_ai_protocol::DiscoveredModel>, AiError> {
    rsi_ai_openai::discovery::list(transport, endpoint, "/models", secret).await
}
use futures_util::StreamExt as _;
use sha2::{Digest as _, Sha256};

/// Explicit wire selection; no fallback or automatic retry changes protocols.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum DeepSeekProtocol {
    /// Stateless `OpenAI` Responses, the default protocol.
    #[default]
    Responses,
    /// Explicit legacy Chat Completions wire.
    ChatCompletions,
}

impl DeepSeekProtocol {
    const fn name(self) -> &'static str {
        match self {
            Self::Responses => "openai-responses",
            Self::ChatCompletions => "chat-completions",
        }
    }
}

/// Fixed endpoint configuration for one `DeepSeek` deployment.
#[derive(Clone, Debug)]
pub struct DeepSeekConfig {
    chat: ChatCompletionsConfig,
    responses: OpenAiConfig,
    protocol: DeepSeekProtocol,
    default_output_tokens: BTreeMap<String, u32>,
    image_input: BTreeMap<String, bool>,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self::with_endpoint("https://api.deepseek.com").expect("static DeepSeek endpoint")
    }
}

impl DeepSeekConfig {
    /// Overrides the origin for an enterprise gateway or a loopback test server.
    #[allow(clippy::missing_panics_doc)] // The only expect validates the constant `off` ID.
    pub fn with_endpoint(endpoint: impl Into<String>) -> Result<Self, AiError> {
        let endpoint = endpoint.into();
        let chat = ChatCompletionsConfig::new(endpoint.clone())?
            .with_stream_idle_timeout(std::time::Duration::from_mins(5))?
            .with_path("/chat/completions")?
            .with_disabled_reasoning_alias(
                rsi_ai_protocol::ReasoningEffortId::new("off").expect("static effort ID"),
            )
            .with_developer_role(DeveloperMessageRole::System);
        let responses = OpenAiConfig::new(endpoint)?
            .with_stream_idle_timeout(std::time::Duration::from_mins(5))?
            .with_responses_options("/responses", ResponsesState::Stateless, MessageRole::System)?
            .with_disabled_reasoning_alias(
                rsi_ai_protocol::ReasoningEffortId::new("off").expect("static effort ID"),
            );
        Ok(Self {
            chat,
            responses,
            protocol: DeepSeekProtocol::default(),
            default_output_tokens: BTreeMap::new(),
            image_input: BTreeMap::new(),
        })
    }

    #[must_use]
    /// Selects a protocol before the deployment is prepared.
    pub const fn with_protocol(mut self, protocol: DeepSeekProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// Overrides the default image support of an exact model.
    pub fn with_image_input(mut self, model: &str, enabled: bool) -> Result<Self, AiError> {
        rsi_ai_protocol::validate_identifier("model", model)
            .map_err(rsi_ai_transport::invalid_request_error)?;
        self.image_input.insert(model.to_owned(), enabled);
        Ok(self)
    }

    /// Overrides a model's request cap before installing its capacity profile.
    pub fn with_default_output_tokens(
        mut self,
        model: impl Into<String>,
        cap: u32,
    ) -> Result<Self, AiError> {
        let model = model.into();
        rsi_ai_protocol::validate_identifier("model", &model)
            .map_err(rsi_ai_transport::invalid_request_error)?;
        rsi_ai_protocol::LanguageSettings::default()
            .with_max_output_tokens(cap)
            .map_err(rsi_ai_transport::invalid_request_error)?;
        self.default_output_tokens.insert(model, cap);
        Ok(self)
    }

    /// Overrides the finite SSE idle allowance for this deployment.
    pub fn with_stream_idle_timeout(
        mut self,
        timeout: std::time::Duration,
    ) -> Result<Self, AiError> {
        self.chat = self.chat.with_stream_idle_timeout(timeout)?;
        self.responses = self.responses.with_stream_idle_timeout(timeout)?;
        Ok(self)
    }

    /// Adds one exact model-capacity profile to this deployment.
    pub fn with_model_profile(
        mut self,
        model: impl Into<String>,
        limits: LanguageModelLimits,
    ) -> Result<Self, AiError> {
        let model = model.into();
        let cap = self
            .default_output_tokens
            .get(&model)
            .copied()
            .unwrap_or(256_000.min(limits.max_output_reserve_tokens()));
        if cap > limits.max_output_reserve_tokens() {
            return Err(rsi_ai_transport::invalid_request_error(
                "DeepSeek default_output_tokens exceeds the model's declared maximum output reserve",
            ));
        }
        self.default_output_tokens.insert(model.clone(), cap);
        self.chat = self.chat.with_model_profile(model.clone(), limits)?;
        self.responses = self.responses.with_model_profile(model, limits)?;
        Ok(self)
    }
    /// Declares exact `DeepSeek` effort choices for a configured model.
    #[allow(clippy::missing_panics_doc)] // The only expect constructs a fixed, valid error.
    pub fn with_reasoning_efforts(
        mut self,
        model: impl Into<String>,
        efforts: rsi_ai_protocol::ReasoningEffortProfile,
    ) -> Result<Self, AiError> {
        if efforts
            .supported()
            .iter()
            .any(|effort| !matches!(effort.as_str(), "off" | "low" | "high" | "max"))
        {
            return Err(AiError::new(
                rsi_ai_protocol::ErrorKind::Unsupported,
                rsi_ai_protocol::ErrorPhase::Prepare,
                rsi_ai_protocol::DispatchStatus::NotStarted,
                "DeepSeek effort choices are off, low, high or max",
            )
            .expect("static error"));
        }
        let model = model.into();
        self.chat = self
            .chat
            .with_reasoning_efforts(model.clone(), efforts.clone())?;
        self.responses = self.responses.with_reasoning_efforts(model, efforts)?;
        Ok(self)
    }
}

/// One generation-pinned `DeepSeek` adapter with provider-owned compatibility admission.
#[derive(Clone, Debug)]
pub struct DeepSeekAdapter {
    inner: Arc<dyn LanguageAdapter>,
    protocol: DeepSeekProtocol,
    default_output_tokens: BTreeMap<String, u32>,
    image_input: BTreeMap<String, bool>,
}

impl DeepSeekAdapter {
    /// Binds `DeepSeek` endpoint policy to the no-retry HTTP transport.
    pub fn new(config: DeepSeekConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            inner: match config.protocol {
                DeepSeekProtocol::Responses => {
                    Arc::new(OpenAiResponsesAdapter::new(config.responses, transport))
                }
                DeepSeekProtocol::ChatCompletions => {
                    Arc::new(ChatCompletionsAdapter::new(config.chat, transport))
                }
            },
            protocol: config.protocol,
            default_output_tokens: config.default_output_tokens,
            image_input: config.image_input,
        }
    }

    fn accepts_images(&self, model: &str) -> bool {
        self.image_input
            .get(model)
            .copied()
            .unwrap_or(model == "deepseek-flash")
    }
}

impl LanguageAdapter for DeepSeekAdapter {
    fn models(&self) -> &rsi_ai_protocol::LanguageModelProfiles {
        self.inner.models()
    }

    fn describe(&self, model: &str) -> Result<rsi_ai_protocol::LanguageProfile, AiError> {
        let mut profile = self.inner.describe(model)?;
        if !self.accepts_images(model) {
            profile =
                profile.with_image_tool_result(rsi_ai_protocol::ImageToolResultCapability::No);
        }
        profile
            .with_default_output_tokens(self.default_output_tokens.get(model).copied())
            .map_err(rsi_ai_transport::invalid_request_error)
    }

    fn validate_request(&self, model: &str, request: &LanguageRequest) -> Result<(), AiError> {
        if self.protocol == DeepSeekProtocol::ChatCompletions && request.settings().seed().is_some()
        {
            return Err(rsi_ai_protocol::AiError::new(
                rsi_ai_protocol::ErrorKind::Unsupported,
                rsi_ai_protocol::ErrorPhase::Prepare,
                rsi_ai_protocol::DispatchStatus::NotStarted,
                "DeepSeek Chat does not support seed controls",
            )
            .expect("static DeepSeek setting error"));
        }
        validate_model_input(request, self.protocol, self.accepts_images(model))?;
        self.inner.validate_request(model, request)
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
        let pending = self.inner.prepare(context, model, request);
        Box::pin(async move {
            let prepared = pending.await?;
            Ok(Prepared::new(prepared.snapshot().clone(), move |abort| {
                Box::pin(async move {
                    let mut input = prepared.start(abort).await?;
                    let output: LanguageAdapterStream = Box::pin(async_stream::stream! {
                        let mut emitted = false;
                        while let Some(event) = input.next().await {
                            match &event {
                                Ok(rsi_ai_protocol::LanguageEvent::ContentDelta { delta, .. }) => {
                                    emitted |= match delta {
                                        rsi_ai_protocol::ContentDelta::Text(value)
                                        | rsi_ai_protocol::ContentDelta::Reasoning(value)
                                        | rsi_ai_protocol::ContentDelta::ToolArguments(value) => !value.is_empty(),
                                    };
                                }
                                Ok(rsi_ai_protocol::LanguageEvent::Failed { error, replay })
                                    if error.kind() == rsi_ai_protocol::ErrorKind::Protocol
                                        && matches!(error.provider_code(), Some("sse.missing_terminal" | "sse.missing_done" | "sse.incomplete_frame")) => {
                                    yield Ok(rsi_ai_protocol::LanguageEvent::Failed {
                                        error: error.clone().with_kind(if emitted { rsi_ai_protocol::ErrorKind::Transport } else { rsi_ai_protocol::ErrorKind::EmptyResponse }), replay: replay.clone() });
                                    return;
                                }
                                Ok(rsi_ai_protocol::LanguageEvent::Finished { reason: rsi_ai_protocol::FinishReason::Stop, replay }) if !emitted => {
                                    yield Ok(rsi_ai_protocol::LanguageEvent::Failed {
                                        error: rsi_ai_transport::provider_error(rsi_ai_protocol::ErrorKind::EmptyResponse,
                                            rsi_ai_protocol::ErrorPhase::Stream, rsi_ai_protocol::DispatchStatus::Dispatched,
                                            "DeepSeek completed without output"), replay: replay.clone() });
                                    return;
                                }
                                _ => {}
                            }
                            yield event;
                        }
                    });
                    Ok(output)
                })
            }))
        })
    }
}

fn validate_model_input(
    request: &LanguageRequest,
    protocol: DeepSeekProtocol,
    images: bool,
) -> Result<(), AiError> {
    const CUSTOM: &str = "DeepSeek Responses limits custom tools to apply_patch";
    let unsupported = |reason| {
        rsi_ai_transport::provider_error(
            rsi_ai_protocol::ErrorKind::Unsupported,
            rsi_ai_protocol::ErrorPhase::Prepare,
            rsi_ai_protocol::DispatchStatus::NotStarted,
            reason,
        )
    };
    let responses = protocol == DeepSeekProtocol::Responses;
    if responses
        && request
            .tools()
            .iter()
            .any(|tool| tool.freeform().is_some() && tool.name() != "apply_patch")
    {
        return Err(unsupported(CUSTOM));
    }
    for message in request.messages() {
        for block in message.content() {
            let parts = match block {
                MessageContent::ToolResult { content, .. } => content.as_slice(),
                item => std::slice::from_ref(item),
            };
            for part in parts {
                let reason = match part {
                    MessageContent::Image(_) if !images => {
                        "Selected DeepSeek model does not accept image input"
                    }
                    MessageContent::Image(_) if message.role() == MessageRole::Assistant => {
                        "DeepSeek accepts images in user messages and tool results"
                    }
                    MessageContent::Audio(_) => "DeepSeek does not accept audio input",
                    MessageContent::ToolCall(call)
                        if responses
                            && call.kind == ToolCallKind::Freeform
                            && call.name != "apply_patch" =>
                    {
                        CUSTOM
                    }
                    _ => continue,
                };
                return Err(unsupported(reason));
            }
        }
    }
    Ok(())
}

/// Finite phase deadlines, in milliseconds, for `DeepSeek` streaming requests.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[allow(clippy::struct_field_names)] // Serialized fields explicitly state their time unit.
struct DeepSeekTimeouts {
    connect_ms: u64,
    headers_ms: u64,
    idle_ms: u64,
}
impl Default for DeepSeekTimeouts {
    fn default() -> Self {
        Self {
            connect_ms: 10_000,
            headers_ms: 300_000,
            idle_ms: 300_000,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeepSeekPluginConfig {
    deployment: String,
    credential: rsi_credentials_protocol::CredentialRef,
    endpoint: String,
    #[serde(default)]
    protocol: DeepSeekProtocol,
    language_models: BTreeMap<String, LanguageModelLimits>,
    #[serde(default)]
    image_input: BTreeMap<String, bool>,
    #[serde(default)]
    http_timeouts: DeepSeekTimeouts,
    #[serde(default)]
    default_output_tokens: BTreeMap<String, u32>,
    #[serde(default)]
    reasoning_efforts: BTreeMap<String, rsi_ai_protocol::ReasoningEffortProfile>,
}

impl DeepSeekPluginConfig {
    fn apply_efforts(&self, mut adapter: DeepSeekConfig) -> Result<DeepSeekConfig, AiError> {
        for model in self.language_models.keys() {
            if !self.reasoning_efforts.contains_key(model)
                && matches!(model.as_str(), "deepseek-flash" | "deepseek-v4-pro")
            {
                let supported = ["off", "low", "high", "max"]
                    .into_iter()
                    .map(rsi_ai_protocol::ReasoningEffortId::new)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(rsi_ai_transport::invalid_request_error)?;
                let default = supported[2].clone();
                adapter = adapter.with_reasoning_efforts(
                    model,
                    rsi_ai_protocol::ReasoningEffortProfile::new(supported, Some(default))
                        .map_err(rsi_ai_transport::invalid_request_error)?,
                )?;
            }
        }
        for (model, efforts) in &self.reasoning_efforts {
            adapter = adapter.with_reasoning_efforts(model, efforts.clone())?;
        }
        Ok(adapter)
    }
}

#[derive(Debug)]
struct PreparedDeepSeekPlugin {
    config: DeepSeekPluginConfig,
    adapter: DeepSeekConfig,
}

/// Ordinary plugin factory for one explicit `DeepSeek` deployment.
#[derive(Clone, Default)]
pub struct DeepSeekFactory {
    transport: Option<Arc<dyn HttpTransport>>,
}

impl DeepSeekFactory {
    /// Uses an injected no-retry transport for deterministic embedders and tests.
    #[must_use]
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport: Some(transport),
        }
    }
}

impl fmt::Debug for DeepSeekFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeepSeekFactory")
            .field("injected_transport", &self.transport.is_some())
            .finish()
    }
}

#[async_trait::async_trait]
impl rsi_meta::PluginFactory for DeepSeekFactory {
    fn prepare(
        &self,
        desired: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        let config: DeepSeekPluginConfig = serde_json::from_value(desired.clone())
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        rsi_ai_protocol::validate_identifier("deployment", &config.deployment)
            .map_err(rsi_meta::MetaError::InvalidInput)?;
        config
            .credential
            .validate()
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        if config.language_models.is_empty() {
            return Err(rsi_meta::MetaError::InvalidInput(
                "DeepSeek deployment requires a nonempty exact model map".into(),
            ));
        }
        if [
            config.http_timeouts.connect_ms,
            config.http_timeouts.headers_ms,
            config.http_timeouts.idle_ms,
        ]
        .contains(&0)
        {
            return Err(rsi_meta::MetaError::InvalidInput(
                "DeepSeek HTTP timeouts must be nonzero".into(),
            ));
        }
        let mut adapter = DeepSeekConfig::with_endpoint(&config.endpoint)
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?
            .with_protocol(config.protocol)
            .with_stream_idle_timeout(std::time::Duration::from_millis(
                config.http_timeouts.idle_ms,
            ))
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        for (model, cap) in &config.default_output_tokens {
            if !config.language_models.contains_key(model) {
                return Err(rsi_meta::MetaError::InvalidInput(
                    "output default names an unconfigured model".into(),
                ));
            }
            adapter = adapter
                .with_default_output_tokens(model, *cap)
                .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        }
        for (model, limits) in &config.language_models {
            adapter = adapter
                .with_model_profile(model, *limits)
                .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        }
        for (model, &enabled) in &config.image_input {
            if !config.language_models.contains_key(model) {
                return Err(rsi_meta::MetaError::InvalidInput(
                    "image input names an unconfigured model".into(),
                ));
            }
            adapter = adapter
                .with_image_input(model, enabled)
                .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        }
        adapter = config
            .apply_efforts(adapter)
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        let retained = serde_json::to_vec(desired)
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?
            .len();
        Ok(rsi_meta::PreparedActivation::with_state(
            desired.clone(),
            PreparedDeepSeekPlugin { config, adapter },
            retained,
        )
        .requiring_local::<LanguageRegistrarContract>())
    }

    async fn activate(&self, mut plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let prepared = plan.take_state::<PreparedDeepSeekPlugin>()?;
        let generation = plan
            .context()
            .owner()
            .ok_or_else(|| rsi_meta::MetaError::Activation("provider has no Fiber owner".into()))?
            .1
            .0;
        let transport: Arc<dyn HttpTransport> = match &self.transport {
            Some(transport) => Arc::clone(transport),
            None => Arc::new(
                rsi_ai_transport::ReqwestTransport::with_streaming_timeouts(
                    std::time::Duration::from_millis(prepared.config.http_timeouts.connect_ms),
                    std::time::Duration::from_millis(prepared.config.http_timeouts.headers_ms),
                )
                .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?,
            ),
        };
        let endpoint_fingerprint = format!(
            "sha256-{}",
            hex::encode(Sha256::digest(prepared.config.endpoint.as_bytes()))
        );
        let registration = ProviderRegistration::builder(&prepared.config.deployment, "deepseek")
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
            .with_retry_policy(
                rsi_ai_protocol::RetryPolicy::new(
                    5,
                    vec![
                        rsi_ai_protocol::ErrorKind::RateLimited,
                        rsi_ai_protocol::ErrorKind::Server,
                        rsi_ai_protocol::ErrorKind::Timeout,
                        rsi_ai_protocol::ErrorKind::Transport,
                        rsi_ai_protocol::ErrorKind::EmptyResponse,
                    ],
                    500,
                    10_000,
                    100,
                )
                .expect("static DeepSeek retry policy")
                .with_language_regeneration(),
            )
            .with_credential(prepared.config.credential)
            .with_protocol(
                prepared.config.protocol.name(),
                "http",
                endpoint_fingerprint,
            )
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
            .with_config_generation(generation)
            .with_language(DeepSeekAdapter::new(
                prepared.adapter,
                Arc::new(fields::Transport {
                    inner: transport,
                    context: plan.context().clone(),
                }),
            ))
            .build()
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        let publication = ProviderPublication::publish(
            Arc::new(registration),
            Some(plan.local::<LanguageRegistrarContract>()?),
            None,
        )
        .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw DeepSeek provider facet",
            Box::new(move || {
                Box::pin(async move {
                    drop(publication);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    #[test]
    fn official_efforts_apply_only_without_an_explicit_declaration() {
        for model in ["deepseek-flash", "deepseek-v4-pro", "custom-model"] {
            for explicit in [false, true] {
                let mut config: DeepSeekPluginConfig = serde_json::from_value(serde_json::json!({
                    "deployment":"test", "endpoint":"https://api.deepseek.com",
                    "credential":{"owner":"test","slot":"default"},
                    "language_models": {model: {"context_window_tokens":1_048_576,"default_output_reserve_tokens":4096,"max_output_reserve_tokens":256_000}}
                })).unwrap();
                if explicit {
                    config.reasoning_efforts.insert(
                        model.into(),
                        rsi_ai_protocol::ReasoningEffortProfile::default(),
                    );
                }
                let adapter = DeepSeekConfig::default()
                    .with_default_output_tokens(model, 4096)
                    .unwrap()
                    .with_model_profile(model, config.language_models[model])
                    .unwrap();
                let adapter = DeepSeekAdapter::new(
                    config.apply_efforts(adapter).unwrap(),
                    Arc::new(rsi_ai_transport::ReqwestTransport::new().unwrap()),
                );
                let profile = adapter.describe(model).unwrap();
                let expected = if explicit || model == "custom-model" {
                    vec![]
                } else {
                    vec!["off", "low", "high", "max"]
                };
                assert_eq!(
                    profile
                        .reasoning_efforts()
                        .supported()
                        .iter()
                        .map(rsi_ai_protocol::ReasoningEffortId::as_str)
                        .collect::<Vec<_>>(),
                    expected
                );
                assert_eq!(
                    profile
                        .reasoning_efforts()
                        .default_effort()
                        .map(rsi_ai_protocol::ReasoningEffortId::as_str),
                    if expected.is_empty() {
                        None
                    } else {
                        Some("high")
                    }
                );
            }
        }
    }
}
