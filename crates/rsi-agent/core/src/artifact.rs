use std::{
    collections::HashSet,
    fmt,
    fs::{DirBuilder, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::{AgentError, Result, digest::sha256_hex, workspace::WorkspaceLease};
use rsi_ai_protocol::{MediaDescriptor, MediaKind};
use serde::{Deserialize, Serialize};

const MAX_ARTIFACTS: usize = 4_096;
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

/// Durable content-addressed media identity. The locator never leaves the Agent workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    descriptor: MediaDescriptor,
}

impl ArtifactRef {
    pub const fn descriptor(&self) -> &MediaDescriptor {
        &self.descriptor
    }

    pub fn id(&self) -> &str {
        self.descriptor.sha256()
    }
}

/// Bounded workspace-owned CAS used by durable Agent media operations.
#[derive(Clone)]
pub struct ArtifactStore {
    root: Arc<PathBuf>,
    usage: Arc<SharedArtifactUsage>,
    _lease: Arc<WorkspaceLease>,
}

#[derive(Debug)]
struct ArtifactUsage {
    count: usize,
    bytes: u64,
    reserved_count: usize,
    reserved_bytes: u64,
    in_flight: HashSet<String>,
}

struct SharedArtifactUsage {
    state: Mutex<ArtifactUsage>,
    changed: Condvar,
}

struct ArtifactReservation {
    usage: Arc<SharedArtifactUsage>,
    digest: String,
    byte_len: u64,
    active: bool,
}

impl ArtifactStore {
    pub(crate) fn open(workspace_root: &Path, lease: Arc<WorkspaceLease>) -> Result<Self> {
        let root = workspace_root.join("artifacts");
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder
            .recursive(false)
            .create(&root)
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|error| AgentError::io("create artifact store", error))?;
        validate_root(&root)?;
        clean_temporary_files(&root)?;
        let (count, bytes) = store_usage(&root)?;
        Ok(Self {
            root: Arc::new(root),
            usage: Arc::new(SharedArtifactUsage {
                state: Mutex::new(ArtifactUsage {
                    count,
                    bytes,
                    reserved_count: 0,
                    reserved_bytes: 0,
                    in_flight: HashSet::new(),
                }),
                changed: Condvar::new(),
            }),
            _lease: lease,
        })
    }

    /// Atomically commits validated bytes before returning their durable reference.
    ///
    /// # Errors
    ///
    /// Returns an error when validation, quota enforcement, or the atomic commit fails.
    pub async fn ingest(
        &self,
        kind: MediaKind,
        mime_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef> {
        let store = self.clone();
        let mime_type = mime_type.into();
        tokio::task::spawn_blocking(move || store.ingest_blocking(kind, mime_type, &bytes))
            .await
            .map_err(|error| AgentError::Ai {
                operation: "join artifact commit",
                message: error.to_string(),
            })?
    }

    /// Resolves and re-verifies a durable artifact without exposing a filesystem locator.
    ///
    /// # Errors
    ///
    /// Returns an error when the reference or backing file fails validation.
    pub async fn read(&self, artifact: &ArtifactRef) -> Result<Vec<u8>> {
        let store = self.clone();
        let artifact = artifact.clone();
        tokio::task::spawn_blocking(move || store.read_blocking(&artifact))
            .await
            .map_err(|error| AgentError::Ai {
                operation: "join artifact read",
                message: error.to_string(),
            })?
    }

    pub(crate) async fn read_descriptor(&self, descriptor: &MediaDescriptor) -> Result<Vec<u8>> {
        self.read(&ArtifactRef {
            descriptor: descriptor.clone(),
        })
        .await
    }

    fn ingest_blocking(
        &self,
        kind: MediaKind,
        mime_type: String,
        bytes: &[u8],
    ) -> Result<ArtifactRef> {
        validate_root(&self.root)?;
        let digest = sha256_hex(bytes);
        let descriptor = MediaDescriptor::new(
            kind,
            mime_type,
            u64::try_from(bytes.len()).map_err(|_| AgentError::Ai {
                operation: "validate artifact",
                message: "artifact length exceeds u64".to_owned(),
            })?,
            digest.clone(),
        )
        .map_err(|error| AgentError::Ai {
            operation: "validate artifact",
            message: error.to_string(),
        })?;
        let destination = self.root.join(&digest);
        let Some(mut reservation) = self.reserve(&digest, descriptor.byte_len(), &destination)?
        else {
            let artifact = ArtifactRef { descriptor };
            self.read_blocking(&artifact)?;
            return Ok(artifact);
        };
        let temporary = self.root.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| AgentError::io("create artifact temporary file", error))?;
        let written = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok::<_, std::io::Error>(())
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(AgentError::io("write artifact", error));
        }
        let created = match std::fs::hard_link(&temporary, &destination) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(AgentError::io("commit artifact", error));
            }
        };
        let removed = std::fs::remove_file(&temporary);
        let synced = std::fs::File::open(self.root.as_ref())
            .and_then(|directory| directory.sync_all())
            .map_err(|error| AgentError::io("sync artifact directory", error));
        reservation.finish(created)?;
        removed.map_err(|error| AgentError::io("remove artifact temporary file", error))?;
        synced?;
        let artifact = ArtifactRef { descriptor };
        if !created {
            self.read_blocking(&artifact)?;
        }
        Ok(artifact)
    }

    fn reserve(
        &self,
        digest: &str,
        byte_len: u64,
        destination: &Path,
    ) -> Result<Option<ArtifactReservation>> {
        loop {
            let mut usage = self.usage.state.lock().map_err(|_| lock_error())?;
            if usage.in_flight.contains(digest) {
                usage = self.usage.changed.wait(usage).map_err(|_| lock_error())?;
                drop(usage);
                continue;
            }
            drop(usage);

            if destination
                .try_exists()
                .map_err(|error| AgentError::io("inspect artifact", error))?
            {
                return Ok(None);
            }

            let mut usage = self.usage.state.lock().map_err(|_| lock_error())?;
            if usage.in_flight.contains(digest) {
                drop(usage);
                continue;
            }
            let projected_count = usage
                .count
                .checked_add(usage.reserved_count)
                .and_then(|count| count.checked_add(1));
            let projected_bytes = usage
                .bytes
                .checked_add(usage.reserved_bytes)
                .and_then(|bytes| bytes.checked_add(byte_len));
            if projected_count.is_none_or(|count| count > MAX_ARTIFACTS)
                || projected_bytes.is_none_or(|bytes| bytes > MAX_ARTIFACT_BYTES)
            {
                return Err(AgentError::Ai {
                    operation: "commit artifact",
                    message: "artifact store quota exceeded".to_owned(),
                });
            }
            usage.reserved_count = usage
                .reserved_count
                .checked_add(1)
                .expect("artifact reservation count was bounded");
            usage.reserved_bytes = usage
                .reserved_bytes
                .checked_add(byte_len)
                .expect("artifact reservation bytes were bounded");
            assert!(usage.in_flight.insert(digest.to_owned()));
            drop(usage);
            return Ok(Some(ArtifactReservation {
                usage: Arc::clone(&self.usage),
                digest: digest.to_owned(),
                byte_len,
                active: true,
            }));
        }
    }

    fn read_blocking(&self, artifact: &ArtifactRef) -> Result<Vec<u8>> {
        artifact
            .descriptor
            .validate()
            .map_err(|error| AgentError::Ai {
                operation: "validate artifact reference",
                message: error.to_string(),
            })?;
        validate_root(&self.root)?;
        let path = self.root.join(artifact.id());
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| AgentError::io("inspect artifact", error))?;
        if !metadata.file_type().is_file() || metadata.len() != artifact.descriptor.byte_len() {
            return Err(AgentError::Ai {
                operation: "validate artifact file",
                message: "artifact is not a regular file with the committed length".to_owned(),
            });
        }
        let bytes = std::fs::read(&path).map_err(|error| AgentError::io("read artifact", error))?;
        if sha256_hex(&bytes) != artifact.id() {
            return Err(AgentError::Ai {
                operation: "verify artifact digest",
                message: "artifact content does not match its durable reference".to_owned(),
            });
        }
        Ok(bytes)
    }
}

