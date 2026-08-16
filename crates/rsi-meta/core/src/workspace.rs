use std::collections::{BTreeMap, BTreeSet};
use std::fs::{DirBuilder, File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rustix::fs::{FlockOperation, Mode, OFlags, flock};

use crate::domain::CompositionWorkspace;
use crate::model::PackageId;
use crate::{HostError, Result};

static HELD_WORKSPACES: OnceLock<Mutex<BTreeSet<PhysicalWorkspaceIdentity>>> = OnceLock::new();
type ProcessFixedFingerprints = BTreeSet<(PackageId, String)>;
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PhysicalWorkspaceIdentity {
    #[cfg(unix)]
    DeviceInode { device: u64, inode: u64 },
    #[cfg(not(unix))]
    CanonicalPath(PathBuf),
}
type ProcessFixedByWorkspace = BTreeMap<PhysicalWorkspaceIdentity, ProcessFixedFingerprints>;
static LOADED_PROCESS_FIXED: OnceLock<Mutex<ProcessFixedByWorkspace>> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct WorkspaceLease {
    identity: PhysicalWorkspaceIdentity,
    #[allow(dead_code)]
    sidecar: File,
    #[allow(dead_code)]
    physical: File,
}

impl WorkspaceLease {
    pub(crate) fn acquire(workspace: &CompositionWorkspace) -> Result<Self> {
        let display_path = normalize_absolute(&workspace.database_path)?;
        let (sidecar, identity) = open_path_guard(&workspace.database_path)?;
        let held = HELD_WORKSPACES.get_or_init(|| Mutex::new(BTreeSet::new()));
        {
            let mut held = held.lock().expect("workspace lease registry poisoned");
            if !held.insert(identity.clone()) {
                return Err(workspace_busy(&display_path));
            }
        }

        match open_physical_guard(&identity, &display_path) {
            Ok(physical) => Ok(Self {
                identity,
                sidecar,
                physical,
            }),
            Err(error) => {
                held.lock()
                    .expect("workspace lease registry poisoned")
                    .remove(&identity);
                Err(error)
            }
        }
    }
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        let _ = flock(&self.physical, FlockOperation::Unlock);
        let _ = flock(&self.sidecar, FlockOperation::Unlock);
        HELD_WORKSPACES
            .get_or_init(|| Mutex::new(BTreeSet::new()))
            .lock()
            .expect("workspace lease registry poisoned")
            .remove(&self.identity);
    }
}

pub(crate) fn installed_files(
    workspace: &CompositionWorkspace,
) -> Result<Option<crate::host::CompositionFiles>> {
    let manifest_exists = workspace
        .manifest_path
        .try_exists()
        .map_err(|source| HostError::Io {
            path: workspace.manifest_path.clone(),
            source,
        })?;
    let lock_exists = workspace
        .lock_path
        .try_exists()
        .map_err(|source| HostError::Io {
            path: workspace.lock_path.clone(),
            source,
        })?;
    match (manifest_exists, lock_exists) {
        (false, false) => Ok(None),
        (true, true) => Ok(Some(crate::host::CompositionFiles::new(
            workspace.manifest_path.clone(),
            workspace.lock_path.clone(),
        ))),
        _ => Err(HostError::OperationRejected {
            code: "torn_installed_pair".to_owned(),
            message: format!(
                "installed manifest {} and lock {} must either both exist or both be absent",
                workspace.manifest_path.display(),
                workspace.lock_path.display()
            ),
            details: std::collections::BTreeMap::new(),
        }),
    }
}

fn open_path_guard(database_path: &Path) -> Result<(File, PhysicalWorkspaceIdentity)> {
    let lock_path = lease_path(database_path)?;
    let parent = lock_path.parent().ok_or_else(|| HostError::Io {
        path: lock_path.clone(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace lease path has no parent",
        ),
    })?;
    std::fs::create_dir_all(parent).map_err(|source| HostError::Io {
        path: parent.to_owned(),
        source,
    })?;
    let sidecar = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| HostError::Io {
            path: lock_path.clone(),
            source,
        })?;
    lock_file(&sidecar, &lock_path, database_path)?;
    let database = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(database_path)
        .map_err(|source| HostError::Io {
            path: database_path.to_owned(),
            source,
        })?;
    let identity = physical_workspace_identity_from_file(&database, database_path)?;
    Ok((sidecar, identity))
}

