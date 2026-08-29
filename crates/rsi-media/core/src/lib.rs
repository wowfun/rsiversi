//! Bounded image normalization and durable Media plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageDecoder, ImageEncoder, ImageReader};
use rsi_media_protocol::{
    MAXIMUM_IMAGE_DESCRIPTOR_BYTES, MAXIMUM_IMAGE_INPUT_BYTES, MAXIMUM_IMAGE_PIXELS, Media,
    MediaBackend, MediaBackendContract, MediaBody, MediaContract, MediaDescriptor, MediaError,
    MediaId, MediaKind, MediaRead, MediaReadContract, MediaRef, Result, StoredMedia,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Cursor, Write};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Absolute number of codec imports admitted concurrently by one Media generation.
pub const MAXIMUM_CONCURRENT_MEDIA_IMPORTS: usize = 32;

/// Configuration accepted by [`MediaFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    /// Maximum accepted encoded source bytes.
    #[serde(default = "default_maximum_input_bytes")]
    pub maximum_input_bytes: usize,
    /// Maximum decoded pixels.
    #[serde(default = "default_maximum_pixels")]
    pub maximum_pixels: u64,
    /// Maximum codec imports executing concurrently in this generation.
    #[serde(default = "default_maximum_concurrent_imports")]
    pub maximum_concurrent_imports: usize,
}

const fn default_maximum_input_bytes() -> usize {
    MAXIMUM_IMAGE_INPUT_BYTES
}

const fn default_maximum_pixels() -> u64 {
    MAXIMUM_IMAGE_PIXELS
}

const fn default_maximum_concurrent_imports() -> usize {
    2
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            maximum_input_bytes: default_maximum_input_bytes(),
            maximum_pixels: default_maximum_pixels(),
            maximum_concurrent_imports: default_maximum_concurrent_imports(),
        }
    }
}

