//! Isolated fixture supervision and build-process settlement.
#[cfg(unix)]
mod unix;
#[cfg(all(unix, test))]
pub(crate) use unix::kill_completion;
#[cfg(unix)]
pub(crate) use unix::{Child, Foreground, Signals, application_succeeded};
mod build;
pub(crate) use build::BuildChild;
