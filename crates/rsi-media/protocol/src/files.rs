use super::{MediaError, MediaId, Result};
use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

/// Immutable verbatim file identity, independent of its storage location.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct FileRef {
    id: MediaId,
    name: String,
    bytes: u64,
}

impl<'de> Deserialize<'de> for FileRef {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: MediaId,
            name: String,
            bytes: u64,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.id, wire.name, wire.bytes).map_err(serde::de::Error::custom)
    }
}

impl FileRef {
    /// Checks external metadata without granting access or verifying file bytes.
    pub fn new(id: MediaId, name: impl Into<String>, bytes: u64) -> Result<Self> {
        let name = name.into();
        if name.is_empty()
            || name.len() > 255
            || name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|c| c <= '\u{1f}' || c == '\u{7f}' || "<>:\"/\\|?*".contains(c))
            || reserved_name(&name)
            || bytes > (1_u64 << 53) - 1
        {
            return Err(MediaError::InvalidInput(
                "invalid verbatim file reference".into(),
            ));
        }
        Ok(Self { id, name, bytes })
    }

    /// SHA-256 of the exact stored bytes.
    pub fn id(&self) -> &MediaId {
        &self.id
    }
    /// Portable display and stored leaf filename.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Exact byte count, including zero for an empty file.
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

fn reserved_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
        || stem.get(..3).is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
        }) && matches!(
            stem.get(3..),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
        )
}

/// Read-only access to already published immutable file attachments.
#[async_trait]
pub trait FileRead: fmt::Debug + Send + Sync + 'static {
    /// Verifies byte count and digest without retaining a complete file in memory.
    async fn verify(&self, reference: &FileRef) -> Result<()>;
    /// Resolves a provider-local path without I/O; it does not grant file access.
    fn local_path(&self, reference: &FileRef) -> Option<PathBuf>;
}

/// Nominal Local contract for verbatim file attachment reads.
#[derive(Debug)]
pub struct FileReadContract;
impl LocalContract for FileReadContract {
    const KEY: &'static str = "rsi.media.files.read";
    type Service = dyn FileRead;
}
