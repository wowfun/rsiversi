use rsi_meta::{LocalContractKey, LocalEventKey, MetaError, PluginId};
use std::path::PathBuf;

/// Failure at the Host catalog, Profile bootstrap, or shutdown boundary.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// One required path was not absolute.
    #[error("{kind} path `{path}` is not absolute")]
    PathNotAbsolute {
        /// Logical path role.
        kind: &'static str,
        /// Rejected path.
        path: PathBuf,
    },
    /// One linked implementation key was already registered.
    #[error("plugin `{plugin}` is already registered")]
    DuplicatePlugin {
        /// Duplicate catalog key.
        plugin: PluginId,
    },
    /// One Local contract key was already bound to a Rust type.
    #[error("Local contract key `{key}` is already registered")]
    DuplicateLocalContractKey {
        /// Duplicate stable key.
        key: LocalContractKey,
    },
    /// One Local contract Rust type was registered more than once.
    #[error("Local contract type `{type_name}` is already registered")]
    DuplicateLocalContractType {
        /// Duplicate Rust type name.
        type_name: &'static str,
    },
    /// One Local event key was already bound to a Rust type.
    #[error("Local event key `{key}` is already registered")]
    DuplicateLocalEventKey {
        /// Duplicate stable key.
        key: LocalEventKey,
    },
    /// One Local event Rust type was registered more than once.
    #[error("Local event type `{type_name}` is already registered")]
    DuplicateLocalEventType {
        /// Duplicate Rust type name.
        type_name: &'static str,
    },
    /// One immutable fragment ID was already registered.
    #[error("Profile fragment `{fragment}` is already registered")]
    DuplicateFragment {
        /// Duplicate fragment ID.
        fragment: String,
    },
    /// One immutable Rhai define key was already registered.
    #[error("Profile define `{key}` is already registered")]
    DuplicateDefine {
        /// Duplicate define key.
        key: String,
    },
    /// An identifier was empty or exceeded the configured byte bound.
    #[error("{kind} identifier must contain 1..={maximum} bytes")]
    InvalidIdentifier {
        /// Identifier role.
        kind: &'static str,
        /// Configured maximum byte length.
        maximum: usize,
    },
    /// A configured Host collection exceeded its explicit bound.
    #[error("{resource} exceeds the configured maximum of {maximum}")]
    CapacityExceeded {
        /// Bounded collection.
        resource: &'static str,
        /// Configured maximum.
        maximum: usize,
    },
    /// Profile compilation, resolution, preparation, or control failed.
    #[error(transparent)]
    Profile(#[from] rsi_meta_profile::ProfileError),
    /// Meta rejected construction or one lifecycle operation.
    #[error(transparent)]
    Meta(#[from] MetaError),
    /// The top-level Profile Fiber did not publish a usable generation.
    #[error("Profile bootstrap failed: {0}")]
    Bootstrap(String),
}

/// Host operation result.
pub type Result<T> = std::result::Result<T, HostError>;
