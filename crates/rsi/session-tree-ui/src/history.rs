use super::{
    HISTORY_PAGE_FACTS, Operation, RECORD_PREVIEW_BYTES, Result, SOURCE_PAGE_BYTES,
    SOURCE_PAGE_ROWS, SessionId, TreeReader, UiElement, UiError, UiView, action_error, button,
    decimal, field,
};
use rsi_agent_session_protocol::{AgentMessageContent, SessionFact, SessionFactBody};
use rsi_ai_protocol::{ContentDelta, LanguageEvent};
use rsi_conversation::{FactField, FieldWindow, SourceRef, ToolState};
use rsi_session_protocol::SessionHandle;

pub(super) async fn read(
    reader: &TreeReader,
    execution: &rsi_meta::Execution,
    handle: &dyn SessionHandle,
    operation: Operation,
    elements: Vec<UiElement>,
) -> Result<UiView> {
    match operation {
        Operation::History {
            selected,
            before,
            watermark,
        } => {
            history_page(
                reader, execution, handle, selected, before, watermark, elements,
            )
            .await
        }
        Operation::Sources {
            selected,
            seq,
            start,
        } => fields_page(reader, execution, handle, selected, seq, start, elements).await,
        Operation::Source {
            selected,
            source,
            start,
        } => source_page(reader, execution, handle, selected, source, start, elements).await,
        Operation::Tree { .. } => unreachable!("tree operations handled before history"),
    }
}
async fn history_page(
    reader: &TreeReader,
    execution: &rsi_meta::Execution,
    handle: &dyn SessionHandle,
    selected: SessionId,
    before: Option<String>,
    watermark: Option<String>,
    mut elements: Vec<UiElement>,
) -> Result<UiView> {
    let current = rsi_client::read_with_capacity_retry(execution, || handle.inspect())
        .await
        .map_err(action_error)?;
    let watermark = watermark
        .as_deref()
        .map(decimal)
        .transpose()?
        .unwrap_or(current.durable_fact_seq);
    let end = watermark
        .checked_add(1)
        .ok_or_else(|| UiError::Invalid("History watermark exhausted".into()))?;
    let before = before.as_deref().map(decimal).transpose()?.unwrap_or(end);
    if watermark > current.durable_fact_seq || before == 0 || before > end {
        return Err(UiError::Invalid(
            "History cursor is outside its captured watermark".into(),
        ));
    }
    let page = rsi_client::read_with_capacity_retry(execution, || {
        handle.history_before(Some(before), HISTORY_PAGE_FACTS)
    })
    .await
    .map_err(action_error)?;
    elements.push(field(
        "History snapshot",
        format!(
            "Through Fact {watermark} · {} records on this page",
            page.facts.len()
        ),
    ));
    elements.push(UiElement::Text { text: "Read-only history. Pages may begin within a Turn; record previews can be shortened. Open an exact field for complete pages.".into() });
    for fact in &page.facts {
        let sources = sources(fact);
        let title = title(fact);
        let preview = preview(fact, &sources)?;
        elements.push(field(&format!("Fact {} · {title}", fact.seq()), preview));
        if !sources.is_empty() {
            elements.push(button(
                reader,
                format!("Open Fact {} fields", fact.seq()),
                Operation::Sources {
                    selected: selected.clone(),
                    seq: fact.seq().to_string(),
                    start: 0,
                },
            ));
        }
    }
    if page.has_more
        && let Some(first) = page.facts.first()
    {
        elements.push(button(
            reader,
            "Earlier history",
            Operation::History {
                selected: selected.clone(),
                before: Some(first.seq().to_string()),
                watermark: Some(watermark.to_string()),
            },
        ));
    }
    elements.push(button(
        reader,
        "Latest history",
        Operation::History {
            selected,
            before: None,
            watermark: None,
        },
    ));
    Ok(UiView {
        title: "Agent conversation".into(),
        elements,
    })
}
async fn fields_page(
    reader: &TreeReader,
    execution: &rsi_meta::Execution,
    handle: &dyn SessionHandle,
    selected: SessionId,
    seq: String,
    start: u16,
    mut elements: Vec<UiElement>,
) -> Result<UiView> {
    let fact = exact(execution, handle, decimal(&seq)?).await?;
    elements.push(field(
        "Record",
        format!("Fact {} · {}", fact.seq(), title(&fact)),
    ));
    let sources = sources(&fact);
    let start = usize::from(start);
    if start > sources.len() {
        return Err(UiError::Invalid("Field page is out of range".into()));
    }
    let end = (start + SOURCE_PAGE_ROWS).min(sources.len());
    elements.push(field(
        "Fields",
        format!(
            "{}–{} of {}",
            if start == end { 0 } else { start + 1 },
            end,
            sources.len()
        ),
    ));
    for source in &sources[start..end] {
        elements.push(button(
            reader,
            source.field.to_string(),
            Operation::Source {
                selected: selected.clone(),
                source: *source,
                start: "0".into(),
            },
        ));
    }
    if start > 0 {
        elements.push(button(
            reader,
            "Previous fields",
            Operation::Sources {
                selected: selected.clone(),
                seq: seq.clone(),
                start: u16::try_from(start.saturating_sub(SOURCE_PAGE_ROWS))
                    .expect("bounded field index"),
            },
        ));
    }
    if end < sources.len() {
        elements.push(button(
            reader,
            "More fields",
            Operation::Sources {
                selected,
                seq,
                start: u16::try_from(end).expect("bounded field index"),
            },
        ));
    }
    Ok(UiView {
        title: "Agent record fields".into(),
        elements,
    })
}
async fn source_page(
    reader: &TreeReader,
    execution: &rsi_meta::Execution,
    handle: &dyn SessionHandle,
    selected: SessionId,
    source: SourceRef,
    start: String,
    mut elements: Vec<UiElement>,
) -> Result<UiView> {
    let start = usize::try_from(decimal(&start)?)
        .map_err(|_| UiError::Invalid("Source cursor is out of range".into()))?;
    let fact = exact(execution, handle, source.seq).await?;
    let value = rsi_conversation::select_field(&fact, source)
        .ok_or_else(|| UiError::Action("Exact Agent field is unavailable".into()))?;
    let window = value
        .window(start, SOURCE_PAGE_BYTES)
        .map_err(action_error)?;
    elements.push(field(
        "Source",
        format!(
            "Fact {} · {} · bytes {}–{}",
            source.seq, source.field, window.start, window.end
        ),
    ));
    elements.push(UiElement::Code { text: window.text });
    if window.start > 0 {
        elements.push(button(
            reader,
            "Previous field page",
            Operation::Source {
                selected: selected.clone(),
                source,
                start: window.start.saturating_sub(SOURCE_PAGE_BYTES).to_string(),
            },
        ));
    }
    if window.more {
        elements.push(button(
            reader,
            "Next field page",
            Operation::Source {
                selected: selected.clone(),
                source,
                start: window.end.to_string(),
            },
        ));
    }
    elements.push(button(
        reader,
        "Record fields",
        Operation::Sources {
            selected,
            seq: source.seq.to_string(),
            start: 0,
        },
    ));
    Ok(UiView {
        title: "Agent exact source".into(),
        elements,
    })
}
async fn exact(
    execution: &rsi_meta::Execution,
    handle: &dyn SessionHandle,
    seq: u64,
) -> Result<SessionFact> {
    let before = seq
        .checked_add(1)
        .filter(|_| seq > 0)
        .ok_or_else(|| UiError::Invalid("Exact Fact cursor is out of range".into()))?;
    let page =
        rsi_client::read_with_capacity_retry(execution, || handle.history_before(Some(before), 1))
            .await
            .map_err(action_error)?;
    page.facts
        .into_iter()
        .find(|fact| fact.seq() == seq)
        .ok_or_else(|| UiError::Action("Exact Agent Fact is unavailable".into()))
}
fn title(fact: &SessionFact) -> String {
    if let Some(tool) = ToolState::from_fact(fact) {
        return tool.title();
    }
    match fact.body() {
        SessionFactBody::InputMessageEntered { .. } | SessionFactBody::TurnAccepted { .. } => {
            "Input".into()
        }
        SessionFactBody::ModelEvent { .. } => "Model output".into(),
        SessionFactBody::ModelIntent { .. } => "Model request".into(),
        SessionFactBody::ImageOutput { .. } => "Generated image".into(),
        SessionFactBody::ImageIntent { .. } => "Image request".into(),
        SessionFactBody::TurnTerminal { .. } => "Turn outcome".into(),
        SessionFactBody::MessageTurnAccepted { .. } => "Message Turn accepted".into(),
        SessionFactBody::StepStarted { .. } => "Step started".into(),
        SessionFactBody::StepEnded { .. } => "Step ended".into(),
        _ => "Session activity".into(),
    }
}
fn sources(fact: &SessionFact) -> Vec<SourceRef> {
    let fields = match fact.body() {
        SessionFactBody::TurnAccepted { .. } => vec![FactField::TurnInput],
        SessionFactBody::InputMessageEntered { content, .. } => content
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let index = u16::try_from(index).expect("bounded Session content");
                match item {
                    AgentMessageContent::Text { .. } => FactField::InputText { index },
                    AgentMessageContent::Image { .. } => FactField::InputImage { index },
                }
            })
            .collect(),
        SessionFactBody::ModelEvent { event, .. } => match event {
            LanguageEvent::ContentDelta { delta, .. } => vec![match delta {
                ContentDelta::Text(_) => FactField::ModelText,
                ContentDelta::Reasoning(_) => FactField::ModelReasoning,
                ContentDelta::ToolArguments(_) => FactField::ModelToolArguments,
            }],
            LanguageEvent::Failed { .. } => vec![FactField::ModelFailure],
            _ => vec![],
        },
        SessionFactBody::ModelIntent { .. } | SessionFactBody::ImageIntent { .. } => {
            vec![FactField::ModelSnapshot]
        }
        SessionFactBody::ToolIntent { .. } => vec![FactField::ToolArguments],
        SessionFactBody::ToolRejected { .. } => {
            vec![FactField::ToolRejection, FactField::ToolArguments]
        }
        SessionFactBody::ToolResult { result, .. } => {
            let mut fields = vec![FactField::ToolValue];
            fields.extend(result.content.iter().enumerate().map(|(index, item)| {
                let index = u16::try_from(index).expect("bounded Tool content");
                match item {
                    rsi_tools_protocol::ToolContent::Text { .. } => FactField::ToolText { index },
                    rsi_tools_protocol::ToolContent::Image { .. } => FactField::ToolImage { index },
                }
            }));
            fields
        }
        SessionFactBody::TurnTerminal { .. } => vec![FactField::TurnOutcome],
        SessionFactBody::ImageOutput { .. } => vec![FactField::ImageOutput],
        _ => vec![],
    };
    fields
        .into_iter()
        .map(|field| SourceRef {
            seq: fact.seq(),
            field,
        })
        .collect()
}

fn preview(fact: &SessionFact, sources: &[SourceRef]) -> Result<String> {
    Ok(if let Some(source) = sources.first() {
        let window = rsi_conversation::select_field(fact, *source)
            .expect("selected matching Fact field")
            .window(0, RECORD_PREVIEW_BYTES)
            .map_err(action_error)?;
        let safe = rsi_tools_protocol::safe_tool_text(window.text.as_bytes());
        let safe =
            FieldWindow::text(&safe, 0, RECORD_PREVIEW_BYTES).expect("bounded record preview");
        format!(
            "{}{}",
            safe.text,
            if window.more || safe.more { " …" } else { "" }
        )
    } else {
        String::new()
    })
}
