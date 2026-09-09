//! Product-owned non-executing storage for explicitly selected native addons.
mod filesystem;
mod source;
use filesystem::StoreDirectory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Maximum encoded source manifest bytes.
pub const MAXIMUM_NATIVE_ADDON_MANIFEST_BYTES: usize = 64 * 1024;
/// Maximum encoded atomic state-index bytes.
pub const MAXIMUM_NATIVE_ADDON_STATE_BYTES: usize = 1024 * 1024;
/// Maximum installed or enabled addon identities.
pub const MAXIMUM_NATIVE_ADDONS: usize = 128;
/// Maximum immutable source objects in a store.
pub const MAXIMUM_NATIVE_ADDON_OBJECTS: usize = 256;
/// Maximum aggregate immutable source-object bytes.
pub const MAXIMUM_NATIVE_ADDON_OBJECT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum Portable generation keys in one source manifest.
pub const MAXIMUM_NATIVE_ADDON_SERVICES: usize = 64;

/// Storage failure; no variant claims that a Runtime applied the desired selection.
#[derive(Debug, thiserror::Error)]
pub enum NativeAddonError {
    /// Invalid bounded source or durable metadata.
    #[error("invalid native addon input: {0}")]
    Invalid(&'static str),
    /// A configured storage bound was exceeded.
    #[error("native addon capacity exceeded: {0}")]
    Capacity(&'static str),
    /// A cooperating writer owns the store directory.
    #[error("native addon store is busy")]
    Busy,
    /// The public root no longer identifies the acquired directories.
    #[error("native addon store directory changed")]
    Conflict,
    /// The selected installed identity is absent.
    #[error("native addon is not installed")]
    Missing,
    /// An enabled addon must be disabled before uninstall.
    #[error("disable the native addon before uninstalling it")]
    Enabled,
    /// The installed target cannot run in this process.
    #[error("native addon target does not match this process")]
    UnsupportedTarget,
    /// An existing content-addressed object has different bytes.
    #[error("native addon object does not match its digest")]
    DigestMismatch,
    /// A filesystem operation failed.
    #[error("native addon filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
}
impl From<rustix::io::Errno> for NativeAddonError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}
type Result<T> = std::result::Result<T, NativeAddonError>;

/// Optional tighter object admission for one dedicated store directory.
#[derive(Clone, Copy, Debug)]
pub struct NativeAddonStoreLimits {
    /// Maximum immutable source objects, at most the product ceiling.
    pub maximum_objects: usize,
    /// Maximum aggregate immutable source bytes, at most the product ceiling.
    pub maximum_object_bytes: u64,
}
impl Default for NativeAddonStoreLimits {
    fn default() -> Self {
        Self {
            maximum_objects: MAXIMUM_NATIVE_ADDON_OBJECTS,
            maximum_object_bytes: MAXIMUM_NATIVE_ADDON_OBJECT_BYTES,
        }
    }
}

/// Validated installed bytes and their explicit Agent contribution description.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "source::RecordWire")]
pub struct NativeAddonRecord {
    id: String,
    plugin: String,
    target: String,
    artifact_sha256: String,
    portable_services: Vec<String>,
}
impl NativeAddonRecord {
    /// Exact product addon identity.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Expected native ABI plugin identity.
    pub fn plugin(&self) -> &str {
        &self.plugin
    }
    /// Exact OS/architecture target.
    pub fn target(&self) -> &str {
        &self.target
    }
    /// SHA-256 of the actually copied top-level artifact.
    pub fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }
    /// Explicit generation-private Portable keys.
    pub fn portable_services(&self) -> &[String] {
        &self.portable_services
    }
}

/// One bounded desired-state observation; it does not attest live activation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeAddonSnapshot {
    /// Monotonic source-state publication revision.
    pub revision: u64,
    /// Latest installed record for each identity.
    pub installed: Vec<NativeAddonRecord>,
    /// Separately selected exact records for future builds.
    pub enabled: Vec<NativeAddonRecord>,
}

/// One installation or selection publication, independent of Runtime convergence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeAddonReceipt {
    /// Source-state revision after the operation.
    pub revision: u64,
    /// Whether the index changed.
    pub changed: bool,
    /// Containing-directory sync after publication; absent for a no-op.
    pub directory_synced: Option<bool>,
    /// Record selected, installed or removed by the operation.
    pub record: Option<NativeAddonRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    format: u32,
    revision: u64,
    #[serde(deserialize_with = "source::records")]
    installed: BTreeMap<String, NativeAddonRecord>,
    #[serde(deserialize_with = "source::records")]
    enabled: BTreeMap<String, NativeAddonRecord>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            format: 1,
            revision: 0,
            installed: BTreeMap::new(),
            enabled: BTreeMap::new(),
        }
    }
}

