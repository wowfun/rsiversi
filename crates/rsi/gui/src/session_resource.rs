use crate::{
    GuiApplication,
    application::{Result, error},
};
use futures_util::future::BoxFuture;
use std::sync::Arc;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    pane: crate::SurfaceId,
    generation: String,
    request: rsi_agent_session_protocol::SessionResourceRequest,
}

impl GuiApplication {
    /// Opens the exact summary cited by a transcript marker in the current pane.
    pub fn compaction_summary(
        self: &Arc<Self>,
        pane: crate::SurfaceId,
        generation: String,
        sequence: u64,
        stop: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'static, Result<String>> {
        self.admit_reply(move |app| async move {
            let pane = app.pane(pane)?;
            let attached = pane.attachment(&generation)?;
            let page = tokio::select! {
                biased;
                () = stop.cancelled() => return Err("Summary closed".into()),
                () = attached.stop.cancelled() => return Err("This pane changed".into()),
                result = rsi_client::read_with_capacity_retry(&app.execution, || {
                    attached.handle.history_before(sequence.checked_add(1), 1)
                }) => result.map_err(error)?,
            };
            let document = page
                .facts
                .first()
                .filter(|fact| fact.seq() == sequence)
                .and_then(|fact| crate::projection::summary_document(fact))
                .ok_or("The compaction summary is unavailable")?;
            pane.attachment(&generation)?;
            serde_json::to_string(&document).map_err(error)
        })
    }

    /// Reads a finite resource without changing the composer's preview state.
    pub fn read_session_resource(
        self: &Arc<Self>,
        source: &str,
    ) -> BoxFuture<'static, Result<String>> {
        let parsed = (|| {
            if source.len() > 8192 {
                return Err("Session resource request exceeds 8 KiB".into());
            }
            let read: Read = serde_json::from_str(source).map_err(error)?;
            read.request.validate().map_err(error)?;
            Ok(read)
        })();
        let read = match parsed {
            Ok(read) => read,
            Err(error) => return Box::pin(async { Err(error) }),
        };
        self.admit(false, None, move |app| async move {
            let pane = app.pane(read.pane)?;
            let attached = pane.attachment(&read.generation)?;
            let response = attached
                .controller
                .read_resource(read.request)
                .await
                .map_err(error)?;
            let current = pane.attachment(&read.generation)?;
            if !Arc::ptr_eq(&attached, &current) {
                return Err("Session resource attachment was replaced".into());
            }
            serde_json::to_string(&response).map_err(error)
        })
    }
}