impl MediaConfig {
    fn validate(&self) -> Result<()> {
        if self.maximum_input_bytes == 0 || self.maximum_input_bytes > MAXIMUM_IMAGE_INPUT_BYTES {
            return Err(MediaError::InvalidInput(format!(
                "maximum_input_bytes must be within 1..={MAXIMUM_IMAGE_INPUT_BYTES}"
            )));
        }
        if self.maximum_pixels == 0 || self.maximum_pixels > MAXIMUM_IMAGE_PIXELS {
            return Err(MediaError::InvalidInput(format!(
                "maximum_pixels must be within 1..={MAXIMUM_IMAGE_PIXELS}"
            )));
        }
        if self.maximum_concurrent_imports == 0
            || self.maximum_concurrent_imports > MAXIMUM_CONCURRENT_MEDIA_IMPORTS
        {
            return Err(MediaError::InvalidInput(format!(
                "maximum_concurrent_imports must be within 1..={MAXIMUM_CONCURRENT_MEDIA_IMPORTS}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Service {
    config: MediaConfig,
    backend: Arc<dyn MediaBackend>,
    import_admission: Arc<Semaphore>,
}

#[async_trait]
impl Media for Service {
    async fn import_image(&self, source: Arc<[u8]>) -> Result<MediaRef> {
        if source.is_empty() || source.len() > self.config.maximum_input_bytes {
            return Err(MediaError::InvalidInput(format!(
                "source image length must be within 1..={} bytes",
                self.config.maximum_input_bytes
            )));
        }
        let maximum_pixels = self.config.maximum_pixels;
        let permit = Arc::clone(&self.import_admission)
            .acquire_owned()
            .await
            .map_err(|_| MediaError::Codec("Media import admission is closed".into()))?;
        let stored = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            normalize(source, maximum_pixels)
        })
        .await
        .map_err(|error| MediaError::Codec(format!("codec task failed: {error}")))??;
        let reference = stored.reference.clone();
        self.backend.put(stored).await?;
        Ok(reference)
    }

    async fn read(&self, reference: &MediaRef) -> Result<StoredMedia> {
        reference.validate()?;
        let stored = self.backend.get(&reference.id).await?;
        if stored.reference != *reference {
            return Err(MediaError::Corrupt(
                "stored metadata does not match requested reference".into(),
            ));
        }
        Ok(stored)
    }
}

#[async_trait]
impl MediaRead for Service {
    async fn read_descriptor(&self, descriptor: &MediaDescriptor) -> Result<MediaBody> {
        descriptor.validate()?;
        if descriptor.kind() != MediaKind::Image {
            return Err(MediaError::InvalidInput(
                "the current durable Media service supports image descriptors only".into(),
            ));
        }
        let id = MediaId::new(descriptor.sha256())?;
        let stored = self.backend.get(&id).await?;
        let dimensions_match = descriptor.dimensions().is_none_or(|(width, height)| {
            stored.reference.width == width && stored.reference.height == height
        });
        if descriptor.mime_type() != stored.reference.mime
            || descriptor.byte_len() != stored.reference.bytes
            || !dimensions_match
        {
            return Err(MediaError::Corrupt(
                "stored media metadata does not match the requested descriptor".into(),
            ));
        }
        Ok(MediaBody {
            descriptor: descriptor.clone(),
            bytes: stored.bytes,
        })
    }
}

/// Ordinary plugin factory for the Media normalization service.
#[derive(Clone, Debug, Default)]
pub struct MediaFactory;

#[async_trait]
impl PluginFactory for MediaFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config = if desired.is_null() {
            MediaConfig::default()
        } else {
            serde_json::from_value(desired.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        Ok(PreparedActivation::with_state(
            serde_json::to_value(&config)
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?,
            config,
            std::mem::size_of::<MediaConfig>(),
        )
        .requiring_local::<MediaBackendContract>())
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<MediaConfig>()?;
        let maximum_concurrent_imports = config.maximum_concurrent_imports;
        let service = Arc::new(Service {
            config,
            backend: plan.local::<MediaBackendContract>()?,
            import_admission: Arc::new(Semaphore::new(maximum_concurrent_imports)),
        });
        let media: Arc<dyn Media> = service.clone();
        let read: Arc<dyn MediaRead> = service;
        let media_supply = plan.context().provide_local::<MediaContract>(media)?;
        let read_supply = match plan.context().provide_local::<MediaReadContract>(read) {
            Ok(supply) => supply,
            Err(error) => {
                drop(media_supply);
                return Err(error);
            }
        };
        plan.defer(
            "withdraw Media services",
            Box::new(move || {
                Box::pin(async move {
                    drop(read_supply);
                    drop(media_supply);
                    Ok(())
                })
            }),
        )
    }
}

fn normalize(source: Arc<[u8]>, maximum_pixels: u64) -> Result<StoredMedia> {
    let reader = ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|error| MediaError::Codec(error.to_string()))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| MediaError::Codec(error.to_string()))?;
    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| MediaError::InvalidInput("image dimensions overflow".into()))?;
    if width == 0 || height == 0 || pixels > maximum_pixels {
        return Err(MediaError::InvalidInput(format!(
            "decoded image exceeds the {maximum_pixels}-pixel limit"
        )));
    }
    let orientation = decoder
        .orientation()
        .map_err(|error| MediaError::Codec(error.to_string()))?;
    let mut image = image::DynamicImage::from_decoder(decoder)
        .map_err(|error| MediaError::Codec(error.to_string()))?;
    image.apply_orientation(orientation);
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    let maximum_bytes = usize::try_from(MAXIMUM_IMAGE_DESCRIPTOR_BYTES).map_err(|_| {
        MediaError::InvalidInput("canonical image byte bound is unsupported".into())
    })?;
    let mut output = BoundedWriter::new(maximum_bytes);
    if let Err(error) = PngEncoder::new(&mut output).write_image(
        image.as_raw(),
        width,
        height,
        ColorType::Rgba8.into(),
    ) {
        if output.exceeded() {
            return Err(MediaError::InvalidInput(
                "canonical image bytes are out of bounds".into(),
            ));
        }
        return Err(MediaError::Codec(error.to_string()));
    }
    let bytes = output.into_inner();
    if bytes.is_empty() {
        return Err(MediaError::InvalidInput(
            "canonical image bytes are out of bounds".into(),
        ));
    }
    let id = MediaId::new(hex::encode(Sha256::digest(&bytes)))?;
    let reference = MediaRef {
        id,
        mime: "image/png".into(),
        bytes: u64::try_from(bytes.len())
            .map_err(|_| MediaError::InvalidInput("image length overflow".into()))?,
        width,
        height,
    };
    reference.validate()?;
    Ok(StoredMedia {
        reference,
        bytes: Arc::from(bytes),
    })
}

#[derive(Debug)]
struct BoundedWriter {
    bytes: Vec<u8>,
    maximum_bytes: usize,
    exceeded: bool,
}

impl BoundedWriter {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum_bytes,
            exceeded: false,
        }
    }

    const fn exceeded(&self) -> bool {
        self.exceeded
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.maximum_bytes.saturating_sub(self.bytes.len());
        if buffer.len() > remaining {
            self.exceeded = true;
            return Err(io::Error::other(
                "bounded media writer reached its byte limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedWriter;
    use std::io::Write as _;

    #[test]
    fn bounded_writer_rejects_a_chunk_before_growing_past_its_limit() {
        let mut output = BoundedWriter::new(4);
        output.write_all(b"abc").unwrap();
        assert!(output.write_all(b"de").is_err());
        assert!(output.exceeded());
        assert_eq!(output.into_inner(), b"abc");
    }
}
