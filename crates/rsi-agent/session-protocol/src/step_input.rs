//! Claimed-input coordinates and authoritative pre-step decisions.

use crate::{AgentMessageContent, MessageAttribution, Result, SessionError};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// Maximum compact JSON bytes in one final ordered input list.
///
/// Fits the existing 16 MiB tool-context and 16 MiB contribution budgets inside
/// the ordinary Fact envelope. Unchanged bodies are retained by coordinate.
pub const MAXIMUM_PREPARED_INPUT_BYTES: usize = 32 * 1024 * 1024;

/// One message in a claimed Fact belonging to the preparation's source Session.
///
/// This is a coordinate, not authority to read arbitrary Facts. The captured
/// execution owner resolves membership and ordinal before entering the result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum InputClaimRef {
    /// Message in an already captured Fact.
    Captured {
        /// Original Session-local sequence.
        fact_seq: NonZeroU64,
        /// Position within the claimed batch.
        ordinal: u32,
    },
    /// Context claim in the preparation publication.
    Contributed {
        /// Distance back from the durable preparation Fact.
        preceding: std::num::NonZeroU32,
        /// Position within the claimed batch.
        ordinal: u32,
    },
}

impl InputClaimRef {
    /// Addresses a previously captured input Fact.
    pub fn new(fact_seq: u64, ordinal: u32) -> Result<Self> {
        Ok(Self::Captured {
            fact_seq: NonZeroU64::new(fact_seq)
                .ok_or_else(|| SessionError::Invalid("input claim Fact sequence is zero".into()))?,
            ordinal,
        })
    }

    /// Addresses a single-message claim in the same publication.
    pub fn preceding(distance: usize) -> Result<Self> {
        let preceding = u32::try_from(distance)
            .ok()
            .and_then(std::num::NonZeroU32::new)
            .ok_or_else(|| SessionError::Invalid("invalid contributed input distance".into()))?;
        Ok(Self::Contributed {
            preceding,
            ordinal: 0,
        })
    }

    /// Source sequence; zero is outside every Session's claim set.
    #[must_use]
    pub const fn fact_seq(self, decision_seq: u64) -> u64 {
        match self {
            Self::Captured { fact_seq, .. } => fact_seq.get(),
            Self::Contributed { preceding, .. } => {
                decision_seq.saturating_sub(preceding.get() as u64)
            }
        }
    }

    /// Original position within that Fact's message batch.
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        match self {
            Self::Captured { ordinal, .. } | Self::Contributed { ordinal, .. } => ordinal,
        }
    }
}

/// Roles supplied by input preparation; model and Tool output use their own Facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedInputRole {
    /// Ordinary conversation input, including application-authored context.
    User,
    /// Context supplied as developer guidance.
    Developer,
}

/// An immutable validated message authored or rewritten during preparation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PreparedInputMessage {
    role: PreparedInputRole,
    attribution: MessageAttribution,
    content: Vec<AgentMessageContent>,
}

impl PreparedInputMessage {
    /// Admits a message without applying fresh-ingress nonempty restrictions.
    pub fn new(
        role: PreparedInputRole,
        attribution: MessageAttribution,
        content: Vec<AgentMessageContent>,
    ) -> Result<Self> {
        attribution.validate()?;
        Self::from_validated_attribution(role, attribution, content)
    }

    fn from_validated_attribution(
        role: PreparedInputRole,
        attribution: MessageAttribution,
        content: Vec<AgentMessageContent>,
    ) -> Result<Self> {
        crate::validate_input_content(&content, crate::MAXIMUM_TURN_TEXT_BYTES, true)?;
        if role == PreparedInputRole::Developer
            && content
                .iter()
                .any(|part| !matches!(part, AgentMessageContent::Text { .. }))
        {
            return Err(SessionError::Invalid(
                "prepared developer input must contain only text".into(),
            ));
        }
        Ok(Self {
            role,
            attribution,
            content,
        })
    }

    /// Final model role, independent of presentation attribution.
    #[must_use]
    pub const fn role(&self) -> PreparedInputRole {
        self.role
    }

    /// Original message ID and authority-neutral metadata.
    #[must_use]
    pub const fn attribution(&self) -> &MessageAttribution {
        &self.attribution
    }

