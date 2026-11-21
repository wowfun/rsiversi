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
    /// Read displayable file bytes under the currently displayed Session and Header.
    pub fn transcript_file(
        self: &Arc<Self>,
        source: &str,
    ) -> BoxFuture<'static, Result<rsi_session_files::DisplayBody>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Kind {
            Image,
            Document,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Display {
            pane: crate::SurfaceId,
            generation: String,
            path: String,
            kind: Kind,
        }
        let read = (|| {
            if source.len() > 2 * rsi_files_protocol::MAXIMUM_FILE_PATH_BYTES + 1024 {
                return Err("File path exceeds its limit".to_string());
            }
            let display: Display = serde_json::from_str(source).map_err(error)?;
            let bytes = display.path.as_bytes();
            let path = match display.kind {
                Kind::Image => rsi_session_files::TranscriptFile::Image(
                    rsi_files_protocol::ReadPath::new(bytes).map_err(error)?,
                ),
                Kind::Document => rsi_session_files::TranscriptFile::Document(
                    rsi_files_protocol::RelativePath::new(bytes).map_err(error)?,
                ),
            };
            Ok((display.pane, display.generation, path))
        })();
        self.admit_reply(move |app| async move {
            let (id, generation, path) = read?;
            let pane = app.pane(id)?;
            let attached = pane.attachment(&generation)?;
            let files = attached
                .files
                .as_ref()
                .ok_or("Files are unavailable for this conversation")?;
            let target = rsi_session_protocol::SessionTarget {
                session_id: attached.id.clone(),
                header_key: attached.header.clone(),
            };
            let body = tokio::select! {
                biased;
                () = attached.stop.cancelled() => return Err("This pane changed".into()),
                result = files.display(target, path) => result.map_err(error)?,
            };
            pane.attachment(&generation)?;
            Ok(body)
        })
    }
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
                *current = attached.stop.child_token();
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
