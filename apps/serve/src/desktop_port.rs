//! Durable Origin identity for ordinary Desktop, under the Host's state lease.
use rsi_files_native_fs::{
    Directory, EntryKind, create_relative_private_file_no_follow,
    open_absolute_directory_no_follow, open_relative_file_no_follow,
};
use std::{
    io::{self, Read as _, Write as _},
    num::NonZeroU16,
    path::Path,
};

const RECORD: &str = "desktop-port";
const PENDING: &str = "desktop-port.pending";

pub(crate) struct DesktopPort {
    directory: Directory,
    pub(crate) port: Option<NonZeroU16>,
}

impl DesktopPort {
    // The manager serializes calls through publication, and the Host owns state.
    pub(crate) fn load(state: &Path) -> io::Result<Self> {
        let directory = open_absolute_directory_no_follow(state)?;
        match directory.child_kind(Path::new(PENDING)) {
            Ok(EntryKind::File) => directory.remove_child(Path::new(PENDING))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err(invalid("desktop-port.pending must be a regular file")),
            Err(error) => return Err(error),
        }
        let port = match open_relative_file_no_follow(&directory, Path::new(RECORD)) {
            Ok(file) => {
                let metadata = file.metadata()?;
                if !metadata.is_file() || metadata.len() != 2 {
                    return Err(invalid("desktop-port must be a two-byte regular file"));
                }
                let mut bytes = Vec::with_capacity(3);
                file.take(3).read_to_end(&mut bytes)?;
                let bytes: [u8; 2] = bytes
                    .try_into()
                    .map_err(|_| invalid("desktop-port changed while reading"))?;
                Some(
                    NonZeroU16::new(u16::from_be_bytes(bytes))
                        .ok_or_else(|| invalid("desktop-port must be nonzero"))?,
                )
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        // Also completes a publication whose previous directory sync failed.
        directory.sync_all()?;
        Ok(Self { directory, port })
    }

    pub(crate) fn publish(self, port: NonZeroU16) -> io::Result<()> {
        let pending = Path::new(PENDING);
        let record = Path::new(RECORD);
        let mut file = create_relative_private_file_no_follow(&self.directory, pending)?;
        file.write_all(&port.get().to_be_bytes())?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        let namespace = self.directory.pin_namespace()?;
        #[cfg(unix)]
        let namespace = &self.directory;
        namespace.link_child(&self.directory, pending, record)?;
        namespace.sync_all()?;
        namespace.remove_child(pending)?;
        namespace.sync_all()
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
