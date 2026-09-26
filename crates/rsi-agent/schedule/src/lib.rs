//! Bounded reminder intent and generation-owned scheduling interface.
#![deny(unsafe_code)]
#![warn(missing_docs)]
mod state;
pub use state::*;
/// Durable reminder domain identity.
pub const SCHEDULE_DOMAIN: &str = "rsi.schedule";
/// Internal atomic reservation command.
pub const SCHEDULE_RESERVE: &str = "rsi.schedule.reserve";
/// Internal terminal settlement command.
pub const SCHEDULE_SETTLE: &str = "rsi.schedule.settle";
mod service;
pub use service::*;
mod plugin;
pub use plugin::{
    ReserveSchedule, ScheduleFactory, SettleSchedule, initial_human_root, mutation_id,
};
