//! Validating, content-addressed loader for trusted native `rsi-meta` plugins.
//!
//! Package manifests describe identity, contracts, and target-specific artifacts.
//! Expected hashes come from the durable lock file instead of being duplicated
//! in `plugin.toml`. A successful stage verifies both locked hashes before it
//! publishes an immutable cache entry. Loaded dynamic libraries deliberately
//! remain mapped until process exit.
//!
//! This workspace-private crate exposes an experimental v0 implementation
//! surface and makes no cross-release compatibility promise.

#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]
#![allow(unsafe_code)] // This crate is the audited native-loading boundary.
#![warn(missing_debug_implementations)]

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use libloading::Library;
use rsi_meta_plugin::{
    ABI_MAJOR, ABI_MINOR, CallOutcome, HostApi, INIT_OK, LANE_CONTROL, LANE_DATA, Lane,
    PLUGIN_ENTRY_SYMBOL, POST_FRAME_ACCEPTED, POST_FRAME_CLOSED, POST_FRAME_WOULD_BLOCK, PluginApi,
    PluginEntryFn,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(feature = "config")]
mod config;
mod manifest_validation;
mod mapping;
#[cfg(all(feature = "test-failpoints", unix))]
mod test_failpoints;

#[cfg(feature = "config")]
pub use config::{ConfigPrepareError, PreparedConfig, prepare_config, prepare_config_with_schema};
use manifest_validation::{validate_manifest_shape, validate_relative_path};

/// Target triple for which this loader was compiled.
pub const BUILD_TARGET: &str = env!("RSI_META_BUILD_TARGET");
/// Package manifest version accepted by this loader.
pub const MANIFEST_FORMAT_VERSION: u32 = 0;

const MAX_PLUGIN_MANIFEST_BYTES: usize = 1024 * 1024;
#[cfg(feature = "config")]
pub(crate) const MAX_CONFIG_SCHEMA_BYTES: usize = 4 * 1024 * 1024;
/// Maximum accepted size of one native plugin artifact.
///
/// This bounds both lock construction and content-addressed staging, including
/// a source file that grows while it is being copied.
#[doc(hidden)]
pub const MAX_PLUGIN_ARTIFACT_BYTES: usize = 256 * 1024 * 1024;

/// A SHA-256 content digest.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Hashes an in-memory byte sequence.
    pub fn digest(bytes: impl AsRef<[u8]>) -> Self {
        Self(Sha256::digest(bytes.as_ref()).into())
    }

    /// Returns the binary digest.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the lowercase hexadecimal representation used by lock files.
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ContentHash")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl FromStr for ContentHash {
    type Err = InvalidContentHash;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InvalidContentHash);
        }
        let mut digest = [0_u8; 32];
        hex::decode_to_slice(value, &mut digest).map_err(|_| InvalidContentHash)?;
        Ok(Self(digest))
    }
}

impl Serialize for ContentHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

/// A string was not exactly one 32-byte hexadecimal digest.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("expected exactly 64 lowercase hexadecimal SHA-256 characters")]
pub struct InvalidContentHash;

/// ABI version offered by a host or required by a package.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiVersion {
    pub major: u32,
    pub minor: u32,
}

impl ApiVersion {
    /// ABI version implemented by `rsi-meta-plugin` in this build.
    pub const CURRENT: Self = Self {
        major: ABI_MAJOR,
        minor: ABI_MINOR,
    };
}

/// Package identity from the manifest. It is not a mounted instance id.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub process_fixed: bool,
}

/// Minimum host ABI accepted by a package.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostApiRequirement {
    pub major: u32,
    pub minimum_minor: u32,
}

impl HostApiRequirement {
    fn is_satisfied_by(self, available: ApiVersion) -> bool {
        self.major == available.major && self.minimum_minor <= available.minor
    }
}

/// One native library artifact for a Rust target triple.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub target: String,
    pub path: PathBuf,
}

/// One service contract injected into a plugin.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InjectionManifest {
    pub contract: String,
    #[serde(default = "required_by_default")]
    pub required: bool,
}

const fn required_by_default() -> bool {
    true
}

