//! Authoritative task lists with atomic Tool result settlement.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod plugin;
mod state;
pub use plugin::TodoFactory;
pub use state::{TodoItem, TodoList, TodoStatus};

/// Exact typed domain identity.
pub const DOMAIN: &str = "rsi.todo";
/// Read-only projection identity.
pub const VIEW: &str = "rsi.todo.view";
/// Whole-list replacement Tool name.
pub const TOOL: &str = "todo_write";