impl ArtifactReservation {
    fn finish(&mut self, created: bool) -> Result<()> {
        let mut usage = self.usage.state.lock().map_err(|_| lock_error())?;
        self.release(&mut usage, created);
        self.active = false;
        drop(usage);
        self.usage.changed.notify_all();
        Ok(())
    }

    fn release(&self, usage: &mut ArtifactUsage, created: bool) {
        assert!(usage.in_flight.remove(&self.digest));
        usage.reserved_count = usage
            .reserved_count
            .checked_sub(1)
            .expect("active reservation contributed one count");
        usage.reserved_bytes = usage
            .reserved_bytes
            .checked_sub(self.byte_len)
            .expect("active reservation contributed its bytes");
        if created {
            usage.count = usage
                .count
                .checked_add(1)
                .expect("artifact count was bounded before commit");
            usage.bytes = usage
                .bytes
                .checked_add(self.byte_len)
                .expect("artifact bytes were bounded before commit");
        }
    }
}

impl Drop for ArtifactReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut usage) = self.usage.state.lock() {
            self.release(&mut usage, false);
            drop(usage);
            self.usage.changed.notify_all();
        }
    }
}

fn lock_error() -> AgentError {
    AgentError::Ai {
        operation: "lock artifact store",
        message: "artifact commit lock is poisoned".to_owned(),
    }
}

fn clean_temporary_files(root: &Path) -> Result<()> {
    for entry in
        std::fs::read_dir(root).map_err(|error| AgentError::io("scan artifact store", error))?
    {
        let entry = entry.map_err(|error| AgentError::io("scan artifact entry", error))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".tmp-") {
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| AgentError::io("inspect artifact temporary file", error))?;
            if !metadata.file_type().is_file() {
                return Err(AgentError::Ai {
                    operation: "clean artifact store",
                    message: "artifact temporary entry is not a regular file".to_owned(),
                });
            }
            std::fs::remove_file(entry.path())
                .map_err(|error| AgentError::io("clean artifact temporary file", error))?;
        }
    }
    Ok(())
}

