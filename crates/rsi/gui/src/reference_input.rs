use super::{Arc, GuiApplication, Result, SessionId, error};
use futures_util::future::BoxFuture;
use rsi_agent_session_protocol::{FrozenReference, ReferenceReadRequest};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    pane: crate::SurfaceId,
    generation: String,
    operation: Operation,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Capture {
        source: SessionId,
    },
    Preview {
        reference: Box<FrozenReference>,
        offset: usize,
        maximum: usize,
    },
    Recorded {
        request: ReferenceReadRequest,
    },
}
impl GuiApplication {
    /// Finite human reference capture or read bound to the pane's actual generation.
    pub fn reference_input(self: &Arc<Self>, source: &str) -> BoxFuture<'static, Result<String>> {
        if source.len() > 64 * 1024 {
            return Box::pin(async { Err("Reference input exceeds 64 KiB".into()) });
        }
        let input: Input = match serde_json::from_str(source) {
            Ok(input) => input,
            Err(failure) => return Box::pin(async move { Err(error(failure)) }),
        };
        self.admit(false, Some(input.pane), move |app| async move {
            let attached = app.pane(input.pane)?.attachment(&input.generation)?;
            let output = match input.operation {
                Operation::Capture { source } => serde_json::to_string(
                    &attached
                        .handle
                        .capture_reference(source)
                        .await
                        .map_err(error)?,
                ),
                Operation::Preview {
                    reference,
                    offset,
                    maximum,
                } => serde_json::to_string(
                    &attached
                        .handle
                        .preview_reference(*reference, offset, maximum)
                        .await
                        .map_err(error)?,
                ),
                Operation::Recorded { request } => serde_json::to_string(
                    &attached
                        .handle
                        .read_recorded_reference(request)
                        .await
                        .map_err(error)?,
                ),
            }
            .map_err(error)?;
            app.pane(input.pane)?.attachment(&input.generation)?;
            Ok(output)
        })
    }
}