#[cfg(unix)]
fn open_physical_guard(
    identity: &PhysicalWorkspaceIdentity,
    workspace_path: &Path,
) -> Result<File> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let effective_uid = rustix::process::geteuid().as_raw();
    let root = PathBuf::from("/tmp").join(format!("rsi-meta-workspace-leases-{effective_uid}"));
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    if let Err(source) = builder.create(&root)
        && source.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(HostError::Io { path: root, source });
    }
    let root_metadata = std::fs::symlink_metadata(&root).map_err(|source| HostError::Io {
        path: root.clone(),
        source,
    })?;
    if !root_metadata.file_type().is_dir()
        || root_metadata.uid() != effective_uid
        || root_metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HostError::Io {
            path: root,
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "workspace lease root must be an owner-only directory",
            ),
        });
    }
    let PhysicalWorkspaceIdentity::DeviceInode { device, inode } = identity;
    let guard_path = root.join(format!("device-{device}-inode-{inode}.lock"));
    let descriptor = rustix::fs::open(
        &guard_path,
        OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|source| HostError::Io {
        path: guard_path.clone(),
        source: source.into(),
    })?;
    let guard = File::from(descriptor);
    let metadata = guard.metadata().map_err(|source| HostError::Io {
        path: guard_path.clone(),
        source,
    })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HostError::Io {
            path: guard_path,
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "workspace identity lease must be an owner-only regular file",
            ),
        });
    }
    lock_file(&guard, &guard_path, workspace_path)?;
    Ok(guard)
}

fn lock_file(file: &File, lock_path: &Path, workspace_path: &Path) -> Result<()> {
    flock(file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN {
            workspace_busy(workspace_path)
        } else {
            HostError::Io {
                path: lock_path.to_owned(),
                source: std::io::Error::from_raw_os_error(error.raw_os_error()),
            }
        }
    })
}

fn lease_path(database_path: &Path) -> Result<PathBuf> {
    let file_name = database_path.file_name().ok_or_else(|| HostError::Io {
        path: database_path.to_owned(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace database path has no file name",
        ),
    })?;
    let mut lease_name = file_name.to_os_string();
    lease_name.push(".workspace.lock");
    Ok(database_path.with_file_name(lease_name))
}

pub(crate) fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|source| HostError::Io {
                path: PathBuf::from("."),
                source,
            })?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

pub(crate) fn record_loaded_process_fixed(
    workspace: &CompositionWorkspace,
    fingerprints: BTreeSet<(PackageId, String)>,
) -> Result<()> {
    if fingerprints.is_empty() {
        return Ok(());
    }
    let identity = physical_workspace_identity(&workspace.database_path)?;
    LOADED_PROCESS_FIXED
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("process-fixed registry poisoned")
        .entry(identity)
        .or_default()
        .extend(fingerprints);
    Ok(())
}

pub(crate) fn require_fresh_process_for_changed_fixed(
    workspace: &CompositionWorkspace,
    candidate: &BTreeSet<(PackageId, String)>,
) -> Result<()> {
    let identity = physical_workspace_identity(&workspace.database_path)?;
    let loaded = LOADED_PROCESS_FIXED
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("process-fixed registry poisoned")
        .get(&identity)
        .cloned()
        .unwrap_or_default();
    if loaded.is_empty() || loaded == *candidate {
        return Ok(());
    }
    Err(HostError::OperationRejected {
        code: "fresh_process_required".to_owned(),
        message: "this process already mapped a different process-fixed composition".to_owned(),
        details: std::collections::BTreeMap::new(),
    })
}

