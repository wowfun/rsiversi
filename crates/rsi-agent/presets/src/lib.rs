//! Immutable Agent Profile fragments for explicit product composition.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use rsi_agent_session_protocol::validate_identifier;
use rsi_host::{ProfileEntry, ProfileFragment};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Linked factory key for the `SQLite` Agent Store.
pub const SQLITE_STORE_FACTORY: &str = "rsi.agent.store.sqlite";
/// Linked factory key for the durable Agent Kernel.
pub const KERNEL_FACTORY: &str = "rsi.agent.kernel";
/// Linked factory key for the sequential Agent executor.
pub const EXECUTOR_FACTORY: &str = "rsi.agent.executor";

/// Stable Headless Agent Profile fragment identity.
pub const HEADLESS_FRAGMENT_ID: &str = "rsi.agent.headless";
/// Stable Store instance identity within the Headless fragment.
pub const HEADLESS_STORE_INSTANCE: &str = "rsi-agent-store";
/// Stable Kernel instance identity within the Headless fragment.
pub const HEADLESS_KERNEL_INSTANCE: &str = "rsi-agent-kernel";
/// Stable executor instance identity within the Headless fragment.
pub const HEADLESS_EXECUTOR_INSTANCE: &str = "rsi-agent-executor";

/// Configuration needed to freeze the standard Headless Agent fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadlessAgentConfig {
    store_root: PathBuf,
    executor_id: String,
}

impl HeadlessAgentConfig {
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
            executor_id: HEADLESS_EXECUTOR_INSTANCE.to_owned(),
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

    /// Absolute Store root frozen into the fragment.
    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    /// Exact executor registration identity frozen into the fragment.
    pub fn executor_id(&self) -> &str {
        &self.executor_id
    }
}

/// Builds the immutable standard Headless Agent fragment.
pub fn headless_fragment(config: &HeadlessAgentConfig) -> ProfileFragment {
    ProfileFragment::new(
        HEADLESS_FRAGMENT_ID,
        [
            ProfileEntry::new(
                HEADLESS_STORE_INSTANCE,
                SQLITE_STORE_FACTORY,
                json!({ "root": config.store_root }),
            ),
            ProfileEntry::new(HEADLESS_KERNEL_INSTANCE, KERNEL_FACTORY, Value::Null),
            ProfileEntry::new(
                HEADLESS_EXECUTOR_INSTANCE,
                EXECUTOR_FACTORY,
                json!({ "executor_id": config.executor_id }),
            ),
        ],
    )
}

/// Rejected preset input.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PresetError {
    /// The durable root was not explicit absolute authority.
    #[error("Agent Store root must be an absolute path")]
    StoreRootNotAbsolute,
    /// The executor identity was empty, oversized, or not an ASCII identifier.
    #[error("executor identity must satisfy the Agent identifier contract")]
    InvalidExecutorId,
}

/// Preset construction result.
pub type Result<T> = std::result::Result<T, PresetError>;