/// Parsed `plugin.toml` package descriptor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub format_version: u32,
    pub package: PackageIdentity,
    pub host_api: HostApiRequirement,
    pub artifacts: Vec<ArtifactManifest>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub injects: Vec<InjectionManifest>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub config_schema: Option<PathBuf>,
}

impl PluginManifest {
    /// Parses a manifest without selecting a host artifact.
    ///
    /// # Errors
    ///
    /// Returns the TOML decoder error when the source is malformed.
    pub fn from_toml(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
    }
}

/// Manifest bytes and their parsed representation.
#[derive(Clone, Debug)]
pub struct PluginPackage {
    manifest_path: PathBuf,
    manifest_hash: ContentHash,
    manifest: PluginManifest,
}

impl PluginPackage {
    /// Reads and parses a package manifest. Locked hashes are checked by
    /// [`PluginLoader::stage`], not by this inspection operation.
    ///
    /// # Errors
    ///
    /// Returns an input, size, UTF-8, or manifest parsing error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LoaderError> {
        let path = path.as_ref();
        let bytes = read_file(path, "read plugin manifest", MAX_PLUGIN_MANIFEST_BYTES)?;
        let manifest_hash = ContentHash::digest(&bytes);
        Self::from_bytes(path, &bytes, manifest_hash)
    }

    fn open_locked(path: &Path, expected_hash: ContentHash) -> Result<Self, LoaderError> {
        let bytes = read_file(path, "read plugin manifest", MAX_PLUGIN_MANIFEST_BYTES)?;
        let manifest_hash = ContentHash::digest(&bytes);
        if manifest_hash != expected_hash {
            return Err(LoaderError::HashMismatch {
                subject: HashSubject::Manifest,
                path: path.to_path_buf(),
                expected: expected_hash,
                actual: manifest_hash,
            });
        }
        Self::from_bytes(path, &bytes, manifest_hash)
    }

    fn from_bytes(
        path: &Path,
        bytes: &[u8],
        manifest_hash: ContentHash,
    ) -> Result<Self, LoaderError> {
        let source = std::str::from_utf8(bytes).map_err(|source| LoaderError::ManifestUtf8 {
            path: path.to_path_buf(),
            source,
        })?;
        let manifest =
            PluginManifest::from_toml(source).map_err(|source| LoaderError::ManifestToml {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            manifest_path: path.to_path_buf(),
            manifest_hash,
            manifest,
        })
    }

    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    pub const fn manifest_hash(&self) -> ContentHash {
        self.manifest_hash
    }

    pub const fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
}

/// Digests pinned by one entry in `rsi-meta.lock`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpectedHashes {
    pub manifest: ContentHash,
    pub artifact: ContentHash,
}

impl ExpectedHashes {
    pub const fn new(manifest: ContentHash, artifact: ContentHash) -> Self {
        Self { manifest, artifact }
    }
}

/// Which locked object failed hash verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HashSubject {
    Manifest,
    Artifact,
    CachedArtifact,
}

impl fmt::Display for HashSubject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest => formatter.write_str("plugin manifest"),
            Self::Artifact => formatter.write_str("plugin artifact"),
            Self::CachedArtifact => formatter.write_str("cached plugin artifact"),
        }
    }
}

