use crate::{OUTPUT_PAGE_BYTES, controller};
use futures_util::future::BoxFuture;
use rsi_conversation::{SourceRef, ToolState};
use rsi_meta::Context;
use rsi_spill::{SpillLocator, SpillPage, SpillReaderContract};
use rsi_ui::{ActionInput, ActionTarget, Result, UiAction, UiElement, UiError, UiView};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stream {
    Stdout,
    Stderr,
    Text,
}
impl Stream {
    fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Text => "output",
        }
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: SpillLocator,
    stream: Stream,
    offset: String,
    hex: bool,
}
impl Request {
    fn offset(&self) -> Result<u64> {
        self.offset
            .parse::<u64>()
            .ok()
            .filter(|offset| *offset <= rsi_spill::MAXIMUM_SPILL_BYTES)
            .ok_or_else(|| UiError::Invalid("invalid output offset".into()))
    }
}
fn button(label: &str, id: &SpillLocator, stream: Stream, offset: u64, hex: bool) -> UiElement {
    UiElement::Button {
        action: "output".into(),
        label: label.into(),
        value: serde_json::to_value(Request {
            id: id.clone(),
            stream,
            offset: offset.to_string(),
            hex,
        })
        .expect("closed output payload"),
    }
}
pub(crate) fn buttons(target: &Context, tool: &ToolState, elements: &mut Vec<UiElement>) {
    if target.lookup_local::<SpillReaderContract>().is_none() {
        return;
    }
    for (stream, output) in [Stream::Stdout, Stream::Stderr]
        .into_iter()
        .zip(&tool.outputs)
    {
        if let Some(output) = output {
            elements.push(button(
                &format!("Read {}", stream.label()),
                output.locator(),
                stream,
                0,
                false,
            ));
        }
    }
    reference_buttons(tool, 0, elements);
}
const REFERENCES_PER_PAGE: usize = 32;
fn reference_buttons(tool: &ToolState, start: usize, elements: &mut Vec<UiElement>) {
    let references: Vec<_> = tool
        .spills
        .iter()
        .filter(|output| {
            !tool
                .outputs
                .iter()
                .flatten()
                .any(|stream| stream == *output)
        })
        .collect();
    for (index, output) in references
        .iter()
        .enumerate()
        .skip(start)
        .take(REFERENCES_PER_PAGE)
    {
        elements.push(button(
            &format!("Read output {}", index + 1),
            output.locator(),
            Stream::Text,
            0,
            false,
        ));
    }
    if let Some(source) = tool.result {
        if start > 0 {
            elements.push(reference_button(
                "Previous outputs",
                source,
                start.saturating_sub(REFERENCES_PER_PAGE),
            ));
        }
        if start + REFERENCES_PER_PAGE < references.len() {
            elements.push(reference_button(
                "More outputs",
                source,
                start + REFERENCES_PER_PAGE,
            ));
        }
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReferencePage {
    source: SourceRef,
    start: usize,
}
fn reference_button(label: &str, source: SourceRef, start: usize) -> UiElement {
    UiElement::Button {
        action: "outputs".into(),
        label: label.into(),
        value: serde_json::to_value(ReferencePage { source, start }).expect("source coordinates"),
    }
}
#[derive(Debug)]
pub(crate) struct ReadReferences;
impl UiAction for ReadReferences {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            let request: ReferencePage =
                serde_json::from_value(input.value).map_err(|e| UiError::Invalid(e.to_string()))?;
            if !input.fields.is_empty()
                || request.source.seq == 0
                || request.source.seq == u64::MAX
                || request.source.field != rsi_conversation::FactField::ToolValue
                || request.start >= rsi_tools_protocol::MAXIMUM_TOOL_CONTENT_ITEMS
                || !request.start.is_multiple_of(REFERENCES_PER_PAGE)
            {
                return Err(UiError::Invalid("invalid output reference page".into()));
            }
            let read = async {
                let controller = controller(target.context())?;
                let session = target
                    .context()
                    .lookup_local::<rsi_session_protocol::SessionContract>()
                    .ok_or(UiError::Retired)?
                    .attach(controller.session_id())
                    .await
                    .map_err(|e| UiError::Action(e.to_string()))?;
                let page = session
                    .history_before(Some(request.source.seq + 1), 1)
                    .await
                    .map_err(|e| UiError::Action(e.to_string()))?;
                let fact = page
                    .facts
                    .first()
                    .filter(|fact| fact.seq() == request.source.seq)
                    .ok_or(UiError::Retired)?;
                if !matches!(
                    fact.body(),
                    rsi_agent_session_protocol::SessionFactBody::ToolResult { .. }
                ) {
                    return Err(UiError::Retired);
                }
                let tool = ToolState::from_fact(fact).ok_or(UiError::Retired)?;
                let mut elements = Vec::new();
                reference_buttons(&tool, request.start, &mut elements);
                Ok(UiView {
                    title: "Tool outputs".into(),
                    elements,
                })
            };
            tokio::select! { biased;
                () = target.cancelled() => Err(UiError::Retired),
                () = target.view_closed() => Err(UiError::Retired),
                result = read => result,
            }
        })
    }
}
#[derive(Debug)]
pub(crate) struct ReadOutput;
impl UiAction for ReadOutput {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(UiError::Invalid(
                    "output reading accepts no form fields".into(),
                ));
            }
            let request: Request = serde_json::from_value(input.value)
                .map_err(|error| UiError::Invalid(error.to_string()))?;
            let offset = request.offset()?;
            let _controller = controller(target.context())?;
            let cache = target
                .context()
                .lookup_local::<SpillReaderContract>()
                .ok_or_else(|| UiError::Action("Completed output reader is unavailable".into()))?;
            let page = tokio::select! { biased;
                () = target.cancelled() => return Err(UiError::Retired),
                () = target.view_closed() => return Err(UiError::Retired),
                result = cache.read(&request.id, offset, OUTPUT_PAGE_BYTES) => result.map_err(|error| UiError::Action(format!("Completed {} read failed: {error}", request.stream.label())))?,
            };
            Ok(view(&request, &page))
        })
    }
}
fn view(request: &Request, page: &SpillPage) -> UiView {
    let mut elements = vec![
        UiElement::Field {
            label: "Cache identity".into(),
            value: page.locator.to_string(),
        },
        UiElement::Field {
            label: "Bytes".into(),
            value: format!(
                "{}–{} of {}",
                page.offset, page.next_offset, page.total_bytes
            ),
        },
        UiElement::Code {
            text: if request.hex {
                rsi_conversation::hex_window(&page.bytes, page.offset)
                    .expect("bounded Process page")
            } else {
                rsi_tools_protocol::safe_tool_text(&page.bytes)
            },
        },
        button(
            if request.hex {
                "View text"
            } else {
                "View exact hex"
            },
            &page.locator,
            request.stream,
            page.offset,
            !request.hex,
        ),
    ];
    if page.offset > 0 {
        elements.push(button(
            "Previous page",
            &page.locator,
            request.stream,
            page.offset.saturating_sub(OUTPUT_PAGE_BYTES as u64),
            request.hex,
        ));
    }
    if page.next_offset < page.total_bytes {
        elements.push(button(
            "Next page",
            &page.locator,
            request.stream,
            page.next_offset,
            request.hex,
        ));
    }
    UiView {
        title: format!("Completed {}", request.stream.label()),
        elements,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_legal_output_references_are_reachable_through_bounded_pages() {
        use rsi_agent_session_protocol::{EffectId, SessionFact, SessionFactBody, TurnId};
        use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
        let mut result = ToolResult::new(serde_json::json!({}), vec![], false).unwrap();
        for index in 0u16..256 {
            let locator = SpillLocator::new(
                &rsi_spill::SpillScope::new("session", "many"),
                u128::from(index).to_be_bytes(),
            );
            result
                .spills
                .push(rsi_spill::SpillRef { locator, bytes: 1 });
        }
        let fact = SessionFact::new(
            4,
            1,
            SessionFactBody::ToolResult {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
                identity: ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64))
                    .unwrap(),
                result,
                conclusion: None,
            },
        )
        .unwrap();
        let tool = ToolState::from_fact(&fact).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for start in (0..256).step_by(REFERENCES_PER_PAGE) {
            let mut elements = vec![
                UiElement::Field {
                    label: "Tool".into(),
                    value: "many".into()
                };
                8
            ];
            reference_buttons(&tool, start, &mut elements);
            for element in &elements {
                if let UiElement::Button { action, value, .. } = element {
                    if action == "output" {
                        assert!(seen.insert(value["id"].as_str().unwrap().to_owned()));
                    }
                    if action == "outputs" {
                        let page: ReferencePage = serde_json::from_value(value.clone()).unwrap();
                        assert_eq!(page.source.seq, fact.seq());
                    }
                }
            }
            UiView {
                title: "Tool".into(),
                elements,
            }
            .validate()
            .unwrap();
        }
        assert_eq!(seen.len(), 256);
    }
    #[test]
    fn output_payloads_reject_paths_and_excess_offsets_and_pages_fit_cards() {
        let mut request = Request {
            id: SpillLocator::new(&rsi_spill::SpillScope::new("test", "ui"), [0; 16]),
            stream: Stream::Stderr,
            offset: "0".into(),
            hex: false,
        };
        assert_eq!(request.offset().unwrap(), 0);
        let page = SpillPage {
            locator: request.id.clone(),
            offset: 0,
            next_offset: OUTPUT_PAGE_BYTES as u64,
            total_bytes: OUTPUT_PAGE_BYTES as u64 * 2,
            bytes: vec![0xff; OUTPUT_PAGE_BYTES].into(),
        };
        for hex in [false, true] {
            request.hex = hex;
            let view = view(&request, &page);
            assert!(serde_json::to_vec(&view).unwrap().len() <= rsi_ui::MAXIMUM_VIEW_BYTES);
            let UiElement::Code { text } = &view.elements[2] else {
                panic!("byte view")
            };
            assert!(!text.contains('\u{1b}'));
        }
        request.offset = u64::MAX.to_string();
        assert!(request.offset().is_err());
        request.offset = "0".into();
        let mut invalid = serde_json::to_value(&request).unwrap();
        invalid["id"] = "../output".into();
        assert!(serde_json::from_value::<Request>(invalid).is_err());
    }
}