/// A dedicated Unix store retaining directory authority across pathname changes.
#[derive(Debug)]
pub struct NativeAddonStore {
    directory: StoreDirectory,
    limits: NativeAddonStoreLimits,
}
impl NativeAddonStore {
    /// Opens or creates a private store using the default product bounds.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        Self::with_limits(path, NativeAddonStoreLimits::default())
    }
    /// Opens with tighter finite object count and byte bounds.
    pub fn with_limits(path: impl Into<PathBuf>, limits: NativeAddonStoreLimits) -> Result<Self> {
        if limits.maximum_objects == 0
            || limits.maximum_objects > MAXIMUM_NATIVE_ADDON_OBJECTS
            || limits.maximum_object_bytes == 0
            || limits.maximum_object_bytes > MAXIMUM_NATIVE_ADDON_OBJECT_BYTES
        {
            return Err(NativeAddonError::Invalid("store limits"));
        }
        Ok(Self {
            directory: StoreDirectory::open(path.into())?,
            limits,
        })
    }
    /// Reads existing source intent without creating or repairing any filesystem state.
    /// An absent root returns None; invalid or incomplete existing stores remain errors.
    pub fn read_snapshot(path: &Path) -> Result<Option<NativeAddonSnapshot>> {
        let Some(directory) = StoreDirectory::open_existing(path.to_owned())? else {
            return Ok(None);
        };
        Self {
            directory,
            limits: NativeAddonStoreLimits::default(),
        }
        .snapshot()
        .map(Some)
    }
    /// Returns validated source intent without loading artifacts or executing builds.
    pub fn snapshot(&self) -> Result<NativeAddonSnapshot> {
        let state = self.read_state()?;
        Ok(NativeAddonSnapshot {
            revision: state.revision,
            installed: state.installed.into_values().collect(),
            enabled: state.enabled.into_values().collect(),
        })
    }
    pub(crate) fn artifact_path(&self, record: &NativeAddonRecord) -> Result<PathBuf> {
        self.directory.object_path(record.artifact_sha256())
    }
    /// Installs exactly the bytes copied from an explicit source manifest.
    /// This never enables the record, runs its build command or loads native code.
    pub fn install(&self, manifest_path: &Path) -> Result<NativeAddonReceipt> {
        let (manifest, artifact) = source::read(manifest_path)?;
        let _lock = self.directory.lock()?;
        let before = self.read_state()?;
        before
            .revision
            .checked_add(1)
            .ok_or(NativeAddonError::Capacity("revision"))?;
        if !before.installed.contains_key(&manifest.id)
            && before.installed.len() >= MAXIMUM_NATIVE_ADDONS
        {
            return Err(NativeAddonError::Capacity("installed identities"));
        }
        let digest = self.directory.install_object(artifact, self.limits)?;
        let record = manifest.into_record(digest);
        let mut after = before.clone();
        after.installed.insert(record.id.clone(), record.clone());
        self.publish(&before, after, Some(record))
    }
    /// Selects the exact latest installed record for this process target.
    pub fn enable(&self, id: &str) -> Result<NativeAddonReceipt> {
        self.mutate(id, |state| {
            let record = state
                .installed
                .get(id)
                .ok_or(NativeAddonError::Missing)?
                .clone();
            if record.target != native_addon_target() {
                return Err(NativeAddonError::UnsupportedTarget);
            }
            state.enabled.insert(id.to_owned(), record.clone());
            Ok(Some(record))
        })
    }
    /// Removes future selection while existing Runtime pins remain independent.
    pub fn disable(&self, id: &str) -> Result<NativeAddonReceipt> {
        self.mutate(id, |state| Ok(state.enabled.remove(id)))
    }
    /// Removes a disabled installation record, retaining immutable source objects.
    pub fn uninstall(&self, id: &str) -> Result<NativeAddonReceipt> {
        self.mutate(id, |state| {
            if state.enabled.contains_key(id) {
                return Err(NativeAddonError::Enabled);
            }
            Ok(state.installed.remove(id))
        })
    }
    fn mutate(
        &self,
        id: &str,
        change: impl FnOnce(&mut State) -> Result<Option<NativeAddonRecord>>,
    ) -> Result<NativeAddonReceipt> {
        source::identifier(id, 64)?;
        let _lock = self.directory.lock()?;
        let before = self.read_state()?;
        let mut after = before.clone();
        let record = change(&mut after)?;
        self.publish(&before, after, record)
    }
    fn read_state(&self) -> Result<State> {
        self.directory.check()?;
        let state = match self.directory.read_index()? {
            Some(bytes) => serde_json::from_slice::<State>(&bytes)
                .map_err(|_| NativeAddonError::Invalid("store state"))?,
            None => State::default(),
        };
        if state.format != 1
            || state
                .enabled
                .keys()
                .any(|id| !state.installed.contains_key(id))
        {
            return Err(NativeAddonError::Invalid(
                "store state membership or format",
            ));
        }
        self.directory.check()?;
        Ok(state)
    }
    fn publish(
        &self,
        before: &State,
        mut after: State,
        record: Option<NativeAddonRecord>,
    ) -> Result<NativeAddonReceipt> {
        if before == &after {
            return Ok(NativeAddonReceipt {
                revision: before.revision,
                changed: false,
                directory_synced: None,
                record,
            });
        }
        after.revision = before
            .revision
            .checked_add(1)
            .ok_or(NativeAddonError::Capacity("revision"))?;
        let encoded = filesystem::encode_index(&after)?;
        self.directory.check()?;
        let synced = self.directory.publish_index(&encoded)?;
        Ok(NativeAddonReceipt {
            revision: after.revision,
            changed: true,
            directory_synced: Some(synced),
            record,
        })
    }
}
/// Returns the exact native target string used for enablement.
pub fn native_addon_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