/// Loader failure with enough context to reject a package deterministically.
#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("cannot {operation} `{path}`")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("plugin manifest `{path}` is not UTF-8")]
    ManifestUtf8 {
        path: PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("cannot parse plugin manifest `{path}`")]
    ManifestToml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported plugin manifest version {found}; expected {expected}")]
    UnsupportedManifestVersion { found: u32, expected: u32 },
    #[error("package id is invalid or exceeds 255 UTF-8 bytes")]
    InvalidPackageId,
    #[error("package version must not be empty")]
    EmptyPackageVersion,
    #[error("manifest field `{field}` contains an empty value")]
    EmptyManifestValue { field: &'static str },
    #[error("manifest field `{field}` contains an invalid contract name")]
    InvalidContractName { field: &'static str },
    #[error("manifest field `{field}` contains duplicate `{value}`")]
    DuplicateManifestValue { field: &'static str, value: String },
    #[error(
        "manifest path `{path}` in field `{field}` must be a non-empty relative path without traversal"
    )]
    UnsafeManifestPath { field: &'static str, path: PathBuf },
    #[error(
        "plugin requires host ABI {required_major}.{required_minor}, but host provides {available_major}.{available_minor}"
    )]
    IncompatibleHostApi {
        required_major: u32,
        required_minor: u32,
        available_major: u32,
        available_minor: u32,
    },
    #[error("plugin has no artifact for host target `{target}`; available targets: {available:?}")]
    BadTarget {
        target: String,
        available: Vec<String>,
    },
    #[error("plugin has multiple artifacts for host target `{0}`")]
    DuplicateTarget(String),
    #[error("{subject} hash mismatch for `{path}`: expected {expected}, got {actual}")]
    HashMismatch {
        subject: HashSubject,
        path: PathBuf,
        expected: ContentHash,
        actual: ContentHash,
    },
    #[error("cache entry `{0}` is not a regular, non-symlink file")]
    InvalidCacheEntry(PathBuf),
    #[error("plugin cache root `{0}` must be a private directory owned by this user")]
    UnsafeCacheRoot(PathBuf),
    #[error("cannot {operation} `{path}` because it is not a regular, non-symlink file")]
    UnsafeInputFile {
        operation: &'static str,
        path: PathBuf,
    },
    #[error("cannot {operation} `{path}` because it exceeds {maximum_bytes} bytes")]
    InputTooLarge {
        operation: &'static str,
        path: PathBuf,
        maximum_bytes: usize,
    },
    #[error("host function table is incompatible with loader ABI {0:?}")]
    InvalidHostTable(ApiVersion),
    #[error("cannot map plugin library `{path}`")]
    DynamicLoad {
        path: PathBuf,
        #[source]
        source: libloading::Error,
    },
    #[error("resident plugin library `{path}` does not export `rsi_meta_plugin_entry_v0`")]
    MissingEntrySymbol {
        path: PathBuf,
        #[source]
        source: libloading::Error,
    },
    #[error("plugin entry point rejected initialization with status {0}")]
    PluginInit(u32),
    #[error("plugin returned an incompatible function table")]
    IncompatiblePluginTable,
    #[error("invalid plugin mailbox option: {0}")]
    InvalidMailboxOptions(&'static str),
}

/// Capacity and frame bound for the safe host callback adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginMailboxOptions {
    pub control_capacity: usize,
    pub data_capacity: usize,
    pub max_frame_bytes: usize,
}

impl Default for PluginMailboxOptions {
    fn default() -> Self {
        Self {
            control_capacity: 64,
            data_capacity: 256,
            max_frame_bytes: 1024 * 1024,
        }
    }
}

impl PluginMailboxOptions {
    fn validate(self) -> Result<Self, LoaderError> {
        if self.control_capacity == 0 {
            return Err(LoaderError::InvalidMailboxOptions(
                "control_capacity must be greater than zero",
            ));
        }
        if self.data_capacity == 0 {
            return Err(LoaderError::InvalidMailboxOptions(
                "data_capacity must be greater than zero",
            ));
        }
        if self.max_frame_bytes == 0 {
            return Err(LoaderError::InvalidMailboxOptions(
                "max_frame_bytes must be greater than zero",
            ));
        }
        Ok(self)
    }
}

/// One frame synchronously copied from an arbitrary plugin thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostedFrame {
    lane: Lane,
    payload: Vec<u8>,
}

impl PostedFrame {
    pub const fn lane(&self) -> Lane {
        self.lane
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }
}

/// Independent bounded receivers for lifecycle/control and service DATA.
#[derive(Debug)]
pub struct PluginMailbox {
    control: tokio::sync::mpsc::Receiver<PostedFrame>,
    data: tokio::sync::mpsc::Receiver<PostedFrame>,
}

impl PluginMailbox {
    pub async fn recv_control(&mut self) -> Option<PostedFrame> {
        self.control.recv().await
    }

    pub async fn recv_data(&mut self) -> Option<PostedFrame> {
        self.data.recv().await
    }

    /// Attempts to receive a control frame without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Empty` or `Disconnected` from the bounded lane.
    pub fn try_recv_control(
        &mut self,
    ) -> Result<PostedFrame, tokio::sync::mpsc::error::TryRecvError> {
        self.control.try_recv()
    }

