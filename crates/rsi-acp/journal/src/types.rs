use crate::{Error, Result};
use serde::Serialize;

pub(crate) const METADATA_BYTES: usize = 16 * 1024;
pub(crate) const MAX_SESSIONS: usize = 4096;
pub(crate) const PAGE_BYTES: usize = 256 * 1024;

/// Configurable quotas which can only tighten the product's maximum bounds.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum observed bytes per conversation, including its metadata reservation.
    pub session_bytes: usize,
    /// Maximum observed bytes across the owner, including metadata reservations.
    pub owner_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            session_bytes: 64 * 1024 * 1024,
            owner_bytes: 1024 * 1024 * 1024,
        }
    }
}
impl Limits {
    pub(crate) fn validate(self) -> Result<()> {
        if self.session_bytes < METADATA_BYTES
            || self.session_bytes > Self::default().session_bytes
            || self.owner_bytes < 1024 * 1024
            || self.owner_bytes > Self::default().owner_bytes
            || self.session_bytes > self.owner_bytes
        {
            return Err(Error::Input);
        }
        Ok(())
    }
}

pub(crate) fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    struct Writer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| Error::Quota)?;
    Ok(writer.bytes)
}
