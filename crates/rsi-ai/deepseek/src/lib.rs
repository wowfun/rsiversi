//! DeepSeek-specific policy over the OpenAI-compatible Chat Completions wire format.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // AiError carries the public failure taxonomy.

use std::{fmt, sync::Arc};

use rsi_ai_openai_compatible::{ChatCompletionsAdapter, ChatCompletionsConfig};
use rsi_ai_protocol::{AiError, LanguageModelLimits, LanguageRequest};
use rsi_ai_provider::{
    AdapterFuture, LanguageAdapter, LanguageAdapterStream, PrepareContext, Prepared,
};
use rsi_ai_transport::HttpTransport;

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

    fn prepare(
        &self,
        context: PrepareContext,
        model: String,
        request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, AiError>> {
        if request.settings().seed().is_some() || request.settings().reasoning_effort().is_some() {
            return Box::pin(async {
                Err(rsi_ai_protocol::AiError::new(
                    rsi_ai_protocol::ErrorKind::Unsupported,
                    rsi_ai_protocol::ErrorPhase::Prepare,
                    rsi_ai_protocol::DispatchStatus::NotStarted,
                    "DeepSeek Chat does not support seed or reasoning_effort controls",
                )
                .expect("static DeepSeek setting error"))
            });
        }
        self.inner.prepare(context, model, request)
    }
}
