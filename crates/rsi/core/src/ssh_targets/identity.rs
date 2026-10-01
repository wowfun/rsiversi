use sha2::{Digest, Sha256};
use std::{
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};
/// Retains no bytes in Debug or durability; callers copy them into a connection-private file.
pub(super) struct Identity {
    pub path: String,
    pub digest: String,
    pub bytes: Vec<u8>,
}
pub(super) async fn read(path: String) -> Result<Identity, ()> {
    tokio::task::spawn_blocking(move || {
        let path = Path::new(&path).canonicalize().map_err(|_| ())?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|_| ())?;
        let metadata = file.metadata().map_err(|_| ())?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
            || !(1..=64 * 1024).contains(&metadata.len())
        {
            return Err(());
        }
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.is_empty() || bytes.len() > 64 * 1024 {
            return Err(());
        }
        Ok(Identity {
            path: path.to_str().ok_or(())?.into(),
            digest: hex::encode(Sha256::digest(&bytes)),
            bytes,
        })
    })
    .await
    .map_err(|_| ())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[tokio::test]
    async fn identity_requires_private_regular_bounded_bytes_and_detects_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        std::fs::write(&path, b"fixture-key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(path.to_str().unwrap().into()).await.is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let first = read(path.to_str().unwrap().into()).await.unwrap();
        std::fs::write(&path, b"replaced-fixture").unwrap();
        let second = read(path.to_str().unwrap().into()).await.unwrap();
        assert_ne!(first.digest, second.digest);
        assert!(read(root.path().to_str().unwrap().into()).await.is_err());
        std::fs::write(&path, vec![0; 64 * 1024 + 1]).unwrap();
        assert!(read(path.to_str().unwrap().into()).await.is_err());
    }
}
