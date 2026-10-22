use rsi_packages::{ExportKind, PackageLease, PackageStore};
use rsi_ssh_client::HelperArtifact;
use std::path::Path;

#[derive(Debug)]
pub(super) enum Failure {
    Busy,
    Unavailable,
}
impl From<rsi_packages::Error> for Failure {
    fn from(error: rsi_packages::Error) -> Self {
        if matches!(error, rsi_packages::Error::Busy) {
            Self::Busy
        } else {
            Self::Unavailable
        }
    }
}

/// Resolve on every connection so installing a missing helper needs no Host restart.
pub(super) async fn installed(root: &Path) -> Result<(HelperArtifact, PackageLease), Failure> {
    let root = root.to_owned();
    let (path, digest, lease) = tokio::task::spawn_blocking(move || selected(&root))
        .await
        .map_err(|_| Failure::Unavailable)??;
    let artifact = HelperArtifact::load(&path, digest)
        .await
        .map_err(|_| Failure::Unavailable)?;
    Ok((artifact, lease))
}

fn selected(root: &Path) -> Result<(std::path::PathBuf, [u8; 32], PackageLease), Failure> {
    let store = PackageStore::open(root).map_err(|_| Failure::Unavailable)?;
    let lease = store.acquire("ssh-helper").map_err(Failure::from)?;
    let manifest = lease.manifest();
    if manifest.target != format!("linux-{}", std::env::consts::ARCH)
        || manifest.requires != [rsi_ssh_protocol::protocol_descriptor()]
    {
        return Err(Failure::Unavailable);
    }
    let export = manifest
        .exports
        .get("ssh-helper")
        .ok_or(Failure::Unavailable)?;
    let file = manifest
        .files
        .iter()
        .find(|file| file.path == export.path && file.executable)
        .ok_or(Failure::Unavailable)?;
    let digest = hex::decode(&file.digest)
        .map_err(|_| Failure::Unavailable)?
        .try_into()
        .map_err(|_| Failure::Unavailable)?;
    let path = lease
        .export("ssh-helper", ExportKind::Executable)
        .map_err(|_| Failure::Unavailable)?;
    Ok((path, digest, lease))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn package_writer_contention_is_busy_not_missing_helper() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("packages");
        let store = PackageStore::open(&path).unwrap();
        store.resolution().unwrap();
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join("writer.lock"))
            .unwrap();
        writer.lock().unwrap();
        assert!(matches!(installed(&path).await, Err(Failure::Busy)));
    }

    use rsi_packages::{Export, FileRecord, InstallSource, Manifest, Provenance};
    use sha2::{Digest, Sha256};
    #[test]
    fn newly_installed_helper_is_resolved_and_old_generation_stays_leased() {
        let root = tempfile::tempdir().unwrap();
        let store = PackageStore::open(&root.path().join("packages")).unwrap();
        assert!(selected(&root.path().join("packages")).is_err());
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let publish = |bytes: &[u8]| {
            std::fs::write(source.join("helper"), bytes).unwrap();
            let manifest = Manifest {
                format: 1,
                name: "ssh-helper".into(),
                version: "fixture".into(),
                target: format!("linux-{}", std::env::consts::ARCH),
                dependencies: vec![],
                requires: vec![rsi_ssh_protocol::protocol_descriptor()],
                files: vec![FileRecord {
                    path: "helper".into(),
                    bytes: bytes.len() as u64,
                    digest: hex::encode(Sha256::digest(bytes)),
                    executable: true,
                }],
                exports: [(
                    "ssh-helper".into(),
                    Export {
                        kind: ExportKind::Executable,
                        path: "helper".into(),
                    },
                )]
                .into(),
            };
            store
                .install(
                    &[InstallSource {
                        directory: source.clone(),
                        manifest: serde_json::to_vec(&manifest).unwrap(),
                        provenance: Provenance::Local,
                    }],
                    &[],
                    &[rsi_ssh_protocol::protocol_descriptor()],
                    &manifest.target,
                )
                .unwrap();
        };
        publish(b"first");
        let (first, _, lease) = selected(&root.path().join("packages")).unwrap();
        publish(b"second");
        let (second, _, newer) = selected(&root.path().join("packages")).unwrap();
        assert_ne!(lease.digest(), newer.digest());
        assert!(store.collect().unwrap().is_empty());
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(second).unwrap(), b"second");
        drop(lease);
        assert_eq!(store.collect().unwrap().len(), 1);
        assert!(!first.exists());
    }
}
