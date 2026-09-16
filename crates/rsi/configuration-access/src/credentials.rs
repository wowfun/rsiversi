use super::{ApiError, Arc, ConfigurationAccess, Deserialize, Result, Serialize};
use rsi_api_protocol::{ApiRegistrar, ApiRegistration, json_handler};
use rsi_configuration_api::{CredentialOperation, CredentialReceipt, ProviderKind};
use rsi_credentials_protocol::{
    CredentialRef, CredentialStoreFailure, CredentialsAdmin, CredentialsError, CredentialsStatus,
    SecretValue,
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
fn mutation_result<T>(
    result: rsi_credentials_protocol::Result<T>,
) -> Result<std::result::Result<T, CredentialStoreFailure>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(CredentialsError::Store(CredentialStoreFailure::LockTimeout)) => {
            Err(ApiError::Capacity)
        }
        Err(CredentialsError::Store(reason)) => Ok(Err(reason)),
        Err(CredentialsError::InvalidInput(_)) => {
            Err(ApiError::Invalid("invalid credential input".into()))
        }
        Err(_) => Err(ApiError::OutcomeUnknown),
    }
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
                authority
                    .retained(&context.origin, async move {
                        mutation_result(credentials.set(&reference, input.secret).await.map(|()| {
                            CredentialReceipt {
                                operation: CredentialOperation::Set,
                                removed: None,
                            }
                        }))
                    })?
                    .await
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
                authority
                    .retained(&context.origin, async move {
                        mutation_result(credentials.unset(&reference).await.map(|removed| {
                            CredentialReceipt {
                                operation: CredentialOperation::Unset,
                                removed: Some(removed),
                            }
                        }))
                    })?
                    .await
            }
        }),
    )?;
    Ok(vec![read, set, unset])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_failure_classification_preserves_contention_and_unknown_outcomes() {
        assert_eq!(
            mutation_result::<()>(Err(CredentialsError::Store(
                CredentialStoreFailure::LockTimeout
            ))),
            Err(ApiError::Capacity)
        );
        assert_eq!(
            mutation_result::<()>(Err(CredentialsError::Store(
                CredentialStoreFailure::Permissions
            ))),
            Ok(Err(CredentialStoreFailure::Permissions))
        );
        assert_eq!(
            mutation_result::<()>(Err(CredentialsError::OutcomeUnknown)),
            Err(ApiError::OutcomeUnknown)
        );
    }
}
