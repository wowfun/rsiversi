//! Explicitly granted, reviewed single-leaf Host Profile operations.
use super::{
    ApiClient, ApiError, DeviceId, OperationAccess, OperationClass, OperationEffect, OperationId,
    OperationSpec, RequestEncoding, Result, call_json,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::sync::Arc;
#[path = "leaf_validate.rs"]
mod validate;
use rsi_api_protocol::HostEpoch;
pub use validate::validate_leaf;

/// Authenticated human identity or an independently granted native Agent Session.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Principal {
    /// Trusted local application; leaf writes still require an explicit scope grant.
    Local,
    /// Exact authenticated device.
    Device(DeviceId),
    /// Exact native Session; never inherited from its human submitter.
    Agent(rsi_agent_session_protocol::SessionId),
}
/// Closed source mutation classes; enable authority does not imply configuration writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Enable one leaf under already enabled ancestors.
    Enable,
    /// Disable one leaf.
    Disable,
    /// Replace its entire literal configuration.
    Configuration,
}
/// Exact source selection, independent of a client-local filesystem path.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Host-issued source-root identity.
    pub root: String,
    /// Existing writable user Host Profile.
    pub profile: String,
    /// Existing all-tree plugin leaf identity.
    pub leaf: String,
}
/// One separately granted principal, exact source and mutation class.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// Actual caller identity selected by a Local administrator.
    pub principal: Principal,
    /// Source selection.
    pub target: Target,
    /// Only this operation is authorized.
    pub operation: ChangeKind,
}
/// Local-only durable grant observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    /// Exact canonical decimal CAS revision.
    pub revision: String,
    /// Sorted unique scopes, at most 256.
    pub scopes: Vec<Grant>,
}
/// Local-issued grant CAS. A lost reply is reconciled by reading grants.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetGrant {
    /// Expected durable grant revision.
    pub expected: String,
    /// Exact scope to add or remove.
    pub scope: Grant,
    /// Desired grant state.
    pub granted: bool,
}
/// One literal leaf mutation; caller-supplied configuration is never returned in previews.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// Set the leaf's own enabled flag.
    Enabled {
        /// Desired literal flag.
        enabled: bool,
    },
    /// Replace the complete desired configuration.
    Configuration {
        /// Exact bounded JSON.
        value: Value,
    },
}
impl std::fmt::Debug for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enabled { enabled } => f.debug_tuple("Enabled").field(enabled).finish(),
            Self::Configuration { .. } => f.write_str("Configuration(<redacted>)"),
        }
    }
}
impl Change {
    /// Returns the exact separately granted mutation class.
    pub const fn kind(&self) -> ChangeKind {
        match self {
            Self::Enabled { enabled: true } => ChangeKind::Enable,
            Self::Enabled { enabled: false } => ChangeKind::Disable,
            Self::Configuration { .. } => ChangeKind::Configuration,
        }
    }
    /// Bounds input before retaining a preview or invoking trusted plugin preparation.
    pub fn validate(&self) -> Result<()> {
        if let Self::Configuration { value } = self {
            validate_configuration(value)?;
        }
        Ok(())
    }
}
/// Bounds exact JSON before any source transform, preparation or retained preview.
pub fn validate_configuration(value: &Value) -> Result<()> {
    let mut pending = vec![(value, 0)];
    let mut nodes = 0;
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > 32 || nodes > 4096 {
            return Err(ApiError::Invalid(
                "Profile configuration bound exceeded".into(),
            ));
        }
        match value {
            Value::Object(values) => {
                if values.len() + pending.len() > 4096 {
                    return Err(ApiError::Invalid(
                        "Profile configuration bound exceeded".into(),
                    ));
                }
                for (key, value) in values {
                    bytes = bytes.saturating_add(key.len());
                    pending.push((value, depth + 1));
                }
            }
            Value::Array(values) => {
                if values.len() + pending.len() > 4096 {
                    return Err(ApiError::Invalid(
                        "Profile configuration bound exceeded".into(),
                    ));
                }
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::String(value) => bytes = bytes.saturating_add(value.len()),
            Value::Number(value) => bytes = bytes.saturating_add(value.as_str().len()),
            _ => {}
        }
        if bytes > 64 * 1024 {
            return Err(ApiError::Invalid(
                "Profile configuration bound exceeded".into(),
            ));
        }
    }
    if serde_json::to_vec(value)
        .map_err(|_| ApiError::Invalid("Profile configuration encoding failed".into()))?
        .len()
        > 64 * 1024
    {
        return Err(ApiError::Invalid(
            "Profile configuration bound exceeded".into(),
        ));
    }
    Ok(())
}
/// Grant-gated request for one prepared preview.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    /// Exact source selected from this Host's catalog.
    pub target: Target,
    /// Single literal mutation.
    pub change: Change,
}
/// One source/publication observation. No field implies runtime activation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    /// Host that retains the proposal.
    pub host_epoch: HostEpoch,
    /// Exact retained proposal identity.
    pub ticket: String,
    /// Exact source target.
    pub target: Target,
    /// Granted operation class.
    pub operation: ChangeKind,
    /// Review digest including original bytes, dependencies, proposal and composition.
    pub digest: String,
    /// Proposed complete source digest.
    pub source_digest: String,
    /// Selected plugin key, with no configuration value.
    pub plugin: String,
    /// Previous own enabled flag.
    pub previous_enabled: bool,
    /// Proposed own enabled flag.
    pub enabled: bool,
    /// Proposed visibility after parent-group disabling.
    pub effective_enabled: bool,
}
/// Exact review accepted by the caller; the ticket is also its retained receipt identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    /// Same Host as preview.
    pub host_epoch: HostEpoch,
    /// Exact reviewed proposal.
    pub ticket: String,
    /// Exact digest displayed during review.
    pub digest: String,
}
/// One ticket read or discarded only by its original principal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    /// Same Host as preview.
    pub host_epoch: HostEpoch,
    /// Proposal/receipt identity.
    pub ticket: String,
}
/// Redacted failure category; never includes source or plugin diagnostic payloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "ancestor",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Failure {
    /// Source, dependencies or frozen catalog changed.
    Conflict,
    /// The requested node is not a plugin leaf.
    NotLeaf,
    /// An exact disabled ancestor blocks enabling.
    DisabledAncestor(String),
    /// A proposed enabled plugin rejected preparation.
    Preparation,
    /// Source cannot be read/written through its protected boundary.
    Source,
    /// Bounded capacity is occupied.
    Busy,
    /// Caller no longer has scope authority.
    Unauthorized,
}
/// Actual observed application, separate from source publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Application {
    /// This Profile is not the current running source.
    NotSelected,
    /// Source is saved and current owner has not converged yet.
    Pending,
    /// Desired and observed source are converged without failed instances.
    Applied,
    /// The Profile owner explicitly requires restart.
    RestartRequired,
    /// The current source or graph failed convergence.
    Failed,
    /// Current owner no longer establishes application state.
    Unknown,
}
/// Retained source write outcome; lost replies are queried, never implicitly repeated.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    /// Exact operation still owned.
    Pending,
    /// Atomic source replacement returned successfully.
    Saved {
        /// Directory durability result after publication.
        directory_synced: bool,
        /// Independent current Profile observation.
        application: Application,
    },
    /// Determinate rejection before source replacement.
    Failed {
        /// Closed reason.
        failure: Failure,
    },
    /// Owner cannot prove source write outcome.
    Unknown,
}
/// Exact ticket's bounded receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Reviewed source and configuration identity, always redacted.
    pub preview: Preview,
    /// Source result and separate runtime observation.
    pub outcome: Outcome,
}
/// Finite source catalog query. Omitted profile lists writable Host roots only.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRequest {
    /// Existing user Host Profile to inspect, without preparation.
    pub profile: Option<String>,
    /// Exact lexical continuation from the previous page.
    pub after: Option<String>,
}
/// One effective source leaf without raw configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Leaf {
    /// Exact target for preview/grant.
    pub target: Target,
    /// Plugin catalog identity.
    pub plugin: String,
    /// Own enabled flag.
    pub enabled: bool,
    /// Effective flag including ancestors.
    pub effective_enabled: bool,
    /// Current caller's separately granted operation classes.
    pub allowed: Vec<ChangeKind>,
}
/// Redacted source selection. An absent root means no writable root is present.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    /// Owner generation.
    pub host_epoch: HostEpoch,
    /// Caller identity, derived by the authenticated owner.
    pub principal: Principal,
    /// Exact root identity if present.
    pub root: Option<String>,
    /// Profiles when no profile was selected; at most 64.
    pub profiles: Vec<String>,
    /// Leaves in the selected profile; at most 64.
    pub leaves: Vec<Leaf>,
    /// Lexical continuation for this query.
    pub next: Option<String>,
}
/// Exact negotiated Profile leaf wire identities.
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    /// Bounded redacted source metadata.
    Catalog,
    /// Prepare one explicitly granted proposal.
    Preview,
    /// Recover this principal's at most four uncommitted proposals after reply loss.
    Previews,
    /// Publish one reviewed proposal.
    Commit,
    /// Reconcile its original receipt.
    Receipt,
    /// Recover this principal's retained receipt ticket identities.
    Receipts,
    /// Release an unused proposal.
    Discard,
    /// Local grant snapshot.
    Grants,
    /// Local expected-revision grant mutation.
    SetGrant,
}
impl Operation {
    /// Returns the closed wire bounds and access policy.
    ///
    /// # Panics
    /// Static operation names satisfy API identifiers.
    pub fn spec(self) -> OperationSpec {
        OperationSpec {
            id: OperationId::new(
                "profile-leaves",
                match self {
                    Self::Catalog => "catalog",
                    Self::Preview => "preview",
                    Self::Previews => "previews",
                    Self::Commit => "commit",
                    Self::Receipt => "receipt",
                    Self::Receipts => "receipts",
                    Self::Discard => "discard",
                    Self::Grants => "grants",
                    Self::SetGrant => "set-grant",
                },
                1,
            )
            .expect("static profile operation"),
            access: if matches!(self, Self::Grants | Self::SetGrant) {
                OperationAccess::Local
            } else {
                OperationAccess::Authenticated
            },
            class: OperationClass::Data,
            effect: if matches!(self, Self::Commit | Self::Discard | Self::SetGrant) {
                OperationEffect::Mutation
            } else {
                OperationEffect::Read
            },
            encoding: RequestEncoding::Json,
            maximum_request_bytes: 128 * 1024,
            maximum_response_bytes: 64 * 1024,
        }
    }
}

