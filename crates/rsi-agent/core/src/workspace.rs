use std::collections::BTreeSet;
use std::fs::{DirBuilder, File};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::{AgentError, AgentWorkspace, Result};

#[cfg(not(unix))]
compile_error!("rsi-agent requires Unix workspace locking; supported targets are Linux and macOS");

static HELD_WORKSPACES: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct WorkspaceLease {
    identity: PathBuf,
    #[allow(dead_code)]
    file: File,
    #[allow(dead_code)]
    database: File,
    #[cfg(unix)]
    database_identity: (u64, u64),
}

impl WorkspaceLease {
    pub(crate) fn acquire(workspace: &AgentWorkspace) -> Result<Self> {
        let resolved_root = resolve_root(workspace.root())?;
        prepare_root(&resolved_root)?;
        let identity = resolved_root
            .canonicalize()
            .map_err(|error| AgentError::io("canonicalize agent workspace", error))?;
        let held = HELD_WORKSPACES.get_or_init(|| Mutex::new(BTreeSet::new()));
        {
            let mut held = held.lock().expect("workspace registry poisoned");
            if !held.insert(identity.clone()) {
                return Err(AgentError::WorkspaceOccupied { path: identity });
            }
        }

        let lease_path = identity.join("agent.lock");
        let database_path = identity.join("agent.sqlite3");
        match open_and_lock(&lease_path, &identity).and_then(|file| {
            let database = open_database(&database_path)?;
            #[cfg(unix)]
            let database_identity = database_identity(&database, &database_path)?;
            Ok(Self {
                identity: identity.clone(),
                file,
                database,
                #[cfg(unix)]
                database_identity,
            })
        }) {
            Ok(lease) => Ok(lease),
            Err(error) => {
                held.lock()
                    .expect("workspace registry poisoned")
                    .remove(&identity);
                Err(error)
            }
        }
    }

    pub(crate) fn verify_database(&self, path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|error| AgentError::io("verify opened agent store", error))?;
            if database_identity(&opened, path)? != self.database_identity {
                return Err(AgentError::Persistence {
                    operation: "verify opened agent store",
                    message: format!("{} changed while SQLite was opening it", path.display()),
                });
            }
        }
        let _ = path;
        Ok(())
    }

    pub(crate) fn database_path(&self) -> PathBuf {
        self.identity.join("agent.sqlite3")
    }
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
        }
        HELD_WORKSPACES
            .get_or_init(|| Mutex::new(BTreeSet::new()))
            .lock()
            .expect("workspace registry poisoned")
            .remove(&self.identity);
    }
}

fn prepare_root(root: &Path) -> Result<()> {
    let existed = root
        .try_exists()
        .map_err(|error| AgentError::io("inspect agent workspace", error))?;
    if !existed {
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .recursive(true)
            .create(root)
            .map_err(|error| AgentError::io("create agent workspace", error))?;
    }

    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| AgentError::io("inspect agent workspace metadata", error))?;
    if !metadata.file_type().is_dir() {
        return Err(AgentError::Persistence {
            operation: "validate agent workspace",
            message: format!("{} must be a non-symlink directory", root.display()),
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let effective_uid = rustix::process::geteuid().as_raw();
        if metadata.uid() != effective_uid || metadata.permissions().mode() & 0o077 != 0 {
            return Err(AgentError::Persistence {
                operation: "validate agent workspace permissions",
                message: format!("{} must be owner-owned with mode 0700", root.display()),
            });
        }
    }
    Ok(())
}

fn resolve_root(root: &Path) -> Result<PathBuf> {
    let absolute = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| AgentError::io("resolve current directory", error))?
            .join(root)
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(cursor) {
            Ok(_) => {
                let mut resolved = cursor
                    .canonicalize()
                    .map_err(|error| AgentError::io("resolve agent workspace ancestor", error))?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = cursor.file_name().ok_or_else(|| AgentError::Persistence {
                    operation: "resolve agent workspace",
                    message: format!("{} has no resolvable ancestor", root.display()),
                })?;
                missing.push(component.to_owned());
                cursor = cursor.parent().ok_or_else(|| AgentError::Persistence {
                    operation: "resolve agent workspace",
                    message: format!("{} has no parent directory", root.display()),
                })?;
            }
            Err(error) => return Err(AgentError::io("inspect agent workspace ancestor", error)),
        }
    }
}

#[cfg(unix)]
fn open_and_lock(path: &Path, identity: &Path) -> Result<File> {
    use rustix::fs::{FlockOperation, Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| AgentError::Persistence {
        operation: "open agent workspace lease",
        message: error.to_string(),
    })?;
    let file = File::from(descriptor);
    rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN {
            AgentError::WorkspaceOccupied {
                path: identity.to_owned(),
            }
        } else {
            AgentError::Persistence {
                operation: "lock agent workspace",
                message: error.to_string(),
            }
        }
    })?;
    Ok(file)
}

#[cfg(unix)]
fn open_database(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let descriptor = rustix::fs::open(
        path,
        OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| AgentError::Persistence {
        operation: "open agent store guard",
        message: error.to_string(),
    })?;
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|error| AgentError::io("inspect agent store guard", error))?;
    let effective_uid = rustix::process::geteuid().as_raw();
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(AgentError::Persistence {
            operation: "validate agent store guard",
            message: format!("{} must be an owner-only regular file", path.display()),
        });
    }
    Ok(file)
}

#[cfg(unix)]
fn database_identity(file: &File, path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file
        .metadata()
        .map_err(|error| AgentError::io("inspect agent store identity", error))?;
    if !metadata.file_type().is_file() {
        return Err(AgentError::Persistence {
            operation: "validate agent store identity",
            message: format!("{} is not a regular file", path.display()),
        });
    }
    Ok((metadata.dev(), metadata.ino()))
}

// These definitions only keep type checking focused on the explicit platform
// error above. They can never be linked because non-Unix builds are rejected.
#[cfg(not(unix))]
fn open_and_lock(_path: &Path, _identity: &Path) -> Result<File> {
    unreachable!("non-Unix builds are rejected")
}

#[cfg(not(unix))]
fn open_database(_path: &Path) -> Result<File> {
    unreachable!("non-Unix builds are rejected")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn one_process_cannot_open_workspace_twice() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let first = WorkspaceLease::acquire(&workspace).expect("first lease");
        assert!(matches!(
            WorkspaceLease::acquire(&workspace),
            Err(AgentError::WorkspaceOccupied { .. })
        ));
        drop(first);
        WorkspaceLease::acquire(&workspace).expect("released lease");
    }

    #[cfg(unix)]
    #[test]
    fn database_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        std::fs::create_dir(workspace.root()).expect("workspace");
        std::fs::set_permissions(
            workspace.root(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("permissions");
        let target = temp.path().join("target.sqlite3");
        std::fs::write(&target, []).expect("target");
        symlink(&target, workspace.database_path()).expect("symlink");
        assert!(WorkspaceLease::acquire(&workspace).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replacing_the_database_while_leased_is_detected() {
        let temp = tempdir().expect("tempdir");
        let workspace = AgentWorkspace::new(temp.path().join("agent"));
        let lease = WorkspaceLease::acquire(&workspace).expect("lease");
        let displaced = temp.path().join("displaced.sqlite3");
        std::fs::rename(workspace.database_path(), &displaced).expect("displace database");
        std::fs::write(workspace.database_path(), []).expect("replacement database");

        assert!(matches!(
            lease.verify_database(&workspace.database_path()),
            Err(AgentError::Persistence { .. })
        ));
    }
}
