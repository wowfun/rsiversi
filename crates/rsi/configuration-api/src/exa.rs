use super::{
    ApiClient, ApiError, Arc, Deserialize, DeserializeOwned, Empty, OperationAccess,
    OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding, Result,
    Serialize, call_json,
};
use rsi_credentials_protocol::{CredentialAvailability, CredentialStoreFailure, SecretValue};
/// Exact fixed Exa credential operations, each held by a Configuration grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExaOperation {
    /// Redacted credential availability.
    Status,
    /// One explicit secret write.
    Set,
    /// One explicit secret removal.
    Unset,
}
impl ExaOperation {
    /// Finite negotiated wire contract with no caller-selected credential target.
    #[expect(
        clippy::missing_panics_doc,
        reason = "Only fixed validated constants and infallible JSON flag serialization are unwrapped."
    )]
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "exa-credential",
                match self {
                    Self::Status => "status",
                    Self::Set => "set",
                    Self::Unset => "unset",
                },
                1,
            )
            .expect("static Exa operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: if self == Self::Status {
                OperationEffect::Read
            } else {
                OperationEffect::Mutation
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: if self == Self::Set { 512 * 1024 } else { 1024 },
            maximum_response_bytes: 8192,
        }
    }
}
/// Exa availability without any secret or local store path.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExaCredentialStatus {
    /// Actual credential source availability.
    pub availability: CredentialAvailability,
    /// Whether the current store permits explicit mutation.
    pub editable: bool,
}
/// Independent credential receipt; no request or settings mutation follows.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExaCredentialReceipt {
    /// Present only for removal, indicating whether a value was removed.
    pub removed: Option<bool>,
}
/// Typed access to the fixed Exa credential, without secret resolution.
#[derive(Clone, Debug)]
pub struct ExaClient {
    api: Arc<dyn ApiClient>,
}
impl ExaClient {
    /// Requires the negotiated availability operation.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if !api.operations().contains(&ExaOperation::Status.spec()) {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: ExaOperation,
        input: &I,
    ) -> Result<O> {
        if !self.api.operations().contains(&operation.spec()) {
            return Err(ApiError::Unavailable);
        }
        match call_json::<_, _, CredentialStoreFailure>(self.api.as_ref(), &operation.spec(), input)
            .await?
        {
            Ok(value) => Ok(value),
            Err(error) => Err(ApiError::Backend(error.to_string())),
        }
    }
    /// Reads the fixed Exa credential's redacted status.
    pub async fn status(&self) -> Result<ExaCredentialStatus> {
        self.call(ExaOperation::Status, &Empty {}).await
    }
    /// Sends one zeroizing secret and validates the independent write receipt.
    pub async fn set(&self, secret: SecretValue) -> Result<ExaCredentialReceipt> {
        #[derive(Serialize)]
        struct Set<'a> {
            secret: &'a str,
        }
        let result: ExaCredentialReceipt = self
            .call(
                ExaOperation::Set,
                &Set {
                    secret: secret.expose_secret(),
                },
            )
            .await?;
        if result.removed.is_some() {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(result)
    }
    /// Removes the fixed credential without replay or any follow-up request.
    pub async fn unset(&self) -> Result<ExaCredentialReceipt> {
        let result: ExaCredentialReceipt = self.call(ExaOperation::Unset, &Empty {}).await?;
        if result.removed.is_none() {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(result)
    }
}
