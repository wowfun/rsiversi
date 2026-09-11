use super::{
    ApiError, Arc, BoxFuture, CallOrigin, ConfigurationAccess, Deserialize, Result, Serialize,
};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, json_handler};
use rsi_configuration_api::{CredentialOperation, CredentialReceipt, ProviderKind};
use rsi_credentials_protocol::{
    CredentialRef, CredentialsAdmin, CredentialsError, CredentialsStatus, SecretValue,
};

#[derive(Serialize)]
enum Never {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    provider: ProviderKind,
    slot: String,
}
impl Reference {
    fn validated(self) -> Result<CredentialRef> {
        CredentialRef::new(self.provider.owner(), self.slot)
            .map_err(|_| ApiError::Invalid("invalid credential reference".into()))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    provider: ProviderKind,
    slot: String,
    #[serde(deserialize_with = "secret")]
    secret: SecretValue,
}
fn secret<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<SecretValue, D::Error> {
    SecretValue::new(String::deserialize(decoder)?).map_err(serde::de::Error::custom)
}
fn error(error: &CredentialsError) -> ApiError {
    match error {
        CredentialsError::EnvironmentShadow(_) => ApiError::Invalid(
            "credential is supplied by the Host environment and cannot be edited".into(),
        ),
        CredentialsError::InvalidInput(_) => ApiError::Invalid("invalid credential input".into()),
        _ => ApiError::OutcomeUnknown,
    }
}
fn retained<T: Send + 'static>(
    owner: &Arc<ConfigurationAccess>,
    origin: &CallOrigin,
    future: impl std::future::Future<Output = Result<T>> + Send + 'static,
) -> Result<BoxFuture<'static, Result<T>>> {
    let lease = owner.admit(origin)?;
    let task = owner.execution.spawn(owner.tasks.track_future(async move {
        let _lease = lease;
        future.await
    }));
    Ok(Box::pin(async move {
        task.await.map_err(|_| ApiError::OutcomeUnknown)?
    }))
}
pub(super) fn register(
    registrar: &dyn ApiRegistrar,
    owner: Arc<ConfigurationAccess>,
    status: Arc<dyn CredentialsStatus>,
    admin: Arc<dyn CredentialsAdmin>,
) -> Result<Vec<ApiRegistration>> {
    let read = registrar.register(
        CredentialOperation::Status.spec(),
        json_handler(move |_context, reference: Reference| {
            let status = status.clone();
            async move {
                status
                    .status(&reference.validated()?)
                    .await
                    .map(Ok::<_, Never>)
                    .map_err(|_| ApiError::Unavailable)
            }
        }),
    )?;
    let authority = owner.clone();
    let credentials = admin.clone();
    let set = registrar.register(
        CredentialOperation::Set.spec(),
        json_handler(move |context, input: Set| {
            let authority = authority.clone();
            let credentials = credentials.clone();
            async move {
                let reference = Reference {
                    provider: input.provider,
                    slot: input.slot,
                }
                .validated()?;
                retained(&authority, &context.origin, async move {
                    credentials
                        .set(&reference, input.secret)
                        .await
                        .map_err(|failure| error(&failure))?;
                    Ok(CredentialReceipt {
                        operation: CredentialOperation::Set,
                        removed: None,
                    })
                })?
                .await
                .map(Ok::<_, Never>)
            }
        }),
    )?;
    let unset = registrar.register(
        CredentialOperation::Unset.spec(),
        json_handler(move |context, input: Reference| {
            let authority = owner.clone();
            let credentials = admin.clone();
            async move {
                let reference = input.validated()?;
                retained(&authority, &context.origin, async move {
                    let removed = credentials
                        .unset(&reference)
                        .await
                        .map_err(|failure| error(&failure))?;
                    Ok(CredentialReceipt {
                        operation: CredentialOperation::Unset,
                        removed: Some(removed),
                    })
                })?
                .await
                .map(Ok::<_, Never>)
            }
        }),
    )?;
    Ok(vec![read, set, unset])
}
