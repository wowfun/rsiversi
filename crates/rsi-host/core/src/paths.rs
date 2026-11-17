use crate::{HostError, Result};
use std::path::{Path, PathBuf};

/// Frozen filesystem authority supplied to one Host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostPaths {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    tmp: PathBuf,
}

impl HostPaths {
    /// Validates five explicit absolute path authorities without accessing them.
    pub fn new(
        config: impl Into<PathBuf>,
        data: impl Into<PathBuf>,
        state: impl Into<PathBuf>,
        cache: impl Into<PathBuf>,
        tmp: impl Into<PathBuf>,
    ) -> Result<Self> {
        let config = absolute("config", config.into())?;
        let data = absolute("data", data.into())?;
        let state = absolute("state", state.into())?;
        let cache = absolute("cache", cache.into())?;
        let tmp = absolute("tmp", tmp.into())?;
        Ok(Self {
            config,
            data,
            state,
            cache,
            tmp,
        })
    }

    /// Configuration root selected by the application.
    pub fn config(&self) -> &Path {
        &self.config
    }

    /// Durable user data root selected by the application.
    pub fn data(&self) -> &Path {
        &self.data
    }

    /// Parent for private per-operation scratch directories.
    pub fn tmp(&self) -> &Path {
        &self.tmp
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
