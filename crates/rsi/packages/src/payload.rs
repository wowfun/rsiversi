use crate::{Error, ExportKind, FileRecord, Manifest, Result, filesystem::Dir, invalid};
use rsi_files_native_fs::EntryKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub(crate) const MAX_PAYLOAD_ENTRIES: usize = 65_536;
const MAX_HEADER: usize = 16 * 1024 * 1024;
/// Portable uncompressed file tree. File order is the payload byte order.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadHeader {
    /// Current container format is one.
    pub format: u32,
    /// Exact regular-file identities; offsets follow preceding file lengths.
    pub files: Vec<FileRecord>,
}
impl PayloadHeader {
    /// Checks path, count, cross-file and aggregate-byte bounds.
    pub fn validate(&self) -> Result<()> {
        self.entries().map(drop)
    }

    fn entries(&self) -> Result<HashMap<&str, EntryKind>> {
        if self.format != 1 || self.files.is_empty() || self.files.len() > 16_384 {
            return Err(invalid("payload format or count"));
        }
        let mut entries = HashMap::with_capacity(self.files.len());
        let mut total = 0_u64;
        for file in &self.files {
            crate::manifest::relative_payload(&file.path)?;
            crate::hash(&file.digest)?;
            if entries
                .insert(file.path.as_str(), EntryKind::File)
                .is_some()
                || file.bytes > crate::MAX_FILE_BYTES
            {
                return Err(invalid("payload duplicate or size"));
            }
            total = total
                .checked_add(file.bytes)
                .ok_or_else(|| invalid("payload size overflow"))?;
            for (end, _) in file.path.rmatch_indices('/') {
                match entries.insert(&file.path[..end], EntryKind::Directory) {
                    Some(EntryKind::Directory) => break,
                    Some(_) => return Err(invalid("payload file-directory collision")),
                    None => {}
                }
            }
            if entries.len() > MAX_PAYLOAD_ENTRIES || total > crate::MAX_FILE_BYTES {
                return Err(invalid("payload tree bounds"));
            }
        }
        let mut folded = HashSet::with_capacity(entries.len());
        if entries
            .keys()
            .any(|path| !folded.insert(path.to_ascii_lowercase()))
        {
            return Err(invalid("payload path case collision"));
        }
        Ok(entries)
    }
}
fn read_header(input: &mut File) -> Result<PayloadHeader> {
    let mut prefix = [0; 12];
    input.read_exact(&mut prefix)?;
    if &prefix[..8] != b"RSITREE1" {
        return Err(invalid("payload magic"));
    }
    let length = u32::from_le_bytes(prefix[8..].try_into().expect("four bytes")) as usize;
    if length > MAX_HEADER {
        return Err(invalid("payload header capacity"));
    }
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes)?;
    let header: PayloadHeader =
        serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
    let expected = header
        .files
        .iter()
        .try_fold(12 + length as u64, |sum, file| sum.checked_add(file.bytes))
        .ok_or_else(|| invalid("payload size overflow"))?;
    if expected != input.metadata()?.len() {
        return Err(Error::Digest);
    }
    Ok(header)
}
pub(crate) fn reserved_bytes(manifest: &Manifest) -> u64 {
    manifest
        .files
        .iter()
        .map(|file| {
            file.bytes
                * if manifest.exports.values().any(|export| {
                    export.kind == ExportKind::PayloadTree && export.path == file.path
                }) {
                    2
                } else {
                    1
                }
        })
        .sum()
}
pub(crate) fn process(object: &Dir, manifest: &Manifest, expand: bool) -> Result<()> {
    let mut aggregate = 0_usize;
    for (name, export) in &manifest.exports {
        if export.kind != ExportKind::PayloadTree {
            continue;
        }
        let mut input = rsi_files_native_fs::open_relative_file_no_follow(
            &object.directory,
            &Path::new("content").join(&export.path),
        )?;
        let header = read_header(&mut input)?;
        let mut expected = header.entries()?;
        aggregate = aggregate
            .checked_add(expected.len())
            .ok_or_else(|| invalid("payload entry overflow"))?;
        if aggregate > MAX_PAYLOAD_ENTRIES {
            return Err(invalid("package payload entry capacity"));
        }
        let tree = object.child(&Path::new("payloads").join(name), expand)?;
        if expand {
            process_files(&tree, &header.files, &mut input, true)?;
        } else {
            verify_files(
                object,
                &Path::new("content").join(&export.path),
                &tree,
                &header.files,
                &mut input,
            )?;
            verify_names(&tree, "", &mut expected)?;
            if !expected.is_empty() {
                return Err(Error::Digest);
            }
        }
    }
    Ok(())
}
// Reader reservations bound total parallelism even if several stores acquire concurrently.
static FREE_READERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(7);

