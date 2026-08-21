use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::storage::CachePin;
use super::{ArtifactManifest, ContentHash, PluginManifest, PluginPackage};

/// A package artifact published in the immutable local cache.
#[derive(Clone, Debug)]
pub struct StagedPlugin {
    pub(super) package: PluginPackage,
    pub(super) artifact: ArtifactManifest,
    pub(super) cached_artifact_path: PathBuf,
    pub(super) artifact_hash: ContentHash,
    pub(super) cache_pin: Arc<CachePin>,
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

    pub fn cached_artifact_path(&self) -> &Path {
        &self.cached_artifact_path
    }

    pub const fn artifact_hash(&self) -> ContentHash {
        self.artifact_hash
    }
}
