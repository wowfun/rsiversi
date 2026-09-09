use super::{
    MAXIMUM_NATIVE_ADDON_STATE_BYTES, NativeAddonError, NativeAddonStoreLimits, Result, State,
    source,
};
use rustix::fs::{AtFlags, Mode, OFlags, openat};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{Read as _, Write as _},
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::PathBuf,
};

#[derive(Debug)]
pub(super) struct StoreDirectory {
    path: PathBuf,
    root: File,
    objects: File,
}
impl StoreDirectory {
    pub(super) fn open(path: PathBuf) -> Result<Self> {
        validate_root_path(&path)?;
        let root = rsi_files_native_fs::create_absolute_directory_no_follow(&path)?.into_std_file();
        Self::from_root(path, root, true)
    }
    pub(super) fn open_existing(path: PathBuf) -> Result<Option<Self>> {
        validate_root_path(&path)?;
        let root = match rsi_files_native_fs::open_absolute_directory_no_follow(&path) {
            Ok(root) => root.into_std_file(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Self::from_root(path, root, false).map(Some)
    }
    fn from_root(path: PathBuf, root: File, create: bool) -> Result<Self> {
        owned(&root)?;
        if create {
            match rustix::fs::mkdirat(&root, "objects", Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let objects = directory_at(&root, "objects")?;
        owned(&objects)?;
        Ok(Self {
            path,
            root,
            objects,
        })
    }
    pub(super) fn check(&self) -> Result<()> {
        owned(&self.root)?;
        owned(&self.objects)?;
        let current =
            rsi_files_native_fs::open_absolute_directory_no_follow(&self.path)?.into_std_file();
        if identity(&current)? != identity(&self.root)?
            || identity(&directory_at(&self.root, "objects")?)? != identity(&self.objects)?
        {
            return Err(NativeAddonError::Conflict);
        }
        Ok(())
    }
    pub(super) fn object_path(&self, digest: &str) -> Result<PathBuf> {
        self.check()?;
        owned(&regular_at(&self.objects, digest)?)?;
        Ok(self.path.join("objects").join(digest))
    }
    pub(super) fn lock(&self) -> Result<File> {
        self.check()?;
        let lock = directory_at(&self.root, ".")?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => NativeAddonError::Busy,
            std::fs::TryLockError::Error(error) => NativeAddonError::Io(error),
        })?;
        self.check()?;
        self.reclaim_index_stages()?;
        Ok(lock)
    }
    fn reclaim_index_stages(&self) -> Result<()> {
        let mut entries = 0_usize;
        for entry in rustix::fs::Dir::read_from(&self.root)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| NativeAddonError::Invalid("store filename"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            entries += 1;
            if entries > 18 {
                return Err(NativeAddonError::Capacity("store directory entries"));
            }
            if matches!(name, "objects" | "state.json") {
                continue;
            }
            reclaim_stage(&self.root, name, MAXIMUM_NATIVE_ADDON_STATE_BYTES as u64)?;
        }
        Ok(())
    }
    pub(super) fn read_index(&self) -> Result<Option<Vec<u8>>> {
        let file = match regular_at(&self.root, "state.json") {
            Ok(file) => file,
            Err(NativeAddonError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        owned(&file)?;
        source::bounded_read(file, MAXIMUM_NATIVE_ADDON_STATE_BYTES).map(Some)
    }
    pub(super) fn install_object(
        &self,
        source: File,
        limits: NativeAddonStoreLimits,
    ) -> Result<String> {
        self.check()?;
        let (objects, bytes) = self.object_usage(limits)?;
        let maximum = limits
            .maximum_object_bytes
            .min(rsi_meta_native_loader::MAX_ARTIFACT_BYTES);
        if source.metadata()?.len() > maximum {
            return Err(NativeAddonError::Capacity("artifact bytes"));
        }
        let mut stage = Staged::new(&self.objects)?;
        let mut reader = source.take(maximum + 1);
        let mut buffer = vec![0; 64 * 1024];
        let mut hash = Sha256::new();
        let mut copied = 0_u64;
        loop {
            let length = reader.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            copied += length as u64;
            if copied > maximum {
                return Err(NativeAddonError::Capacity("artifact bytes"));
            }
            hash.update(&buffer[..length]);
            stage.file.write_all(&buffer[..length])?;
        }
        let digest = hex::encode(hash.finalize());
        stage
            .file
            .set_permissions(std::fs::Permissions::from_mode(0o400))?;
        stage.file.sync_all()?;
        match regular_at(&self.objects, &digest) {
            Ok(existing) => {
                verify_object(existing, &digest, copied)?;
                return Ok(digest);
            }
            Err(NativeAddonError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if objects >= limits.maximum_objects
            || copied > limits.maximum_object_bytes.saturating_sub(bytes)
        {
            return Err(NativeAddonError::Capacity("source objects"));
        }
        self.check()?;
        match rustix::fs::linkat(
            &self.objects,
            &stage.name,
            &self.objects,
            &digest,
            AtFlags::empty(),
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {
                verify_object(regular_at(&self.objects, &digest)?, &digest, copied)?;
            }
            Err(error) => return Err(error.into()),
        }
        self.objects.sync_all()?;
        Ok(digest)
    }
    fn object_usage(&self, limits: NativeAddonStoreLimits) -> Result<(usize, u64)> {
        let mut objects = 0_usize;
        let mut bytes = 0_u64;
        let mut entries = 0_usize;
        for entry in rustix::fs::Dir::read_from(&self.objects)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| NativeAddonError::Invalid("object filename"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            entries += 1;
            if entries > super::MAXIMUM_NATIVE_ADDON_OBJECTS + 16 {
                return Err(NativeAddonError::Capacity("object directory entries"));
            }
            if staged_name(name) {
                reclaim_stage(
                    &self.objects,
                    name,
                    rsi_meta_native_loader::MAX_ARTIFACT_BYTES,
                )?;
                continue;
            }
            if !source::digest(name) {
                return Err(NativeAddonError::Invalid("unmanaged object"));
            }
            let file = regular_at(&self.objects, name)?;
            owned(&file)?;
            let length = file.metadata()?.len();
            if length > rsi_meta_native_loader::MAX_ARTIFACT_BYTES {
                return Err(NativeAddonError::Capacity("artifact bytes"));
            }
            objects += 1;
            bytes = bytes
                .checked_add(length)
                .ok_or(NativeAddonError::Capacity("object bytes"))?;
            if objects > limits.maximum_objects || bytes > limits.maximum_object_bytes {
                return Err(NativeAddonError::Capacity("source objects"));
            }
        }
        Ok((objects, bytes))
    }
    pub(super) fn publish_index(&self, bytes: &[u8]) -> Result<bool> {
        let mut stage = Staged::new(&self.root)?;
        stage.file.write_all(bytes)?;
        stage.file.sync_all()?;
        self.check()?;
        rustix::fs::renameat(&self.root, &stage.name, &self.root, "state.json")?;
        stage.published = true;
        Ok(self.root.sync_all().is_ok())
    }
}
fn reclaim_stage(directory: &File, name: &str, maximum: u64) -> Result<()> {
    // The independent writer lock excludes live cooperative staging in this
    // dedicated namespace. Never follow, truncate or remove unmanaged entries.
    if !staged_name(name) {
        return Err(NativeAddonError::Invalid("unmanaged store entry"));
    }
    let file = regular_at(directory, name)?;
    owned(&file)?;
    if file.metadata()?.len() > maximum {
        return Err(NativeAddonError::Capacity("abandoned staging bytes"));
    }
    rustix::fs::unlinkat(directory, name, AtFlags::empty())?;
    Ok(())
}
fn owned(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
        return Err(NativeAddonError::Invalid(
            "store must be owned by this user without group/other write access",
        ));
    }
    Ok(())
}
fn identity(file: &File) -> Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
fn directory_at(parent: &File, name: &str) -> Result<File> {
    Ok(openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?
    .into())
}
fn regular_at(parent: &File, name: &str) -> Result<File> {
    let file = File::from(openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?);
    if !file.metadata()?.is_file() {
        return Err(NativeAddonError::Invalid("regular store file required"));
    }
    Ok(file)
}
fn verify_object(file: File, expected: &str, bytes: u64) -> Result<()> {
    owned(&file)?;
    if file.metadata()?.len() != bytes {
        return Err(NativeAddonError::DigestMismatch);
    }
    let mut reader = file.take(bytes + 1);
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut read = 0_u64;
    loop {
        let length = reader.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        read += length as u64;
        hash.update(&buffer[..length]);
    }
    if read != bytes || hex::encode(hash.finalize()) != expected {
        return Err(NativeAddonError::DigestMismatch);
    }
    Ok(())
}
fn staged_name(name: &str) -> bool {
    name.strip_prefix(".rsi-addon-")
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|name| {
            name.len() == 32
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
}
struct Staged<'a> {
    directory: &'a File,
    name: String,
    file: File,
    published: bool,
}
impl<'a> Staged<'a> {
    fn new(directory: &'a File) -> Result<Self> {
        let mut random = [0; 16];
        getrandom::fill(&mut random)
            .map_err(|_| NativeAddonError::Invalid("temporary entropy unavailable"))?;
        let name = format!(".rsi-addon-{}.tmp", hex::encode(random));
        let file = openat(
            directory,
            &name,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )?
        .into();
        Ok(Self {
            directory,
            name,
            file,
            published: false,
        })
    }
}
impl Drop for Staged<'_> {
    fn drop(&mut self) {
        if !self.published {
            let _ = rustix::fs::unlinkat(self.directory, &self.name, AtFlags::empty());
        }
    }
}
pub(super) fn encode_index(state: &State) -> Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAXIMUM_NATIVE_ADDON_STATE_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("native addon state byte bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoded = Bounded(Vec::new());
    serde_json::to_writer(&mut encoded, state)
        .map_err(|_| NativeAddonError::Capacity("state bytes"))?;
    Ok(encoded.0)
}

fn validate_root_path(path: &std::path::Path) -> Result<()> {
    if path.as_os_str().len() > 4096 || path.components().count() > 64 {
        return Err(NativeAddonError::Invalid("store path bounds"));
    }
    Ok(())
}