impl fmt::Debug for ArtifactStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactStore")
            .finish_non_exhaustive()
    }
}

fn validate_root(root: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| AgentError::io("inspect artifact store", error))?;
    if !metadata.file_type().is_dir() {
        return Err(AgentError::Ai {
            operation: "validate artifact store",
            message: "artifact root is not a directory".to_owned(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(AgentError::Ai {
                operation: "validate artifact store",
                message: "artifact root must be owner-owned with mode 0700".to_owned(),
            });
        }
    }
    Ok(())
}

fn store_usage(root: &Path) -> Result<(usize, u64)> {
    let mut count = 0_usize;
    let mut total = 0_u64;
    for entry in
        std::fs::read_dir(root).map_err(|error| AgentError::io("scan artifact store", error))?
    {
        let entry = entry.map_err(|error| AgentError::io("scan artifact entry", error))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".tmp-") {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| AgentError::io("inspect artifact entry", error))?;
        if !metadata.file_type().is_file()
            || name.len() != 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(AgentError::Ai {
                operation: "scan artifact store",
                message: "artifact store contains an invalid entry".to_owned(),
            });
        }
        count = count.saturating_add(1);
        total = total
            .checked_add(metadata.len())
            .ok_or_else(|| AgentError::Ai {
                operation: "scan artifact store",
                message: "artifact store byte count overflowed".to_owned(),
            })?;
    }
    Ok((count, total))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::AgentWorkspace;

    #[tokio::test]
    async fn commits_deduplicates_and_reverifies_artifacts() {
        let temp = tempdir().expect("temporary workspace");
        let root = temp.path().join("agent");
        let workspace = AgentWorkspace::new(&root);
        let lease = Arc::new(WorkspaceLease::acquire(&workspace).expect("lease"));
        let store = ArtifactStore::open(&root, lease).expect("artifact store");
        let first = store
            .ingest(MediaKind::Image, "image/png", b"png bytes".to_vec())
            .await
            .expect("ingest");
        let second = store
            .ingest(MediaKind::Image, "image/png", b"png bytes".to_vec())
            .await
            .expect("deduplicate");
        assert_eq!(first, second);
        assert_eq!(store.read(&first).await.expect("read"), b"png bytes");

        std::fs::write(store.root.join(first.id()), b"corrupt").expect("corrupt fixture");
        assert!(store.read(&first).await.is_err());
    }

    #[tokio::test]
    async fn concurrent_deduplication_consumes_one_quota_reservation() {
        let temp = tempdir().expect("temporary workspace");
        let root = temp.path().join("agent");
        let workspace = AgentWorkspace::new(&root);
        let lease = Arc::new(WorkspaceLease::acquire(&workspace).expect("lease"));
        let store = ArtifactStore::open(&root, lease).expect("artifact store");
        let first_store = store.clone();
        let second_store = store.clone();
        let bytes = vec![b'x'; 2 * 1024 * 1024];
        let (first, second) = tokio::join!(
            first_store.ingest(MediaKind::Image, "image/png", bytes.clone()),
            second_store.ingest(MediaKind::Image, "image/png", bytes),
        );
        assert_eq!(first.expect("first ingest"), second.expect("second ingest"));

        let usage = store.usage.state.lock().expect("usage");
        assert_eq!(usage.count, 1);
        assert_eq!(usage.reserved_count, 0);
        assert_eq!(usage.reserved_bytes, 0);
        assert!(usage.in_flight.is_empty());
    }

    #[tokio::test]
    async fn rejects_media_that_exceeds_the_owned_kind_bound() {
        let temp = tempdir().expect("temporary workspace");
        let root = temp.path().join("agent");
        let workspace = AgentWorkspace::new(&root);
        let lease = Arc::new(WorkspaceLease::acquire(&workspace).expect("lease"));
        let store = ArtifactStore::open(&root, lease).expect("artifact store");
        let error = store
            .ingest(MediaKind::Audio, "image/png", b"wrong kind".to_vec())
            .await
            .expect_err("MIME kind mismatch");
        assert!(matches!(error, AgentError::Ai { .. }));
    }

    #[test]
    fn cloned_artifact_store_keeps_the_workspace_lease() {
        let temp = tempdir().expect("temporary workspace");
        let root = temp.path().join("agent");
        let workspace = AgentWorkspace::new(&root);
        let lease = Arc::new(WorkspaceLease::acquire(&workspace).expect("lease"));
        let store = ArtifactStore::open(&root, Arc::clone(&lease)).expect("artifact store");
        let clone = store.clone();
        drop(lease);
        drop(store);
        assert!(WorkspaceLease::acquire(&workspace).is_err());
        drop(clone);
        WorkspaceLease::acquire(&workspace).expect("clone released lease");
    }
}
