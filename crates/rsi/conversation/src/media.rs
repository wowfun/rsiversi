use crate::{FieldValue, SourceRef, select_field};
use rsi_agent_session_protocol::SessionFact;
use rsi_media_protocol::MediaRef;

/// Exact image provenance and borrowed immutable metadata, without bytes or a Fact lease.
#[derive(Clone, Copy, Debug)]
pub struct MediaSource<'a> {
    /// Attachment-local exact image field.
    pub source: SourceRef,
    /// Validated metadata; acquiring bytes requires the owning Media capability.
    pub media: &'a MediaRef,
}
impl<'a> MediaSource<'a> {
    /// Selects only a matching sequence and image field, never arbitrary JSON or text.
    pub fn select(fact: &'a SessionFact, source: SourceRef) -> Option<Self> {
        let FieldValue::Media(media) = select_field(fact, source)? else {
            return None;
        };
        Some(Self { source, media })
    }
    /// Formats small validated metadata, never content bytes or a resource URL.
    pub fn label(self) -> String {
        format!(
            "[Image · {} · {}×{} · {} bytes]",
            self.media.mime, self.media.width, self.media.height, self.media.bytes
        )
    }
}