fn physical_workspace_identity(database_path: &Path) -> Result<PhysicalWorkspaceIdentity> {
    #[cfg(unix)]
    {
        let database = File::open(database_path).map_err(|source| HostError::Io {
            path: database_path.to_owned(),
            source,
        })?;
        physical_workspace_identity_from_file(&database, database_path)
    }
    #[cfg(not(unix))]
    {
        std::fs::canonicalize(database_path)
            .map(PhysicalWorkspaceIdentity::CanonicalPath)
            .map_err(|source| HostError::Io {
                path: database_path.to_owned(),
                source,
            })
    }
}

#[cfg(unix)]
fn physical_workspace_identity_from_file(
    database: &File,
    database_path: &Path,
) -> Result<PhysicalWorkspaceIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = database.metadata().map_err(|source| HostError::Io {
        path: database_path.to_owned(),
        source,
    })?;
    Ok(PhysicalWorkspaceIdentity::DeviceInode {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn workspace_busy(path: &Path) -> HostError {
    HostError::OperationRejected {
        code: "workspace_busy".to_owned(),
        message: format!("workspace {} is already in use", path.display()),
        details: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn workspace(temp: &TempDir) -> CompositionWorkspace {
        CompositionWorkspace {
            database_path: temp.path().join("state.sqlite3"),
            cache_root: temp.path().join("cache"),
            manifest_path: temp.path().join("composition.toml"),
            lock_path: temp.path().join("rsi-meta.lock"),
        }
    }

    #[test]
    fn same_process_cannot_hold_a_workspace_twice() {
        let temp = TempDir::new().unwrap();
        let workspace = workspace(&temp);
        let first = WorkspaceLease::acquire(&workspace).unwrap();
        let error = WorkspaceLease::acquire(&workspace).unwrap_err();
        assert!(matches!(
            error,
            HostError::OperationRejected { ref code, .. } if code == "workspace_busy"
        ));
        drop(first);
        WorkspaceLease::acquire(&workspace).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_alias_cannot_hold_the_same_workspace_twice() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let first_workspace = workspace(&first_root);
        let second_workspace = workspace(&second_root);
        std::fs::write(&first_workspace.database_path, []).unwrap();
        std::fs::hard_link(
            &first_workspace.database_path,
            &second_workspace.database_path,
        )
        .unwrap();

        let first = WorkspaceLease::acquire(&first_workspace).unwrap();
        let error = WorkspaceLease::acquire(&second_workspace).unwrap_err();
        assert!(matches!(
            error,
            HostError::OperationRejected { ref code, .. } if code == "workspace_busy"
        ));
        drop(first);
        WorkspaceLease::acquire(&second_workspace).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn process_fixed_registry_uses_the_database_inode_across_hard_link_aliases() {
        let first_root = TempDir::new().unwrap();
        let second_root = TempDir::new().unwrap();
        let first_workspace = workspace(&first_root);
        let second_workspace = workspace(&second_root);
        std::fs::write(&first_workspace.database_path, []).unwrap();
        std::fs::hard_link(
            &first_workspace.database_path,
            &second_workspace.database_path,
        )
        .unwrap();
        record_loaded_process_fixed(
            &first_workspace,
            BTreeSet::from([(PackageId::new("fixed"), "artifact-a".to_owned())]),
        )
        .unwrap();

        let error = require_fresh_process_for_changed_fixed(
            &second_workspace,
            &BTreeSet::from([(PackageId::new("fixed"), "artifact-b".to_owned())]),
        )
        .expect_err("physical database aliases must share process-fixed state");

        assert!(matches!(
            error,
            HostError::OperationRejected { ref code, .. } if code == "fresh_process_required"
        ));
    }

    #[test]
    fn installed_pair_is_all_or_nothing() {
        let temp = TempDir::new().unwrap();
        let workspace = workspace(&temp);
        assert!(installed_files(&workspace).unwrap().is_none());
        std::fs::write(&workspace.manifest_path, b"manifest").unwrap();
        let error = installed_files(&workspace).unwrap_err();
        assert!(matches!(
            error,
            HostError::OperationRejected { ref code, .. } if code == "torn_installed_pair"
        ));
    }
}
