use crate::owner::io_error;
use crate::{ServiceHostError, ServiceHostPaths};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use tokio::net::UnixListener;

#[derive(Debug)]
pub(crate) struct PublishedSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl PublishedSocket {
    pub(crate) fn bind(paths: &ServiceHostPaths) -> Result<(UnixListener, Self), ServiceHostError> {
        create_private_runtime_directory(paths.runtime_directory())?;
        remove_stale_socket_after_failed_probe(paths.socket())?;
        // Keep this name shorter than `host.sock`: a public path at the Unix
        // sockaddr limit must remain publishable. The owner lease serializes
        // publishers; any crash-left socket is still probed before removal.
        let staging = paths.runtime_directory().join(".s");
        remove_stale_socket_after_failed_probe(&staging)?;
        let listener = UnixListener::bind(&staging).map_err(io_error)?;
        let staged_metadata = fs::symlink_metadata(&staging).map_err(io_error)?;
        if !staged_metadata.file_type().is_socket() {
            let _ = fs::remove_file(&staging);
            return Err(ServiceHostError::Invalid(
                "staged Service Host endpoint is not a socket".into(),
            ));
        }
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        if let Err(error) = fs::hard_link(&staging, paths.socket()) {
            let _ = fs::remove_file(&staging);
            return Err(io_error(error));
        }
        let published = fs::symlink_metadata(paths.socket()).map_err(io_error)?;
        let _ = fs::remove_file(&staging);
        if !published.file_type().is_socket()
            || published.dev() != staged_metadata.dev()
            || published.ino() != staged_metadata.ino()
        {
            return Err(ServiceHostError::Invalid(
                "published Service Host endpoint changed during staged bind".into(),
            ));
        }
        Ok((
            listener,
            Self {
                path: paths.socket().to_owned(),
                device: published.dev(),
                inode: published.ino(),
            },
        ))
    }
}

impl Drop for PublishedSocket {
    fn drop(&mut self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let tombstone = parent.join(format!(".host.cleanup.{}.sock", self.inode));
        if fs::rename(&self.path, &tombstone).is_err() {
            return;
        }
        let matches = fs::symlink_metadata(&tombstone).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        });
        if matches {
            let _ = fs::remove_file(tombstone);
        } else if !self.path.exists() {
            let _ = fs::rename(tombstone, &self.path);
        }
    }
}

fn create_private_runtime_directory(path: &Path) -> Result<(), ServiceHostError> {
    let parent = path.parent().ok_or_else(|| {
        ServiceHostError::Invalid("Service Host runtime path has no parent".into())
    })?;
    if parent.file_name().is_some_and(|name| name == "rsi") {
        let runtime_root = parent.parent().ok_or_else(|| {
            ServiceHostError::Invalid("Service Host runtime root is missing".into())
        })?;
        fs::create_dir_all(runtime_root).map_err(io_error)?;
        validate_effective_user_directory(runtime_root, "Service Host runtime root")?;
        create_directory(parent)?;
        validate_effective_user_directory(parent, "Service Host runtime parent")?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        create_directory(path)?;
    } else {
        fs::create_dir_all(path).map_err(io_error)?;
    }
    validate_effective_user_directory(path, "Service Host runtime path")?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

fn create_directory(path: &Path) -> Result<(), ServiceHostError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn validate_effective_user_directory(path: &Path, label: &str) -> Result<(), ServiceHostError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ServiceHostError::Invalid(format!(
            "{label} is not a real directory"
        )));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(ServiceHostError::Invalid(format!(
            "{label} is not owned by the effective user"
        )));
    }
    Ok(())
}

fn remove_stale_socket_after_failed_probe(path: &Path) -> Result<(), ServiceHostError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.file_type().is_socket() {
        return Err(ServiceHostError::Invalid(
            "existing Service Host endpoint is not a socket".into(),
        ));
    }
    match probe_socket(path) {
        Ok(()) => Err(ServiceHostError::OwnerActive),
        Err(error)
            if error.kind() == io::ErrorKind::WouldBlock
                || error.raw_os_error() == Some(libc::EINPROGRESS) =>
        {
            Err(ServiceHostError::OwnerActive)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            let current = match fs::symlink_metadata(path) {
                Ok(current) => current,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(io_error(error)),
            };
            if current.file_type().is_socket()
                && current.dev() == metadata.dev()
                && current.ino() == metadata.ino()
            {
                match fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(io_error(error)),
                }
            } else {
                Err(ServiceHostError::Invalid(
                    "Service Host endpoint changed during stale probe".into(),
                ))
            }
        }
        Err(error) => Err(io_error(error)),
    }
}

fn probe_socket(path: &Path) -> io::Result<()> {
    // Tokio creates a nonblocking, close-on-exec socket on every supported Unix,
    // including systems without atomic SOCK_NONBLOCK/SOCK_CLOEXEC flags.
    let socket = tokio::net::UnixSocket::new_stream()?;
    Ok(rustix::net::connect(
        &socket,
        &rustix::net::SocketAddrUnix::new(path)?,
    )?)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn full_backlog_is_a_live_owner_without_blocking_the_publisher() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        rustix::net::listen(&listener, 1).unwrap();
        let mut peers = Vec::new();
        loop {
            use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
            let peer = rustix::net::socket_with(
                AddressFamily::UNIX,
                SocketType::STREAM,
                SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
                None,
            )
            .unwrap();
            match rustix::net::connect(&peer, &SocketAddrUnix::new(&path).unwrap()) {
                Ok(()) => peers.push(peer),
                Err(rustix::io::Errno::AGAIN) => break,
                Err(error) => panic!("fixture connect: {error}"),
            }
            assert!(peers.len() < 8);
        }
        let original = fs::symlink_metadata(&path).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let probe_path = path.clone();
        let probe = std::thread::spawn(move || {
            sender
                .send(remove_stale_socket_after_failed_probe(&probe_path))
                .unwrap();
        });
        let result = receiver.recv_timeout(std::time::Duration::from_secs(1));
        drop(listener); // Also releases a regressed blocking probe before joining.
        probe.join().unwrap();
        assert!(matches!(
            result.unwrap(),
            Err(ServiceHostError::OwnerActive)
        ));
        assert_eq!(fs::symlink_metadata(&path).unwrap().ino(), original.ino());
        drop(peers);
        remove_stale_socket_after_failed_probe(&path).unwrap();
        assert!(!path.exists());
        remove_stale_socket_after_failed_probe(&path).unwrap();
    }
}
