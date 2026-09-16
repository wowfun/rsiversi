use super::{
    ActionInput, ActionTarget, Arc, BlockInput, BlockRenderer, BoxFuture, Context, Deserialize,
    Result, Serialize, SourceRef, SurfaceRenderer, UiAction, UiElement, UiError, UiView,
    controller,
};
use rsi_agent_session_protocol::{SessionFact, SessionFactBody};
use rsi_conversation::{BlockIdentity, FactField};
use rsi_retrieval_protocol::{RetrievalOperation, RetrievalResult};
use rsi_ui::UiModel;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    intent: SourceRef,
    result: SourceRef,
    index: usize,
    offset: usize,
}
fn unavailable() -> UiError {
    UiError::Invalid("This recorded web source is unavailable".into())
}
impl Entry {
    fn validate(&self) -> Result<()> {
        if self.intent.field != FactField::ToolArguments
            || self.result.field != FactField::ToolValue
            || self.intent.seq == 0
            || self.intent.seq >= self.result.seq
            || self.result.seq == u64::MAX
            || self.index >= 10
            || self.offset > rsi_retrieval_protocol::MAX_TEXT
        {
            return Err(unavailable());
        }
        Ok(())
    }
}
fn resolve(
    entry: &Entry,
    intents: &[SessionFact],
    results: &[SessionFact],
) -> Result<RetrievalResult> {
    entry.validate()?;
    let ([intent], [result]) = (intents, results) else {
        return Err(unavailable());
    };
    if intent.seq() != entry.intent.seq
        || result.seq() != entry.result.seq
        || BlockIdentity::tool(intent).map(BlockIdentity::key)
            != BlockIdentity::tool(result).map(BlockIdentity::key)
    {
        return Err(unavailable());
    }
    let SessionFactBody::ToolIntent {
        name, arguments, ..
    } = intent.body()
    else {
        return Err(unavailable());
    };
    let SessionFactBody::ToolResult { result, .. } = result.body() else {
        return Err(unavailable());
    };
    if result.is_error {
        return Err(unavailable());
    }
    let value: RetrievalResult =
        serde_json::from_value(result.value.clone()).map_err(|_| unavailable())?;
    value.validate().map_err(|_| unavailable())?;
    let field = match (name.as_str(), value.operation) {
        ("web_fetch", RetrievalOperation::Fetch) => "url",
        ("web_search", RetrievalOperation::Search) => "query",
        _ => return Err(unavailable()),
    };
    if arguments.get(field).and_then(serde_json::Value::as_str) != Some(&value.request) {
        return Err(unavailable());
    }
    Ok(value)
}
async fn recorded(target: &Context, entry: &Entry) -> Result<RetrievalResult> {
    entry.validate()?;
    let controller = controller(target)?;
    let session = target
        .lookup_local::<rsi_session_protocol::SessionContract>()
        .ok_or(UiError::Retired)?
        .attach(controller.session_id())
        .await
        .map_err(|error| UiError::Action(error.to_string()))?;
    let intent = session
        .history_before(entry.intent.seq.checked_add(1), 1)
        .await
        .map_err(|error| UiError::Action(error.to_string()))?;
    let result = session
        .history_before(entry.result.seq.checked_add(1), 1)
        .await
        .map_err(|error| UiError::Action(error.to_string()))?;
    resolve(entry, &intent.facts, &result.facts)
}
#[derive(Debug)]
pub(super) struct Renderer;
impl BlockRenderer for Renderer {
    fn render(&self, _: &Context, _: &BlockInput<'_>) -> Result<Option<UiView>> {
        Ok(None)
    }
    fn inline(
        &self,
        _: &Context,
        block: &BlockInput<'_>,
    ) -> Result<Option<Arc<dyn SurfaceRenderer>>> {
        let Some(tool) = block.tool.filter(|tool| {
            matches!(tool.name.as_deref(), Some("web_fetch" | "web_search"))
                && matches!(
                    tool.phase,
                    rsi_conversation::ToolPhase::Settled(rsi_conversation::ToolOutcome::Completed)
                )
        }) else {
            return Ok(None);
        };
        let (Some(intent), Some(result)) = (tool.arguments, tool.result) else {
            return Ok(None);
        };
        let entry = Entry {
            intent,
            result,
            index: 0,
            offset: 0,
        };
        entry.validate()?;
        Ok(Some(Arc::new(Card(entry))))
    }
}
fn button(label: &str, entry: Entry) -> UiElement {
    UiElement::Button {
        action: "retrieval".into(),
        label: label.into(),
        value: serde_json::to_value(entry).expect("bounded source coordinates"),
    }
}
fn preview(text: &str, maximum: usize) -> String {
    text[..text.floor_char_boundary(text.len().min(maximum))].to_owned()
}
fn card(entry: &Entry, result: &RetrievalResult) -> UiView {
    let mut elements = vec![
        UiElement::Text {
            text: "Recorded external sources. Reading this card does not fetch any URL.".into(),
        },
        UiElement::Field {
            label: match result.operation {
                RetrievalOperation::Fetch => "Requested URL",
                RetrievalOperation::Search => "Search query",
            }
            .into(),
            value: result.request.clone(),
        },
    ];
    if result.truncated {
        elements.push(UiElement::Text {
            text: "This retrieval was truncated; the recorded sources are incomplete.".into(),
        });
    }
    if result.omitted > 0 {
        elements.push(UiElement::Text {
            text: format!(
                "{} provider entries omitted because no usable source URL or highlight was available.",
                result.omitted
            ),
        });
    }
    if result.sources.is_empty() {
        elements.push(UiElement::Text {
            text: "No usable source highlights were returned.".into(),
        });
    }
    for (index, source) in result.sources.iter().enumerate() {
        elements.push(UiElement::Field {
            label: if source.title.is_empty() {
                format!("Source {}", index + 1)
            } else {
                source.title.clone()
            },
            value: source.url.clone(),
        });
        if let Some(date) = &source.published_at {
            elements.push(UiElement::Field {
                label: "Published".into(),
                value: date.clone(),
            });
        }
        elements.push(UiElement::Text {
            text: preview(&source.text, 800),
        });
        if source.text.len() > 800 {
            elements.push(button(
                "Read recorded source text",
                Entry {
                    index,
                    offset: 0,
                    ..entry.clone()
                },
            ));
        }
        if source.truncated {
            elements.push(UiElement::Text {
                text: "Source content was truncated during retrieval.".into(),
            });
        }
    }
    UiView {
        title: "Web sources".into(),
        elements,
    }
}
#[derive(Debug)]
struct Card(Entry);
impl SurfaceRenderer for Card {
    fn model(&self, target: Context) -> BoxFuture<'_, Result<UiModel>> {
        Box::pin(async move {
            let result = recorded(&target, &self.0).await;
            let view = match result {
                Ok(result) => card(&self.0, &result),
                Err(_) => UiView {
                    title: "Web sources".into(),
                    elements: vec![UiElement::Text {
                        text: "Recorded sources are unavailable. Reopen this card to retry.".into(),
                    }],
                },
            };
            Ok(UiModel::standard(view)?)
        })
    }
}
#[derive(Debug)]
pub(super) struct Read;
impl UiAction for Read {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(unavailable());
            }
            let entry: Entry = serde_json::from_value(input.value).map_err(|_| unavailable())?;
            entry.validate()?;
            let result = tokio::select! {biased;
                ()=target.cancelled()=>return Err(UiError::Retired),
                ()=target.view_closed()=>return Err(UiError::Retired),
                result=recorded(target.context(),&entry)=>result?,
            };
            let source = result.sources.get(entry.index).ok_or_else(unavailable)?;
            if entry.offset > source.text.len() || !source.text.is_char_boundary(entry.offset) {
                return Err(unavailable());
            }
            let end = source
                .text
                .floor_char_boundary((entry.offset + 8192).min(source.text.len()));
            let mut elements = vec![
                UiElement::Field {
                    label: source.title.clone(),
                    value: source.url.clone(),
                },
                UiElement::Text {
                    text: format!(
                        "Recorded bytes {}–{} of {}{}",
                        entry.offset,
                        end,
                        source.text.len(),
                        if source.truncated {
                            " · retrieval truncated"
                        } else {
                            ""
                        }
                    ),
                },
                UiElement::Code {
                    text: source.text[entry.offset..end].into(),
                },
            ];
            if entry.offset > 0 {
                elements.push(button(
                    "Previous source page",
                    Entry {
                        offset: source
                            .text
                            .floor_char_boundary(entry.offset.saturating_sub(8192)),
                        ..entry.clone()
                    },
                ));
            }
            if end < source.text.len() {
                elements.push(button(
                    "Next source page",
                    Entry {
                        offset: end,
                        ..entry
                    },
                ));
            }
            Ok(UiView {
                title: "Recorded web source".into(),
                elements,
            })
        })
    }
}

#[cfg(test)]
mod tests;
