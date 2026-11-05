use async_trait::async_trait;
use rsi_files_native_fs as native;
use rsi_media_protocol::{
    FileRead, FileReadContract, FileRef, MAXIMUM_IMAGE_DESCRIPTOR_BYTES, MediaError, MediaId,
    MediaObjectReader, MediaRef, PublishedImagesContract, Result, StoredMedia,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Retained publisher namespace for verbatim files and normalized images.
#[derive(Debug)]
pub struct LocalAttachmentReader {
    root: PathBuf,
    directory: Arc<native::Directory>,
    slots: Arc<tokio::sync::Semaphore>,
    reads: rsi_api_protocol::ByteBudget,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentConfig {
    root: PathBuf,
}

/// Supplies both readers from one immutable attachment namespace.
#[derive(Clone, Debug, Default)]
pub struct LocalAttachmentReaderFactory;

#[async_trait]
impl PluginFactory for LocalAttachmentReaderFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: AttachmentConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        if !config.root.is_absolute() {
            return Err(MetaError::InvalidInput(
                "attachment namespace root must be absolute".into(),
            ));
        }
        let retained = config.root.as_os_str().len();
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<AttachmentConfig>()?;
        let reader = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&config.root).map_err(super::native::io_error)?;
            LocalAttachmentReader::open_directory(&config.root).map(Arc::new)
        })
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let files = plan
            .context()
            .provide_local::<FileReadContract>(reader.clone())?;
        let images = plan
            .context()
            .provide_local::<PublishedImagesContract>(reader)?;
        plan.defer(
            "withdraw local attachment readers",
            Box::new(move || {
                Box::pin(async move {
                    drop((files, images));
                    Ok(())
                })
            }),
        )
    }
}

impl LocalAttachmentReader {
    /// Opens an existing explicit root without following internal directory links.
    pub async fn open(root: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::open_directory(&root))
            .await
            .map_err(|error| super::join_error(&error))?
    }

    fn open_directory(root: &Path) -> Result<Self> {
        let root =
            native::resolve_absolute_root_alias(root, false).map_err(super::native::io_error)?;
        let directory =
            native::open_absolute_directory_no_follow(&root).map_err(super::native::io_error)?;
        Ok(Self {
            root,
            directory: Arc::new(directory),
            slots: Arc::new(tokio::sync::Semaphore::new(64)),
            reads: rsi_api_protocol::ByteBudget::default(),
        })
    }
}

#[async_trait]
impl MediaObjectReader for LocalAttachmentReader {
    async fn get(&self, id: &MediaId) -> Result<StoredMedia> {
        let slot = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("attachment reader stays open");
        let directory = self.directory.clone();
        let reads = self.reads.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let path = Path::new("objects")
                .join(&id.as_str()[..2])
                .join(id.as_str());
            let file =
                native::open_relative_file_no_follow(&directory, &path).map_err(|error| {
                    if native::is_link_rejection(&error) {
                        MediaError::Corrupt("image attachment path contains a link".into())
                    } else {
                        super::read_error(&error, &id)
                    }
                })?;
            let bytes = super::read_opened_file(file, &id, &reads, MAXIMUM_IMAGE_DESCRIPTOR_BYTES)?
                .into_bytes();
            if hex::encode(Sha256::digest(&bytes)) != id.as_str() {
                return Err(MediaError::Corrupt(
                    "image bytes do not match their published identity".into(),
                ));
            }
            let corrupt = |error: image::ImageError| MediaError::Corrupt(error.to_string());
            let format = image::guess_format(&bytes).map_err(corrupt)?;
            let (width, height) =
                image::ImageReader::with_format(std::io::Cursor::new(bytes.as_ref()), format)
                    .into_dimensions()
                    .map_err(corrupt)?;
            let reference = MediaRef {
                id,
                mime: format.to_mime_type().into(),
                bytes: bytes.len() as u64,
                width,
                height,
            };
            reference
                .validate()
                .map_err(|error| MediaError::Corrupt(error.to_string()))?;
            Ok(StoredMedia { reference, bytes })
        })
        .await
        .map_err(|error| super::join_error(&error))?
    }
}

