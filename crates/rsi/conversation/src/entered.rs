//! Borrowed presentation of one final user message.
use crate::{FactField, SourceRef};
use rsi_agent_session_protocol::{AgentMessageContent, InputMessageSource};
use rsi_media_protocol::{ImagePresentation, MediaRef};

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
    /// An immutable image identity and the publisher's optional presentation for this occurrence.
    Image(&'a MediaRef, Option<&'a ImagePresentation>),
    /// An immutable verbatim file identity.
    File(&'a rsi_media_protocol::FileRef),
}

/// Origin of the message that actually entered the Step.
#[derive(Clone, Copy, Debug)]
pub enum EnteredSource<'a> {
    /// Direct Turn text.
    Direct,
    /// An unchanged native claim.
    Claim(&'a InputMessageSource),
    /// Original application source of an authored message or Tool context.
    Application(&'a serde_json::Value),
}

fn application_presentation<'a>(
    source: &'a serde_json::Value,
    context_role: &'static str,
) -> (&'static str, Option<std::borrow::Cow<'a, str>>) {
    match source.get("kind").and_then(serde_json::Value::as_str) {
        Some("user") => ("user", None),
        Some(kind) if !kind.is_empty() => (context_role, Some(kind.into())),
        _ => (context_role, Some("Context".into())),
    }
}

/// A row in final input order. The original source can be from an older Fact.
#[derive(Clone, Copy, Debug)]
pub struct EnteredInput<'a> {
    /// Index within the entire prepared list, including hidden guidance.
    pub index: usize,
    /// Presentation provenance of the final message.
    pub source: EnteredSource<'a>,
    /// Fact that owns the text or media.
    pub seq: u64,
    /// First content index within that Fact.
    pub offset: u32,
    /// Ordered borrowed message.
    pub content: EnteredContent<'a>,
}

impl<'a> EnteredInput<'a> {
    /// Whether the final message is attributed to the human composer.
    pub fn is_human(&self) -> bool {
        let source = match self.source {
            EnteredSource::Direct
            | EnteredSource::Claim(InputMessageSource::Human {
                attribution: None, ..
            }) => return true,
            EnteredSource::Application(source) => source,
            EnteredSource::Claim(InputMessageSource::PluginContext {
                tool: Some(tool), ..
            }) => &tool.source,
            EnteredSource::Claim(
                InputMessageSource::Human {
                    attribution: Some(attribution),
                    ..
                }
                | InputMessageSource::Application {
                    attribution: Some(attribution),
                    ..
                },
            ) => &attribution.source,
            EnteredSource::Claim(InputMessageSource::Command { source, .. }) => {
                return source
                    .attribution
                    .as_ref()
                    .is_some_and(|a| a.source["kind"] == "user");
            }
            EnteredSource::Claim(_) => return false,
        };
        source["kind"] == "user"
    }

    /// Display role and participant label derived from the recorded input source.
    pub fn presentation(&self) -> (&'static str, Option<std::borrow::Cow<'a, str>>) {
        use EnteredSource::{Application, Claim};
        let (role, title) = match self.source {
            Claim(InputMessageSource::Agent {
                source_session_id, ..
            }) => (
                "trigger-agent",
                format!("Message from {source_session_id}").into(),
            ),
            Claim(InputMessageSource::Completion {
                child_session_id, ..
            }) => (
                "trigger-completion",
                format!("Completion from {child_session_id}").into(),
            ),
            Claim(InputMessageSource::Continuation { source, .. }) => match source.domain.id() {
                "rsi.schedule" => ("trigger-schedule", "Automation task".into()),
                "rsi.goal" => ("trigger-goal", "Goal continuation".into()),
                _ => ("trigger-request", "Automatic continuation".into()),
            },
            Claim(InputMessageSource::RuntimeContext) => ("step-context", "Runtime context".into()),
            Claim(InputMessageSource::Command { source, .. })
                if source.command.as_str() == "rsi.schedule.run" =>
            {
                ("trigger-schedule", "Automation task".into())
            }
            Claim(InputMessageSource::Command { source, .. })
                if source.command.as_str() == "rsi.schedule.delete" =>
            {
                ("step-context", "Automation task".into())
            }
            Claim(InputMessageSource::PluginContext {
                tool: Some(rsi_agent_session_protocol::ToolContextOrigin { source, .. }),
                ..
            })
            | Application(source) => return application_presentation(source, "step-context"),
            Claim(InputMessageSource::Application {
                attribution: Some(rsi_agent_session_protocol::MessageAttribution { source, .. }),
                ..
            }) => return application_presentation(source, "context"),
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
                        AgentMessageContent::Image {
                            media,
                            presentation,
                        } => InputPart::Image(media, presentation.as_deref()),
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
