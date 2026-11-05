//! Immutable history provenance, independent of Agent execution ownership.
use crate::{
    AgentPath, EMPTY_CONTROL_PREFIX_DIGEST, EMPTY_FACT_PREFIX_DIGEST, ForkTurnSelection, Result,
    SessionError, SessionId, TurnId, validate_identifier, validate_sha256,
};
use serde::{Deserialize, Serialize};

/// Maximum inherited source intervals retained by one history selection.
pub const MAXIMUM_FORK_HISTORY_DEPTH: usize = 32;

/// Exact durable source boundary. A through point must already be terminal.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ForkPoint {
    /// Excludes the invoking Turn and every later Turn.
    BeforeTurn {
        /// Exact invoking identity.
        turn_id: TurnId,
    },
    /// Includes this completed Turn and excludes every later Turn.
    ThroughTurn {
        /// Exact terminal identity.
        turn_id: TurnId,
    },
}
impl ForkPoint {
    /// Selects history strictly before an invoking Turn.
    pub fn before(turn_id: &TurnId) -> Self {
        Self::BeforeTurn {
            turn_id: turn_id.clone(),
        }
    }
    /// Selects history through an already completed Turn.
    pub fn through(turn_id: &TurnId) -> Self {
        Self::ThroughTurn {
            turn_id: turn_id.clone(),
        }
    }
    /// Borrows the source Turn selected by this point.
    pub const fn turn_id(&self) -> &TurnId {
        match self {
            Self::BeforeTurn { turn_id } | Self::ThroughTurn { turn_id } => turn_id,
        }
    }
}

/// Exact selected history with authenticated source Header and prefix bindings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryOrigin {
    /// Exact parent session.
    pub parent_session_id: SessionId,
    /// Fingerprint of the immutable parent Header.
    pub parent_header_fingerprint: String,
    /// Exact source Turn and whether its terminal is included.
    pub point: ForkPoint,
    /// Exclusive parent Fact cursor before the first inherited completed Turn.
    pub resolved_after_seq: u64,
    /// Last inherited terminal session sequence, or zero for `none`.
    pub resolved_terminal_seq: u64,
    /// Canonical Fact-prefix digest at the resolved terminal sequence.
    pub terminal_prefix_sha256: String,
    /// Exact canonical control horizon recorded with the selected terminal, or zero.
    pub resolved_terminal_control_seq: u64,
    /// Canonical control-prefix digest at that horizon.
    pub terminal_control_prefix_sha256: String,
    /// Caller-requested selection.
    pub requested_turns: ForkTurnSelection,
    /// Number of completed turns actually retained.
    pub effective_turns: u64,
}

impl HistoryOrigin {
    /// Revalidates the immutable lineage boundary.
    pub fn validate(&self) -> Result<()> {
        self.requested_turns.validate()?;
        validate_sha256("parent header fingerprint", &self.parent_header_fingerprint)?;
        validate_sha256("fork terminal prefix", &self.terminal_prefix_sha256)?;
        validate_sha256(
            "fork terminal control prefix",
            &self.terminal_control_prefix_sha256,
        )?;
        let cursors_empty = self.resolved_after_seq == 0 && self.resolved_terminal_seq == 0;
        if (self.effective_turns == 0) != cursors_empty {
            return Err(SessionError::Invalid(
                "fork interval emptiness must match the effective turn count".into(),
            ));
        }
        let empty = cursors_empty && self.effective_turns == 0;
        if (self.resolved_terminal_control_seq == 0) != empty
            || (empty
                && (self.terminal_prefix_sha256 != hex::encode(EMPTY_FACT_PREFIX_DIGEST)
                    || self.terminal_control_prefix_sha256
                        != hex::encode(EMPTY_CONTROL_PREFIX_DIGEST)))
        {
            return Err(SessionError::Invalid(
                "fork boundary requires an exact Fact/control pair".into(),
            ));
        }
        if matches!(self.requested_turns, ForkTurnSelection::None) && !empty {
            return Err(SessionError::Invalid(
                "fork `none` selection must resolve to an empty parent prefix".into(),
            ));
        }
        if self.resolved_terminal_seq == 0 && !empty {
            return Err(SessionError::Invalid(
                "fork effective turn count requires a terminal boundary".into(),
            ));
        }
        if self.resolved_after_seq >= self.resolved_terminal_seq && self.resolved_terminal_seq != 0
        {
            return Err(SessionError::Invalid(
                "fork inherited Fact interval must be nonempty and ordered".into(),
            ));
        }
        Ok(())
    }
}

/// Agent-tree membership in addition to immutable history provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkOrigin {
    /// Immutable history selected before the spawning Turn.
    pub history: HistoryOrigin,
    /// Agent-tree root shared by descendants.
    pub root_session_id: SessionId,
    /// Stable child path below the root.
    pub path: AgentPath,
    /// Sibling-unique model-facing task name.
    pub task_name: String,
}
impl ForkOrigin {
    /// Revalidates Agent membership and its history binding.
    pub fn validate(&self) -> Result<()> {
        self.history.validate()?;
        validate_identifier("subagent task name", &self.task_name)?;
        if self.path.depth() == 0 || !matches!(self.history.point, ForkPoint::BeforeTurn { .. }) {
            return Err(SessionError::Invalid(
                "Agent lineage requires a non-root path and a before-Turn history cut".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SessionOrigin {
    Agent { origin: ForkOrigin },
    Independent { history: HistoryOrigin },
}
