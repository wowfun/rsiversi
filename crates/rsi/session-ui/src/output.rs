use crate::{OUTPUT_PAGE_BYTES, controller};
use futures_util::future::BoxFuture;
use rsi_conversation::ToolState;
use rsi_meta::Context;
use rsi_process::{OutputPage, ProcessOutputCacheContract};
use rsi_ui::{ActionInput, ActionTarget, Result, UiAction, UiElement, UiError, UiView};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stream {
    Stdout,
    Stderr,
}
impl Stream {
    fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    stream: Stream,
    offset: String,
    hex: bool,
}
impl Request {
    fn offset(&self) -> Result<u64> {
        rsi_process::validate_output_read(&self.id, OUTPUT_PAGE_BYTES)
            .map_err(|error| UiError::Invalid(error.to_string()))?;
        self.offset
            .parse::<u64>()
            .ok()
            .filter(|offset| *offset <= rsi_process::MAXIMUM_COMPLETED_OUTPUT_BYTES)
            .ok_or_else(|| UiError::Invalid("invalid output offset".into()))
    }
}
fn button(label: &str, id: &str, stream: Stream, offset: u64, hex: bool) -> UiElement {
    UiElement::Button {
        action: "output".into(),
        label: label.into(),
        value: serde_json::to_value(Request {
            id: id.into(),
            stream,
            offset: offset.to_string(),
            hex,
        })
        .expect("closed output payload"),
    }
}
pub(crate) fn buttons(target: &Context, tool: &ToolState, elements: &mut Vec<UiElement>) {
    if target
        .lookup_local::<ProcessOutputCacheContract>()
        .is_none()
    {
        return;
    }
    for (stream, output) in [Stream::Stdout, Stream::Stderr]
        .into_iter()
        .zip(&tool.outputs)
    {
        if let Some(output) = output {
            elements.push(button(
                &format!("Read {}", stream.label()),
                output.as_str(),
                stream,
                0,
                false,
            ));
        }
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
                .lookup_local::<ProcessOutputCacheContract>()
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
fn view(request: &Request, page: &OutputPage) -> UiView {
    let mut elements = vec![
        UiElement::Field {
            label: "Cache identity".into(),
            value: page.id.clone(),
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
            &page.id,
            request.stream,
            page.offset,
            !request.hex,
        ),
    ];
    if page.offset > 0 {
        elements.push(button(
            "Previous page",
            &page.id,
            request.stream,
            page.offset.saturating_sub(OUTPUT_PAGE_BYTES as u64),
            request.hex,
        ));
    }
    if page.next_offset < page.total_bytes {
        elements.push(button(
            "Next page",
            &page.id,
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
    fn output_payloads_reject_paths_and_excess_offsets_and_pages_fit_cards() {
        let mut request = Request {
            id: "0".repeat(32),
            stream: Stream::Stderr,
            offset: "0".into(),
            hex: false,
        };
        assert_eq!(request.offset().unwrap(), 0);
        let page = OutputPage {
            id: request.id.clone(),
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
        request.id = "../output".into();
        assert!(request.offset().is_err());
    }
}
