use super::{Entries, Entry, read_header, read_record, workers};
use crate::{Error, FileRecord, Result, filesystem::Dir, invalid};
use rsi_files_native_fs::EntryKind;
use sha2::{Digest as _, Sha256};
use std::{path::Path, sync::Arc};

pub(crate) fn verify(
    object: &Dir,
    name: &str,
    record: &FileRecord,
    index_digest: &str,
    remaining: &mut usize,
) -> Result<()> {
    let mut file = rsi_files_native_fs::open_relative_file_no_follow(
        &object.directory,
        &Path::new("content").join(&record.path),
    )?;
    let size = file.metadata()?.len();
    if size > (super::MAX_HEADER + 12) as u64 {
        return Err(invalid("retained payload index capacity"));
    }
    let (header, index) = read_header(&mut file, record.bytes)?;
    if size != index.len() as u64 || hex::encode(Sha256::digest(&index)) != index_digest {
        return Err(Error::Digest);
    }
    let mut expected = header.entries(remaining)?;
    let root = Arc::new(object.child(&Path::new("payloads").join(name), false)?);
    workers::run(
        header.files.len(),
        7,
        || {
            let mut buffer = vec![0; 64 * 1024];
            move |(parent, files): Batch<'_>| verify_files(&parent, &files, &mut buffer)
        },
        |send| walk(root, "", &mut expected, send),
    )?;
    if !expected.is_empty() {
        return Err(Error::Digest);
    }
    Ok(())
}

// A batch retains one parent until every file in it has been checked.
type Batch<'a> = (Arc<Dir>, Vec<&'a FileRecord>);

fn walk<'a>(
    directory: Arc<Dir>,
    prefix: &str,
    expected: &mut Entries<'a>,
    send: &mut dyn FnMut(Batch<'a>) -> Result<()>,
) -> Result<()> {
    let mut files = Vec::new();
    for (name, kind) in directory.names(super::MAX_PAYLOAD_ENTRIES)? {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match (kind, expected.remove(path.as_str())) {
            (EntryKind::Directory, Some(Entry::Directory)) => {
                let child = directory.child(Path::new(&name), false)?;
                walk(Arc::new(child), &path, expected, send)?;
            }
            (EntryKind::File, Some(Entry::File(file))) => {
                files.push(file);
                if files.len() == 32 {
                    send((directory.clone(), std::mem::take(&mut files)))?;
                }
            }
            _ => return Err(invalid("undeclared or substituted payload entry")),
        }
    }
    if !files.is_empty() {
        send((directory, files))?;
    }
    Ok(())
}

fn verify_files(parent: &Dir, files: &[&FileRecord], buffer: &mut [u8]) -> Result<()> {
    for file in files {
        let name = Path::new(&file.path).file_name().expect("validated file");
        let mut input =
            rsi_files_native_fs::open_relative_file_no_follow(&parent.directory, Path::new(name))?;
        if input.metadata()?.len() != file.bytes {
            return Err(Error::Digest);
        }
        read_record(&mut input, file, buffer, std::io::sink())?;
    }
    Ok(())
}
