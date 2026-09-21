use super::{
    ActionInput, ActionTarget, Arc, BlockInput, BlockRenderer, BoxFuture, CancellationToken,
    Context, Result, SurfaceRenderer, UiAction, UiElement, UiError, UiView, controller,
};
use rsi_agent_session_protocol::{ReferenceReadRequest, ReferenceTextPage};
use rsi_conversation::FactField;
use rsi_ui::UiModel;

#[derive(Debug)]
pub(super) struct Renderer;
impl BlockRenderer for Renderer {
    fn render(&self, _: &Context, _: &BlockInput<'_>) -> Result<Option<UiView>> {
        Ok(None)
    }
    fn inline(
        &self,
        target: &Context,
        block: &BlockInput<'_>,
    ) -> Result<Option<Arc<dyn SurfaceRenderer>>> {
        let sources: Vec<_> = block
            .sources
            .iter()
            .filter(|source| matches!(source.field, FactField::InputReference { .. }))
            .take(4)
            .collect();
        if sources.is_empty() {
            return Ok(None);
        }
        let session = controller(target)?.session_id().clone();
        Ok(Some(Arc::new(Card(
            sources
                .into_iter()
                .map(|source| {
                    let FactField::InputReference { index } = source.field else {
                        unreachable!()
                    };
                    ReferenceReadRequest {
                        recorded_session_id: session.clone(),
                        fact_seq: source.seq,
                        content_index: usize::from(index),
                        offset: 0,
                        maximum: 8192,
                    }
                })
                .collect(),
        ))))
    }
}
#[derive(Debug)]
struct Card(Vec<ReferenceReadRequest>);
impl SurfaceRenderer for Card {
    fn model(&self, target: Context) -> BoxFuture<'_, Result<UiModel>> {
        Box::pin(async move {
            let controller = controller(&target)?;
            let stop = CancellationToken::new();
            let _cancel = stop.clone().drop_guard();
            let mut elements = Vec::new();
            for request in &self.0 {
                let page = controller
                    .read_recorded_reference(request.clone(), stop.clone())
                    .await
                    .map_err(|error| UiError::Action(error.to_string()))?;
                elements.extend(view(page)?.elements);
            }
            Ok(UiModel::standard(UiView {
                title: "Frozen conversation references".into(),
                elements,
            })?)
        })
    }
}
fn button(label: &str, request: ReferenceReadRequest) -> UiElement {
    UiElement::Button {
        action: "reference".into(),
        label: label.into(),
        value: serde_json::to_value(request).expect("bounded reference coordinates"),
    }
}
fn view(page: ReferenceTextPage) -> Result<UiView> {
    let mut request = page
        .recorded
        .clone()
        .ok_or_else(|| UiError::Invalid("Recorded reference coordinates are missing".into()))?;
    let meta = &page.reference.metadata;
    let mut elements = vec![
        UiElement::Field {
            label: "Source conversation".into(),
            value: meta.source.to_string(),
        },
        UiElement::Field {
            label: "Captured interval".into(),
            value: format!(
                "Through record {} · retained records {}–{}",
                meta.through_seq(),
                meta.retained_interval().0,
                meta.retained_interval().1
            ),
        },
        UiElement::Field {
            label: "Omitted material".into(),
            value: if meta.omissions().is_empty() {
                "None within the captured conversation".into()
            } else {
                format!("{:?}", meta.omissions())
            },
        },
        UiElement::Field {
            label: "Frozen text bytes".into(),
            value: format!(
                "{}–{} of {}",
                page.offset, page.next_offset, meta.text_bytes
            ),
        },
        UiElement::Code { text: page.text },
    ];
    if page.offset > 0 {
        request.offset = page.offset.saturating_sub(8192);
        elements.push(button("Previous reference page", request.clone()));
    }
    if page.has_more {
        request.offset = page.next_offset;
        elements.push(button("Next reference page", request));
    }
    Ok(UiView {
        title: "Frozen conversation reference".into(),
        elements,
    })
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
                return Err(UiError::Invalid(
                    "Reference reads do not accept form fields".into(),
                ));
            }
            let mut request: ReferenceReadRequest = serde_json::from_value(input.value)
                .map_err(|error| UiError::Invalid(error.to_string()))?;
            request
                .validate()
                .map_err(|error| UiError::Invalid(error.to_string()))?;
            request.maximum = 8192;
            let controller = controller(target.context())?;
            let stop = CancellationToken::new();
            let _cancel = stop.clone().drop_guard();
            let page = tokio::select! {biased;
                () = target.cancelled() => return Err(UiError::Retired),
                () = target.view_closed() => return Err(UiError::Retired),
                result = controller.read_recorded_reference(request,stop) => result.map_err(|error|UiError::Action(error.to_string()))?,
            };
            view(page)
        })
    }
}
