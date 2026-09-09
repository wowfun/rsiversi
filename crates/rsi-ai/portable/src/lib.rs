//! Generation-bound Portable provider contributions to ordinary Language/Image routers.
#![deny(unsafe_code)]

mod adapter;
mod compatibility;
mod exchange;

use async_trait::async_trait;
use rsi_ai_protocol::{AiError, DispatchStatus, ErrorKind, ErrorPhase, RetryPolicy, portable};
use rsi_ai_provider::{
    ImageRegistrarContract, LanguageRegistrarContract, ProviderPublication, ProviderRegistration,
};
use rsi_api_protocol::ByteBudget;
use rsi_credentials_protocol::CredentialRef;
use rsi_meta::{
    ActivationPlan, ConfigValue, ContractVersion, MetaError, PluginFactory, PreparedActivation,
    Requirement,
};
use serde::Deserialize;
use std::sync::{Arc, LazyLock};

static WIRE_BUDGET: LazyLock<ByteBudget> = LazyLock::new(ByteBudget::default);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    service: String,
    deployment: String,
    provider_family: String,
    protocol: String,
    endpoint_fingerprint: String,
    credential: Option<CredentialRef>,
    #[serde(default)]
    retry_policy: RetryPolicy,
    language: bool,
    image: bool,
}
impl Config {
    fn registration(
        &self,
    ) -> Result<rsi_ai_provider::ProviderRegistrationBuilder, rsi_ai_provider::ProviderSdkError>
    {
        let mut builder = ProviderRegistration::builder(&self.deployment, &self.provider_family)?
            .with_protocol(&self.protocol, "portable", &self.endpoint_fingerprint)?
            .with_retry_policy(self.retry_policy.clone());
        if let Some(credential) = &self.credential {
            builder = builder.with_credential(credential.clone());
        }
        Ok(builder)
    }
}

/// Imports one explicit native business service using existing provider publication gates.
#[derive(Clone, Debug, Default)]
pub struct PortableProviderFactory;

#[async_trait]
impl PluginFactory for PortableProviderFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let retained = rsi_api_protocol::measure_json(desired, portable::MAXIMUM_METADATA_BYTES)
            .map_err(|_| invalid_config())?;
        let config: Config =
            serde_json::from_value(desired.clone()).map_err(|_| invalid_config())?;
        rsi_ai_protocol::validate_identifier("service", &config.service)
            .map_err(|_| invalid_config())?;
        if !config.language && !config.image {
            return Err(invalid_config());
        }
        config.registration().map_err(|_| invalid_config())?;
        if let Some(credential) = &config.credential {
            credential.validate().map_err(|_| invalid_config())?;
        }
        let language = config.language;
        let image = config.image;
        let requirement = Requirement::new(
            config.service.clone(),
            portable::PROVIDER_CONTRACT,
            ContractVersion(portable::PROVIDER_VERSION),
        );
        let mut prepared = PreparedActivation::with_state(desired.clone(), config, retained)
            .requiring(requirement);
        if language {
            prepared = prepared.requiring_local::<LanguageRegistrarContract>();
        }
        if image {
            prepared = prepared.requiring_local::<ImageRegistrarContract>();
        }
        Ok(prepared)
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let capability = plan
            .inject(&config.service)
            .ok_or_else(activation_error)?
            .clone();
        let adapter = adapter::Adapter::load(capability)
            .await
            .map_err(|_| activation_error())?;
        if (config.language && adapter.description.language.is_empty())
            || (config.image && adapter.description.image.is_empty())
        {
            return Err(activation_error());
        }
        let generation = plan.context().owner().ok_or_else(activation_error)?.1.0;
        let mut builder = config
            .registration()
            .map_err(|_| activation_error())?
            .with_config_generation(generation);
        if config.language {
            builder = builder.with_language(adapter.clone());
        }
        if config.image {
            builder = builder.with_image(adapter);
        }
        let registration = builder.build().map_err(|_| activation_error())?;
        let language = if config.language {
            Some(plan.local::<LanguageRegistrarContract>()?)
        } else {
            None
        };
        let image = if config.image {
            Some(plan.local::<ImageRegistrarContract>()?)
        } else {
            None
        };
        let publication = ProviderPublication::publish(Arc::new(registration), language, image)
            .map_err(|_| activation_error())?;
        plan.defer(
            "withdraw Portable provider facets",
            Box::new(move || {
                Box::pin(async move {
                    drop(publication);
                    Ok(())
                })
            }),
        )
    }
}
fn invalid_config() -> MetaError {
    MetaError::InvalidInput("invalid Portable provider configuration".into())
}
fn activation_error() -> MetaError {
    MetaError::Activation("Portable provider description or publication failed".into())
}
fn error(kind: ErrorKind, phase: ErrorPhase, dispatch: DispatchStatus) -> AiError {
    AiError::new(kind, phase, dispatch, "Portable provider exchange failed")
        .expect("static safe provider error")
}
fn unsupported() -> AiError {
    error(
        ErrorKind::Unsupported,
        ErrorPhase::Prepare,
        DispatchStatus::NotStarted,
    )
}
