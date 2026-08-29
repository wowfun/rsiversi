//! Official `OpenAI` adapters for Responses and Images.

#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // AiError carries the public failure taxonomy.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write as _},
    sync::{Arc, Mutex},
};

use async_stream::{stream, try_stream};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use futures_util::StreamExt as _;
use http::{HeaderValue, Method};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rsi_ai_protocol::{
    AiError, ContentDelta, ContentStart, DispatchStatus, ErrorKind, ErrorPhase, FinishReason,
    HostedTool, ImageEvent, ImageRequest, LanguageEvent, LanguageModelLimits,
    LanguageModelProfiles, LanguageRequest, MAX_LANGUAGE_OUTPUT_BYTES, MessageContent, MessageRole,
    ProviderExtension, ResponseFormat, Source, TokenUsage, ToolCallKind, ToolChoice,
    ToolDefinition, validate_json_structure,
};
use rsi_ai_provider::{
    AbortSignal, AdapterFuture, DeferredLanguageAdapterHandle, DeferredLanguageAdapterStream,
    DeferredLanguageBatch, DeferredLanguageCheckpoint, DeferredLanguageOperation, DeferredStatus,
    ImageAdapter, ImageAdapterStream, ImageRegistrarContract, LanguageAdapter,
    LanguageAdapterStream, LanguageRegistrarContract, PrepareContext, Prepared,
    ProviderPublication, ProviderRegistration,
};
use rsi_ai_transport::{
    BoundedJsonExtractor, ByteStream, HttpRequest, HttpTransport, JsonBase64Replacement,
    JsonExtractEvent, JsonExtractionLimits, JsonProjectionLimits, JsonRequestBody,
    MAX_PROVIDER_REQUEST_BODY_BYTES, SseTermination, TransportError, decode_sse,
    invalid_request_error, json_base64_body, project_json_body, provider_error as ai_error,
    provider_http_error, reclassify_context_limit, transport_body_error, transport_connect_error,
    transport_json_response_error, transport_stream_error,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

// Ten maximum-size (32 MiB) decoded images require about 427 MiB of base64;
// the remaining headroom covers the bounded JSON envelope. This limits total
// streamed response bytes; the incremental extractor does not retain that sum.
const MAX_JSON_BODY_BYTES: usize = 448 * 1024 * 1024;
const MAX_IMAGE_ENVELOPE_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_ITEM_JSON_BYTES: usize = 43 * 1024 * 1024;
// A full deferred Response may repeat the maximum decoded Language output.
// JSON can encode one UTF-8 byte as a six-byte `\u00XX` escape; the remaining
// MiB bounds identifiers, status, usage, and structural envelope data.
const MAX_DEFERRED_CONTROL_BODY_BYTES: usize = MAX_LANGUAGE_OUTPUT_BYTES * 6 + 1024 * 1024;
const OUTPUT_CHUNK_BYTES: usize = 256 * 1024;
const ENCODED_OUTPUT_CHUNK_BYTES: usize = (OUTPUT_CHUNK_BYTES / 3) * 4;
const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Fixed official `OpenAI` endpoint policy shared by all HTTP capabilities.
#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    endpoint: String,
    language_models: LanguageModelProfiles,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com".to_owned(),
            language_models: LanguageModelProfiles::default(),
        }
    }
}

impl OpenAiConfig {
    /// Creates endpoint policy after validating every capability URL.
    pub fn new(endpoint: impl Into<String>) -> Result<Self, AiError> {
        let config = Self {
            endpoint: endpoint.into().trim_end_matches('/').to_owned(),
            language_models: LanguageModelProfiles::default(),
        };
        for path in ["/v1/responses", "/v1/images/generations"] {
            HttpRequest::new(Method::POST, config.url(path)).map_err(invalid_request_error)?;
        }
        Ok(config)
    }

    /// Adds one exact model-capacity profile; duplicates and oversized maps fail.
    pub fn with_model_profile(
        mut self,
        model: impl Into<String>,
        limits: LanguageModelLimits,
    ) -> Result<Self, AiError> {
        self.language_models
            .insert(model, limits)
            .map_err(|error| invalid_language_profile(error.reason()))?;
        Ok(self)
    }

