//! Windows root privacy and retained namespace authority.
pub(super) use super::native::Store;
use super::native::{directory, io_error};
use super::{Path, Result};
use rsi_files_native_fs as native;

impl Store {
    pub(super) fn open(root: &Path) -> Result<Self> {
        native::create_absolute_directory_and_sync_no_follow(root).map_err(io_error)?;
        let root = native::set_private_directory_permissions_no_follow(root).map_err(io_error)?;
        let root_namespace = root.pin_namespace().map_err(io_error)?;
        let objects = directory(&root, "objects")?;
        let objects_namespace = objects.pin_namespace().map_err(io_error)?;
        objects_namespace
            .sync_all()
            .and_then(|()| root_namespace.sync_all())
            .map_err(io_error)?;
        Ok(Self {
            objects,
            objects_namespace,
            _root_namespace: root_namespace,
        })
    }
}
