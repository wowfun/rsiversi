mod verify;
use crate::{Error, FileRecord, Manifest, Result, filesystem::Dir, invalid};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufReader, Read, Write},
    path::Path,
};
pub(crate) use verify::verify;

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
enum Entry<'a> {
    File(&'a FileRecord),
    Directory,
}

type Entries<'a> = HashMap<&'a str, Entry<'a>>;

impl PayloadHeader {
    /// Checks path, count, cross-file and aggregate-byte bounds.
    pub fn validate(&self) -> Result<()> {
        let mut remaining = MAX_PAYLOAD_ENTRIES;
        self.entries(&mut remaining).map(drop)
    }

    fn entries(&self, remaining: &mut usize) -> Result<Entries<'_>> {
        if self.format != 1 || self.files.is_empty() || self.files.len() > 16_384 {
            return Err(invalid("payload format or count"));
        }
        let mut entries = HashMap::with_capacity(self.files.len());
        let mut total = 0_u64;
        for file in &self.files {
            crate::manifest::relative_payload(&file.path)?;
            crate::hash(&file.digest)?;
            if entries
                .insert(file.path.as_str(), Entry::File(file))
                .is_some()
                || file.bytes > crate::MAX_FILE_BYTES
            {
                return Err(invalid("payload duplicate or size"));
            }
            total = total
                .checked_add(file.bytes)
                .ok_or_else(|| invalid("payload size overflow"))?;
            for (end, _) in file.path.rmatch_indices('/') {
                match entries.insert(&file.path[..end], Entry::Directory) {
                    Some(Entry::Directory) => break,
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
        *remaining = remaining
            .checked_sub(entries.len())
            .ok_or_else(|| invalid("package payload entry capacity"))?;
        Ok(entries)
    }
}
fn read_header(input: &mut impl Read, archive_bytes: u64) -> Result<(PayloadHeader, Vec<u8>)> {
    let mut prefix = [0; 12];
    input.read_exact(&mut prefix)?;
    if &prefix[..8] != b"RSITREE1" {
        return Err(invalid("payload magic"));
    }
    let length = u32::from_le_bytes(prefix[8..].try_into().expect("four bytes")) as usize;
    if length > MAX_HEADER {
        return Err(invalid("payload header capacity"));
    }
    let mut bytes = vec![0; 12 + length];
    bytes[..12].copy_from_slice(&prefix);
    input.read_exact(&mut bytes[12..])?;
    let header: PayloadHeader =
        serde_json::from_slice(&bytes[12..]).map_err(|e| invalid(e.to_string()))?;
    let expected = header
        .files
        .iter()
        .try_fold(12 + length as u64, |sum, file| sum.checked_add(file.bytes))
        .ok_or_else(|| invalid("payload size overflow"))?;
    if expected != archive_bytes {
        return Err(Error::Digest);
    }
    Ok((header, bytes))
}
pub(crate) fn reserved_bytes(manifest: &Manifest) -> u64 {
    manifest.files.iter().map(|file| file.bytes).sum()
}

struct Archive {
    input: BufReader<File>,
    digest: Sha256,
}
impl Read for Archive {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
}

pub(crate) fn expand(
    object: &Dir,
    name: &str,
    record: &FileRecord,
    index_digest: &str,
    input: File,
    mut output: File,
    remaining: &mut usize,
) -> Result<()> {
    if input.metadata()?.len() != record.bytes {
        return Err(Error::Digest);
    }
    let mut input = Archive {
        input: BufReader::with_capacity(256 * 1024, input),
        digest: Sha256::new(),
    };
    let (header, index) = read_header(&mut input, record.bytes)?;
    if hex::encode(Sha256::digest(&index)) != index_digest {
        return Err(Error::Digest);
    }
    header.entries(remaining)?;
    output.write_all(&index)?;
    let tree = object.child(&Path::new("payloads").join(name), true)?;
    expand_files(&tree, &header.files, &mut input)?;
    if input.read(&mut [0])? != 0 || hex::encode(input.digest.finalize()) != record.digest {
        return Err(Error::Digest);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        output.set_permissions(std::fs::Permissions::from_mode(0o400))?;
    }
    output.sync_all()?;
    Ok(())
}

fn read_record(
    input: &mut impl Read,
    file: &FileRecord,
    buffer: &mut [u8],
    mut output: impl Write,
) -> Result<()> {
    let mut remaining = file.bytes;
    let mut digest = Sha256::new();
    while remaining > 0 {
        let size = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        input.read_exact(&mut buffer[..size])?;
        digest.update(&buffer[..size]);
        output.write_all(&buffer[..size])?;
        remaining -= size as u64;
    }
    if hex::encode(digest.finalize()) != file.digest {
        return Err(Error::Digest);
    }
    Ok(())
}

fn expand_files(tree: &Dir, files: &[FileRecord], input: &mut impl Read) -> Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    let mut ancestors: Vec<(&str, Dir)> = Vec::new();
    for file in files {
        let (parents, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
        let components = parents.split_terminator('/');
        let common = ancestors
            .iter()
            .zip(components.clone())
            .take_while(|((known, _), current)| known == current)
            .count();
        ancestors.truncate(common);
        for component in components.skip(common) {
            let parent = ancestors.last().map_or(tree, |(_, directory)| directory);
            ancestors.push((component, parent.child(Path::new(component), true)?));
        }
        let parent = ancestors.last().map_or(tree, |(_, directory)| directory);
        let mut output = rsi_files_native_fs::create_relative_private_file_no_follow(
            &parent.directory,
            Path::new(name),
        )?;
        read_record(input, file, &mut buffer, &mut output)?;
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
    Ok(())
}