    /// Attempts to receive a DATA frame without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Empty` or `Disconnected` from the bounded lane.
    pub fn try_recv_data(&mut self) -> Result<PostedFrame, tokio::sync::mpsc::error::TryRecvError> {
        self.data.try_recv()
    }

    /// Splits the mailbox into independently owned control and data receivers.
    ///
    /// The tuple order is `(control, data)`. Each receiver has a fixed lane,
    /// while every [`PostedFrame`] retains its lane for defensive routing and
    /// observability at the consumer boundary.
    pub fn into_lanes(self) -> (PluginLaneReceiver, PluginLaneReceiver) {
        (
            PluginLaneReceiver {
                lane: Lane::Control,
                receiver: self.control,
            },
            PluginLaneReceiver {
                lane: Lane::Data,
                receiver: self.data,
            },
        )
    }
}

/// Independently owned receiver for exactly one plugin output lane.
#[derive(Debug)]
pub struct PluginLaneReceiver {
    lane: Lane,
    receiver: tokio::sync::mpsc::Receiver<PostedFrame>,
}

impl PluginLaneReceiver {
    /// Lane permanently associated with this receiver.
    pub const fn lane(&self) -> Lane {
        self.lane
    }

    /// Waits for the next frame on this lane.
    pub async fn recv(&mut self) -> Option<PostedFrame> {
        self.receiver.recv().await
    }

    /// Attempts to receive a frame without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Empty` or `Disconnected` from this bounded lane.
    pub fn try_recv(&mut self) -> Result<PostedFrame, tokio::sync::mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

#[derive(Debug)]
struct QueueHostContext {
    control: tokio::sync::mpsc::Sender<PostedFrame>,
    data: tokio::sync::mpsc::Sender<PostedFrame>,
    max_frame_bytes: usize,
}

unsafe extern "C" fn queue_post_frame(
    host_handle: *mut core::ffi::c_void,
    lane: u32,
    data_ptr: *const u8,
    data_len: usize,
) -> u32 {
    if host_handle.is_null() || data_len > 0 && data_ptr.is_null() {
        return POST_FRAME_CLOSED;
    }
    // SAFETY: `load_queued` retains this context at a stable address until the
    // plugin destroy callback returns. Failed initialization leaks the context
    // because the loader cannot prove that plugin-created threads stopped.
    let context = unsafe { &*host_handle.cast::<QueueHostContext>() };
    if data_len > context.max_frame_bytes {
        return POST_FRAME_WOULD_BLOCK;
    }
    let payload = if data_len == 0 {
        Vec::new()
    } else {
        // SAFETY: The ABI requires an outgoing slice to be readable only for
        // this callback. `to_vec` performs the required synchronous copy.
        unsafe { std::slice::from_raw_parts(data_ptr, data_len) }.to_vec()
    };
    let sender = match lane {
        LANE_CONTROL => &context.control,
        LANE_DATA => &context.data,
        _ => return POST_FRAME_CLOSED,
    };
    match sender.try_send(PostedFrame {
        lane: Lane::from_raw(lane).expect("validated lane"),
        payload,
    }) {
        Ok(()) => POST_FRAME_ACCEPTED,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => POST_FRAME_WOULD_BLOCK,
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => POST_FRAME_CLOSED,
    }
}

/// A package artifact published in the immutable local cache.
#[derive(Clone, Debug)]
pub struct StagedPlugin {
    package: PluginPackage,
    artifact: ArtifactManifest,
    source_artifact_path: PathBuf,
    cached_artifact_path: PathBuf,
    artifact_hash: ContentHash,
}

impl StagedPlugin {
    pub const fn package(&self) -> &PluginPackage {
        &self.package
    }

    pub const fn manifest(&self) -> &PluginManifest {
        self.package.manifest()
    }

    pub const fn artifact(&self) -> &ArtifactManifest {
        &self.artifact
    }

    pub fn source_artifact_path(&self) -> &Path {
        &self.source_artifact_path
    }

    pub fn cached_artifact_path(&self) -> &Path {
        &self.cached_artifact_path
    }

