//! Native service ownership and local transport for the standard RSI product.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod approval;
mod changes;
mod diagnostics;
mod identity;
#[cfg(unix)]
mod local_api;
mod owner;
mod owner_plugin;
mod questions;
#[cfg(unix)]
mod socket;

pub use approval::{ApprovalBroker, ApprovalBrokerContract, ApprovalBrokerFactory};
pub use diagnostics::{ServiceHostDiagnostics, ServiceHostDiagnosticsSnapshot};
pub use identity::ServiceIdentityFactory;
#[cfg(unix)]
pub use local_api::{
    LocalApiFactory, LocalApiListener, LocalApiListenerContract, LocalApiServer,
    SERVICE_HOST_DRAIN_TIMEOUT, local_compatibility_key,
};
pub use owner::{
    HostOwnerLease, HostOwnerMetadata, HostOwnerMode, HostSignal, SERVICE_HOST_PROTOCOL_EPOCH,
    ServiceHostError, ServiceHostPaths, owner_process_is_current, service_host_product_build,
    signal_owner,
};
pub use owner_plugin::{ServiceOwnerContract, ServiceOwnerFactory};
pub use questions::{QuestionBroker, QuestionBrokerFactory};
