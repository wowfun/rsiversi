//! Grant-held product operations above the MCP owner; stdio control stays Local.
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiError, ApiRegistrarContract, ApiRegistration, CallOrigin, Result, json_handler,
};
use rsi_configuration_access::ConfigurationAccessContract;
use rsi_configuration_api::{
    McpCredentialReceipt, McpCredentialStatus, McpCredentialTarget, McpOperation,
    McpRefreshRequest, McpRefreshResult,
};
use rsi_credentials_protocol::{
    CredentialStoreFailure, CredentialsAdminContract, CredentialsError, CredentialsStatusContract,
    SecretValue,
};
use rsi_mcp::{McpOwner, McpOwnerContract, TransportConfig};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
#[derive(Serialize)]
enum Never {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    target: McpCredentialTarget,
    #[serde(deserialize_with = "secret")]
    secret: SecretValue,
}
fn secret<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<SecretValue, D::Error> {
    SecretValue::new(String::deserialize(decoder)?).map_err(serde::de::Error::custom)
}
fn target(owner: &McpOwner, target: &McpCredentialTarget) -> Result<()> {
    target.validate()?;
    let config = owner.http_config().map_err(|_| ApiError::Unavailable)?;
    if !config.servers.iter().any(|server|server.id==target.server && matches!(&server.transport,TransportConfig::StreamableHttp{credential:Some(reference),..} if reference==&target.reference)) {return Err(ApiError::Unauthorized);}
    Ok(())
}
fn mutation<T>(
    value: rsi_credentials_protocol::Result<T>,
) -> Result<std::result::Result<T, CredentialStoreFailure>> {
    match value {
        Ok(value) => Ok(Ok(value)),
        Err(CredentialsError::Store(CredentialStoreFailure::LockTimeout)) => {
            Err(ApiError::Capacity)
        }
        Err(CredentialsError::Store(reason)) => Ok(Err(reason)),
        Err(CredentialsError::InvalidInput(_)) => {
            Err(ApiError::Invalid("Invalid MCP credential input".into()))
        }
        Err(_) => Err(ApiError::OutcomeUnknown),
    }
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
#[derive(Debug, Default)]
pub(crate) struct McpApiFactory;
#[expect(
    clippy::too_many_lines,
    reason = "One exhaustive API activation registers the finite granted operations"
)]
#[async_trait]
impl PluginFactory for McpApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("MCP API configuration must be null"));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<McpOwnerContract>()
            .requiring_local::<ConfigurationAccessContract>()
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<CredentialsStatusContract>()
            .requiring_local::<CredentialsAdminContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let owner = plan.local::<McpOwnerContract>()?;
        let grant = plan.local::<ConfigurationAccessContract>()?;
        let credentials = plan.local::<CredentialsStatusContract>()?;
        let admin = plan.local::<CredentialsAdminContract>()?;
        let mut registrations: Vec<ApiRegistration> = vec![];
        let observed = owner.clone();
        let authority = grant.clone();
        registrations.push(
            registrar
                .register(
                    McpOperation::Status.spec(),
                    json_handler(move |context, _: Empty| {
                        let owner = observed.clone();
                        let grant = authority.clone();
                        async move {
                            let _lease = grant.admit(&context.origin)?;
                            let status = owner.observation();
                            status.validate().map_err(|_| ApiError::Unavailable)?;
                            Ok(Ok::<_, Never>(status))
                        }
                    }),
                )
                .map_err(meta)?,
        );
        let refreshed = owner.clone();
        let authority = grant.clone();
        registrations.push(
            registrar
                .register(
                    McpOperation::Refresh.spec(),
                    json_handler(move |context, input: McpRefreshRequest| {
                        let owner = refreshed.clone();
                        let grant = authority.clone();
                        async move {
                            input.validate()?;
                            // This check precedes every effect; a grant never grants remote process launch.
                            if input.server.as_deref().is_some_and(|id| owner.is_stdio(id))
                                && !matches!(context.origin, CallOrigin::Local)
                            {
                                return Err(ApiError::Unauthorized);
                            }
                            grant
                                .retained(&context.origin, async move {
                                    let error = owner
                                        .refresh(input.server.as_deref(), CancellationToken::new())
                                        .await
                                        .err();
                                    let status = owner.observation();
                                    status.validate().map_err(|_| ApiError::Unavailable)?;
                                    Ok(Ok::<_, Never>(McpRefreshResult {
                                        server: input.server,
                                        error,
                                        status,
                                    }))
                                })?
                                .await
                        }
                    }),
                )
                .map_err(meta)?,
        );
        let configured = owner.clone();
        let authority = grant.clone();
        registrations.push(
            registrar
                .register(
                    McpOperation::CredentialStatus.spec(),
                    json_handler(move |context, input: McpCredentialTarget| {
                        let owner = configured.clone();
                        let grant = authority.clone();
                        let credentials = credentials.clone();
                        async move {
                            grant
                                .retained(&context.origin, async move {
                                    target(&owner, &input)?;
                                    let status = credentials
                                        .status(&input.reference)
                                        .await
                                        .map_err(|_| ApiError::Unavailable)?;
                                    Ok(Ok::<_, Never>(McpCredentialStatus {
                                        target: input,
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
        let configured = owner.clone();
        let authority = grant.clone();
        let writer = admin.clone();
        registrations.push(
            registrar
                .register(
                    McpOperation::CredentialSet.spec(),
                    json_handler(move |context, input: Set| {
                        let owner = configured.clone();
                        let grant = authority.clone();
                        let admin = writer.clone();
                        async move {
                            grant
                                .retained(&context.origin, async move {
                                    target(&owner, &input.target)?;
                                    mutation(
                                        admin.set(&input.target.reference, input.secret).await.map(
                                            |()| McpCredentialReceipt {
                                                target: input.target,
                                                removed: None,
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
        registrations.push(
            registrar
                .register(
                    McpOperation::CredentialUnset.spec(),
                    json_handler(move |context, input: McpCredentialTarget| {
                        let owner = owner.clone();
                        let grant = grant.clone();
                        let admin = admin.clone();
                        async move {
                            grant
                                .retained(&context.origin, async move {
                                    target(&owner, &input)?;
                                    mutation(admin.unset(&input.reference).await.map(|removed| {
                                        McpCredentialReceipt {
                                            target: input,
                                            removed: Some(removed),
                                        }
                                    }))
                                })?
                                .await
                        }
                    }),
                )
                .map_err(meta)?,
        );
        plan.defer(
            "close MCP configuration API",
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