    fn model_limits(&self, model: &str) -> Result<LanguageModelLimits, AiError> {
        self.language_models.get(model).ok_or_else(|| {
            invalid_language_profile("language model has no configured capacity profile")
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint)
    }
}

fn invalid_language_profile(message: impl Into<String>) -> AiError {
    ai_error(
        ErrorKind::InvalidRequest,
        ErrorPhase::Prepare,
        DispatchStatus::NotStarted,
        message,
    )
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
            /// Binds the official endpoint policy to the no-retry HTTP transport.
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

mod media;
mod responses;
mod shared;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiPluginConfig {
    deployment: String,
    credential: rsi_credentials_protocol::CredentialRef,
    endpoint: String,
    language: bool,
    image: bool,
    language_models: BTreeMap<String, LanguageModelLimits>,
}

#[derive(Debug)]
struct PreparedOpenAiPlugin {
    config: OpenAiPluginConfig,
    adapter: OpenAiConfig,
}

/// Ordinary plugin factory for one explicit `OpenAI` deployment.
#[derive(Clone, Default)]
pub struct OpenAiFactory {
    transport: Option<Arc<dyn HttpTransport>>,
}

impl OpenAiFactory {
    /// Uses an injected no-retry transport for deterministic embedders and tests.
    #[must_use]
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport: Some(transport),
        }
    }
}

impl fmt::Debug for OpenAiFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiFactory")
            .field("injected_transport", &self.transport.is_some())
            .finish()
    }
}

#[async_trait]
impl rsi_meta::PluginFactory for OpenAiFactory {
    fn prepare(
        &self,
        desired: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        let config: OpenAiPluginConfig = serde_json::from_value(desired.clone())
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        rsi_ai_protocol::validate_identifier("deployment", &config.deployment)
            .map_err(rsi_meta::MetaError::InvalidInput)?;
        config
            .credential
            .validate()
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        if !config.language && !config.image {
            return Err(rsi_meta::MetaError::InvalidInput(
                "OpenAI deployment must enable Language, Image, or both".into(),
            ));
        }
        if config.language == config.language_models.is_empty() {
            return Err(rsi_meta::MetaError::InvalidInput(
                "OpenAI Language requires a nonempty exact model map, and disabled Language requires an empty map"
                    .into(),
            ));
        }
        let mut adapter = OpenAiConfig::new(&config.endpoint)
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        for (model, limits) in &config.language_models {
            adapter = adapter
                .with_model_profile(model, *limits)
                .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?;
        }
        let retained = serde_json::to_vec(desired)
            .map_err(|error| rsi_meta::MetaError::InvalidInput(error.to_string()))?
            .len();
        let language = config.language;
        let image = config.image;
        let mut prepared = rsi_meta::PreparedActivation::with_state(
            desired.clone(),
            PreparedOpenAiPlugin { config, adapter },
            retained,
        );
        if language {
            prepared = prepared.requiring_local::<LanguageRegistrarContract>();
        }
        if image {
            prepared = prepared.requiring_local::<ImageRegistrarContract>();
        }
        Ok(prepared)
    }

    async fn activate(&self, mut plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let prepared = plan.take_state::<PreparedOpenAiPlugin>()?;
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
        let mut registration = ProviderRegistration::builder(&prepared.config.deployment, "openai")
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
            .with_credential(prepared.config.credential)
            .with_protocol("openai-responses", "http", endpoint_fingerprint)
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
            .with_image_protocol("openai-images")
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?
            .with_config_generation(generation);
        if prepared.config.language {
            registration = registration.with_language(OpenAiResponsesAdapter::new(
                prepared.adapter.clone(),
                Arc::clone(&transport),
            ));
        }
        if prepared.config.image {
            registration =
                registration.with_image(OpenAiImageAdapter::new(prepared.adapter, transport));
        }
        let registration = Arc::new(
            registration
                .build()
                .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?,
        );
        let language = if prepared.config.language {
            Some(plan.local::<LanguageRegistrarContract>()?)
        } else {
            None
        };
        let image = if prepared.config.image {
            Some(plan.local::<ImageRegistrarContract>()?)
        } else {
            None
        };
        let publication = ProviderPublication::publish(registration, language, image)
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw OpenAI provider facets",
            Box::new(move || {
                Box::pin(async move {
                    drop(publication);
                    Ok(())
                })
            }),
        )
    }
}
