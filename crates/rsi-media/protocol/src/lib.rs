//! Runtime-independent durable Media contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Maximum accepted source image bytes.
pub const MAXIMUM_IMAGE_INPUT_BYTES: usize = 256 * 1024 * 1024;
/// Maximum decoded pixels in one image.
pub const MAXIMUM_IMAGE_PIXELS: u64 = 100_000_000;
/// Maximum canonical image bytes referenced by Media or an AI request.
pub const MAXIMUM_IMAGE_DESCRIPTOR_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum audio bytes referenced by an AI request.
pub const MAXIMUM_AUDIO_DESCRIPTOR_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum one image dimension carried by a descriptor.
pub const MAXIMUM_IMAGE_DIMENSION: u32 = 65_535;

/// Media class whose durable bytes travel separately from semantic JSON.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Still-image bytes with an `image/` MIME type.
    Image,
    /// Audio bytes with an `audio/` MIME type.
    Audio,
}

/// Locator-free durable identity and bounded metadata for one media body.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaDescriptor {
    kind: MediaKind,
    mime_type: String,
    byte_len: u64,
    sha256: String,
    width: Option<u32>,
    height: Option<u32>,
    duration_ms: Option<u64>,
}

impl<'de> Deserialize<'de> for MediaDescriptor {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDescriptor {
            kind: MediaKind,
            mime_type: String,
            byte_len: u64,
            sha256: String,
            width: Option<u32>,
            height: Option<u32>,
            duration_ms: Option<u64>,
        }

        let wire = WireDescriptor::deserialize(deserializer)?;
        let descriptor = Self {
            kind: wire.kind,
            mime_type: wire.mime_type,
            byte_len: wire.byte_len,
            sha256: wire.sha256,
            width: wire.width,
            height: wire.height,
            duration_ms: wire.duration_ms,
        };
        descriptor
            .validate()
            .map(|()| descriptor)
            .map_err(serde::de::Error::custom)
    }
}

impl MediaDescriptor {
    /// Creates locator-free media identity from kind, MIME type, length, and SHA-256.
    pub fn new(
        kind: MediaKind,
        mime_type: impl Into<String>,
        byte_len: u64,
        sha256: impl Into<String>,
    ) -> Result<Self> {
        let descriptor = Self {
            kind,
            mime_type: mime_type.into(),
            byte_len,
            sha256: sha256.into(),
            width: None,
            height: None,
            duration_ms: None,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    /// Adds a bounded nonzero width and height to an image descriptor.
    pub fn with_image_dimensions(mut self, width: u32, height: u32) -> Result<Self> {
        if self.kind != MediaKind::Image {
            return Err(MediaError::InvalidInput(
                "dimensions are valid only for images".into(),
            ));
        }
        self.width = Some(width);
        self.height = Some(height);
        self.validate()?;
        Ok(self)
    }

    /// Adds a positive duration in milliseconds to an audio descriptor.
    pub fn with_audio_duration_ms(mut self, duration_ms: u64) -> Result<Self> {
        if self.kind != MediaKind::Audio {
            return Err(MediaError::InvalidInput(
                "duration is valid only for audio".into(),
            ));
        }
        self.duration_ms = Some(duration_ms);
        self.validate()?;
        Ok(self)
    }

    /// Returns the media class.
    pub const fn kind(&self) -> MediaKind {
        self.kind
    }

    /// Returns the validated MIME type.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// Returns the exact durable byte length.
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    /// Returns the lowercase SHA-256 identity.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns optional image dimensions.
    pub const fn dimensions(&self) -> Option<(u32, u32)> {
        match (self.width, self.height) {
            (Some(width), Some(height)) => Some((width, height)),
            _ => None,
        }
    }

    /// Returns the optional audio duration.
    pub const fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    /// Revalidates an untrusted descriptor.
    pub fn validate(&self) -> Result<()> {
        let maximum = match self.kind {
            MediaKind::Image => MAXIMUM_IMAGE_DESCRIPTOR_BYTES,
            MediaKind::Audio => MAXIMUM_AUDIO_DESCRIPTOR_BYTES,
        };
        if self.byte_len == 0 || self.byte_len > maximum {
            return Err(MediaError::InvalidInput(format!(
                "media byte length must be within 1..={maximum}"
            )));
        }
        let prefix = match self.kind {
            MediaKind::Image => "image/",
            MediaKind::Audio => "audio/",
        };
        if self.mime_type.len() <= prefix.len()
            || self.mime_type.len() > 127
            || !self.mime_type.starts_with(prefix)
            || !self.mime_type[prefix.len()..].bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'+' | b'-')
            })
        {
            return Err(MediaError::InvalidInput(format!(
                "media MIME type must be a bounded {prefix} value"
            )));
        }
        MediaId::new(self.sha256.clone())?;
        match self.kind {
            MediaKind::Image => {
                let invalid_dimensions = self.width.is_some() != self.height.is_some()
                    || self.width.zip(self.height).is_some_and(|(width, height)| {
                        width == 0
                            || height == 0
                            || width > MAXIMUM_IMAGE_DIMENSION
                            || height > MAXIMUM_IMAGE_DIMENSION
                            || u64::from(width).saturating_mul(u64::from(height))
                                > MAXIMUM_IMAGE_PIXELS
                    });
                if self.duration_ms.is_some() || invalid_dimensions {
                    return Err(MediaError::InvalidInput(
                        "image metadata must contain optional bounded dimensions and no duration"
                            .into(),
                    ));
                }
            }
            MediaKind::Audio => {
                if self.width.is_some() || self.height.is_some() || self.duration_ms == Some(0) {
                    return Err(MediaError::InvalidInput(
                        "audio metadata may contain only a positive optional duration".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Lowercase SHA-256 identity of canonical media bytes.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MediaId(String);

impl<'de> Deserialize<'de> for MediaId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .and_then(|value| Self::new(value).map_err(serde::de::Error::custom))
    }
}

impl MediaId {
    /// Validates an exact lowercase SHA-256 string.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(MediaError::InvalidInput(
                "MediaId must be a lowercase SHA-256 digest".into(),
            ));
        }
        Ok(Self(value))
    }