    pub const fn artifact_hash(&self) -> ContentHash {
        self.artifact_hash
    }
}

/// Validates packages for one host ABI/target and owns its CAS location.
#[derive(Clone, Debug)]
pub struct PluginLoader {
    cache_root: PathBuf,
    host_target: String,
    host_api: ApiVersion,
}

impl PluginLoader {
    pub fn new(
        cache_root: impl Into<PathBuf>,
        host_target: impl Into<String>,
        host_api: ApiVersion,
    ) -> Self {
        Self {
            cache_root: cache_root.into(),
            host_target: host_target.into(),
            host_api,
        }
    }

    /// Constructs a loader for the current Cargo target and plugin ABI.
    pub fn for_current_process(cache_root: impl Into<PathBuf>) -> Self {
        Self::new(cache_root, BUILD_TARGET, ApiVersion::CURRENT)
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn host_target(&self) -> &str {
        &self.host_target
    }

    pub const fn host_api(&self) -> ApiVersion {
        self.host_api
    }

    /// Validates package-owned metadata and returns the artifact selected for
    /// this loader's exact target triple.
    ///
    /// # Errors
    ///
    /// Returns a manifest-shape, ABI, or target-selection error.
    pub fn validate_manifest<'manifest>(
        &self,
        manifest: &'manifest PluginManifest,
    ) -> Result<&'manifest ArtifactManifest, LoaderError> {
        validate_manifest_shape(manifest)?;
        if !manifest.host_api.is_satisfied_by(self.host_api) {
            return Err(LoaderError::IncompatibleHostApi {
                required_major: manifest.host_api.major,
                required_minor: manifest.host_api.minimum_minor,
                available_major: self.host_api.major,
                available_minor: self.host_api.minor,
            });
        }