struct CancelRead(Arc<AtomicBool>);
impl Drop for CancelRead {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[async_trait]
impl FileRead for LocalAttachmentReader {
    async fn verify(&self, reference: &FileRef) -> Result<()> {
        let slot = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("file reader never closes its admission semaphore");
        let reference = reference.clone();
        let directory = self.directory.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = CancelRead(cancelled.clone());
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let file = native::open_relative_file_no_follow(&directory, &relative(&reference))
                .map_err(|error| {
                    if native::is_link_rejection(&error) {
                        MediaError::Corrupt("file attachment path contains a link".into())
                    } else {
                        super::read_error(&error, reference.id())
                    }
                })?;
            if file.metadata().map_err(super::native::io_error)?.len() != reference.bytes() {
                return Err(MediaError::Corrupt("file attachment length changed".into()));
            }
            verify_bytes(file, &reference, &cancelled)
        })
        .await
        .map_err(|error| super::join_error(&error))?
    }

    fn local_path(&self, reference: &FileRef) -> Option<PathBuf> {
        Some(self.root.join(relative(reference)))
    }
}

fn relative(reference: &FileRef) -> PathBuf {
    Path::new("files")
        .join(&reference.id().as_str()[..2])
        .join(reference.id().as_str())
        .join(reference.name())
}

fn verify_bytes(mut source: impl Read, reference: &FileRef, cancelled: &AtomicBool) -> Result<()> {
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(MediaError::Io("file verification cancelled".into()));
        }
        // One extra byte detects growth without following an unbounded producer.
        let available = usize::try_from((reference.bytes() - length + 1).min(buffer.len() as u64))
            .expect("bounded read buffer length");
        let read = source
            .read(&mut buffer[..available])
            .map_err(super::native::io_error)?;
        if read == 0 {
            break;
        }
        length += read as u64;
        if length > reference.bytes() {
            return Err(MediaError::Corrupt(
                "file attachment exceeds declared length".into(),
            ));
        }
        digest.update(&buffer[..read]);
    }
    if length != reference.bytes() || hex::encode(digest.finalize()) != reference.id().as_str() {
        return Err(MediaError::Corrupt(
            "file attachment bytes do not match its reference".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_media_protocol::MediaId;

    fn reference(bytes: &[u8]) -> FileRef {
        FileRef::new(
            MediaId::new(hex::encode(Sha256::digest(bytes))).unwrap(),
            "input.bin",
            bytes.len() as u64,
        )
        .unwrap()
    }

    #[test]
    fn verification_bounds_reads_and_observes_cancellation_between_chunks() {
        struct Source<'a> {
            remaining: usize,
            largest: usize,
            stop: Option<&'a AtomicBool>,
        }
        impl Read for Source<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.largest = self.largest.max(buffer.len());
                let read = buffer.len().min(self.remaining);
                buffer[..read].fill(7);
                self.remaining -= read;
                if let Some(stop) = self.stop {
                    stop.store(true, Ordering::Relaxed);
                }
                Ok(read)
            }
        }
        let data = vec![7; 1024 * 1024 + 17];
        let stop = AtomicBool::new(false);
        let mut source = Source {
            remaining: data.len(),
            largest: 0,
            stop: None,
        };
        verify_bytes(&mut source, &reference(&data), &stop).unwrap();
        assert_eq!(source.largest, 64 * 1024);
        let mut source = Source {
            remaining: data.len(),
            largest: 0,
            stop: Some(&stop),
        };
        assert!(verify_bytes(&mut source, &reference(&data), &stop).is_err());
        assert_eq!(source.remaining, data.len() - 64 * 1024);
        stop.store(false, Ordering::Relaxed);
        let mut growing = Source {
            remaining: 100_000,
            largest: 0,
            stop: None,
        };
        assert!(verify_bytes(&mut growing, &reference(&[7; 10]), &stop).is_err());
        assert_eq!(growing.remaining, 100_000 - 11);
    }
}
