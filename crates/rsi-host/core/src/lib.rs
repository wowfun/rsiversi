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
pub use host::{Host, HostProfilePreview, HostProfilePreviewLeaf, RunningHost};
pub use paths::HostPaths;
pub use rsi_meta_profile::{
    Profile, ProfileControl, ProfileControlContract, ProfileEntry, ProfileFragment, ProfileGroup,
    ProfileHealth, ProfileInstanceState, ProfileInstanceStatus, ProfileLimits, ProfileNode,
    ProfilePatch, ProfileProgram, ProfileSnapshot, ProfileStatus, ProfileStep, ProfileTargetStatus,
    ReloadOutcome, SnapshotNode, WatcherHealth,
};
