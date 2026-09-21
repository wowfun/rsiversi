use crate::SourceRef;
use rsi_acp_protocol::observation::{ConversationId, RecordKind, Snapshot};
use rsi_agent_session_protocol::SessionId;
use serde::{Deserialize, Serialize};

/// Product conversation identity, preserving each owner's independent authority.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ConversationIdentity {
    /// Durable native Agent Session.
    Native(SessionId),
    /// Locally observed external agent conversation.
    External(ConversationId),
}

/// Exact external observation source, with lossless decimal wire coordinates.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "ExternalWire")]
pub struct ExternalSource {
    conversation: ConversationId,
    #[serde(with = "crate::source::decimal")]
    epoch: u64,
    #[serde(with = "crate::source::decimal")]
    sequence: u64,
    kind: RecordKind,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalWire {
    conversation: ConversationId,
    epoch: String,
    sequence: String,
    kind: RecordKind,
}
impl TryFrom<ExternalWire> for ExternalSource {
    type Error = &'static str;
    fn try_from(value: ExternalWire) -> Result<Self, Self::Error> {
        fn coordinate(text: &str) -> Result<u64, &'static str> {
            let value = text
                .parse::<u64>()
                .map_err(|_| "invalid external source coordinate")?;
            if value == 0 || value > i64::MAX as u64 || value.to_string() != text {
                return Err("invalid external source coordinate");
            }
            Ok(value)
        }
        Self::new(
            value.conversation,
            coordinate(&value.epoch)?,
            coordinate(&value.sequence)?,
            value.kind,
        )
    }
}
impl ExternalSource {
    /// Captures an exact record in one complete observed epoch.
    ///
    /// # Errors
    /// Rejects zero coordinates or values exceeding the journal integer range.
    pub fn new(
        conversation: ConversationId,
        epoch: u64,
        sequence: u64,
        kind: RecordKind,
    ) -> Result<Self, &'static str> {
        if epoch == 0 || sequence == 0 || epoch > i64::MAX as u64 || sequence > i64::MAX as u64 {
            return Err("invalid external source coordinate");
        }
        Ok(Self {
            conversation,
            epoch,
            sequence,
            kind,
        })
    }
    /// Local external conversation identity.
    pub fn conversation(&self) -> &ConversationId {
        &self.conversation
    }
    /// Complete projection epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Exact local sequence.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Exact observed content kind.
    pub const fn kind(&self) -> RecordKind {
        self.kind
    }
}

/// A source remains bound to its owning conversation semantics.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationSource {
    /// Exact native Fact field.
    Native {
        /// Native Session identity.
        session: SessionId,
        /// Field in that Session.
        source: SourceRef,
    },
    /// Exact external observed record.
    External {
        /// Bound observation identity.
        source: ExternalSource,
    },
}

/// Independently available product operations; no capability is inferred from labels.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ConversationCapabilities {
    /// Native Goal contribution is present.
    pub goal: bool,
    /// Unpublished native draft permits preset selection.
    pub preset: bool,
    /// A connected owner currently accepts prompt submission.
    pub submit: bool,
    /// Remote resume was explicitly advertised.
    pub resume: bool,
    /// Remote load was explicitly advertised.
    pub load: bool,
}
impl ConversationCapabilities {
    /// Native capabilities remain selected by the native controller's actual state.
    pub const fn native(goal: bool, preset: bool, submit: bool) -> Self {
        Self {
            goal,
            preset,
            submit,
            resume: false,
            load: false,
        }
    }
    /// External conversations never acquire native Goal or preset authority.
    pub fn external(snapshot: &Snapshot, connected: bool) -> Self {
        Self {
            submit: connected
                && matches!(
                    snapshot.status,
                    rsi_acp_protocol::observation::Status::Ready
                        | rsi_acp_protocol::observation::Status::Completed
                        | rsi_acp_protocol::observation::Status::Cancelled
                        | rsi_acp_protocol::observation::Status::Discarded
                        | rsi_acp_protocol::observation::Status::Failed
                ),
            resume: snapshot.capabilities.resume,
            load: snapshot.capabilities.load,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn external_source_preserves_large_coordinates_and_rejects_ambiguous_wire_identity() {
        let id = ConversationId::new("observed").unwrap();
        let source =
            ExternalSource::new(id.clone(), 1, 9_007_199_254_740_993, RecordKind::User).unwrap();
        let wire = serde_json::to_value(&source).unwrap();
        assert_eq!(wire["sequence"], "9007199254740993");
        assert_eq!(
            serde_json::from_value::<ExternalSource>(wire.clone())
                .unwrap()
                .sequence(),
            source.sequence()
        );
        for field in ["epoch", "sequence"] {
            for value in [
                json!(1),
                json!("0"),
                json!("01"),
                json!("+1"),
                json!("9223372036854775808"),
            ] {
                let mut malformed = wire.clone();
                malformed[field] = value;
                assert!(serde_json::from_value::<ExternalSource>(malformed).is_err());
            }
        }
        let mut malformed = wire;
        malformed["session"] = json!("native");
        assert!(serde_json::from_value::<ExternalSource>(malformed).is_err());
        let native = ConversationIdentity::Native(SessionId::new("observed").unwrap());
        assert_ne!(native, ConversationIdentity::External(id));
    }
}
