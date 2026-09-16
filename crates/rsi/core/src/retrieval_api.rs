//! Fixed Exa credential operations through the actual Configuration grant owner.
use async_trait::async_trait;
use rsi_api_protocol::{ApiError, ApiRegistrarContract, ApiRegistration, Result, json_handler};
use rsi_configuration_access::ConfigurationAccessContract;
use rsi_configuration_api::{ExaCredentialReceipt, ExaCredentialStatus, ExaOperation};
use rsi_credentials_protocol::{
    CredentialStoreFailure, CredentialsAdminContract, CredentialsError, CredentialsStatusContract,
    SecretValue,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
#[derive(Serialize)]
enum Never {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    #[serde(deserialize_with = "secret")]
    secret: SecretValue,
}
fn secret<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<SecretValue, D::Error> {
    SecretValue::new(String::deserialize(decoder)?).map_err(serde::de::Error::custom)
}
fn mutation<T>(
    value: rsi_credentials_protocol::Result<T>,
) -> Result<std::result::Result<T, CredentialStoreFailure>> {
    match value {
        Ok(value) => Ok(Ok(value)),
        Err(CredentialsError::Store(CredentialStoreFailure::LockTimeout)) => {
            Err(ApiError::Capacity)
        }
        Err(CredentialsError::Store(error)) => Ok(Err(error)),
        Err(CredentialsError::InvalidInput(_)) => {
            Err(ApiError::Invalid("Invalid Exa credential".into()))
        }
        Err(_) => Err(ApiError::OutcomeUnknown),
    }
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
#[derive(Debug, Default)]
pub(crate) struct RetrievalApiFactory;
#[async_trait]
impl PluginFactory for RetrievalApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("Exa credential API configuration must be null"));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<ConfigurationAccessContract>()
            .requiring_local::<CredentialsStatusContract>()
            .requiring_local::<CredentialsAdminContract>()
            .requiring_local::<rsi_retrieval::RetrievalContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let grant = plan.local::<ConfigurationAccessContract>()?;
        let status = plan.local::<CredentialsStatusContract>()?;
        let admin = plan.local::<CredentialsAdminContract>()?;
        let mut registrations: Vec<ApiRegistration> = vec![];
        let authority = grant.clone();
        registrations.push(
            registrar
                .register(
                    ExaOperation::Status.spec(),
                    json_handler(move |context, _: Empty| {
                        let grant = authority.clone();
                        let status = status.clone();
                        async move {
                            grant
                                .retained(&context.origin, async move {
                                    let status = status
                                        .status(&rsi_retrieval::exa_credential())
                                        .await
                                        .map_err(|_| ApiError::Unavailable)?;
                                    Ok(Ok::<_, Never>(ExaCredentialStatus {
                                        availability: status.availability,
                                        editable: status.editable,
                                    }))
                                })?
                                .await
                        }
                    }),
                )
                .map_err(meta)?,
        );
        let authority = grant.clone();
        let writer = admin.clone();
        registrations.push(
            registrar
                .register(
                    ExaOperation::Set.spec(),
                    json_handler(move |context, input: Set| {
                        let grant = authority.clone();
                        let admin = writer.clone();
                        async move {
                            grant
                                .retained(&context.origin, async move {
                                    mutation(
                                        admin
                                            .set(&rsi_retrieval::exa_credential(), input.secret)
                                            .await
                                            .map(|()| ExaCredentialReceipt { removed: None }),
                                    )
                                })?
                                .await
                        }
                    }),
                )
                .map_err(meta)?,
        );
        registrations.push(
            registrar
                .register(
                    ExaOperation::Unset.spec(),
                    json_handler(move |context, _: Empty| {
                        let grant = grant.clone();
                        let admin = admin.clone();
                        async move {
                            grant
                                .retained(&context.origin, async move {
                                    mutation(
                                        admin.unset(&rsi_retrieval::exa_credential()).await.map(
                                            |removed| ExaCredentialReceipt {
                                                removed: Some(removed),
                                            },
                                        ),
                                    )
                                })?
                                .await
                        }
                    }),
                )
                .map_err(meta)?,
        );
        plan.defer(
            "close Exa credential API",
            Box::new(move || {
                Box::pin(async move {
                    for registration in registrations {
                        registration.close().await;
                    }
                    Ok(())
                })
            }),
        )
    }
}
