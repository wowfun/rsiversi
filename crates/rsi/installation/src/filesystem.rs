//! Bounded reads and same-directory publication through retained handles.
use crate::{Result, error};
use rsi_files_native_fs::{self as native, Directory, EntryKind};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const DOCUMENT_LIMIT: usize = 64 * 1024;
pub const FILE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
// Shared by every journal version; changing this name would split maintenance authority.
pub const LOCK: &str = ".rsi-core-upgrade.lock";

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                if !clean.pop() {
                    return Err(error("path escapes root"));
                }
            }
            std::path::Component::CurDir => {}
            part => clean.push(part.as_os_str()),
        }
    }
    // Check every component before any canonical spelling lookup, never redirect links.
    let mut existing = None;
    for parent in clean.ancestors() {
        match std::fs::symlink_metadata(parent) {
            Ok(meta) => {
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    meta.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let linked = meta.file_type().is_symlink();
                if linked {
                    return Err(error(format!(
                        "path traverses a link/junction at {}; pass its resolved directory explicitly",
                        parent.display()
                    )));
                }
                if !meta.is_dir() {
                    return Err(error(format!(
                        "installation parent is not a directory: {}",
                        parent.display()
                    )));
                }
                existing.get_or_insert(parent);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    if let Some(parent) = existing {
        #[cfg(windows)]
        let normalized = {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let canonical = std::fs::canonicalize(parent)?;
            let wide: Vec<_> = canonical.as_os_str().encode_wide().collect();
            let plain = wide.strip_prefix(&[92, 92, 63, 92]).unwrap_or(&wide);
            let canonical = PathBuf::from(std::ffi::OsString::from_wide(plain));
            // Canonicalization may correct case, but must not introduce a redirected path.
            if !canonical
                .to_string_lossy()
                .eq_ignore_ascii_case(&parent.to_string_lossy())
            {
                return Err(error(format!(
                    "path spelling or link changed at {}; pass its resolved long-name path explicitly",
                    parent.display()
                )));
            }
            native::open_absolute_directory_no_follow(&canonical)?;
            let remaining = clean.strip_prefix(parent).map_err(error)?;
            if remaining.as_os_str().is_empty() {
                canonical
            } else {
                canonical.join(remaining)
            }
        };
        #[cfg(not(windows))]
        let normalized = {
            native::open_absolute_directory_no_follow(parent)?;
            clean
        };
        return Ok(normalized);
    }
    Ok(clean)
}

pub fn id() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(error)?;
    Ok(hex::encode(bytes))
}
pub fn kind(dir: &Directory, name: &str) -> Result<Option<EntryKind>> {
    match dir.child_kind(Path::new(name)) {
        Ok(kind) => Ok(Some(kind)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
pub fn read(dir: &Directory, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
    match kind(dir, name)? {
        None => return Ok(None),
        Some(EntryKind::File) => {}
        _ => return Err(error(format!("not a regular file: {name}"))),
    }
    let file = native::open_relative_file_no_follow(dir, Path::new(name))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(error(format!("{name} exceeds {limit} bytes")));
    }
    Ok(Some(bytes))
}
pub fn digest_file(mut file: File) -> Result<String> {
    stream(&mut file, None)
}
pub(crate) fn stream(file: &mut File, mut destination: Option<&mut File>) -> Result<String> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > FILE_LIMIT {
        return Err(error("invalid or oversized executable"));
    }
    let mut hash = Sha256::new();
    let mut bytes = vec![0; 65536].into_boxed_slice();
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > FILE_LIMIT {
            return Err(error("file exceeds 4 GiB"));
        }
        hash.update(&bytes[..count]);
        if let Some(destination) = destination.as_mut() {
            destination.write_all(&bytes[..count])?;
        }
    }
    if total != metadata.len() {
        return Err(error("file changed while reading"));
    }
    Ok(hex::encode(hash.finalize()))
}
pub fn digest(dir: &Directory, name: &str) -> Result<Option<String>> {
    match kind(dir, name)? {
        None => Ok(None),
        Some(EntryKind::File) => {
            digest_file(native::open_relative_file_no_follow(dir, Path::new(name))?).map(Some)
        }
        _ => Err(error(format!("not a regular installation file: {name}"))),
    }
}
pub fn source(path: &Path) -> Result<File> {
    let path = std::path::absolute(path)?;
    let parent = native::open_absolute_directory_no_follow(
        path.parent()
            .ok_or_else(|| error("missing source parent"))?,
    )?;
    let file = native::open_relative_file_no_follow(
        &parent,
        Path::new(
            path.file_name()
                .ok_or_else(|| error("missing source name"))?,
        ),
    )?;
    if !file.metadata()?.is_file() {
        return Err(error("source is not an ordinary file"));
    }
    Ok(file)
}
pub fn rename(dir: &Directory, from: &str, to: &str, replace: bool) -> Result<()> {
    #[cfg(windows)]
    {
        native::set_relative_private_file_permissions_no_follow(dir, Path::new(from))?;
        dir.publish_child(Path::new(from), Path::new(to), EntryKind::File, replace)?;
    }
    #[cfg(unix)]
    rustix::fs::renameat_with(
        dir,
        from,
        dir,
        to,
        if replace {
            rustix::fs::RenameFlags::empty()
        } else {
            rustix::fs::RenameFlags::NOREPLACE
        },
    )?;
    dir.sync_all()
}
pub fn write(dir: &Directory, name: &str, bytes: &[u8], replace: bool) -> Result<()> {
    if bytes.len() > DOCUMENT_LIMIT {
        return Err(error("installation document exceeds 64 KiB"));
    }
    let stage = format!(".rsi-write-{}", id()?);
    let mut file = native::create_relative_private_file_no_follow(dir, Path::new(&stage))?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        rename(dir, &stage, name, replace)
    })();
    if kind(dir, &stage)?.is_some() {
        dir.remove_child(Path::new(&stage))?;
    }
    result
}
pub fn copy(dir: &Directory, name: &str, input: &Path, executable: bool) -> Result<String> {
    let mut input = source(input)?;
    let mut file = native::create_relative_private_file_no_follow(dir, Path::new(name))?;
    let result = (|| {
        let expected = stream(&mut input, Some(&mut file))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(if executable {
                0o700
            } else {
                0o600
            }))?;
        }
        #[cfg(windows)]
        let _ = executable;
        file.sync_all()?;
        Ok(expected)
    })();
    drop(file);
    let result = result.and_then(|expected| {
        if digest(dir, name)?.as_deref() != Some(&expected) {
            return Err(error("source changed while copying"));
        }
        Ok(expected)
    });
    if let Err(failure) = &result {
        dir.remove_child(Path::new(name)).map_err(|cleanup| {
            error(format!(
                "{failure}; owned stage {name} could not be cleaned: {cleanup}"
            ))
        })?;
    }
    result
}

