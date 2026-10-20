//! Application-owned supervision, shared by ordinary development and fixtures.
#[cfg(unix)]
mod unix;
#[cfg(all(unix, test))]
pub(crate) use unix::kill_completion;
#[cfg(unix)]
pub(crate) use unix::{Child, Foreground, Signals, application_interrupted, application_succeeded};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{Child, Signals, application_interrupted, application_succeeded};
mod build;
pub(crate) use build::BuildChild;
