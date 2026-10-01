//! Exact SSH stdio management; wire coordinates are never grants.
use crate::{
    ApiClient, ApiError, Arc, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json,
};
use rsi_api_protocol::HostEpoch;
use rsi_execution_protocol::ExecutionTargetId;
use rsi_mcp_protocol::{McpError, ServerConfig, TransportConfig};
use serde::{Deserialize, Serialize};

/// Exact server selected in the negotiated Host lifetime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Current Host lifetime.
    pub host_epoch: HostEpoch,
    /// Exact execution target.
    pub target: ExecutionTargetId,
    /// Configured server identity.
    pub server: String,
}
impl Target {
    /// Checks bounded identity before grants or target I/O.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        if &self.host_epoch != epoch {
            return Err(ApiError::Invalid("Stale MCP Host lifetime".into()));
        }
        rsi_mcp_protocol::validate_server_id(&self.server)
            .map_err(|_| ApiError::Invalid("Invalid MCP server".into()))
    }
}
/// One finite configuration mutation or explicit refresh.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// Exact target/server selection.
    pub target: Target,
    /// Canonical document revision, also fencing deletion and recreation.
    pub expected: String,
    /// Present only for Put; launch policy remains explicit.
    pub config: Option<ServerConfig>,
}
impl Change {
    /// Checks shape and target identity before authorizing references.
    pub fn validate(&self, epoch: &HostEpoch, put: bool) -> Result<()> {
        self.target.validate(epoch)?;
        crate::revision(&self.expected)?;
        if put != self.config.is_some() {
            return Err(ApiError::Invalid("Invalid SSH stdio operation".into()));
        }
        if let Some(config) = &self.config {
            rsi_mcp_protocol::McpConfig {
                servers: vec![config.clone()],
            }
            .validate()
            .map_err(ApiError::Invalid)?;
            if config.id != self.target.server
                || !matches!(&config.transport, TransportConfig::SshStdio { target, .. } if target == &self.target.target)
            {
                return Err(ApiError::Invalid(
                    "MCP target does not match its selection".into(),
                ));
            }
        }
        Ok(())
    }
}
/// Authorized configuration observation or committed mutation receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// Echoed selection.
    pub target: Target,
    /// Current document revision.
    pub revision: String,
    /// This exact server only; literals are visible solely to its administrator.
    pub config: Option<ServerConfig>,
    /// Durable mutation is committed even when connection application fails.
    pub apply_error: Option<McpError>,
}
impl State {
    /// Validates the full response against the exact requested server.
    pub fn validate(&self, expected: &Target) -> Result<()> {
        if &self.target != expected {
            return Err(ApiError::Invalid("MCP reply changed its target".into()));
        }
        Change {
            target: self.target.clone(),
            expected: self.revision.clone(),
            config: self.config.clone(),
        }
        .validate(&expected.host_epoch, self.config.is_some())
    }
}
/// Closed management outcomes independent of transport errors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    /// Another configuration change advanced the document.
    Conflict,
    /// The selected server does not exist.
    NotFound,
    /// Complete local/HTTP/SSH catalog identities or budgets conflict.
    Configuration,
}
/// Independently negotiated management operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Read one authorized configuration.
    Get,
    /// Commit complete server inputs.
    Put,
    /// Remove one server and retire its connection.
    Remove,
    /// Explicitly connect under current Use authority.
    Refresh,
}
impl Operation {
    /// Finite versioned API contract.
    #[expect(clippy::missing_panics_doc, reason = "fixed operation identity")]
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "mcp-configuration",
                match self {
                    Self::Get => "ssh-get",
                    Self::Put => "ssh-put",
                    Self::Remove => "ssh-remove",
                    Self::Refresh => "ssh-refresh",
                },
                1,
            )
            .expect("static MCP SSH operation"),
            access: OperationAccess::Authenticated,
            class: OperationClass::Data,
            effect: if self == Self::Get {
                OperationEffect::Read
            } else {
                OperationEffect::Mutation
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: if self == Self::Put { 256 * 1024 } else { 4096 },
            maximum_response_bytes: 384 * 1024,
        }
    }
}
/// Authenticated client; the server independently enforces exact grants.
#[derive(Clone)]
pub struct Client(Arc<dyn ApiClient>);
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSshClient").finish_non_exhaustive()
    }
}
impl Client {
    /// Retains the negotiated API connection.
    pub fn new(api: Arc<dyn ApiClient>) -> Self {
        Self(api)
    }
    /// Reads only the exact authorized server.
    pub async fn get(&self, target: Target) -> Result<std::result::Result<State, Failure>> {
        target.validate(&self.0.description().host_epoch)?;
        let result: std::result::Result<State, Failure> =
            call_json(self.0.as_ref(), &Operation::Get.spec(), &target).await?;
        if let Ok(state) = &result {
            state.validate(&target)?;
        }
        Ok(result)
    }
    /// Sends one explicit mutation without retrying an uncertain effect.
    pub async fn change(
        &self,
        operation: Operation,
        change: Change,
    ) -> Result<std::result::Result<State, Failure>> {
        if operation == Operation::Get {
            return Err(ApiError::Invalid("Get requires a Target".into()));
        }
        change.validate(
            &self.0.description().host_epoch,
            operation == Operation::Put,
        )?;
        let result: std::result::Result<State, Failure> =
            call_json(self.0.as_ref(), &operation.spec(), &change).await?;
        if let Ok(state) = &result {
            state.validate(&change.target)?;
        }
        Ok(result)
    }
}
