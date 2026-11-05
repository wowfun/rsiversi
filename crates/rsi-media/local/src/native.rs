//! Shared descriptor-relative immutable object publication and reads.
use super::{
    ByteBudget, File, MAXIMUM_HEADER_BYTES, MediaError, MediaId, Ordering, Path, PathBuf, Result,
    StoredMedia, TEMP_SEQUENCE, decode_object, publication_unknown, read_error, read_opened_file,
};
use rsi_files_native_fs as native;
use std::io::{self, Write as _};

#[cfg(unix)]
type Namespace = native::Directory;
#[cfg(windows)]
type Namespace = native::NamespaceLease;

#[derive(Debug)]
pub(super) struct Store {
    pub(super) objects: native::Directory,
    #[cfg(windows)]
    pub(super) objects_namespace: Namespace,
    #[cfg(windows)]
    pub(super) _root_namespace: Namespace,
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the native error."
)]
pub(super) fn io_error(error: io::Error) -> MediaError {
    if native::is_link_rejection(&error) {
        MediaError::Corrupt("Media path contains a link or non-directory".into())
    } else {
        MediaError::Io(error.to_string())
    }
}

pub(super) fn create_directory(
    parent: &native::Directory,
    name: &std::ffi::OsStr,
) -> Result<native::Directory> {
    match native::create_directory_no_follow(parent, name) {
        Ok(directory) => Ok(directory),
        Err(native::DirectoryCreationError::NotCreated(error))
            if error.kind() == io::ErrorKind::AlreadyExists =>
        {
            native::open_relative_directory_no_follow(parent, Path::new(name)).map_err(io_error)
        }
        Err(
            native::DirectoryCreationError::NotCreated(error)
            | native::DirectoryCreationError::Created(error),
        ) => Err(io_error(error)),
    }
}

pub(super) fn directory(parent: &native::Directory, name: &str) -> Result<native::Directory> {
    let directory = create_directory(parent, name.as_ref())?;
    #[cfg(unix)]
    super::unix::make_private(&directory)?;
    #[cfg(windows)]
    directory.verify_private().map_err(io_error)?;
    Ok(directory)
}

impl Store {
    fn namespace(&self) -> &Namespace {
        #[cfg(unix)]
        {
            &self.objects
        }
        #[cfg(windows)]
        {
            &self.objects_namespace
        }
    }

    pub(super) fn get(&self, id: &MediaId, reads: &ByteBudget) -> Result<StoredMedia> {
        let shard =
            native::open_relative_directory_no_follow(&self.objects, Path::new(&id.as_str()[..2]))
                .map_err(|e| object_read_error(&e, id))?;
        read(&shard, id, reads)
    }

    pub(super) fn put(&self, media: &StoredMedia, reads: &ByteBudget) -> Result<()> {
        self.put_with_sync(media, reads, Namespace::sync_all)
    }

    pub(super) fn put_with_sync(
        &self,
        media: &StoredMedia,
        reads: &ByteBudget,
        mut sync: impl FnMut(&Namespace) -> io::Result<()>,
    ) -> Result<()> {
        let id = &media.reference.id;
        let shard = directory(&self.objects, &id.as_str()[..2])?;
        #[cfg(windows)]
        let pinned = shard.pin_namespace().map_err(io_error)?;
        #[cfg(windows)]
        let namespace = &pinned;
        #[cfg(unix)]
        let namespace = &shard;
        match read(&shard, id, reads) {
            Ok(existing) => {
                verify_existing(&existing, media)?;
                return sync(namespace)
                    .and_then(|()| sync(self.namespace()))
                    .map_err(|_| publication_unknown());
            }
            Err(MediaError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        let header =
            serde_json::to_vec(&media.reference).map_err(|e| MediaError::Io(e.to_string()))?;
        if header.len() > MAXIMUM_HEADER_BYTES {
            return Err(MediaError::InvalidInput(
                "object header is too large".into(),
            ));
        }
        let (staging, mut file) = staging(&shard, id)?;
        let result = (|| {
            file.write_all(&header)
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.write_all(&media.bytes))
                .and_then(|()| file.sync_all())
                .map_err(io_error)?;
            drop(file);
            match namespace.link_child(&shard, &staging, &object_name(id)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let existing = read(&shard, id, reads)?;
                    verify_existing(&existing, media)?;
                }
                Err(e) => return Err(io_error(e)),
            }
            namespace
                .remove_child(&staging)
                .map_err(|_| publication_unknown())?;
            sync(namespace)
                .and_then(|()| sync(self.namespace()))
                .map_err(|_| publication_unknown())
        })();
        if result.is_err() {
            let _ = namespace.remove_child(&staging);
        }
        result
    }
}

fn object_name(id: &MediaId) -> PathBuf {
    format!("{id}.rsi-media").into()
}

fn read(shard: &native::Directory, id: &MediaId, reads: &ByteBudget) -> Result<StoredMedia> {
    let file = native::open_relative_file_no_follow(shard, &object_name(id))
        .map_err(|e| object_read_error(&e, id))?;
    decode_object(
        read_opened_file(file, id, reads, super::MAXIMUM_ENVELOPE_BYTES)?,
        id,
    )
}

fn verify_existing(existing: &StoredMedia, expected: &StoredMedia) -> Result<()> {
    if existing.reference != expected.reference || existing.bytes != expected.bytes {
        return Err(MediaError::Corrupt(
            "existing MediaId has different content".into(),
        ));
    }
    Ok(())
}

fn staging(shard: &native::Directory, id: &MediaId) -> Result<(PathBuf, File)> {
    for _ in 0..64 {
        let sequence = TEMP_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| MediaError::Io("Media staging sequence exhausted".into()))?;
        let name = PathBuf::from(format!(".{id}.{}.{sequence}.tmp", std::process::id()));
        match native::create_relative_private_file_no_follow(shard, &name) {
            Ok(file) => return Ok((name, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    Err(MediaError::Io(
        "Media staging collision limit exceeded".into(),
    ))
}

fn object_read_error(error: &io::Error, id: &MediaId) -> MediaError {
    if native::is_link_rejection(error) {
        MediaError::Corrupt("Media path contains a link or non-directory".into())
    } else {
        read_error(error, id)
    }
}