    /// Borrows the exact digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MediaId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Immutable canonical image reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaRef {
    /// Content identity of final canonical bytes.
    pub id: MediaId,
    /// Canonical MIME type. Current producers emit `image/png`.
    pub mime: String,
    /// Exact final byte length.
    pub bytes: u64,
    /// Decoded width in pixels.
    pub width: u32,
    /// Decoded height in pixels.
    pub height: u32,
}

impl<'de> Deserialize<'de> for MediaRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireMediaRef {
            id: MediaId,
            mime: String,
            bytes: u64,
            width: u32,
            height: u32,
        }

        let wire = WireMediaRef::deserialize(deserializer)?;
        let reference = Self {
            id: wire.id,
            mime: wire.mime,
            bytes: wire.bytes,
            width: wire.width,
            height: wire.height,
        };
        reference
            .validate()
            .map(|()| reference)
            .map_err(serde::de::Error::custom)
    }
}

impl MediaRef {
    /// Validates closed current image-reference invariants.
    pub fn validate(&self) -> Result<()> {
        if self.mime != "image/png" {
            return Err(MediaError::InvalidInput(
                "current MediaRef MIME must be image/png".into(),
            ));
        }
        if self.bytes == 0 || self.bytes > MAXIMUM_IMAGE_DESCRIPTOR_BYTES {
            return Err(MediaError::InvalidInput(
                "MediaRef byte length is out of bounds".into(),
            ));
        }
        let pixels = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .ok_or_else(|| MediaError::InvalidInput("image dimensions overflow".into()))?;
        if self.width == 0
            || self.height == 0
            || self.width > MAXIMUM_IMAGE_DIMENSION
            || self.height > MAXIMUM_IMAGE_DIMENSION
            || pixels > MAXIMUM_IMAGE_PIXELS
        {
            return Err(MediaError::InvalidInput(
                "MediaRef dimensions are out of bounds".into(),
            ));
        }
        Ok(())
    }
}

/// Canonical bytes paired with their immutable reference.
#[derive(Clone)]
pub struct StoredMedia {
    /// Validated immutable reference.
    pub reference: MediaRef,
    /// Exact canonical bytes.
    pub bytes: Arc<[u8]>,
}

/// Durable bytes paired with their locator-free descriptor.
#[derive(Clone)]
pub struct MediaBody {
    /// Validated descriptor requested by the caller.
    pub descriptor: MediaDescriptor,
    /// Exact verified durable bytes.
    pub bytes: Arc<[u8]>,
}

impl fmt::Debug for MediaBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaBody")
            .field("descriptor", &self.descriptor)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

impl fmt::Debug for StoredMedia {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredMedia")
            .field("reference", &self.reference)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

/// Closed Media failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MediaError {
    /// Malformed or out-of-bounds caller input.
    #[error("invalid media input: {0}")]
    InvalidInput(String),
    /// Source raster decoding or canonical encoding failed.
    #[error("media codec failed: {0}")]
    Codec(String),
    /// Immutable object is absent.
    #[error("media object `{0}` was not found")]
    NotFound(MediaId),
    /// Durable object does not match its identity or metadata.
    #[error("media object is corrupt: {0}")]
    Corrupt(String),
    /// Backend I/O failed.
    #[error("media I/O failed: {0}")]
    Io(String),
}

/// Media result.
pub type Result<T> = std::result::Result<T, MediaError>;

/// Immutable canonical-object backend.
#[async_trait]
pub trait MediaBackend: fmt::Debug + Send + Sync + 'static {
    /// Idempotently publishes one complete canonical object.
    async fn put(&self, media: StoredMedia) -> Result<()>;
    /// Loads and verifies one immutable object.
    async fn get(&self, id: &MediaId) -> Result<StoredMedia>;
}

/// Nominal Local contract for [`MediaBackend`].
#[derive(Debug)]
pub struct MediaBackendContract;

impl LocalContract for MediaBackendContract {
    const KEY: &'static str = "rsi.media.backend";
    type Service = dyn MediaBackend;
}

/// Bounded image normalization and durable reference service.
#[async_trait]
pub trait Media: fmt::Debug + Send + Sync + 'static {
    /// Decodes, canonicalizes, and durably publishes one raster image.
    async fn import_image(&self, source: Arc<[u8]>) -> Result<MediaRef>;
    /// Loads canonical bytes for one exact reference.
    async fn read(&self, reference: &MediaRef) -> Result<StoredMedia>;
}

/// Point-of-use durable media read service used by capability plugins.
#[async_trait]
pub trait MediaRead: fmt::Debug + Send + Sync + 'static {
    /// Reads and verifies one exact locator-free descriptor.
    async fn read_descriptor(&self, descriptor: &MediaDescriptor) -> Result<MediaBody>;
}

/// Nominal Local contract for [`Media`].
#[derive(Debug)]
pub struct MediaContract;

impl LocalContract for MediaContract {
    const KEY: &'static str = "rsi.media";
    type Service = dyn Media;
}

/// Nominal Local contract for point-of-use durable reads.
#[derive(Debug)]
pub struct MediaReadContract;

impl LocalContract for MediaReadContract {
    const KEY: &'static str = "rsi.media.read";
    type Service = dyn MediaRead;
}
