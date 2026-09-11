use super::{
    ApiClient, ApiError, Arc, Deserialize, DeserializeOwned, Never, OperationAccess,
    OperationClass, OperationEffect, OperationId, OperationSpec, ProviderKind, RequestEncoding,
    Result, Serialize, call_json,
};
use rsi_credentials_protocol::{CredentialRef, CredentialStatus, SecretValue};

/// Closed credential configuration operations; resolution is deliberately absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialOperation {
    /// Redacted effective availability and editability.
    Status,
    /// Set one provider-owned slot.
    Set,
    /// Delete one provider-owned slot.
    Unset,
}
impl CredentialOperation {
    /// Exact negotiated credential configuration contract.
    ///
    /// # Panics
    /// Panics if the static operation identities are invalid.
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "provider-credentials",
                match self {
                    Self::Status => "status",
                    Self::Set => "set",
                    Self::Unset => "unset",
                },
                1,
            )
            .expect("static credential operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: if self == Self::Status {
                OperationEffect::Read
            } else {
                OperationEffect::Mutation
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 512 * 1024,
            maximum_response_bytes: 4096,
        }
    }
}
/// Acknowledges exactly one credential operation, independently of provider apply.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialReceipt {
    /// The completed mutation.
    pub operation: CredentialOperation,
    /// For unset, whether an entry existed; absent for set.
    pub removed: Option<bool>,
}
#[derive(Serialize)]
struct Reference<'a> {
    provider: ProviderKind,
    slot: &'a str,
}
/// Credential setup client without access to resolved secret values.
#[derive(Clone, Debug)]
pub struct ProviderCredentialsClient {
    api: Arc<dyn ApiClient>,
}
impl ProviderCredentialsClient {
    /// Requires exact setup operations from the connected Host.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if [
            CredentialOperation::Status,
            CredentialOperation::Set,
            CredentialOperation::Unset,
        ]
        .into_iter()
        .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    fn reference(provider: ProviderKind, slot: &str) -> Result<Reference<'_>> {
        CredentialRef::new(provider.owner(), slot)
            .map_err(|_| ApiError::Invalid("invalid credential slot".into()))?;
        Ok(Reference { provider, slot })
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: CredentialOperation,
        input: &I,
    ) -> Result<O> {
        match call_json::<_, _, Never>(self.api.as_ref(), &operation.spec(), input).await? {
            Ok(value) => Ok(value),
            Err(never) => match never {},
        }
    }
    /// Reads redacted current status; it does not test model connectivity.
    pub async fn status(&self, provider: ProviderKind, slot: &str) -> Result<CredentialStatus> {
        self.call(
            CredentialOperation::Status,
            &Self::reference(provider, slot)?,
        )
        .await
    }
    /// Sends one explicit secret write, with no replay or coupled configuration action.
    pub async fn set(
        &self,
        provider: ProviderKind,
        slot: &str,
        secret: SecretValue,
    ) -> Result<CredentialReceipt> {
        #[derive(Serialize)]
        struct Set<'a> {
            provider: ProviderKind,
            slot: &'a str,
            secret: &'a str,
        }
        Self::reference(provider, slot)?;
        let receipt: CredentialReceipt = self
            .call(
                CredentialOperation::Set,
                &Set {
                    provider,
                    slot,
                    secret: secret.expose_secret(),
                },
            )
            .await?;
        if receipt.operation != CredentialOperation::Set || receipt.removed.is_some() {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(receipt)
    }
    /// Sends one explicit deletion, with an independent receipt.
    pub async fn unset(&self, provider: ProviderKind, slot: &str) -> Result<CredentialReceipt> {
        let receipt: CredentialReceipt = self
            .call(
                CredentialOperation::Unset,
                &Self::reference(provider, slot)?,
            )
            .await?;
        if receipt.operation != CredentialOperation::Unset || receipt.removed.is_none() {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(receipt)
    }
}
