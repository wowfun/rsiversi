use crate::{HostError, Result};
use std::path::{Path, PathBuf};

/// Frozen filesystem authority supplied to one Host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostPaths {
    config: PathBuf,
    state: PathBuf,
    cache: PathBuf,
}

impl HostPaths {
    /// Validates three explicit absolute path authorities without accessing them.
    pub fn new(
        config: impl Into<PathBuf>,
        state: impl Into<PathBuf>,
        cache: impl Into<PathBuf>,
    ) -> Result<Self> {
        let config = absolute("config", config.into())?;
        let state = absolute("state", state.into())?;
        let cache = absolute("cache", cache.into())?;
        Ok(Self {
            config,
            state,
            cache,
        })
    }

    /// Configuration root selected by the application.
    pub fn config(&self) -> &Path {
        &self.config
    }

    /// Durable state root selected by the application.
    pub fn state(&self) -> &Path {
        &self.state
    }

    /// Rebuildable cache root selected by the application.
    pub fn cache(&self) -> &Path {
        &self.cache
    }
}

fn absolute(kind: &'static str, path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(HostError::PathNotAbsolute { kind, path })
    }
}