#[derive(Debug)]
pub struct Lease {
    pub directory: Directory,
    _lock: File,
    #[cfg(windows)]
    _namespace: Option<native::NamespaceLease>,
}
impl Lease {
    pub fn acquire(path: &Path, create: bool, private: bool) -> Result<Self> {
        let directory = if create {
            native::create_absolute_directory_no_follow(path)?
        } else {
            native::open_absolute_directory_no_follow(path)?
        };
        #[cfg(windows)]
        let namespace = if private {
            Some(directory.pin_namespace().map_err(|e| error(format!(
                "core upgrade requires a private current-user installation; shared directory ACLs are preserved: {e}"
            )))?)
        } else {
            None
        };
        #[cfg(not(windows))]
        let _ = private;
        let file = match native::create_relative_private_file_no_follow(&directory, Path::new(LOCK))
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                #[cfg(windows)]
                let file =
                    native::open_relative_private_file_no_follow(&directory, Path::new(LOCK))?;
                #[cfg(unix)]
                let file = native::open_relative_file_no_follow(&directory, Path::new(LOCK))?;
                file
            }
            Err(e) => return Err(e),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(error("installation lock must be an empty regular file"));
        }
        file.try_lock().map_err(|_| {
            error(format!(
                "another maintenance operation owns {}",
                path.display()
            ))
        })?;
        Ok(Self {
            directory,
            _lock: file,
            #[cfg(windows)]
            _namespace: namespace,
        })
    }
}
