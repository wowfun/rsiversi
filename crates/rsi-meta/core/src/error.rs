use std::collections::BTreeMap;

use crate::model::{InstanceId, ServiceKey};

/// Errors at the embedded host interface.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {format} document {path}: {message}")]
    DocumentParse {
        path: std::path::PathBuf,
        format: &'static str,
        message: String,
    },
    #[error("composition manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("composition lock does not match the manifest: {0}")]
    LockMismatch(String),
    #[error("candidate lock path already exists: {path}")]
    LockAlreadyExists { path: std::path::PathBuf },
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "SQLite store schema version {found} is unsupported; this build requires version {supported}"
    )]
    UnsupportedStoreSchema { found: u32, supported: u32 },
    #[error("plugin package validation failed: {0}")]
    Loader(#[from] rsi_meta_loader::LoaderError),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("plugin frame error: {0}")]
    PluginFrame(#[from] rsi_meta_plugin::FrameError),
    #[error("registry actor is no longer available")]
    RegistryClosed,
    #[error("registry actor dropped a command response")]
    ResponseDropped,
    #[error("shutdown did not finish before its deadline")]
    ShutdownDeadline,
    #[error("unsupported protocol {protocol:?} version {version}")]
    UnsupportedProtocol { protocol: String, version: u32 },
    #[error("invalid command envelope: {0}")]
    InvalidEnvelope(String),
    #[error("command id {command_id:?} was already used for a different command")]
    CommandIdConflict { command_id: String },
    #[error("operation rejected with {code}: {message}")]
    OperationRejected {
        code: String,
        message: String,
        details: BTreeMap<String, serde_json::Value>,
    },
    #[error("instance {0} is not present in the routing snapshot")]
    UnknownInstance(InstanceId),
    #[error("instance {instance} is inactive")]
    InstanceInactive { instance: InstanceId },
    #[error("instance {consumer} did not declare required service {service}")]
    UndeclaredService {
        consumer: InstanceId,
        service: ServiceKey,
    },
    #[error("service {service} is unresolved for instance {consumer}")]
    UnresolvedService {
        consumer: InstanceId,
        service: ServiceKey,
    },
    #[error("operation is not implemented in platform v0: {0}")]
    Unsupported(&'static str),
    #[error("event subscriber lagged by {skipped} events; resubscribe from the last cursor")]
    SubscriberLagged { skipped: u64 },
    #[error(
        "event cursor {requested} expired; the minimum resumable cursor is {minimum_available}"
    )]
    EventCursorExpired {
        requested: u64,
        minimum_available: u64,
    },
    #[error("plugin state quota {quota} exceeded: requested {requested}, maximum {maximum}")]
    StateQuotaExceeded {
        quota: &'static str,
        requested: usize,
        maximum: usize,
    },
    #[error("native runtime for instance {instance} is closed")]
    PluginRuntimeClosed { instance: InstanceId },
    #[error("cannot start native runtime thread for instance {instance}: {source}")]
    PluginRuntimeStart {
        instance: InstanceId,
        #[source]
        source: std::io::Error,
    },
    #[error("native runtime for instance {instance} is not committed")]
    PluginRuntimeNotCommitted { instance: InstanceId },
    #[error("native plugin {instance} {lane} lane is at capacity")]
    PluginQueueFull {
        instance: InstanceId,
        lane: &'static str,
    },
    #[error("native plugin {instance} failed {operation}: {outcome}")]
    PluginCallFailed {
        instance: InstanceId,
        operation: String,
        outcome: String,
    },
    #[error("native plugin {instance} rejected prepare with {code}: {message}")]
    PluginPrepareFailed {
        instance: InstanceId,
        code: String,
        message: String,
    },
    #[error("native plugin {instance} did not acknowledge {phase} within the lifecycle deadline")]
    PluginLifecycleTimeout {
        instance: InstanceId,
        phase: &'static str,
    },
    #[error("host terminated after durable commit because runtime publication failed: {message}")]
    PostCommitLifecycleFailure { message: String },
    #[error("native plugin {instance} frame has {bytes} bytes, maximum is {maximum}")]
    PluginFrameTooLarge {
        instance: InstanceId,
        bytes: usize,
        maximum: usize,
    },
    #[error("service stream {stream_id} is closed")]
    StreamClosed { stream_id: String },
    #[error(
        "service stream {stream_id} byte budget exceeded: requested {requested}, available {available}"
    )]
    StreamByteBudgetExceeded {
        stream_id: String,
        requested: u64,
        available: u64,
    },
    #[error("failed to restore previous manifest/lock pair for command {command_id}: {message}")]
    PairRestoreFailed { command_id: String, message: String },
    #[error("plugin configuration preparation failed: {0}")]
    PluginConfig(#[from] rsi_meta_loader::ConfigPrepareError),
}

pub type Result<T, E = HostError> = std::result::Result<T, E>;
