//! Bounded Agent-preset catalog, authoring, and Session composition fragment.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use rsi_agent_session_protocol::validate_identifier;
use rsi_host::{ProfileEntry, ProfileFragment};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use thiserror::Error;

mod authoring;
mod catalog;
#[cfg(unix)]
mod owned_root;
mod source;

fn clean_metadata_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty() && !value.chars().any(char::is_control)).then(|| value.to_owned())
    })
}

pub use catalog::{
    AgentPresetCatalog, AgentPresetCatalogConfig, AgentPresetDefaultStore, AgentPresetDocument,
    AgentPresetHealth, AgentPresetLaunchIdentity, AgentPresetLaunchRoot, AgentPresetRoot,
    AgentPresetRoster, AgentPresetRow, AgentPresetSource, AgentPresetTrust, COMPOSITION_FILE,
    MAX_COPY_BYTES, MAX_COPY_DEPTH, MAX_COPY_ENTRIES, MAX_METADATA_BYTES, MAX_ROOTS,
    MAX_ROSTER_ROWS, METADATA_FILE,
};
#[cfg(unix)]
pub use owned_root::{OwnedPresetRoot, open_existing_preset_root, open_or_create_preset_root};
pub use rsi_agent_session_protocol::AgentPresetId;
pub use source::{AgentPresetProfileCompiler, MAX_PROFILE_HEALTH_REASON_BYTES};

/// Linked factory key for the `SQLite` Agent Store.
pub const SQLITE_STORE_FACTORY: &str = "rsi.agent.store.sqlite";
/// Linked factory key for the durable Agent Kernel.
pub const KERNEL_FACTORY: &str = "rsi.agent.kernel";
/// Linked factory key for the sequential Agent executor.
pub const EXECUTOR_FACTORY: &str = "rsi.agent.executor";

/// Stable Session Agent Profile fragment identity.
pub const SESSION_FRAGMENT_ID: &str = "rsi.agent.session";
/// Stable Store instance identity within the Session fragment.
pub const SESSION_STORE_INSTANCE: &str = "rsi-agent-store";
/// Stable Kernel instance identity within the Session fragment.
pub const SESSION_KERNEL_INSTANCE: &str = "rsi-agent-kernel";
/// Stable executor instance identity within the Session fragment.
pub const SESSION_EXECUTOR_INSTANCE: &str = "rsi-agent-executor";

/// Configuration needed to freeze the standard Session Agent fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionAgentConfig {
    store_root: PathBuf,
    executor_id: String,
    maximum_active_turns: usize,
}

impl SessionAgentConfig {
    /// Creates a fragment configuration from explicit durable authority.
    ///
    /// # Errors
    ///
    /// Returns [`PresetError::StoreRootNotAbsolute`] unless `store_root` is an
    /// explicit absolute path.
    pub fn new(store_root: impl Into<PathBuf>) -> Result<Self> {
        let store_root = store_root.into();
        if !store_root.is_absolute() || store_root.as_os_str().is_empty() {
            return Err(PresetError::StoreRootNotAbsolute);
        }
        Ok(Self {
            store_root,
            executor_id: SESSION_EXECUTOR_INSTANCE.to_owned(),
            maximum_active_turns: 1,
        })
    }

    /// Replaces the exact executor registration identity.
    ///
    /// # Errors
    ///
    /// Returns [`PresetError::InvalidExecutorId`] when the identity is empty,
    /// longer than the Agent protocol bound, or outside its identifier
    /// alphabet.
    pub fn with_executor_id(mut self, executor_id: impl Into<String>) -> Result<Self> {
        let executor_id = executor_id.into();
        if validate_identifier("executor", &executor_id).is_err() {
            return Err(PresetError::InvalidExecutorId);
        }
        self.executor_id = executor_id;
        Ok(self)
    }

    /// Replaces the maximum number of turns active across distinct Sessions.
    ///
    /// The executor factory remains the sole owner of range validation.
    #[must_use]
    pub const fn with_maximum_active_turns(mut self, maximum: usize) -> Self {
        self.maximum_active_turns = maximum;
        self
    }

