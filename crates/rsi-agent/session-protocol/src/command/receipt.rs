//! Compact command result projections; canonical state remains with its owner.

use super::{CommandRevision, SessionCommandInvocation};
use crate::{
    ContributionId, DomainMutationSource, DomainRequestId, DomainStateCommit, Result, SessionError,
};
use serde::{Deserialize, Serialize};

/// The actual mutation boundary crossed by a command.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandOutcome {
    /// State changed only within the existing process-local draft lease.
    DraftChanged {
        /// Successor draft revision.
        revision: u64,
    },
    /// A canonical command control was durably committed.
    Committed {
        /// Exact canonical control sequence.
        control_seq: u64,
    },
}

/// Bounded receipt for clients; does not duplicate complete domain state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommandReceipt {
    command: ContributionId,
    request_id: DomainRequestId,
    outcome: CommandOutcome,
    invocation_sha256: String,
    state_sha256: String,
}

impl SessionCommandReceipt {
    /// Binds a draft successor to the original invocation and entire new baseline.
    pub fn draft_changed(
        invocation: &SessionCommandInvocation,
        baseline_sha256: impl Into<String>,
    ) -> Result<Self> {
        let CommandRevision::Draft { revision } = invocation.expected_revision else {
            return Err(SessionError::Invalid(
                "draft receipt requires a draft invocation".into(),
            ));
        };
        let revision = revision
            .checked_add(1)
            .ok_or_else(|| SessionError::Invalid("draft revision exhausted".into()))?;
        let receipt = Self {
            command: invocation.command.clone(),
            request_id: invocation.request_id.clone(),
            outcome: CommandOutcome::DraftChanged { revision },
            invocation_sha256: invocation.digest()?,
            state_sha256: baseline_sha256.into(),
        };
        receipt.validate()?;
        Ok(receipt)
    }

    /// Projects a canonical command control with exact predecessor correspondence.
    pub fn committed(control_seq: u64, commit: &DomainStateCommit) -> Result<Self> {
        let DomainMutationSource::Command { invocation } = commit.source() else {
            return Err(SessionError::Invalid(
                "command receipt requires a command control".into(),
            ));
        };
        if invocation.expected_revision
            != (CommandRevision::Durable {
                control_seq: control_seq.saturating_sub(1),
            })
        {
            return Err(SessionError::Invalid(
                "command receipt changed its control predecessor".into(),
            ));
        }
        let receipt = Self {
            command: invocation.command.clone(),
            request_id: invocation.request_id.clone(),
            outcome: CommandOutcome::Committed { control_seq },
            invocation_sha256: invocation.digest()?,
            state_sha256: commit.request_sha256().into(),
        };
        receipt.validate()?;
        Ok(receipt)
    }

    fn validate(&self) -> Result<()> {
        let revision = match self.outcome {
            CommandOutcome::DraftChanged { revision } => revision,
            CommandOutcome::Committed { control_seq } => control_seq,
        };
        if revision == 0 {
            return Err(SessionError::Invalid(
                "command receipt requires a positive successor".into(),
            ));
        }
        crate::validate_sha256("command invocation digest", &self.invocation_sha256)?;
        crate::validate_sha256("command state digest", &self.state_sha256)
    }
    /// Returns the exact dispatched contribution.
    pub const fn command(&self) -> &ContributionId {
        &self.command
    }
    /// Returns the idempotency identity scoped to the selected Session or draft lease.
    pub const fn request_id(&self) -> &DomainRequestId {
        &self.request_id
    }
    /// Returns the actual draft or durable mutation result.
    pub const fn outcome(&self) -> CommandOutcome {
        self.outcome
    }
    /// Returns the successor revision for a later explicit command.
    pub const fn revision(&self) -> CommandRevision {
        match self.outcome {
            CommandOutcome::DraftChanged { revision } => CommandRevision::Draft { revision },
            CommandOutcome::Committed { control_seq } => CommandRevision::Durable { control_seq },
        }
    }
    /// Returns the original complete logical invocation digest.
    pub fn invocation_sha256(&self) -> &str {
        &self.invocation_sha256
    }
    /// Returns the entire draft baseline or canonical durable domain request digest.
    pub fn state_sha256(&self) -> &str {
        &self.state_sha256
    }
}

impl<'de> Deserialize<'de> for SessionCommandReceipt {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            command: ContributionId,
            request_id: DomainRequestId,
            outcome: CommandOutcome,
            invocation_sha256: String,
            state_sha256: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        let receipt = Self {
            command: wire.command,
            request_id: wire.request_id,
            outcome: wire.outcome,
            invocation_sha256: wire.invocation_sha256,
            state_sha256: wire.state_sha256,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}