    /// Ordered final blocks; an empty slice is a valid rewrite result.
    #[must_use]
    pub fn content(&self) -> &[AgentMessageContent] {
        &self.content
    }
}

impl<'de> Deserialize<'de> for PreparedInputMessage {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            role: PreparedInputRole,
            attribution: MessageAttribution,
            content: Vec<AgentMessageContent>,
        }
        let wire = Wire::deserialize(deserializer)?;
        // MessageAttribution's decoder already validated its input.
        Self::from_validated_attribution(wire.role, wire.attribution, wire.content)
            .map_err(serde::de::Error::custom)
    }
}

/// One entry in the final ordered list; omission drops an original from model input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparedInput {
    /// Preserve an original body and attribution without copying them into this record.
    Claimed {
        /// Exact captured input coordinate.
        claim: InputClaimRef,
    },
    /// Replace the message while retaining its original claim provenance.
    Replacement {
        /// Exact captured input coordinate.
        claim: InputClaimRef,
        /// Final message; its metadata cannot change claim authority.
        message: PreparedInputMessage,
    },
    /// Additional context with no original mailbox or Tool receipt.
    Added {
        /// Final message, attributed to the enclosing preparation's producer.
        message: PreparedInputMessage,
    },
}

/// Validated final input decision, distinct from acceptance and claim consumption.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct StepInputDecision(Decision, #[serde(skip)] usize);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Decision {
    Enter { messages: Vec<PreparedInput> },
    Reject {},
}

impl StepInputDecision {
    /// Admits the final order, including an empty list or repeated coordinates.
    ///
    /// Membership belongs to the captured execution boundary, not this structural
    /// decoder. Repeated presentation does not consume an input receipt twice.
    pub fn enter(messages: Vec<PreparedInput>) -> Result<Self> {
        let bytes = crate::bounded_compact_json_len(&messages, MAXIMUM_PREPARED_INPUT_BYTES)?;
        Ok(Self(
            Decision::Enter { messages },
            bytes + r#"{"kind":"enter","messages":}"#.len(),
        ))
    }

    /// Rejects preparation without authorizing a provider request.
    #[must_use]
    pub const fn reject() -> Self {
        Self(Decision::Reject {}, r#"{"kind":"reject"}"#.len())
    }

    /// Compact UTF-8 JSON size, counted during admission.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        self.1
    }

    /// Final entries, or `None` for rejection. Empty entry is a different decision.
    #[must_use]
    pub fn messages(&self) -> Option<&[PreparedInput]> {
        match &self.0 {
            Decision::Enter { messages } => Some(messages),
            Decision::Reject {} => None,
        }
    }

    /// Coordinates whose original claim supplies content or provenance.
    pub fn claims(&self) -> impl Iterator<Item = InputClaimRef> + '_ {
        self.messages()
            .unwrap_or_default()
            .iter()
            .filter_map(|input| match input {
                PreparedInput::Claimed { claim } | PreparedInput::Replacement { claim, .. } => {
                    Some(*claim)
                }
                PreparedInput::Added { .. } => None,
            })
    }

    /// Transfers the ordered entries without cloning authored message bodies.
    #[must_use]
    pub fn into_messages(self) -> Option<Vec<PreparedInput>> {
        match self.0 {
            Decision::Enter { messages } => Some(messages),
            Decision::Reject {} => None,
        }
    }

    /// Authored blocks in durable order; indices are local to this preparation Fact.
    /// Unchanged claim entries retain their original Fact's content coordinates.
    pub fn authored_content(&self) -> impl Iterator<Item = &AgentMessageContent> {
        self.messages()
            .unwrap_or_default()
            .iter()
            .flat_map(|input| match input {
                PreparedInput::Claimed { .. } => &[][..],
                PreparedInput::Replacement { message, .. } | PreparedInput::Added { message } => {
                    message.content()
                }
            })
    }
}

impl<'de> Deserialize<'de> for StepInputDecision {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        match Decision::deserialize(deserializer)? {
            Decision::Enter { messages } => Self::enter(messages).map_err(serde::de::Error::custom),
            Decision::Reject {} => Ok(Self::reject()),
        }
    }
}