    /// Absolute Store root frozen into the fragment.
    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    /// Exact executor registration identity frozen into the fragment.
    pub fn executor_id(&self) -> &str {
        &self.executor_id
    }
}

/// Builds the immutable standard Session Agent fragment.
pub fn session_fragment(config: &SessionAgentConfig) -> ProfileFragment {
    ProfileFragment::new(
        SESSION_FRAGMENT_ID,
        [
            ProfileEntry::new(
                SESSION_STORE_INSTANCE,
                SQLITE_STORE_FACTORY,
                json!({ "root": config.store_root }),
            ),
            ProfileEntry::new(SESSION_KERNEL_INSTANCE, KERNEL_FACTORY, Value::Null),
            ProfileEntry::new(
                SESSION_EXECUTOR_INSTANCE,
                EXECUTOR_FACTORY,
                json!({
                    "executor_id": config.executor_id,
                    "maximum_active_turns": config.maximum_active_turns,
                }),
            ),
        ],
    )
}

/// Rejected preset input.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PresetError {
    /// A configured root was relative, duplicated, or not a real directory.
    #[error("invalid Agent preset root: {0}")]
    InvalidRoot(String),
    /// The configured root count exceeded the catalog bound.
    #[error("Agent preset roots exceed the maximum of {maximum}")]
    TooManyRoots {
        /// Exact accepted maximum.
        maximum: usize,
    },
    /// Discovery exceeded the bounded number of filesystem rows.
    #[error("Agent preset roster exceeds the maximum of {maximum} rows")]
    RosterCapacity {
        /// Exact accepted maximum.
        maximum: usize,
    },
    /// No configured root supplied the requested preset id.
    #[error("Agent preset {id:?} was not found (available: {available:?})")]
    PresetNotFound {
        /// Requested id.
        id: String,
        /// Currently discoverable ids.
        available: Vec<String>,
    },
    /// A discovered preset cannot supply a bounded composition document.
    #[error("Agent preset {id:?} is broken: {reason}")]
    BrokenPreset {
        /// Broken id.
        id: String,
        /// Safe catalog diagnostic.
        reason: String,
    },
    /// Authoring requires an explicit writable user root.
    #[error("Agent preset authoring requires an explicit user root")]
    NoUserRoot,
    /// An entry already occupies this preset id in the catalog or user root.
    #[error("Agent preset {id:?} already exists")]
    PresetExists {
        /// Occupied preset id.
        id: String,
    },
    /// Only the explicit user root grants delete authority.
    #[error("Agent preset {id:?} is read-only")]
    ReadOnlyPreset {
        /// Requested preset id.
        id: String,
    },
    /// The deployment base cannot be removed through a user override operation.
    #[error("Agent preset {id:?} is the deployment base default and cannot be deleted")]
    BaseDefaultPreset {
        /// Requested preset id.
        id: String,
    },
    /// Default mutation requires an injected persistence adapter.
    #[error("Agent preset default mutation requires a default-store adapter")]
    DefaultStoreUnavailable,
    /// Authoring encountered a symlink, special file, or otherwise unsafe row.
    #[error("unsafe Agent preset entry {}: {reason}", path.display())]
    UnsafeEntry {
        /// Exact local entry path.
        path: PathBuf,
        /// Safe rejection reason.
        reason: String,
    },
    /// One fixed authoring resource bound was exceeded.
    #[error("Agent preset copy exceeds the {resource} maximum of {maximum}")]
    CopyLimit {
        /// Stable bounded resource name.
        resource: &'static str,
        /// Exact accepted maximum.
        maximum: u64,
    },
    /// A filesystem operation failed.
    #[error("Agent preset {operation} failed for {}: {message}", path.display())]
    Io {
        /// Stable operation label.
        operation: &'static str,
        /// Exact host-local path.
        path: PathBuf,
        /// Underlying safe diagnostic.
        message: String,
    },
    /// The durable root was not explicit absolute authority.
    #[error("Agent Store root must be an absolute path")]
    StoreRootNotAbsolute,
    /// The executor identity was empty, oversized, or not an ASCII identifier.
    #[error("executor identity must satisfy the Agent identifier contract")]
    InvalidExecutorId,
}

/// Preset construction result.
pub type Result<T> = std::result::Result<T, PresetError>;
