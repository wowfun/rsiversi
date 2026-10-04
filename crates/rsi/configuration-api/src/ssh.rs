//! SSH target data never grants permission to trust or execute on a machine.
use crate::{
    ApiClient, ApiError, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json, revision,
};
use rsi_api_protocol::HostEpoch;
use rsi_execution_protocol::ExecutionTargetId;
use rsi_ssh_protocol::{SshEndpoint, SshHostKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::sync::Arc;

/// A candidate identity is supplied once so reply loss cannot create duplicate targets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    /// Stable identity independent of hostnames and connection epochs.
    pub target: ExecutionTargetId,
    /// Human label, at most 128 bytes without control characters.
    pub name: String,
    /// Strict literal address with no SSH options or aliases.
    pub endpoint: SshEndpoint,
}
impl Candidate {
    /// Validates display data before retaining it.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
/// Exact target revision in one Host lifetime.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    /// Host lifetime negotiated by this connection.
    pub host_epoch: HostEpoch,
    /// Target identity, never an SSH alias.
    pub target: ExecutionTargetId,
    /// Canonical positive decimal target revision.
    pub revision: String,
}
impl Selection {
    /// Validates the generation and nonzero revision.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        if &self.host_epoch != epoch || revision(&self.revision)? == 0 {
            return Err(invalid());
        }
        Ok(())
    }
}
/// Connection CAS prevents a stale view from disconnecting a replacement epoch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRequest {
    /// Exact durable candidate revision.
    pub selection: Selection,
    /// Last observed live epoch; None requires a disconnected target.
    pub expected_connection_epoch: Option<String>,
}
impl ConnectionRequest {
    /// Validates both durable and transient identity before dispatch.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        self.selection.validate(epoch)?;
        if let Some(value) = &self.expected_connection_epoch
            && revision(value)? == 0
        {
            return Err(invalid());
        }
        Ok(())
    }
}
/// Target directory resolution for an explicitly selected connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveDirectory {
    /// Exact target revision and last observed connection epoch.
    pub connection: ConnectionRequest,
    /// Absolute POSIX target path, interpreted only by its helper.
    pub path: String,
}
impl ResolveDirectory {
    /// Validates bounds before any target I/O.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        self.connection.validate(epoch)?;
        rsi_ssh_protocol::execution::validate_path(&self.path).map_err(|_| invalid())
    }
}
/// Candidate creation or replacement with revision CAS.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutCandidate {
    /// Negotiated Host lifetime.
    pub host_epoch: HostEpoch,
    /// Zero creates; otherwise names the exact existing revision.
    pub expected: String,
    /// Complete replacement data, without trust or executable policy.
    pub candidate: Candidate,
}
impl PutCandidate {
    /// Validates bounded data and the negotiated lifetime.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        if &self.host_epoch != epoch {
            return Err(invalid());
        }
        revision(&self.expected)?;
        self.candidate.validate()
    }
}
/// Local-only trust decision. This request must never be available to a Web principal.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmTrust {
    /// Exact candidate being confirmed.
    pub selection: Selection,
    /// Public key verified out of band by the Local administrator.
    pub host_key: SshHostKey,
    /// Exact displayed SHA256 fingerprint being confirmed.
    pub fingerprint: String,
    /// Absolute Local private-key path; never returned in a target catalog.
    pub identity_path: String,
}
impl std::fmt::Debug for ConfirmTrust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmTrust")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}
impl ConfirmTrust {
    /// Rejects mismatched confirmations and unbounded paths before filesystem access.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        self.selection.validate(epoch)?;
        if self.fingerprint != self.host_key.fingerprint()
            || !self.identity_path.starts_with('/')
            || self.identity_path.len() > 16 * 1024
            || self.identity_path.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
/// Effective permission display; these booleans cannot admit operations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Current caller may acquire a live execution lease.
    pub use_target: bool,
    /// Current caller may edit this exact candidate.
    pub manage: bool,
}
/// Redacted target state; every observation is scoped to the authenticated caller.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Complete public candidate data.
    pub candidate: Candidate,
    /// Current durable revision, including trust changes.
    pub revision: String,
    /// Present only after an explicit Local confirmation.
    pub fingerprint: Option<String>,
    /// Current live connection epoch, absent after restart or disconnect.
    pub connection_epoch: Option<String>,
    /// Whether that retained epoch is currently available for execution.
    pub connected: bool,
    /// Explicitly selected target programs absent during initialization.
    pub unavailable_programs: Vec<String>,
    /// Current caller's distinct scopes.
    pub permissions: Permissions,
}
impl Target {
    /// Validates redacted externally decoded state.
    pub fn validate(&self) -> Result<()> {
        self.candidate.validate()?;
        if revision(&self.revision)? == 0 || self.unavailable_programs.len() > 128 {
            return Err(invalid());
        }
        if let Some(value) = &self.fingerprint
            && (!value.starts_with("SHA256:")
                || value.len() != 50
                || !value[7..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/')))
        {
            return Err(invalid());
        }
        if let Some(value) = &self.connection_epoch
            && (revision(value)? == 0 || self.fingerprint.is_none())
        {
            return Err(invalid());
        }
        if self.connection_epoch.is_none()
            && (self.connected || !self.unavailable_programs.is_empty())
        {
            return Err(invalid());
        }
        if self.unavailable_programs.windows(2).any(|v| v[0] >= v[1]) {
            return Err(invalid());
        }
        for selector in &self.unavailable_programs {
            rsi_ssh_protocol::execution::validate_selector(selector).map_err(|_| invalid())?;
        }
        Ok(())
    }
}
/// Complete bounded caller-visible catalog.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    /// Current Host lifetime.
    pub host_epoch: HostEpoch,
    /// Identity-sorted targets, at most 64.
    pub targets: Vec<Target>,
}
impl Catalog {
    /// Rejects stale Host state, duplicates and invalid target projections.
    pub fn validate(&self, epoch: &HostEpoch) -> Result<()> {
        if &self.host_epoch != epoch
            || self.targets.len() > 64
            || self
                .targets
                .windows(2)
                .any(|v| v[0].candidate.target >= v[1].candidate.target)
        {
            return Err(invalid());
        }
        for target in &self.targets {
            target.validate()?;
        }
        Ok(())
    }
}
/// Known finite rejection, separate from a lost mutation response.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Failure {
    /// Another operation currently owns this target or registry publication.
    Busy {},
    /// Expected target revision differs.
    Conflict {},
    /// Target has no current Local trust confirmation.
    TrustRequired {},
    /// Identity changed, lacks private permissions, or is unavailable.
    IdentityUnavailable {},
    /// Adjacent verified helper distribution is unavailable.
    HelperUnavailable {},
    /// SSH initialization did not publish a connection.
    ConnectionFailed {},
    /// Remote cache admission expired; this is not evidence of absent remote effects.
    CacheContentionTimeout {},
}
/// Known domain outcome is distinct from transport uncertainty.
pub type Reply<T> = Result<std::result::Result<T, Failure>>;
/// Closed target operations.
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    /// Canonicalize a directory through the admitted target lease.
    ResolveDirectory,
    /// Current caller-visible target states.
    Catalog,
    /// Submit or replace one candidate.
    PutCandidate,
    /// Confirm the exact endpoint key and Local identity.
    ConfirmTrust,
    /// Deploy and initialize one new connection epoch.
    Connect,
    /// Explicitly close the selected connection.
    Disconnect,
}
impl Operation {
    /// Exact negotiated limits and authority boundary.
    ///
    /// # Panics
    /// Static operation names satisfy the API identifier grammar.
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "ssh-targets",
                match self {
                    Self::Catalog => "catalog",
                    Self::ResolveDirectory => "resolve-directory",
                    Self::PutCandidate => "put-candidate",
                    Self::ConfirmTrust => "confirm-trust",
                    Self::Connect => "connect",
                    Self::Disconnect => "disconnect",
                },
                1,
            )
            .expect("static SSH operation"),
            access: if matches!(self, Self::ConfirmTrust) {
                OperationAccess::Local
            } else {
                OperationAccess::Authenticated
            },
            class: OperationClass::Data,
            effect: if matches!(self, Self::Catalog | Self::ResolveDirectory) {
                OperationEffect::Read
            } else {
                OperationEffect::Mutation
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 32 * 1024,
            maximum_response_bytes: 128 * 1024,
        }
    }
}
/// Connection-scoped client; unknown mutations are never repeated automatically.
#[derive(Clone, Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
impl Client {
    /// Requires the authenticated target operations; Local trust negotiates separately.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        for operation in [
            Operation::Catalog,
            Operation::ResolveDirectory,
            Operation::PutCandidate,
            Operation::Connect,
            Operation::Disconnect,
        ] {
            if !api.operations().contains(&operation.spec()) {
                return Err(ApiError::Unavailable);
            }
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: Operation,
        input: &I,
    ) -> Reply<O> {
        if !self.api.operations().contains(&operation.spec()) {
            return Err(ApiError::Unavailable);
        }
        call_json(self.api.as_ref(), &operation.spec(), input).await
    }
    /// Resolves only on the selected target, never on the Service filesystem.
    pub async fn resolve_directory(
        &self,
        input: ResolveDirectory,
    ) -> Reply<rsi_execution_protocol::ExecutionCoordinates> {
        input.validate(&self.api.description().host_epoch)?;
        let result = self
            .call::<_, rsi_execution_protocol::ExecutionCoordinates>(
                Operation::ResolveDirectory,
                &input,
            )
            .await?;
        if let Ok(value) = &result
            && value.location()
                != &(rsi_execution_protocol::ExecutionLocation::Ssh {
                    target: input.connection.selection.target,
                })
        {
            return Err(ApiError::Unavailable);
        }
        Ok(result)
    }
    /// Observes without granting permission or creating a connection.
    pub async fn catalog(&self) -> Reply<Catalog> {
        let result = self
            .call::<_, Catalog>(Operation::Catalog, &serde_json::json!({}))
            .await?;
        if let Ok(value) = &result {
            value.validate(&self.api.description().host_epoch)?;
        }
        Ok(result)
    }
    async fn mutate<I: Serialize + Sync>(
        &self,
        operation: Operation,
        input: &I,
        target: &ExecutionTargetId,
    ) -> Reply<Target> {
        let result = self.call::<_, Target>(operation, input).await?;
        if let Ok(value) = &result {
            value.validate().map_err(|_| ApiError::OutcomeUnknown)?;
            if &value.candidate.target != target {
                return Err(ApiError::OutcomeUnknown);
            }
        }
        Ok(result)
    }
    /// Writes one exact candidate revision; zero means creation.
    pub async fn put_candidate(&self, input: PutCandidate) -> Reply<Target> {
        input.validate(&self.api.description().host_epoch)?;
        let result = self
            .mutate(Operation::PutCandidate, &input, &input.candidate.target)
            .await?;
        if let Ok(value) = &result
            && (value.candidate != input.candidate
                || revision(&input.expected)?.checked_add(1) != Some(revision(&value.revision)?))
        {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(result)
    }
    /// Confirms Local trust without granting any device Use scope.
    pub async fn confirm_trust(&self, input: ConfirmTrust) -> Reply<Target> {
        input.validate(&self.api.description().host_epoch)?;
        let result = self
            .mutate(Operation::ConfirmTrust, &input, &input.selection.target)
            .await?;
        if let Ok(value) = &result
            && (value.fingerprint.as_ref() != Some(&input.fingerprint)
                || revision(&input.selection.revision)?.checked_add(1)
                    != Some(revision(&value.revision)?))
        {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(result)
    }
    /// Connects only after owner-side Use admission and trust validation.
    pub async fn connect(&self, input: ConnectionRequest) -> Reply<Target> {
        input.validate(&self.api.description().host_epoch)?;
        let result = self
            .mutate(Operation::Connect, &input, &input.selection.target)
            .await?;
        if let Ok(value) = &result
            && (value.revision != input.selection.revision || value.connection_epoch.is_none())
        {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(result)
    }
    /// Stops the exact currently selected target connection.
    pub async fn disconnect(&self, input: ConnectionRequest) -> Reply<Target> {
        input.validate(&self.api.description().host_epoch)?;
        let result = self
            .mutate(Operation::Disconnect, &input, &input.selection.target)
            .await?;
        if let Ok(value) = &result
            && (value.revision != input.selection.revision || value.connection_epoch.is_some())
        {
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(result)
    }
}
fn invalid() -> ApiError {
    ApiError::Invalid("invalid SSH target data".into())
}
