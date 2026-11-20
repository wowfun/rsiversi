//! Borrowed presentation of one final user message.
use crate::{FactField, SourceRef};
use rsi_agent_session_protocol::{AgentMessageContent, InputMessageSource};
use rsi_media_protocol::MediaRef;

/// Contents of an entered input, still borrowing the owning Fact.
#[derive(Clone, Copy, Debug)]
pub enum EnteredContent<'a> {
    /// Direct Turn text.
    Direct(&'a str),
    /// Ordinary or authored message blocks.
    Blocks(&'a [AgentMessageContent]),
    /// A message within a Tool context batch.
    Tool(&'a [String]),
}

/// One displayed content span and its exact detail source.
#[derive(Clone, Copy, Debug)]
pub enum InputPart<'a> {
    /// User text or a recorded reference preview.
    Text(&'a str),
    /// An immutable image identity.
    Image(&'a MediaRef),
    /// An immutable verbatim file identity.
    File(&'a rsi_media_protocol::FileRef),
}

/// A row in final input order. The original source can be from an older Fact.
#[derive(Clone, Copy, Debug)]
pub struct EnteredInput<'a> {
    /// Index within the entire prepared list, including hidden guidance.
    pub index: usize,
    /// Original input provenance, when the claim carries one.
    pub source: Option<&'a InputMessageSource>,
    /// Fact that owns the text or media.
    pub seq: u64,
    /// First content index within that Fact.
    pub offset: u32,
    /// Ordered borrowed message.
    pub content: EnteredContent<'a>,
}

impl<'a> EnteredInput<'a> {
    /// Participant label for an attributed message, when it is not the local user.
    pub fn attribution(&self) -> Option<std::borrow::Cow<'static, str>> {
        self.presentation().1
    }

    /// Display role and participant label derived from the recorded input source.
    pub fn presentation(&self) -> (&'static str, Option<std::borrow::Cow<'static, str>>) {
        let (role, title) = match self.source {
            Some(InputMessageSource::Agent {
                source_session_id, ..
            }) => (
                "trigger-agent",
                format!("Message from {source_session_id}").into(),
            ),
            Some(InputMessageSource::Completion {
                child_session_id, ..
            }) => (
                "trigger-completion",
                format!("Completion from {child_session_id}").into(),
            ),
            Some(InputMessageSource::Continuation { source, .. }) => match source.domain.id() {
                "rsi.schedule" => ("trigger-schedule", "Automation task".into()),
                "rsi.goal" => ("trigger-goal", "Goal continuation".into()),
                _ => ("trigger-request", "Automatic continuation".into()),
            },
            Some(InputMessageSource::RuntimeContext) => ("context", "Runtime context".into()),
            Some(InputMessageSource::Command { source, .. })
                if source.command.as_str() == "rsi.schedule.delete" =>
            {
                ("step-context", "Automation task".into())
            }
            _ => return ("user", None),
        };
        (role, Some(title))
    }

    /// Iterates blocks once without re-reading or copying their payloads.
    pub fn parts(&self) -> impl Iterator<Item = (SourceRef, InputPart<'a>)> + '_ {
        let mut next = 0;
        std::iter::from_fn(move || {
            let index = self.offset + u32::try_from(next).ok()?;
            let (field, value) = match self.content {
                EnteredContent::Direct(text) if next == 0 => {
                    (FactField::TurnInput, InputPart::Text(text))
                }
                EnteredContent::Direct(_) => return None,
                EnteredContent::Tool(text) => (
                    FactField::InputText { index },
                    InputPart::Text(text.get(next)?),
                ),
                EnteredContent::Blocks(blocks) => {
                    let block = blocks.get(next)?;
                    let part = match block {
                        AgentMessageContent::Text { text } => InputPart::Text(text),
                        AgentMessageContent::Reference { reference } => {
                            InputPart::Text(&reference.preview)
                        }
                        AgentMessageContent::Image { media, .. } => InputPart::Image(media),
                        AgentMessageContent::File { file } => InputPart::File(file),
                    };
                    (FactField::input(index, block), part)
                }
            };
            next += 1;
            Some((
                SourceRef {
                    seq: self.seq,
                    field,
                },
                value,
            ))
        })
    }
}
