//! Pure Goal state and Agent composition contributions, without scheduling authority.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod state;
pub use state::*;
mod plugin;
mod report;
pub use plugin::{GoalFactory, ReserveGoal, SettleGoal};

/// Exact durable Goal domain name.
pub const GOAL_DOMAIN: &str = "rsi.goal";
/// Current durable Goal codec version, including continuation authority identity.
pub const GOAL_CODEC_VERSION: u32 = 2;
/// Application state command; only the Host controller may arm execution afterwards.
pub const GOAL_COMMAND: &str = "rsi.goal.command";
/// Internal round allocation command.
pub const GOAL_RESERVE: &str = "rsi.goal.reserve";
/// Internal canonical outcome reconciliation command.
pub const GOAL_SETTLE: &str = "rsi.goal.settle";
/// Pure durable Goal projection.
pub const GOAL_PROJECTION: &str = "rsi.goal.view";
