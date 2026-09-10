//! `DeepSeek` endpoint policy with default stateless Responses and explicit Chat support.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // AiError carries the public failure taxonomy.

use std::{collections::BTreeMap, fmt, sync::Arc};

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
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self::with_endpoint("https://api.deepseek.com").expect("static DeepSeek endpoint")
    }
}

impl DeepSeekConfig {
    /// Overrides the origin for an enterprise gateway or a loopback test server.
    pub fn with_endpoint(endpoint: impl Into<String>) -> Result<Self, AiError> {
        let endpoint = endpoint.into();
        let chat = ChatCompletionsConfig::new(endpoint.clone())?
            .with_path("/chat/completions")?
            .with_image_input(false)
            .with_developer_role(DeveloperMessageRole::System);
        let responses = OpenAiConfig::new(endpoint)?.with_responses_options(
            "/responses",
            ResponsesState::Stateless,
            MessageRole::System,
        )?;
        Ok(Self {
            chat,
            responses,
            protocol: DeepSeekProtocol::default(),
        })
    }

    #[must_use]
    /// Selects a protocol before the deployment is prepared.
    pub const fn with_protocol(mut self, protocol: DeepSeekProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// Adds one exact model-capacity profile to this deployment.
    pub fn with_model_profile(
        mut self,
        model: impl Into<String>,
        limits: LanguageModelLimits,
    ) -> Result<Self, AiError> {
        let model = model.into();
        self.chat = self.chat.with_model_profile(model.clone(), limits)?;
        self.responses = self.responses.with_model_profile(model, limits)?;
        Ok(self)
    }
}

/// One generation-pinned `DeepSeek` adapter with provider-owned compatibility admission.
#[derive(Clone)]
pub struct DeepSeekAdapter {
    inner: Arc<dyn LanguageAdapter>,
    protocol: DeepSeekProtocol,
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
        }
    }
}

impl fmt::Debug for DeepSeekAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeepSeekAdapter")
            .field("inner", &self.inner)
            .field("protocol", &self.protocol)
            .finish()
    }
}

impl LanguageAdapter for DeepSeekAdapter {
    fn models(&self) -> &rsi_ai_protocol::LanguageModelProfiles {
        self.inner.models()
    }

    fn describe(&self, model: &str) -> Result<rsi_ai_protocol::LanguageProfile, AiError> {
        let profile = self.inner.describe(model)?;
        if self.protocol == DeepSeekProtocol::ChatCompletions {
            return Ok(profile);
        }
        Ok(rsi_ai_protocol::LanguageProfile::new(
            profile.context_window_tokens(),
            profile.default_output_reserve_tokens(),
            profile.max_output_reserve_tokens(),
            rsi_ai_protocol::ToolDialect::Responses,
            true,
            rsi_ai_protocol::ImageToolResultCapability::No,
            Vec::new(),
        )
        .expect("validated DeepSeek model limits"))
    }

    fn validate_request(&self, model: &str, request: &LanguageRequest) -> Result<(), AiError> {
        if self.protocol == DeepSeekProtocol::ChatCompletions
            && (request.settings().seed().is_some()
                || request.settings().reasoning_effort().is_some())
        {
            return Err(rsi_ai_protocol::AiError::new(
                rsi_ai_protocol::ErrorKind::Unsupported,
                rsi_ai_protocol::ErrorPhase::Prepare,
                rsi_ai_protocol::DispatchStatus::NotStarted,
                "DeepSeek Chat does not support seed or reasoning_effort controls",
            )
            .expect("static DeepSeek setting error"));
        }
        if self.protocol == DeepSeekProtocol::Responses {
            validate_responses_support(request)?;
        }
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
        self.inner.prepare(context, model, request)
    }
}

fn validate_responses_support(request: &LanguageRequest) -> Result<(), AiError> {
    fn unsupported(content: &MessageContent) -> bool {
        match content {
            MessageContent::Image(_) | MessageContent::Audio(_) => true,
            MessageContent::ToolResult { content, .. } => content.iter().any(unsupported),
            MessageContent::ToolCall(call) => {
                call.kind == ToolCallKind::Freeform && call.name != "apply_patch"
            }
            _ => false,
        }
    }
    if request
        .messages()
        .iter()
        .flat_map(rsi_ai_protocol::Message::content)
        .any(unsupported)
        || request
            .tools()
            .iter()
            .any(|tool| tool.freeform().is_some() && tool.name() != "apply_patch")
    {
        return Err(AiError::new(
            rsi_ai_protocol::ErrorKind::Unsupported,
            rsi_ai_protocol::ErrorPhase::Prepare,
            rsi_ai_protocol::DispatchStatus::NotStarted,
            "DeepSeek Responses requires text-only input and limits custom tools to apply_patch",
        )
        .expect("static compatibility error"));
    }
    Ok(())
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
        let mut adapter = DeepSeekConfig::with_endpoint(&config.endpoint)
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?
            .with_protocol(config.protocol);
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
                rsi_ai_transport::ReqwestTransport::new()
                    .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?,
            ),
        };
        let endpoint_fingerprint = format!(
            "sha256-{}",
            hex::encode(Sha256::digest(prepared.config.endpoint.as_bytes()))
        );
        let registration = ProviderRegistration::builder(&prepared.config.deployment, "deepseek")
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
            .with_credential(prepared.config.credential)
            .with_protocol(
                prepared.config.protocol.name(),
                "http",
                endpoint_fingerprint,
            )
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
            .with_config_generation(generation)
            .with_language(DeepSeekAdapter::new(prepared.adapter, transport))
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
