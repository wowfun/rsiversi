use rsi_agent_session_protocol::{EffectId, MessageId, SessionFact, SessionFactBody, TurnId};
use rsi_tools_protocol::ToolResultIdentity;
use serde::Serialize;

/// Borrowed semantic identity; presentation keys never flatten user identifiers with delimiters.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlockIdentity<'a> {
    /// Direct input has no mailbox Message identity.
    TurnInput {
        /// Owning Turn.
        turn: &'a TurnId,
    },
    /// A durable mailbox input, also used for its accepted control preview.
    Message {
        /// Exact mailbox identity within the Session.
        message: &'a MessageId,
    },
    /// An indexed provider content block.
    Model {
        /// Owning Turn.
        turn: &'a TurnId,
        /// Prepared provider effect.
        effect: &'a EffectId,
        /// Provider content index.
        index: u32,
    },
    /// One durable generated image, distinct from a Language content block.
    Image {
        /// Owning Turn.
        turn: &'a TurnId,
        /// Prepared Image effect.
        effect: &'a EffectId,
        /// Provider output index.
        index: u32,
    },
    /// One exact Tool registration and invocation.
    Tool {
        /// Owning Turn.
        turn: &'a TurnId,
        /// Prepared Tool effect.
        effect: &'a EffectId,
        /// Complete retained-result identity.
        identity: &'a ToolResultIdentity,
    },
    /// A Turn's terminal outcome.
    Terminal {
        /// Exact terminal Turn.
        turn: &'a TurnId,
    },
}
impl<'a> BlockIdentity<'a> {
    /// Selects identity from any Tool lifecycle Fact.
    pub fn tool(fact: &'a SessionFact) -> Option<Self> {
        match fact.body() {
            SessionFactBody::ToolIntent {
                turn_id,
                effect_id,
                identity,
                ..
            }
            | SessionFactBody::ToolStarted {
                turn_id,
                effect_id,
                identity,
            }
            | SessionFactBody::ToolRejected {
                turn_id,
                effect_id,
                identity,
                ..
            }
            | SessionFactBody::ToolResult {
                turn_id,
                effect_id,
                identity,
                ..
            } => Some(Self::Tool {
                turn: turn_id,
                effect: effect_id,
                identity,
            }),
            _ => None,
        }
    }
    /// Encodes a canonical opaque key without loss of identifier boundaries.
    ///
    /// # Panics
    /// Only if serialization of the closed identity fails.
    pub fn key(self) -> String {
        serde_json::to_string(&self).expect("closed conversation identity")
    }
}
