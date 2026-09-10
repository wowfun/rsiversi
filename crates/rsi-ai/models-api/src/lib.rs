//! Domain-owned read-only Models API and client plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_ai_protocol::{
    LanguageModelPage, LanguageModels, LanguageModelsContract, MAX_LANGUAGE_MODEL_PAGE, ModelRef,
    ModelsError,
};
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiError, ApiRegistrar, ApiRegistrarContract, ApiRegistration,
    OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding, call_json,
    json_handler,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn operation() -> OperationSpec {
    OperationSpec {
        id: OperationId::new("models", "list", 1).expect("constant operation"),
        class: OperationClass::Data,
        effect: OperationEffect::Read,
        access: rsi_api_protocol::OperationAccess::Authenticated,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 4096,
        maximum_response_bytes: 1024 * 1024,
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    after: Option<ModelRef>,
    limit: usize,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
enum Failure {
    Invalid,
    Capacity,
    ShuttingDown,
}

/// Registers the domain's exact catalog operation using only read-only model authority.
pub fn register_models(
    registrar: &dyn ApiRegistrar,
    models: Arc<dyn LanguageModels>,
) -> rsi_api_protocol::Result<ApiRegistration> {
    registrar.register(
        operation(),
        json_handler(move |_, input: Request| {
            let models = models.clone();
            async move {
                Ok(
                    match models.list_models(input.after.as_ref(), input.limit).await {
                        Ok(page) => Ok(page),
                        Err(ModelsError::Invalid(_)) => Err(Failure::Invalid),
                        Err(ModelsError::Capacity) => Err(Failure::Capacity),
                        Err(ModelsError::ShuttingDown) => Err(Failure::ShuttingDown),
                        Err(ModelsError::Backend(_)) => {
                            return Err(ApiError::Backend("model catalog failed".into()));
                        }
                        Err(ModelsError::Api(error)) => return Err(error),
                    },
                )
            }
        }),
    )
}

/// Read-only model catalog proxy over a negotiated API connection.
#[derive(Debug)]
pub struct ModelsClient {
    api: Arc<dyn ApiClient>,
}
impl ModelsClient {
    /// Requires the exact catalog operation before exposing a model enumeration service.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if !api.operations().contains(&operation()) {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
}
#[async_trait]
impl LanguageModels for ModelsClient {
    async fn list_models(
        &self,
        after: Option<&ModelRef>,
        limit: usize,
    ) -> Result<LanguageModelPage, ModelsError> {
        if !(1..=MAX_LANGUAGE_MODEL_PAGE).contains(&limit) {
            return Err(ModelsError::Invalid(
                "model page limit must be 1..=256".into(),
            ));
        }
        let page = call_json::<_, LanguageModelPage, Failure>(
            self.api.as_ref(),
            &operation(),
            &Request {
                after: after.cloned(),
                limit,
            },
        )
        .await
        .map_err(ModelsError::Api)?
        .map_err(|failure| match failure {
            Failure::Invalid => ModelsError::Invalid("remote catalog rejected input".into()),
            Failure::Capacity => ModelsError::Capacity,
            Failure::ShuttingDown => ModelsError::ShuttingDown,
        })?;
        page.validate(after, limit)
            .map_err(|_| ModelsError::Backend("invalid remote model page".into()))?;
        Ok(page)
    }
}
fn prepare(config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !config.is_null() && !config.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(MetaError::InvalidInput(
            "Models API configuration must be null or empty".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
/// Ordinary endpoint plugin requiring only Models and the generic registrar.
#[derive(Clone, Debug, Default)]
pub struct ModelsApiFactory;
#[async_trait]
impl PluginFactory for ModelsApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?
            .requiring_local::<LanguageModelsContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let registration =
            register_models(registrar.as_ref(), plan.local::<LanguageModelsContract>()?)
                .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Models API",
            Box::new(move || {
                Box::pin(async move {
                    registration.close().await;
                    Ok(())
                })
            }),
        )
    }
}
/// Ordinary client plugin exposing no provider invocation capability.
#[derive(Clone, Debug, Default)]
pub struct ModelsClientFactory;
#[async_trait]
impl PluginFactory for ModelsClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?.requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = ModelsClient::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<LanguageModelsContract>(Arc::new(client))?;
        plan.defer(
            "withdraw Models client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
