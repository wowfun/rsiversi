use std::path::PathBuf;

use rsi_meta_loader::{
    ContentHash, PluginPackage, read_bounded_file, resolve_package_relative_file,
};

use crate::host::MAX_CONFIG_SCHEMA_BYTES;
use crate::{HostError, Result};

pub(super) fn config_schema_hash(package: &PluginPackage) -> Result<Option<ContentHash>> {
    config_schema_bytes(package).map(|bytes| bytes.as_deref().map(ContentHash::digest))
}

pub(super) fn config_schema_bytes(package: &PluginPackage) -> Result<Option<Vec<u8>>> {
    config_schema_path(package)?
        .as_ref()
        .map(|path| {
            read_bounded_file(path, "read plugin config schema", MAX_CONFIG_SCHEMA_BYTES)
                .map_err(HostError::from)
        })
        .transpose()
}

pub(super) fn config_schema_path(package: &PluginPackage) -> Result<Option<PathBuf>> {
    package
        .manifest()
        .config_schema
        .as_ref()
        .map(|relative| {
            resolve_package_relative_file(
                package.manifest_path(),
                relative,
                "config_schema",
                "resolve plugin config schema",
            )
            .map_err(HostError::from)
        })
        .transpose()
}