        let mut matches = manifest
            .artifacts
            .iter()
            .filter(|artifact| artifact.target == self.host_target);
        let Some(artifact) = matches.next() else {
            return Err(LoaderError::BadTarget {
                target: self.host_target.clone(),
                available: manifest
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.target.clone())
                    .collect(),
            });
        };
        if matches.next().is_some() {
            return Err(LoaderError::DuplicateTarget(self.host_target.clone()));
        }
        Ok(artifact)
    }

    /// Verifies lock-file hashes, then atomically publishes the selected
    /// artifact into the content-addressed cache.
    ///
    /// # Errors
    ///
    /// Returns an input, integrity, cache-security, or cache-publication error.
    pub fn stage(
        &self,
        manifest_path: impl AsRef<Path>,
        expected: ExpectedHashes,
    ) -> Result<StagedPlugin, LoaderError> {
        let package = PluginPackage::open_locked(manifest_path.as_ref(), expected.manifest)?;

        let artifact = self.validate_manifest(&package.manifest)?.clone();
        let source_artifact_path = resolve_package_relative_file(
            &package.manifest_path,
            &artifact.path,
            "artifacts.path",
            "resolve plugin artifact",
        )?;
        let mut source_artifact = open_regular_file(&source_artifact_path, "read plugin artifact")?;
        let artifact_hash = hash_open_file(
            &mut source_artifact,
            &source_artifact_path,
            "read plugin artifact",
        )?;
        if artifact_hash != expected.artifact {
            return Err(LoaderError::HashMismatch {
                subject: HashSubject::Artifact,
                path: source_artifact_path.clone(),
                expected: expected.artifact,
                actual: artifact_hash,
            });
        }

        ensure_private_cache_root(&self.cache_root)?;
        let cached_artifact_path = self.cache_path(artifact_hash, &artifact.path);
        publish_cache_entry(
            &cached_artifact_path,
            &source_artifact_path,
            &mut source_artifact,
            artifact_hash,
        )?;

        Ok(StagedPlugin {
            package,
            artifact,
            source_artifact_path,
            cached_artifact_path,
            artifact_hash,
        })
    }

    /// Maps and initializes a staged trusted plugin.
    ///
    /// The mapped library is intentionally leaked as soon as `dlopen` succeeds,
    /// even when symbol lookup or initialization later fails. This prevents an
    /// unprovable `dlclose` while plugin-created threads or copied code pointers
    /// might still exist.
    ///
    /// Raw host tables cross an explicit unsafe boundary:
    ///
    /// ```compile_fail
    /// use rsi_meta_loader::{PluginLoader, StagedPlugin};
    /// use rsi_meta_plugin::HostApi;
    ///
    /// fn load_raw(loader: &PluginLoader, staged: &StagedPlugin, host: HostApi) {
    ///     let _ = loader.load(staged, host);
    /// }
    /// ```
    ///
    /// # Safety
    ///
    /// The host table's opaque context and callback must satisfy the lifetime,
    /// concurrency, and unwind requirements of [`HostApi::new`] through the
    /// loaded plugin's destroy callback. Prefer [`Self::load_queued`] when a raw
    /// host callback is unnecessary.
    ///
    /// # Errors
    ///
    /// Returns a mapping, ABI, symbol, or plugin-initialization error.
    pub unsafe fn load(
        &self,
        staged: &StagedPlugin,
        host_table: HostApi,
    ) -> Result<LoadedPlugin, LoaderError> {
        self.load_inner(staged, host_table, None)
    }

    /// Maps a plugin with a safe, bounded host callback adapter.
    ///
    /// Control and DATA frames have independent capacity. The callback copies
    /// accepted bytes before returning and is callable from arbitrary plugin
    /// threads. Core hosts should prefer this over constructing raw tables.
    ///
    /// # Errors
    ///
    /// Returns an option, mapping, ABI, symbol, or plugin-initialization error.
    pub fn load_queued(
        &self,
        staged: &StagedPlugin,
        options: PluginMailboxOptions,
    ) -> Result<(LoadedPlugin, PluginMailbox), LoaderError> {
        let options = options.validate()?;
        let (control, control_receiver) = tokio::sync::mpsc::channel(options.control_capacity);
        let (data, data_receiver) = tokio::sync::mpsc::channel(options.data_capacity);
        let mut context = Box::new(QueueHostContext {
            control,
            data,
            max_frame_bytes: options.max_frame_bytes,
        });
        let context_ptr = (&raw mut *context).cast::<core::ffi::c_void>();
        // SAFETY: `load_inner` retains the boxed context at a stable address
        // through plugin destruction. Its senders are thread-safe, copy every
        // accepted slice synchronously, and the callback never unwinds.
        let host_table = unsafe { HostApi::new(context_ptr, queue_post_frame) };
        let loaded = self.load_inner(staged, host_table, Some(context))?;
        Ok((
            loaded,
            PluginMailbox {
                control: control_receiver,
                data: data_receiver,
            },
        ))
    }

    fn load_inner(
        &self,
        staged: &StagedPlugin,
        host_table: HostApi,
        mut host_context: Option<Box<QueueHostContext>>,
    ) -> Result<LoadedPlugin, LoaderError> {
        ensure_private_cache_root(&self.cache_root)?;
        self.validate_manifest(staged.manifest())?;
        let verified_cache_entry =
            verify_cache_entry(&staged.cached_artifact_path, staged.artifact_hash)?;

        let table_version = ApiVersion {
            major: host_table.abi_major,
            minor: host_table.abi_minor,
        };
        if !host_table.is_compatible() || table_version != self.host_api {
            return Err(LoaderError::InvalidHostTable(table_version));
        }

        let host_table = Box::new(host_table);
        let library_path = staged.cached_artifact_path.clone();
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let mapping_path = {
            use std::os::fd::AsRawFd;

            PathBuf::from(format!(
                "/proc/self/fd/{}",
                verified_cache_entry.as_raw_fd()
            ))
        };
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let mapping_path = library_path.clone();
        // SAFETY: The package is trusted native code, its bytes and exact target
        // were checked before staging. On Linux/Android the loader maps through
        // the same still-open file description that was re-hashed above, closing
        // the path replacement window. Other supported platforms retain the
        // verified handle while mapping from the private cache path.
        let library = unsafe { mapping::open_now(&mapping_path, &library_path) }?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // The process-resident library may retain the `/proc/self/fd/N`
            // name in the dynamic loader. Keep N allocated for the same inode
            // so a later plugin can never reuse that pathname for other bytes.
            let _ = Box::leak(Box::new(verified_cache_entry));
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        drop(verified_cache_entry);

        // Intentionally no matching `Box::from_raw`: this process-resident leak
        // is the mechanism that guarantees the library is never actively closed.
        let library: &'static Library = Box::leak(Box::new(library));
        // SAFETY: The fixed symbol name is NUL-terminated and the ABI crate owns
        // its exact function signature. The library is process-resident, so the
        // copied function pointer cannot outlive its mapping.
        let entry: PluginEntryFn = unsafe {
            *library
                .get::<PluginEntryFn>(PLUGIN_ENTRY_SYMBOL)
                .map_err(|source| LoaderError::MissingEntrySymbol {
                    path: library_path.clone(),
                    source,
                })?
        };

        let mut plugin_table = PluginApi::EMPTY;
        // SAFETY: `host_table` has stable boxed storage retained by LoadedPlugin;
        // `plugin_table` is writable for the full fixed table. The trusted entry
        // point must obey the C ABI and may not retain the output pointer.
        let status = unsafe {
            entry(
                &raw const *host_table,
                &raw mut plugin_table,
                core::mem::size_of::<PluginApi>(),
            )
        };
        if status != INIT_OK {
            // A rejected plugin may retain both raw input pointers and use them
            // from background cleanup. Without a validated destroy callback,
            // retaining both allocations is the only safe raw-ABI lifetime.
            let _ = Box::leak(host_table);
            if let Some(context) = host_context.take() {
                // The rejected plugin observed the callback pointer. Without a
                // validated destroy callback, retaining this small allocation
                // is the only way to rule out callback-context UAF.
                let _ = Box::leak(context);
            }
            return Err(LoaderError::PluginInit(status));
        }
        if !plugin_table.is_compatible() {
            // Nothing in an incompatible table is callable, including its
            // apparent `destroy` pointer. The trusted plugin may have leaked
            // an initialization allocation, but invoking an unvalidated ABI
            // would be memory-unsafe. Process exit reclaims both allocation
            // and the deliberately resident library mapping.
            let _ = Box::leak(host_table);
            if let Some(context) = host_context.take() {
                let _ = Box::leak(context);
            }
            return Err(LoaderError::IncompatiblePluginTable);
        }

        Ok(LoadedPlugin {
            staged: staged.clone(),
            host_table,
            _host_context: host_context,
            plugin_table,
            shutdown: false,
            destroyed: false,
        })
    }

    fn cache_path(&self, hash: ContentHash, artifact_path: &Path) -> PathBuf {
        let mut file_name = OsString::from("artifact");
        if let Some(extension) = artifact_path.extension() {
            file_name.push(".");
            file_name.push(extension);
        }
        self.cache_root
            .join("sha256")
            .join(hash.to_hex())
            .join(file_name)
    }
}

