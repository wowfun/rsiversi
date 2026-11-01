//! Unix root acquisition and private directory modes.
pub(super) use super::native::Store;
use super::native::{create_directory, directory, io_error};
use super::{Path, Result};
use rsi_files_native_fs as native;
use std::os::unix::fs::PermissionsExt as _;

pub(super) fn make_private(directory: &native::Directory) -> Result<()> {
    directory
        .try_clone()
        .map_err(io_error)?
        .into_std_file()
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(io_error)
}

impl Store {
    pub(super) fn open(root: &Path) -> Result<Self> {
        let path = native::resolve_absolute_root_alias(root, true).map_err(io_error)?;
        let mut ancestors =
            vec![native::open_absolute_directory_no_follow(Path::new("/")).map_err(io_error)?];
        for component in path.components() {
            if let std::path::Component::Normal(name) = component {
                let parent = ancestors.last().expect("root retained");
                ancestors.push(create_directory(parent, name)?);
            }
        }
        let objects = directory(ancestors.last().expect("root retained"), "objects")?;
        objects.sync_all().map_err(io_error)?;
        for ancestor in ancestors.iter().rev() {
            ancestor.sync_all().map_err(io_error)?;
        }
        Ok(Self { objects })
    }
}
