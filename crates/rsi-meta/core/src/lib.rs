//! Embedded composition host.
//!
//! [`CompositionProject`] validates and locks offline candidates without opening
//! durable host state. [`CompositionHost`] is the online caller-facing interface;
//! graph mutation, durable operation handling, route publication, and generation
//! retirement stay behind it so callers cannot observe a partially committed graph.
//!
//! This is an experimental v0 API with no cross-release compatibility promise.

#![deny(unsafe_code)]

mod composition;
mod domain;
mod error;
mod frame;
mod host;
mod model;
mod persistence;
mod protocol;
mod recovery;
mod resolver;
mod runtime;
#[cfg(feature = "test-failpoints")]
mod test_failpoints;
mod workspace;

pub use domain::{
    ApplyRequest, ApplyResult, CompositionChangeSource, CompositionDigest, CompositionProject,
    CompositionWorkspace, EventPage, HostEvent, HostEventRecord, HostSnapshot, InstallRequest,
    InstallResult, LockResult, OperationId, PluginInspection, ShutdownReceipt, TokenRotation,
};
pub use error::{HostError, Result};
pub use host::{CompositionHost, EventStream, OpenOptions, ServiceOpenRequest, ServiceStream};
pub use model::{
    BindingSnapshot, CompositionLock, CompositionManifest, CompositionMetadata, CompositionMode,
    Diagnostic, DiagnosticSeverity, GraphRevision, GraphSnapshot, InactiveReason, InstanceId,
    InstanceSnapshot, InstanceSpec, InstanceStatus, LockedPackage, PackageId, PackageSource,
    RetirementPhase, RetiringInstanceSnapshot, ScopeId, ScopeSpec, ServiceKey, ServiceRequirement,
    ValidationReport,
};
pub use protocol::{STREAM_PROTOCOL, STREAM_VERSION, StreamEnvelope, StreamKind};
pub use rsi_meta_loader::ContentHash;

/// Link marker used only by explicit process-crash conformance builds.
#[cfg(feature = "test-failpoints")]
#[doc(hidden)]
pub const __TEST_CRASH_GATE_MARKER: &str = test_failpoints::CRASH_GATE_ENV;