struct Readers(usize);
impl Drop for Readers {
    fn drop(&mut self) {
        FREE_READERS.fetch_add(self.0, std::sync::atomic::Ordering::Relaxed);
    }
}
fn verify_files(
    object: &Dir,
    archive: &Path,
    tree: &Dir,
    files: &[FileRecord],
    input: &mut File,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    let wanted = files.len().div_ceil(512).saturating_sub(1).min(7);
    let previous = FREE_READERS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |free| {
            Some(free.saturating_sub(wanted))
        })
        .expect("reservation always returns an update");
    let readers = Readers(wanted.min(previous));
    if readers.0 == 0 {
        return process_files(tree, files, input, false);
    }
    let mut offset = input.stream_position()?;
    let count = files.len().div_ceil(readers.0 + 1);
    std::thread::scope(|scope| -> Result<()> {
        let mut ranges = files.chunks(count);
        let first = ranges.next().expect("validated nonempty tree");
        offset += first.iter().map(|file| file.bytes).sum::<u64>();
        let mut workers = Vec::new();
        for range in ranges {
            let position = offset;
            offset += range.iter().map(|file| file.bytes).sum::<u64>();
            workers.push(
                std::thread::Builder::new()
                    .name("rsi-payload-read".into())
                    .spawn_scoped(scope, move || {
                        let mut archive = rsi_files_native_fs::open_relative_file_no_follow(
                            &object.directory,
                            archive,
                        )?;
                        archive.seek(SeekFrom::Start(position))?;
                        process_files(tree, range, &mut archive, false)
                    })?,
            );
        }
        let result = process_files(tree, first, input, false);
        for worker in workers {
            worker
                .join()
                .map_err(|_| invalid("payload reader failed"))??;
        }
        result
    })
}
fn process_files(tree: &Dir, files: &[FileRecord], input: &mut File, expand: bool) -> Result<()> {
    let mut disk = vec![0_u8; if expand { 0 } else { 64 * 1024 }];
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut ancestors: Vec<(&str, Dir)> = Vec::new();
    for file in files {
        let path = Path::new(&file.path);
        let parts: Vec<_> = file.path.split('/').collect();
        let parents = &parts[..parts.len() - 1];
        let common = ancestors
            .iter()
            .zip(parents)
            .take_while(|((name, _), next)| name == *next)
            .count();
        ancestors.truncate(common);
        for component in &parents[common..] {
            let parent = ancestors.last().map_or(tree, |(_, directory)| directory);
            let directory = parent.child(Path::new(component), expand)?;
            ancestors.push((component, directory));
        }
        let parent = ancestors.last().map_or(tree, |(_, directory)| directory);
        let name = Path::new(path.file_name().expect("validated file"));
        let mut output = if expand {
            Some(rsi_files_native_fs::create_relative_private_file_no_follow(
                &parent.directory,
                name,
            )?)
        } else {
            None
        };
        let mut existing = if expand {
            None
        } else {
            Some(rsi_files_native_fs::open_relative_file_no_follow(
                &parent.directory,
                name,
            )?)
        };
        if existing.as_ref().is_some_and(|file_on_disk| {
            file_on_disk
                .metadata()
                .map_or(true, |meta| meta.len() != file.bytes)
        }) {
            return Err(Error::Digest);
        }
        let mut remaining = file.bytes;
        let mut digest = Sha256::new();

        while remaining > 0 {
            let size = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| invalid("payload read size"))?;
            input.read_exact(&mut buffer[..size])?;
            digest.update(&buffer[..size]);
            if let Some(output) = &mut output {
                output.write_all(&buffer[..size])?;
            }
            if let Some(existing) = &mut existing {
                existing.read_exact(&mut disk[..size])?;
                if disk[..size] != buffer[..size] {
                    return Err(Error::Digest);
                }
            }
            remaining -= size as u64;
        }
        if hex::encode(digest.finalize()) != file.digest {
            return Err(Error::Digest);
        }
        if let Some(output) = output {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                output.set_permissions(std::fs::Permissions::from_mode(if file.executable {
                    0o500
                } else {
                    0o400
                }))?;
            }
            output.sync_all()?;
        }
    }
    Ok(())
}
fn verify_names(
    directory: &Dir,
    prefix: &str,
    expected: &mut HashMap<&str, EntryKind>,
) -> Result<()> {
    for (name, kind) in directory.names(MAX_PAYLOAD_ENTRIES)? {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let declared = expected
            .remove(path.as_str())
            .ok_or_else(|| invalid("undeclared payload entry"))?;
        if kind != declared {
            return Err(invalid("payload entry type"));
        }
        if kind == EntryKind::Directory {
            verify_names(&directory.child(Path::new(&name), false)?, &path, expected)?;
        }
    }
    Ok(())
}
