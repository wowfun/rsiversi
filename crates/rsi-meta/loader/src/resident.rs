use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use libloading::Library;
use rsi_meta_plugin::{PLUGIN_ENTRY_SYMBOL, PluginEntryFn};

use super::{
    ApiVersion, ContentHash, LoaderError, MAX_RESIDENT_ARTIFACTS, MAX_RESIDENT_MAPPED_BYTES,
    PluginLoader, StagedPlugin, mapping,
};
use crate::storage::{CachePin, verify_cache_entry};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ResidentArtifactKey {
    pub(super) target: String,
    pub(super) host_api: ApiVersion,
    pub(super) hash: ContentHash,
}

pub(super) struct ResidentMapping {
    library: Library,
    _cache_pin: Arc<CachePin>,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    _verified_cache_entry: std::fs::File,
}

impl fmt::Debug for ResidentMapping {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentMapping")
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(super) struct ResidentArtifact {
    _mapping: Arc<ResidentMapping>,
    pub(super) entry: PluginEntryFn,
}

#[derive(Clone, Debug)]
pub(super) enum ResidentFailure {
    Preparation {
        path: PathBuf,
        message: String,
    },
    DynamicLoad {
        path: PathBuf,
        source: Arc<libloading::Error>,
    },
    MissingEntrySymbol {
        path: PathBuf,
        source: Arc<libloading::Error>,
        _mapping: Arc<ResidentMapping>,
    },
}

impl ResidentFailure {
    const fn cacheable(&self) -> bool {
        matches!(self, Self::MissingEntrySymbol { .. })
    }

    fn to_loader_error(&self) -> LoaderError {
        match self {
            Self::Preparation { path, message } => LoaderError::ResidentArtifactPreparation {
                path: path.clone(),
                message: message.clone(),
            },
            Self::DynamicLoad { path, source } => LoaderError::DynamicLoad {
                path: path.clone(),
                source: Arc::clone(source),
            },
            Self::MissingEntrySymbol { path, source, .. } => LoaderError::MissingEntrySymbol {
                path: path.clone(),
                source: Arc::clone(source),
            },
        }
    }
}

type ResidentResult = Result<Arc<ResidentArtifact>, ResidentFailure>;
type ResidentCell = OnceLock<ResidentResult>;

#[derive(Debug)]
pub(super) struct ResidentArtifactRegistry {
    maximum: usize,
    maximum_bytes: usize,
    pub(super) state: Mutex<ResidentRegistryState>,
}

#[derive(Debug, Default)]
pub(super) struct ResidentRegistryState {
    pub(super) entries: HashMap<ResidentArtifactKey, ResidentRegistryEntry>,
    pub(super) mapped_bytes: usize,
}

#[derive(Debug)]
pub(super) struct ResidentRegistryEntry {
    cell: Arc<ResidentCell>,
    mapped_bytes: usize,
}

impl ResidentArtifactRegistry {
    pub(super) fn new(maximum: usize, maximum_bytes: usize) -> Self {
        Self {
            maximum,
            maximum_bytes,
            state: Mutex::new(ResidentRegistryState::default()),
        }
    }

    pub(super) fn cell(
        &self,
        key: ResidentArtifactKey,
        mapped_bytes: usize,
    ) -> Result<Arc<ResidentCell>, LoaderError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = state.entries.get(&key) {
            return Ok(Arc::clone(&entry.cell));
        }
        if state.entries.len() >= self.maximum {
            return Err(LoaderError::ResidentArtifactLimit {
                maximum: self.maximum,
            });
        }
        let requested_bytes = state.mapped_bytes.checked_add(mapped_bytes).ok_or(
            LoaderError::ResidentMappedBytesLimit {
                maximum_bytes: self.maximum_bytes,
                requested_bytes: usize::MAX,
            },
        )?;
        if requested_bytes > self.maximum_bytes {
            return Err(LoaderError::ResidentMappedBytesLimit {
                maximum_bytes: self.maximum_bytes,
                requested_bytes,
            });
        }
        let cell = Arc::new(ResidentCell::new());
        state.mapped_bytes = requested_bytes;
        state.entries.insert(
            key,
            ResidentRegistryEntry {
                cell: Arc::clone(&cell),
                mapped_bytes,
            },
        );
        Ok(cell)
    }

