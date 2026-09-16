use super::{
    ApiClient, ApiError, Arc, Deserialize, DeserializeOwned, Empty, OperationAccess,
    OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding, Result,
    Serialize, call_json,
};
use rsi_credentials_protocol::{
    CredentialAvailability, CredentialRef, CredentialStoreFailure, SecretValue,
};
pub use rsi_mcp_protocol::{McpError, McpStatus, McpToolChoice, McpTransportKind, ServerStatus};
/// Exact MCP configuration operations; every Host handler also holds a configuration grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpOperation {
    /// Redacted actual readiness.
    Status,
    /// Explicit connection verification; remote callers are HTTP-only.
    Refresh,
    /// Redacted availability for one exact configured credential reference.
    CredentialStatus,
    /// One explicit secret write.
    CredentialSet,
    /// One explicit secret removal.
    CredentialUnset,
}
impl McpOperation {
    /// Exact finite negotiated contract.
    #[expect(
        clippy::missing_panics_doc,
        reason = "Only fixed validated constants and infallible JSON flag serialization are unwrapped."
    )]
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "mcp-configuration",
                match self {
                    Self::Status => "status",
                    Self::Refresh => "refresh",
                    Self::CredentialStatus => "credential-status",
                    Self::CredentialSet => "credential-set",
                    Self::CredentialUnset => "credential-unset",
                },
                1,
            )
            .expect("static MCP operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: if matches!(self, Self::Status | Self::CredentialStatus) {
                OperationEffect::Read
            } else {
                OperationEffect::Mutation
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: if self == Self::CredentialSet {
                512 * 1024
            } else {
                4096
            },
            maximum_response_bytes: if matches!(self, Self::Status | Self::Refresh) {
                256 * 1024
            } else {
                8192
            },
        }
    }
}
/// Explicit HTTP refresh, or one Local-only stdio refresh.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpRefreshRequest {
    /// Exact configured server; `None` applies settings and refreshes all HTTP endpoints.
    pub server: Option<String>,
}
fn server(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'_' | b'-'))
    {
        return Err(ApiError::Invalid("Invalid MCP endpoint identity".into()));
    }
    Ok(())
}
impl McpRefreshRequest {
    /// Checks identity before admission or connection work.
    pub fn validate(&self) -> Result<()> {
        if let Some(id) = &self.server {
            server(id)?;
        }
        Ok(())
    }
}
/// Completion of one explicit refresh, independent of new Session creation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpRefreshResult {
    /// Exact requested endpoint/all-endpoints identity.
    pub server: Option<String>,
    /// Closed failure, if verification did not complete.
    pub error: Option<McpError>,
    /// Fresh actual observations after the attempt.
    pub status: McpStatus,
}
/// Exact endpoint and credential binding shown to the user before a credential action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCredentialTarget {
    /// Current configured HTTP endpoint.
    pub server: String,
    /// Exact expected MCP-owned reference; never a resolved secret.
    pub reference: CredentialRef,
}
impl McpCredentialTarget {
    /// Validates both identities before checking the current configuration binding.
    pub fn validate(&self) -> Result<()> {
        server(&self.server)?;
        self.reference
            .validate()
            .map_err(|_| ApiError::Invalid("Invalid MCP credential reference".into()))?;
        if self.reference.owner.as_str() != rsi_mcp_protocol::CREDENTIAL_OWNER {
            return Err(ApiError::Unauthorized);
        }
        Ok(())
    }
}
/// Credential availability without a local store path or secret value.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCredentialStatus {
    /// Exact current binding.
    pub target: McpCredentialTarget,
    /// Closed source availability.
    pub availability: CredentialAvailability,
    /// Whether the current provider permits explicit writes.
    pub editable: bool,
}
/// Exact receipt for a credential mutation, independent of reconnection.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCredentialReceipt {
    /// Exact written or removed binding.
    pub target: McpCredentialTarget,
    /// Removal outcome; absent for a write.
    pub removed: Option<bool>,
}
/// Typed MCP workbench access, without secret resolution or Local process authority.
#[derive(Clone, Debug)]
pub struct McpClient {
    api: Arc<dyn ApiClient>,
}
impl McpClient {
    /// Requires the negotiated status operation; individual actions negotiate independently.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if !api.operations().contains(&McpOperation::Status.spec()) {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: McpOperation,
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
    /// Reads and validates one complete redacted observation.
    pub async fn status(&self) -> Result<McpStatus> {
        let status: McpStatus = self.call(McpOperation::Status, &Empty {}).await?;
        status
            .validate()
            .map_err(|_| ApiError::Invalid("Invalid MCP status response".into()))?;
        Ok(status)
    }
    /// Requests one explicit bounded connection verification, without replay.
    pub async fn refresh(&self, request: &McpRefreshRequest) -> Result<McpRefreshResult> {
        request.validate()?;
        let value: McpRefreshResult = self.call(McpOperation::Refresh, request).await?;
        if value.server != request.server {
            return Err(ApiError::OutcomeUnknown);
        }
        value
            .status
            .validate()
            .map_err(|_| ApiError::Invalid("Invalid MCP refresh response".into()))?;
        Ok(value)
    }
    /// Reads the exact configured credential's redacted availability.
    pub async fn credential_status(
        &self,
        target: &McpCredentialTarget,
    ) -> Result<McpCredentialStatus> {
        target.validate()?;
        let value: McpCredentialStatus = self.call(McpOperation::CredentialStatus, target).await?;
        if value.target != *target {
            return Err(ApiError::Invalid("MCP credential binding changed".into()));
        }
        Ok(value)
    }
    /// Sends one exact secret write; reconnection remains a separate user action.
    pub async fn credential_set(
        &self,
        target: &McpCredentialTarget,
        secret: SecretValue,
    ) -> Result<McpCredentialReceipt> {
        #[derive(Serialize)]
        struct Set<'a> {
            target: &'a McpCredentialTarget,
            secret: &'a str,
        }
        target.validate()?;
        let value: McpCredentialReceipt = self
            .call(
                McpOperation::CredentialSet,
                &Set {
                    target,
                    secret: secret.expose_secret(),
                },
            )
            .await?;
        if value.target != *target || value.removed.is_some() {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(value)
    }
    /// Sends one exact secret removal, without replay.
    pub async fn credential_unset(
        &self,
        target: &McpCredentialTarget,
    ) -> Result<McpCredentialReceipt> {
        target.validate()?;
        let value: McpCredentialReceipt = self.call(McpOperation::CredentialUnset, target).await?;
        if value.target != *target || value.removed.is_none() {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(value)
    }
}
