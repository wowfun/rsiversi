use super::{Entries, Entry, read_header, read_record};
use crate::{Error, FileRecord, Result, filesystem::Dir, invalid};
use rsi_files_native_fs::EntryKind;
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

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

pub(crate) fn verify(
    object: &Dir,
    name: &str,
    record: &FileRecord,
    remaining: &mut usize,
) -> Result<()> {
    let file = rsi_files_native_fs::open_relative_file_no_follow(
        &object.directory,
        &Path::new("content").join(&record.path),
    )?;
    if file.metadata()?.len() != record.bytes {
        return Err(Error::Digest);
    }
    let mut archive = Archive {
        input: BufReader::with_capacity(256 * 1024, file),
        digest: Sha256::new(),
    };
    let header = read_header(&mut archive, record.bytes)?;
    let mut expected = header.entries(remaining)?;
    let mut buffer = vec![0; 64 * 1024];
    for file in &header.files {
        read_record(&mut archive, file, &mut buffer, std::io::sink())?;
    }
    if archive.read(&mut [0])? != 0 || hex::encode(archive.digest.finalize()) != record.digest {
        return Err(Error::Digest);
    }
    let root = Arc::new(object.child(&Path::new("payloads").join(name), false)?);
    let readers = Readers::reserve(header.files.len());
    if readers.0 == 0 {
        walk(root, "", &mut expected, &mut |parent, files| {
            verify_files(&parent, &files, &mut buffer)
        })?;
    } else {
        verify_parallel(root, &mut expected, readers.0, &mut buffer)?;
    }
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
    send: &mut impl FnMut(Arc<Dir>, Vec<&'a FileRecord>) -> Result<()>,
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
                    send(directory.clone(), std::mem::take(&mut files))?;
                }
            }
            _ => return Err(invalid("undeclared or substituted payload entry")),
        }
    }
    if !files.is_empty() {
        send(directory, files)?;
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

static FREE_READERS: AtomicUsize = AtomicUsize::new(7);
struct Readers(usize);
impl Readers {
    fn reserve(files: usize) -> Self {
        let wanted = files.div_ceil(512).saturating_sub(1).min(7);
        let free = FREE_READERS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |free| {
                Some(free.saturating_sub(wanted))
            })
            .expect("reservation always returns an update");
        Self(free.min(wanted))
    }
}
impl Drop for Readers {
    fn drop(&mut self) {
        FREE_READERS.fetch_add(self.0, Ordering::Relaxed);
    }
}

fn verify_parallel(
    root: Arc<Dir>,
    expected: &mut Entries<'_>,
    readers: usize,
    buffer: &mut [u8],
) -> Result<()> {
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel::<Batch<'_>>(2 * readers);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(readers);
        for _ in 0..readers {
            let receiver = receiver.clone();
            workers.push(
                std::thread::Builder::new()
                    .name("rsi-payload-read".into())
                    .spawn_scoped(scope, move || -> Result<()> {
                        let mut buffer = vec![0; 64 * 1024];
                        loop {
                            let batch = receiver
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .recv();
                            let Ok((parent, files)) = batch else {
                                return Ok(());
                            };
                            verify_files(&parent, &files, &mut buffer)?;
                        }
                    })?,
            );
        }
        drop(receiver);
        let result = walk(
            root,
            "",
            expected,
            &mut |dir, files| match sender.try_send((dir, files)) {
                Ok(()) => Ok(()),
                Err(mpsc::TrySendError::Full((parent, files))) => {
                    verify_files(&parent, &files, buffer)
                }
                Err(mpsc::TrySendError::Disconnected(_)) => Err(invalid("payload readers stopped")),
            },
        );
        drop(sender);
        for worker in workers {
            worker
                .join()
                .map_err(|_| invalid("payload reader failed"))??;
        }
        result
    })
}