    fn remove_retryable_failure(&self, key: &ResidentArtifactKey, cell: &Arc<ResidentCell>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .entries
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(&current.cell, cell))
            && let Some(removed) = state.entries.remove(key)
        {
            state.mapped_bytes = state.mapped_bytes.saturating_sub(removed.mapped_bytes);
        }
    }
}

fn resident_artifacts() -> &'static ResidentArtifactRegistry {
    static REGISTRY: OnceLock<ResidentArtifactRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        ResidentArtifactRegistry::new(MAX_RESIDENT_ARTIFACTS, MAX_RESIDENT_MAPPED_BYTES)
    })
}

impl PluginLoader {
    pub(super) fn resident_artifact(
        &self,
        staged: &StagedPlugin,
    ) -> Result<Arc<ResidentArtifact>, LoaderError> {
        let key = ResidentArtifactKey {
            target: self.host_target.clone(),
            host_api: self.host_api,
            hash: staged.artifact_hash,
        };
        let registry = resident_artifacts();
        let mapped_bytes = usize::try_from(
            std::fs::metadata(&staged.cached_artifact_path)
                .map_err(|source| LoaderError::Io {
                    operation: "inspect cached plugin artifact",
                    path: staged.cached_artifact_path.clone(),
                    source,
                })?
                .len(),
        )
        .map_err(|_| LoaderError::ResidentMappedBytesLimit {
            maximum_bytes: MAX_RESIDENT_MAPPED_BYTES,
            requested_bytes: usize::MAX,
        })?;
        let cell = registry.cell(key.clone(), mapped_bytes)?;
        let resident = cell.get_or_init(|| Self::map_resident_artifact(staged));
        match resident {
            Ok(resident) => Ok(Arc::clone(resident)),
            Err(failure) => {
                if !failure.cacheable() {
                    registry.remove_retryable_failure(&key, &cell);
                }
                Err(failure.to_loader_error())
            }
        }
    }

    fn map_resident_artifact(staged: &StagedPlugin) -> ResidentResult {
        let verified_cache_entry =
            verify_cache_entry(&staged.cached_artifact_path, staged.artifact_hash).map_err(
                |error| ResidentFailure::Preparation {
                    path: staged.cached_artifact_path.clone(),
                    message: error.to_string(),
                },
            )?;
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
        // the path replacement window. Other supported platforms map the
        // immutable private cache path while the staged cross-process pin
        // prevents supported cache maintenance from replacing it; same-UID
        // out-of-band mutation remains inside the trusted deployment boundary.
        let library = match unsafe { mapping::open_now(&mapping_path, &library_path) } {
            Ok(library) => library,
            Err(LoaderError::DynamicLoad { path, source }) => {
                return Err(ResidentFailure::DynamicLoad { path, source });
            }
            Err(_) => unreachable!("mapping returns only dynamic-load failures"),
        };
        let mapping = Arc::new(ResidentMapping {
            library,
            _cache_pin: Arc::clone(&staged.cache_pin),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            _verified_cache_entry: verified_cache_entry,
        });
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        drop(verified_cache_entry);

        // SAFETY: The fixed symbol name is NUL-terminated and the ABI crate owns
        // its exact function signature. The process registry retains `mapping`,
        // so the copied function pointer cannot outlive its library.
        let entry: PluginEntryFn =
            match unsafe { mapping.library.get::<PluginEntryFn>(PLUGIN_ENTRY_SYMBOL) } {
                Ok(entry) => *entry,
                Err(source) => {
                    return Err(ResidentFailure::MissingEntrySymbol {
                        path: library_path,
                        source: Arc::new(source),
                        _mapping: mapping,
                    });
                }
            };

        Ok(Arc::new(ResidentArtifact {
            _mapping: mapping,
            entry,
        }))
    }
}
