//! Generic static composition host above `rsi-meta`.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod builder;
mod error;
mod host;
mod paths;

pub use builder::{HostBuilder, HostLimits};
pub use error::{HostError, Result};
pub use host::{
    Host, HostProfileEditPreview, HostProfilePreview, HostProfilePreviewLeaf,
    HostProfileSourceFingerprint, RunningHost,
};
pub use paths::HostPaths;
pub use rsi_meta_profile::{
    Profile, ProfileBootstrap, ProfileBundle, ProfileControl, ProfileControlContract, ProfileEntry,
    ProfileError, ProfileFragment, ProfileGroup, ProfileHealth, ProfileInput, ProfileInstanceState,
    ProfileInstanceStatus, ProfileLimits, ProfileNode, ProfilePatch, ProfileProgram,
    ProfileSnapshot, ProfileStatus, ProfileStep, ProfileTargetStatus, ProfileUpdateHandle,
    ProfileUpdateTicket, ReloadOutcome, SnapshotNode, WatcherHealth,
};
