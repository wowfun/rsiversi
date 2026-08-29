//! DeepSeek-specific policy over the OpenAI-compatible Chat Completions wire format.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // AiError carries the public failure taxonomy.

use std::{collections::BTreeMap, fmt, sync::Arc};

use rsi_ai_openai_compatible::{ChatCompletionsAdapter, ChatCompletionsConfig};
use rsi_ai_protocol::{AiError, LanguageModelLimits, LanguageRequest};
use rsi_ai_provider::{
    AdapterFuture, LanguageAdapter, LanguageAdapterStream, LanguageRegistrarContract,
    PrepareContext, Prepared, ProviderPublication, ProviderRegistration,
};
use rsi_ai_transport::HttpTransport;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// Fixed endpoint configuration for one `DeepSeek` deployment.
#[derive(Clone, Debug)]
pub struct DeepSeekConfig {
    chat: ChatCompletionsConfig,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            chat: ChatCompletionsConfig::new("https://api.deepseek.com")
                .and_then(|config| config.with_path("/chat/completions"))
                .expect("the static DeepSeek endpoint is valid")
                .with_image_input(false),
        }
    }
}

impl DeepSeekConfig {
    /// Overrides the origin for an enterprise gateway or a loopback test server.
    pub fn with_endpoint(endpoint: impl Into<String>) -> Result<Self, AiError> {
        let chat = ChatCompletionsConfig::new(endpoint)?
            .with_path("/chat/completions")?
            .with_image_input(false);
        Ok(Self { chat })
    }

    /// Adds one exact model-capacity profile to this deployment.
    pub fn with_model_profile(
        mut self,
        model: impl Into<String>,
        limits: LanguageModelLimits,
    ) -> Result<Self, AiError> {
        self.chat = self.chat.with_model_profile(model, limits)?;
        Ok(self)
    }
}

/// `DeepSeek` chat/reasoner adapter. Reasoning replay is preserved on tool-call turns.
#[derive(Clone)]
pub struct DeepSeekAdapter {
    inner: ChatCompletionsAdapter,
}

impl DeepSeekAdapter {
    /// Binds `DeepSeek` endpoint policy to the no-retry HTTP transport.
    pub fn new(config: DeepSeekConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            inner: ChatCompletionsAdapter::new(config.chat, transport),
        }
    }
}

impl fmt::Debug for DeepSeekAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeepSeekAdapter")
            .field("inner", &self.inner)
            .finish()
    }
}

impl LanguageAdapter for DeepSeekAdapter {
    fn describe(&self, model: &str) -> Result<rsi_ai_protocol::LanguageProfile, AiError> {
        self.inner.describe(model)
    }

    fn validate_request(&self, model: &str, request: &LanguageRequest) -> Result<(), AiError> {
        if request.settings().seed().is_some() || request.settings().reasoning_effort().is_some() {
            return Err(rsi_ai_protocol::AiError::new(
                rsi_ai_protocol::ErrorKind::Unsupported,
                rsi_ai_protocol::ErrorPhase::Prepare,
                rsi_ai_protocol::DispatchStatus::NotStarted,
                "DeepSeek Chat does not support seed or reasoning_effort controls",
            )
            .expect("static DeepSeek setting error"));
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeepSeekPluginConfig {
    deployment: String,
    credential: rsi_credentials_protocol::CredentialRef,
    endpoint: String,
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
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
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
            .with_protocol("chat-completions", "http", endpoint_fingerprint)
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
