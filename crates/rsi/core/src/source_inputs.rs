//! Shared bounded input capture for explicitly trusted local build recipes.
use rsi_files_native_fs::{
    Directory, open_relative_directory_no_follow, open_relative_file_no_follow,
};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet,
    io::{self, Read as _},
    path::{Component, Path, PathBuf},
};

pub(crate) const MAX_ENTRIES: usize = 1024;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("source input limit: {0}")]
    Capacity(&'static str),
    #[error("source inputs changed during capture")]
    Changed,
    #[error(transparent)]
    Io(#[from] io::Error),
}
type Result<T> = std::result::Result<T, InputError>;
fn invalid(message: &'static str) -> InputError {
    InputError::Invalid(message)
}

pub(crate) fn relative(path: &Path) -> Result<()> {
    let components: Vec<_> = path.components().collect();
    if components.is_empty()
        || components.len() > 32
        || path.as_os_str().len() > 4096
        || components
            .iter()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(invalid(
            "build input must be a bounded confined relative path",
        ));
    }
    Ok(())
}

pub(crate) fn layout(output: &Path, inputs: &[PathBuf]) -> Result<()> {
    relative(output)?;
    let directory = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .filter(|_| output.file_name() == Some(std::ffi::OsStr::new("package.json")))
        .ok_or_else(|| invalid("source output needs a dedicated package.json directory"))?;
    if inputs.is_empty() || inputs.len() > 256 {
        return Err(invalid("source inputs exceed bounds"));
    }
    for input in inputs {
        relative(input)?;
        if path_key(input).starts_with(path_key(directory))
            || path_key(directory).starts_with(path_key(input))
        {
            return Err(invalid("source output overlaps observed inputs"));
        }
    }
    Ok(())
}
pub(crate) fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_uppercase())
    }
    #[cfg(not(windows))]
    {
        path.to_owned()
    }
}

/// Reads only through the retained directory authority, with finite traversal and bytes.
pub(crate) fn fingerprint(root: &Directory, inputs: &[PathBuf]) -> Result<String> {
    capture(root, inputs, true)
}
/// Cheap observation hint only; publication always requires a full fingerprint.
#[cfg(any(test, target_os = "linux", windows))]
pub(crate) fn metadata_fingerprint(root: &Directory, inputs: &[PathBuf]) -> Result<String> {
    capture(root, inputs, false)
}
fn capture(root: &Directory, inputs: &[PathBuf], content: bool) -> Result<String> {
    if inputs.len() > MAX_ENTRIES {
        return Err(InputError::Capacity("build input entries"));
    }
    let mut pending = inputs.to_vec();
    pending.sort();
    let mut visited = BTreeSet::new();
    let mut hash = Sha256::new();
    hash.update(b"rsi.source-inputs/v1\0");
    let mut total = 0;
    let mut buffer = vec![0; if content { 64 * 1024 } else { 0 }].into_boxed_slice();
    while let Some(path) = pending.pop() {
        relative(&path)?;
        if !visited.insert(path.clone()) {
            continue;
        }
        if visited.len() > MAX_ENTRIES {
            return Err(InputError::Capacity("build input entries"));
        }
        let spelling = path.as_os_str().as_encoded_bytes();
        hash.update((spelling.len() as u64).to_le_bytes());
        hash.update(spelling);
        match open_relative_directory_no_follow(root, &path) {
            Ok(directory) => {
                hash.update(b"directory");
                let before = directory_stamp(&directory)?;
                if !content {
                    hash.update(format!("{before:?}"));
                }
                let mut children = Vec::new();
                for child in directory.entries()? {
                    if visited.len() + pending.len() + children.len() >= MAX_ENTRIES {
                        return Err(InputError::Capacity("build input entries"));
                    }
                    let child = child?;
                    if !matches!(
                        child.file_type()?,
                        rsi_files_native_fs::EntryKind::File
                            | rsi_files_native_fs::EntryKind::Directory
                    ) {
                        return Err(invalid("regular build inputs required"));
                    }
                    children.push(path.join(child.file_name()));
                }
                if directory_stamp(&directory)? != before {
                    return Err(InputError::Changed);
                }
                children.sort();
                pending.extend(children);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                hash.update(b"missing");
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => {}
            Err(error) => return Err(error.into()),
        }
        let file = open_relative_file_no_follow(root, &path)?;
        let before = file_stamp(&file)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("regular build inputs required"));
        }
        let maximum = MAX_FILE_BYTES.min(MAX_TOTAL_BYTES - total);
        if metadata.len() > maximum {
            return Err(InputError::Capacity("build input bytes"));
        }
        hash.update(b"file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            hash.update(metadata.mode().to_le_bytes());
        }
        if !content {
            hash.update(format!("{before:?}"));
            total += metadata.len();
            continue;
        }
        let mut reader = (&file).take(maximum + 1);
        let mut content = Sha256::new();
        let mut count = 0;
        loop {
            let length = reader.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            count += length as u64;
            if count > maximum {
                return Err(InputError::Capacity("build input bytes"));
            }
            content.update(&buffer[..length]);
        }
        if file_stamp(&file)? != before {
            return Err(InputError::Changed);
        }
        total += count;
        hash.update(count.to_le_bytes());
        hash.update(content.finalize());
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(unix)]
type Stamp = (u64, u64, u64, i64, i64, i64, i64, u32);
#[cfg(windows)]
type Stamp = rsi_files_native_fs::FileVersion;
#[cfg(unix)]
fn metadata_stamp(metadata: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt as _;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.mode(),
    )
}
fn file_stamp(file: &std::fs::File) -> io::Result<Stamp> {
    #[cfg(unix)]
    {
        Ok(metadata_stamp(&file.metadata()?))
    }
    #[cfg(windows)]
    {
        rsi_files_native_fs::file_version(file)
    }
}
fn directory_stamp(directory: &Directory) -> io::Result<Stamp> {
    #[cfg(unix)]
    {
        Ok(metadata_stamp(&directory.dir_metadata()?))
    }
    #[cfg(windows)]
    {
        directory.version()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_tree_tracks_missing_added_and_changed_files_and_rejects_escape() {
        let temporary = tempfile::tempdir().unwrap();
        let path =
            rsi_files_native_fs::resolve_absolute_root_alias(temporary.path(), false).unwrap();
        std::fs::create_dir(path.join("tree")).unwrap();
        std::fs::write(path.join("tree/input"), "first").unwrap();
        let root = rsi_files_native_fs::open_absolute_directory_no_follow(&path).unwrap();
        let inputs = vec![PathBuf::from("tree"), PathBuf::from("missing")];
        let first_hint = metadata_fingerprint(&root, &inputs).unwrap();
        assert_eq!(first_hint, metadata_fingerprint(&root, &inputs).unwrap());
        let first = fingerprint(&root, &inputs).unwrap();
        assert_eq!(first, fingerprint(&root, &inputs).unwrap());
        std::fs::write(path.join("tree/input"), "other").unwrap();
        assert_ne!(first_hint, metadata_fingerprint(&root, &inputs).unwrap());
        let edited = fingerprint(&root, &inputs).unwrap();
        assert_ne!(first, edited);
        std::fs::write(path.join("missing"), "added").unwrap();
        assert_ne!(edited, fingerprint(&root, &inputs).unwrap());
        assert!(fingerprint(&root, &[PathBuf::from("../outside")]).is_err());
        let file = std::fs::File::create(path.join("too-large")).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(fingerprint(&root, &[PathBuf::from("too-large")]).is_err());
    }
}