/// Live plugin instance. Safe methods serialize calls through `&mut self`.
pub struct LoadedPlugin {
    staged: StagedPlugin,
    // The plugin may retain the pointer supplied to its entry point.
    host_table: Box<HostApi>,
    // Present for the safe queued adapter and retained through destroy.
    _host_context: Option<Box<QueueHostContext>>,
    plugin_table: PluginApi,
    shutdown: bool,
    destroyed: bool,
}

// SAFETY: The ABI requires a plugin handle to support serialized callbacks
// after being moved between host threads. Safe methods require `&mut self`, so
// callback/lifecycle invocation cannot be concurrent through this wrapper.
// Host callbacks independently require a thread-safe context at construction.
unsafe impl Send for LoadedPlugin {}

impl fmt::Debug for LoadedPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedPlugin")
            .field("package", &self.staged.manifest().package)
            .field("cached_artifact_path", &self.staged.cached_artifact_path)
            .field(
                "host_abi",
                &(self.host_table.abi_major, self.host_table.abi_minor),
            )
            .field("shutdown", &self.shutdown)
            .finish_non_exhaustive()
    }
}

impl LoadedPlugin {
    pub const fn staged(&self) -> &StagedPlugin {
        &self.staged
    }

    /// Delivers one borrowed control- or data-lane frame.
    pub fn dispatch(&mut self, lane: Lane, payload: &[u8]) -> CallOutcome {
        if self.shutdown || self.destroyed {
            return CallOutcome::Closed;
        }
        let Some(dispatch) = self.plugin_table.on_frame else {
            return CallOutcome::Failed;
        };
        // SAFETY: Compatibility validation established a live handle and
        // callback. The borrow remains readable for this call and cannot be
        // retained by an ABI-conforming plugin.
        let status = unsafe {
            dispatch(
                self.plugin_table.plugin_handle,
                lane.as_raw(),
                payload.as_ptr(),
                payload.len(),
            )
        };
        CallOutcome::from_raw(status)
    }

