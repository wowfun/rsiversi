//! Same-user local control plane for the standard Session application.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod approval;
mod diagnostics;
mod owner;
#[cfg(unix)]
mod transport;

pub use approval::ApprovalBroker;
pub use diagnostics::{SessionHostDiagnostics, SessionHostDiagnosticsSnapshot};
pub use owner::{
    HostEpoch, HostOwnerLease, HostOwnerMetadata, HostOwnerMode, HostSignal,
    SESSION_HOST_PROTOCOL_EPOCH, SessionHostError, SessionHostPaths, owner_process_is_current,
    session_host_product_build, signal_owner,
};
#[cfg(unix)]
pub use transport::{SESSION_HOST_DRAIN_TIMEOUT, UdsSessionApplication, UdsSessionServer};
