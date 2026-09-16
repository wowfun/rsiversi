use super::{Arc, GuiApplication, Result, error};
use futures_util::future::BoxFuture;
use rsi_session_files_ui::FilePickerRequest;
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    pane: crate::SurfaceId,
    generation: String,
    request: Option<FilePickerRequest>,
}
impl GuiApplication {
    /// Finite `SessionFiles` picker read, or cancellation, for the actual pane generation.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned the file-read cancellation lock.
    pub fn file_input(self: &Arc<Self>, source: &str) -> BoxFuture<'static, Result<String>> {
        if source.len() > 128 * 1024 {
            return Box::pin(async { Err("File picker request exceeds 128 KiB".into()) });
        }
        let input: Input = match serde_json::from_str(source) {
            Ok(value) => value,
            Err(failure) => return Box::pin(async move { Err(error(failure)) }),
        };
        self.admit(false, Some(input.pane), move |app| async move {
            let attached = app.pane(input.pane)?.attachment(&input.generation)?;
            let stop = {
                let mut current = attached
                    .file_read
                    .lock()
                    .expect("File picker cancellation poisoned");
                current.cancel();
                *current = tokio_util::sync::CancellationToken::new();
                current.clone()
            };
            let Some(request) = input.request else {
                return Ok("null".into());
            };
            let _cancel = stop.clone().drop_guard();
            let browser = attached
                .files
                .as_ref()
                .ok_or("Files are unavailable for this conversation")?;
            let page = browser.pick(request, stop).await.map_err(error)?;
            app.pane(input.pane)?.attachment(&input.generation)?;
            serde_json::to_string(&page).map_err(error)
        })
    }
}
