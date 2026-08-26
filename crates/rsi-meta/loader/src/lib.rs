//! Native ABI v2 execution adapter and the ordinary Loader plugin.
//!
//! Core owns composition. This crate owns verified artifact staging, native
//! table admission, capability translation, callback lifetime, and teardown.

#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]
#![allow(unsafe_code)] // Audited dynamic-library and C-ABI adapter.
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

use rsi_meta::ContractVersion;
use std::path::PathBuf;
use thiserror::Error;

mod catalog;
mod catalog_io;
mod catalog_resources;
mod loader_plugin;
mod native;
mod panic_containment;
mod worker;

pub use catalog::{CatalogOptions, NativeCatalog};
pub use catalog_resources::{NativeCatalogLimits, NativeCatalogSnapshot};
pub use loader_plugin::{LoaderConfig, LoaderEntry, LoaderFactory};
pub use native::NativeFactory;
use native::NativeModule;

pub const LOADER_SERVICE_KEY: &str = "rsi.meta.loader";
pub const LOADER_CONTRACT_ID: &str = "rsi.meta.loader.v1";
pub const LOADER_CONTRACT_VERSION: ContractVersion = ContractVersion(1);
pub const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) use rsi_meta_plugin::MAX_DIAGNOSTIC_BYTES as MAX_NATIVE_DIAGNOSTIC_BYTES;
pub(crate) const MAX_NATIVE_IDENTITY_BYTES: usize = 256;
pub(crate) const MAX_NATIVE_CONFIG_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_NATIVE_REQUIREMENTS: usize = 256;
pub(crate) const MAX_NATIVE_MESSAGE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_NATIVE_MESSAGE_CAPABILITIES: usize = 256;

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("native operation timed out: {0}")]
    Timeout(&'static str),
    #[error("native operation is busy: {operation}")]
    Busy { operation: &'static str },
    #[error("native operation is reentrant in the same lineage: {operation}")]
    Reentrant { operation: &'static str },
    #[error("invalid loader input: {0}")]
    InvalidInput(String),
    #[error("native artifact exceeds the {MAX_ARTIFACT_BYTES}-byte limit")]
    ArtifactTooLarge,
    #[error("private staged artifact changed after its digest was computed")]
    StagedArtifactChanged,
    #[error("content-addressed cache collision at {0}")]
    CacheCollision(PathBuf),
    #[error("native cache directory is already owned by another catalog: {0}")]
    CacheLocked(PathBuf),
    #[error("native cache durability is poisoned after an unprovable rollback")]
    CachePoisoned,
    #[error("native catalog load admission is closed after a retained finalization failure")]
    FinalizationPoisoned,
    #[error("native {resource} capacity is exhausted at limit {limit}")]
    CapacityExhausted { resource: &'static str, limit: u64 },
    #[error("native plugin entry failed with status {status}")]
    PluginEntry { status: u32 },
    #[error(
        "native plugin ABI is incompatible (host {host_major}.{host_minor}, plugin {plugin_major}.{plugin_minor})"
    )]
    IncompatibleAbi {
        host_major: u32,
        host_minor: u32,
        plugin_major: u32,
        plugin_minor: u32,
    },
    #[error("native {operation} callback failed: {message}")]
    Callback {
        operation: &'static str,
        message: String,
    },
    #[error("native wire protocol error during {operation}: {message}")]
    Protocol {
        operation: &'static str,
        message: String,
    },
    #[error("native library error: {0}")]
    Library(#[from] libloading::Error),
    #[error("native artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("native JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}
