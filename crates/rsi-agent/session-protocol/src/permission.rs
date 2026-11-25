use super::{
    DomainSnapshot, Result, SandboxMode, SessionError, SessionHeader, validate_safe_diagnostic,
};
use serde::{Deserialize, Serialize};

/// How to resolve an actual approval requirement in this Session.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalPromptPolicy {
    /// Consult the configured approval answerers.
    #[default]
    Ask,
    /// Deny the requested action without asking an answerer.
    Never,
}

/// The knobs and preset identity selected together by the Session owner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionPermission {
    /// Public name from the contributing permission preset catalog.
    pub preset: String,
    /// Requested filesystem boundary.
    pub sandbox: SandboxMode,
    /// Resolution policy for actions that still require approval.
    pub approval: ApprovalPromptPolicy,
}

impl SessionPermission {
    /// Session domain shared by the permission command and its execution consumers.
    pub const DOMAIN: &'static str = "rsi.permission";
    /// Current persisted permission selection format.
    pub const VERSION: u32 = 1;
    /// Owner-only command updating the selected pair of knobs.
    pub const COMMAND: &'static str = "rsi.permission.select";
    /// Projection of the selection used by connected clients.
    pub const VIEW: &'static str = "rsi.permission.view";

    /// Resolves a native composer's selection from the standard permission catalog.
    pub fn standard(name: &str) -> Result<Self> {
        let (sandbox, approval) = match name {
            "read-only" => (SandboxMode::ReadOnly, ApprovalPromptPolicy::Ask),
            "workspace-write" => (SandboxMode::WorkspaceWrite, ApprovalPromptPolicy::Ask),
            "danger-full-access" => (SandboxMode::DangerFullAccess, ApprovalPromptPolicy::Never),
            _ => {
                return Err(SessionError::Invalid(
                    "Unknown standard permission preset".into(),
                ));
            }
        };
        Ok(Self {
            preset: name.into(),
            sandbox,
            approval,
        })
    }

    /// Validates a catalog name crossing the command or persistence boundary.
    pub fn validate(&self) -> Result<()> {
        validate_safe_diagnostic("permission preset", &self.preset)
    }

    /// Bounds an inherited sandbox to the narrower ceiling frozen for this child.
    pub fn bounded_sandbox(header: &SessionHeader, selected: SandboxMode) -> SandboxMode {
        if header.execution_owner().is_none() {
            return selected;
        }
        Self::narrower_sandbox(header.settings().sandbox(), selected)
    }

    /// Intersects a delegate's filesystem scope with its caller's authority.
    pub fn narrower_sandbox(first: SandboxMode, second: SandboxMode) -> SandboxMode {
        use SandboxMode::{DangerFullAccess, ReadOnly, WorkspaceWrite};
        match (first, second) {
            (ReadOnly, _) | (_, ReadOnly) => ReadOnly,
            (WorkspaceWrite, _) | (_, WorkspaceWrite) => WorkspaceWrite,
            (DangerFullAccess, DangerFullAccess) => DangerFullAccess,
        }
    }

    /// Resolves the initial prompt policy before a Session has a selection.
    pub fn initial_approval(header: &SessionHeader) -> ApprovalPromptPolicy {
        if matches!(
            header.execution_owner(),
            Some(
                super::ExecutionOwner::Plugin { .. } | super::ExecutionOwner::TurnActivation { .. }
            )
        ) {
            ApprovalPromptPolicy::Never
        } else {
            header
                .settings()
                .permission()
                .map_or(ApprovalPromptPolicy::Ask, |value| value.approval)
        }
    }

    /// Reads a versioned snapshot. An unselected Session retains its configured policy.
    pub fn from_snapshot(snapshot: &DomainSnapshot) -> Result<Option<Self>> {
        if snapshot.identity().id() != Self::DOMAIN
            || snapshot.identity().version() != Self::VERSION
        {
            return Err(SessionError::Invalid(
                "unsupported Session permission domain".into(),
            ));
        }
        let selected: Option<Self> = Deserialize::deserialize(snapshot.state().value())
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        if let Some(value) = &selected {
            value.validate()?;
        }
        Ok(selected)
    }
}
