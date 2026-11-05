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