/// Public response preserves a known domain rejection separately from reply loss.
pub type Reply<T> = Result<std::result::Result<T, Failure>>;
/// One authenticated connection; unknown source mutations are never replayed.
#[derive(Clone, Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
impl Client {
    /// Requires the closed human management operations; Local grant methods negotiate separately.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        for op in [
            Operation::Catalog,
            Operation::Preview,
            Operation::Previews,
            Operation::Commit,
            Operation::Receipt,
            Operation::Receipts,
            Operation::Discard,
        ] {
            if !api.operations().contains(&op.spec()) {
                return Err(ApiError::Unavailable);
            }
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        op: Operation,
        input: &I,
    ) -> Reply<O> {
        if !self.api.operations().contains(&op.spec()) {
            return Err(ApiError::Unavailable);
        }
        let result = call_json(self.api.as_ref(), &op.spec(), input).await?;
        if let Err(error) = &result {
            Failure::validate(error)?;
        }
        Ok(result)
    }
    /// Reads one redacted page of source choices and current caller authority.
    pub async fn catalog(&self, request: CatalogRequest) -> Reply<Catalog> {
        request.validate()?;
        let result = self.call(Operation::Catalog, &request).await?;
        if let Ok(page) = &result {
            Catalog::validate(page, &request, &self.api.description().host_epoch)?;
        }
        Ok(result)
    }
    /// Runs a granted preparation and retains its exact redacted proposal.
    pub async fn preview(&self, request: PreviewRequest) -> Reply<Preview> {
        request.target.validate()?;
        request.change.validate()?;
        let result = self.call(Operation::Preview, &request).await?;
        if let Ok(preview) = &result {
            Preview::validate(preview, &self.api.description().host_epoch)?;
            if preview.target != request.target || preview.operation != request.change.kind() {
                return Err(ApiError::Unavailable);
            }
        }
        Ok(result)
    }
    /// Recovers only this principal's bounded retained proposals; never prepares again.
    pub async fn previews(&self) -> Reply<Vec<Preview>> {
        let result = self
            .call::<_, Vec<Preview>>(Operation::Previews, &serde_json::json!({}))
            .await?;
        if let Ok(previews) = &result {
            validate::previews(previews, &self.api.description().host_epoch)?;
        }
        Ok(result)
    }
    /// Admits at most one source publication for this retained ticket.
    pub async fn commit(&self, preview: &Preview) -> Reply<Receipt> {
        preview.validate(&self.api.description().host_epoch)?;
        let result = self
            .call(
                Operation::Commit,
                &Commit {
                    host_epoch: preview.host_epoch.clone(),
                    ticket: preview.ticket.clone(),
                    digest: preview.digest.clone(),
                },
            )
            .await?;
        if let Ok(receipt) = &result {
            Receipt::validate(receipt, &self.api.description().host_epoch)
                .map_err(|_| ApiError::OutcomeUnknown)?;
            if receipt.preview != *preview {
                return Err(ApiError::OutcomeUnknown);
            }
        }
        Ok(result)
    }
    /// Queries the same ticket after reply loss without repeating its write.
    pub async fn receipt(&self, ticket: &str) -> Reply<Receipt> {
        validate::hex(ticket, 32)?;
        let result = self
            .call(
                Operation::Receipt,
                &Ticket {
                    host_epoch: self.api.description().host_epoch.clone(),
                    ticket: ticket.into(),
                },
            )
            .await?;
        if let Ok(receipt) = &result {
            Receipt::validate(receipt, &self.api.description().host_epoch)?;
            if receipt.preview.ticket != ticket {
                return Err(ApiError::Unavailable);
            }
        }
        Ok(result)
    }
    /// Lists only this principal's bounded exact receipt identities after reconnect.
    pub async fn receipts(&self) -> Reply<Vec<String>> {
        let result = self
            .call::<_, Vec<String>>(Operation::Receipts, &serde_json::json!({}))
            .await?;
        if let Ok(tickets) = &result {
            if tickets.len() > 256 || tickets.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(ApiError::Unavailable);
            }
            for ticket in tickets {
                validate::hex(ticket, 32)?;
            }
        }
        Ok(result)
    }
    /// Releases an uncommitted proposal belonging to this caller.
    pub async fn discard(&self, ticket: &str) -> Reply<()> {
        validate::hex(ticket, 32)?;
        self.call(
            Operation::Discard,
            &Ticket {
                host_epoch: self.api.description().host_epoch.clone(),
                ticket: ticket.into(),
            },
        )
        .await
    }
    /// Reads the Local administrator's complete bounded grant document.
    pub async fn grants(&self) -> Reply<Grants> {
        let result = self.call(Operation::Grants, &serde_json::json!({})).await?;
        if let Ok(grants) = &result {
            Grants::validate(grants)?;
        }
        Ok(result)
    }
    /// Changes one explicit grant with revision CAS; never available remotely.
    pub async fn set_grant(&self, request: SetGrant) -> Reply<Grants> {
        request.validate()?;
        let result = self.call(Operation::SetGrant, &request).await?;
        if let Ok(grants) = &result {
            Grants::validate(grants).map_err(|_| ApiError::OutcomeUnknown)?;
            if super::revision(&request.expected)?.checked_add(1)
                != Some(super::revision(&grants.revision)?)
                || grants.scopes.contains(&request.scope) != request.granted
            {
                return Err(ApiError::OutcomeUnknown);
            }
        }
        Ok(result)
    }
}