    /// Requests idempotent graceful shutdown. Destruction remains owned by Drop.
    pub fn shutdown(&mut self) -> CallOutcome {
        if self.destroyed || self.shutdown {
            return CallOutcome::Ok;
        }
        let outcome = if let Some(shutdown) = self.plugin_table.shutdown {
            // SAFETY: This is a live validated handle; `&mut self` prevents safe
            // concurrent lifecycle calls through this wrapper.
            CallOutcome::from_raw(unsafe { shutdown(self.plugin_table.plugin_handle) })
        } else {
            CallOutcome::Ok
        };
        if matches!(outcome, CallOutcome::Ok | CallOutcome::Closed) {
            self.shutdown = true;
        }
        outcome
    }

    fn destroy(&mut self) {
        if self.destroyed {
            return;
        }
        if !self.shutdown {
            let outcome = self.shutdown();
            if !matches!(outcome, CallOutcome::Ok | CallOutcome::Closed) {
                tracing::error!(?outcome, "plugin shutdown failed during destruction");
            }
        }
        if let Some(destroy) = self.plugin_table.destroy {
            // SAFETY: Compatibility validation established this callback and
            // `destroyed` ensures it is invoked at most once for the handle.
            let outcome =
                CallOutcome::from_raw(unsafe { destroy(self.plugin_table.plugin_handle) });
            if !matches!(outcome, CallOutcome::Ok | CallOutcome::Closed) {
                tracing::error!(?outcome, "plugin destroy callback failed");
            }
        }
        self.plugin_table.plugin_handle = std::ptr::null_mut();
        self.destroyed = true;
    }
}

impl Drop for LoadedPlugin {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Resolves a manifest-declared file through a physical parent path confined
/// to the package directory. The final component remains subject to the
/// caller's no-follow open.
#[doc(hidden)]
pub fn resolve_package_relative_file(
    manifest_path: &Path,
    relative: &Path,
    field: &'static str,
    operation: &'static str,
) -> Result<PathBuf, LoaderError> {
    validate_relative_path(field, relative)?;
    let package_root = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    storage::resolve_confined_file(package_root, relative, field, operation)
}

mod storage;

pub(crate) use storage::read_file;
use storage::{
    ensure_private_cache_root, hash_open_file, open_regular_file, publish_cache_entry,
    verify_cache_entry,
};
pub use storage::{hash_regular_file, read_bounded_file, read_bounded_file_following_symlinks};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_frame_is_a_rejected_attempt_not_a_closed_lane() {
        let (control, mut control_receiver) = tokio::sync::mpsc::channel(1);
        let (data, _data_receiver) = tokio::sync::mpsc::channel(1);
        let mut context = QueueHostContext {
            control,
            data,
            max_frame_bytes: 4,
        };
        let oversized = [0_u8; 5];
        let small = [0_u8; 4];

        assert_eq!(
            // SAFETY: `context` and both input slices remain alive for each
            // synchronous callback and the receiver keeps the lane open.
            unsafe {
                queue_post_frame(
                    (&raw mut context).cast(),
                    LANE_CONTROL,
                    oversized.as_ptr(),
                    oversized.len(),
                )
            },
            POST_FRAME_WOULD_BLOCK
        );
        assert_eq!(
            // SAFETY: Same stable callback context and borrowed-slice contract.
            unsafe {
                queue_post_frame(
                    (&raw mut context).cast(),
                    LANE_CONTROL,
                    small.as_ptr(),
                    small.len(),
                )
            },
            POST_FRAME_ACCEPTED
        );
        assert_eq!(control_receiver.try_recv().unwrap().payload(), small);
    }
}
