use crate::{Error, Result, invalid};
use rsi_files_native_fs::{Directory, EntryKind};
use std::{
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct Dir {
    pub path: PathBuf,
    pub directory: Directory,
    #[cfg(windows)]
    pin: rsi_files_native_fs::NamespaceLease,
}
impl Dir {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        let directory = if create {
            rsi_files_native_fs::create_absolute_directory_no_follow(path)?
        } else {
            rsi_files_native_fs::open_absolute_directory_no_follow(path)?
        };
        #[cfg(windows)]
        let directory = if create {
            rsi_files_native_fs::set_private_directory_permissions_no_follow(path)?
        } else {
            directory
        };
        Self::admit(path, directory)
    }
    fn admit(path: &Path, directory: Directory) -> Result<Self> {
        #[cfg(windows)]
        let pin = directory.pin_namespace()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = directory.dir_metadata()?;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0
            {
                return Err(invalid("package directory is not private and owned"));
            }
        }
        Ok(Self {
            path: path.into(),
            directory,
            #[cfg(windows)]
            pin,
        })
    }
    pub fn child(&self, relative: &Path, create: bool) -> Result<Self> {
        let mut directory =
            rsi_files_native_fs::open_relative_directory_no_follow(&self.directory, Path::new(""))?;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(invalid("invalid child directory"));
            };
            directory = match rsi_files_native_fs::open_relative_directory_no_follow(
                &directory,
                Path::new(name),
            ) {
                Ok(child) => child,
                Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                    rsi_files_native_fs::create_directory_no_follow(&directory, name)
                        .map_err(|e| invalid(e.to_string()))?
                }
                Err(error) => return Err(error.into()),
            };
        }
        Self::admit(&self.path.join(relative), directory)
    }
    pub fn read(&self, name: &str, maximum: usize) -> Result<Vec<u8>> {
        let file =
            rsi_files_native_fs::open_relative_file_no_follow(&self.directory, Path::new(name))?;
        if !file.metadata()?.is_file() {
            return Err(invalid("expected regular file"));
        }
        let mut bytes = Vec::new();
        file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > maximum {
            return Err(invalid("document size limit"));
        }
        Ok(bytes)
    }
    pub fn write(&self, name: &str, bytes: &[u8], replace: bool) -> Result<()> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|e| invalid(e.to_string()))?;
        let staging = format!(".write-{}", hex::encode(random));
        let mut file = rsi_files_native_fs::create_relative_private_file_no_follow(
            &self.directory,
            Path::new(&staging),
        )?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        self.rename(&staging, name, EntryKind::File, replace)
    }
    pub fn rename(&self, from: &str, to: &str, kind: EntryKind, replace: bool) -> Result<()> {
        #[cfg(windows)]
        self.pin
            .publish_child(Path::new(from), Path::new(to), kind, replace)?;
        #[cfg(unix)]
        {
            let _ = kind;
            rustix::fs::renameat_with(
                &self.directory,
                from,
                &self.directory,
                to,
                if replace {
                    rustix::fs::RenameFlags::empty()
                } else {
                    rustix::fs::RenameFlags::NOREPLACE
                },
            )?;
        }
        self.directory.sync_all()?;
        Ok(())
    }
    pub fn lock(&self, name: &str, shared: bool) -> Result<File> {
        let file = match rsi_files_native_fs::create_relative_private_file_no_follow(
            &self.directory,
            Path::new(name),
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                #[cfg(windows)]
                {
                    rsi_files_native_fs::open_relative_file_read_write_no_follow(
                        &self.directory,
                        Path::new(name),
                    )?
                }
                #[cfg(unix)]
                {
                    rsi_files_native_fs::open_relative_file_no_follow(
                        &self.directory,
                        Path::new(name),
                    )?
                }
            }
            Err(error) => return Err(error.into()),
        };
        if !file.metadata()?.is_file() || file.metadata()?.len() != 0 {
            return Err(invalid("invalid lease file"));
        }
        #[cfg(windows)]
        rsi_files_native_fs::verify_private_file(&file)?;
        #[cfg(unix)]
        let file = if shared {
            drop(file);
            rsi_files_native_fs::open_relative_file_no_follow(&self.directory, Path::new(name))?
        } else {
            file
        };
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        result.map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => Error::Busy,
            std::fs::TryLockError::Error(error) => Error::Io(error),
        })?;
        Ok(file)
    }
    pub fn names(&self, maximum: usize) -> Result<Vec<(String, EntryKind)>> {
        let mut names = Vec::new();
        for entry in self.directory.entries()? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".rsi-namespace-pin" {
                continue;
            }
            if names.len() == maximum {
                return Err(invalid("directory entry capacity"));
            }
            names.push((name, entry.file_type()?));
        }
        Ok(names)
    }
}
