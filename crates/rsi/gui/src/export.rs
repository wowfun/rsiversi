use super::{Arc, GuiApplication, Result, error};
use futures_util::{StreamExt, future::BoxFuture};
use rsi_session_protocol::export::{ExportEvent, ExportStream, ExportVerifier};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

pub(super) struct Active {
    token: String,
    source: ExportStream,
}
impl std::fmt::Debug for Active {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveExport").finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub(super) struct State {
    active: tokio::sync::Mutex<Option<Active>>,
    cancel: std::sync::Mutex<Option<(String, CancellationToken)>>,
    work: Arc<tokio::sync::Semaphore>,
    stop: CancellationToken,
}
impl Default for State {
    fn default() -> Self {
        Self {
            active: tokio::sync::Mutex::new(None),
            cancel: std::sync::Mutex::new(None),
            work: Arc::new(tokio::sync::Semaphore::new(1)),
            stop: CancellationToken::new(),
        }
    }
}
impl State {
    pub(super) async fn close(&self) {
        self.stop.cancel();
        self.active.lock().await.take();
    }
}
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
    Open { arguments: String },
    Next { token: String },
    Cancel { token: String },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    pane: crate::SurfaceId,
    generation: String,
    arguments: String,
}

impl GuiApplication {
    /// Opens one native save stream for a validated current pane. No path authority is accepted.
    pub fn open_export(
        self: &Arc<Self>,
        source: &str,
    ) -> BoxFuture<'static, Result<(ExportStream, String)>> {
        let input = if source.len() <= 16 * 1024 {
            serde_json::from_str::<Open>(source).map_err(error)
        } else {
            Err("Export input exceeds its bound".into())
        };
        let app = self.clone();
        Box::pin(async move {
            let input = input?;
            app.export_stream(input.pane, &input.generation, &input.arguments)
                .await
        })
    }
    async fn export_stream(
        self: &Arc<Self>,
        pane: crate::SurfaceId,
        generation: &str,
        arguments: &str,
    ) -> Result<(ExportStream, String)> {
        let command = rsi_client::parse_export_arguments(arguments).map_err(error)?;
        let attached = self.pane(pane)?.attachment(generation)?;
        let permit = attached
            .export
            .work
            .clone()
            .try_acquire_owned()
            .map_err(|_| "An export is already active for this pane")?;
        let header = attached.handle.header().await.map_err(error)?;
        let header_hash = header.fingerprint().map_err(error)?;
        let options = command.options;
        let mut source = attached
            .handle
            .export(options.clone())
            .await
            .map_err(error)?;
        self.pane(pane)?.attachment(generation)?;
        let name = command
            .path
            .as_deref()
            .and_then(|path| path.rsplit(['/', '\\']).next())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .map_or_else(
                || rsi_session_protocol::export::default_filename(&attached.id, options.format),
                str::to_owned,
            );
        if name.len() > 256 {
            return Err("Export filename exceeds 256 bytes".into());
        }
        let stop = attached.export.stop.clone();
        let application_stop = self.stop.clone();
        let id = attached.id.clone();
        let source = Box::pin(async_stream::try_stream! {
            let _permit = permit;
            let mut verifier = ExportVerifier::default();
            loop {
                let item = tokio::select! { biased;
                    () = stop.cancelled() => Some(Err(rsi_session_protocol::SessionError::ShuttingDown)),
                    () = application_stop.cancelled() => Some(Err(rsi_session_protocol::SessionError::ShuttingDown)),
                    item = source.next() => item,
                };
                let Some(item) = item else { break; };
                let item = item?;
                verifier.accept(&item, Some((&id,&header_hash,&options)))?;
                yield item;
            }
            verifier.finish()?;
        });
        Ok((source, name))
    }
    /// Pulls bounded export events; the document never receives an unrestricted Session selector.
    pub fn export_input(self: &Arc<Self>, source: &str) -> BoxFuture<'static, Result<String>> {
        let input = if source.len() <= 16 * 1024 {
            serde_json::from_str::<Input>(source).map_err(error)
        } else {
            Err("Export input exceeds its bound".into())
        };
        let app = self.clone();
        Box::pin(async move {
            let input = input?;
            let attached = app.pane(input.pane)?.attachment(&input.generation)?;
            match input.operation {
                Operation::Open { arguments } => {
                    let (mut source, filename) = app
                        .export_stream(input.pane, &input.generation, &arguments)
                        .await?;
                    let first = source
                        .next()
                        .await
                        .ok_or("Export has no start")?
                        .map_err(error)?;
                    if !matches!(&first, ExportEvent::Start { .. }) {
                        return Err("Export has no start".into());
                    }
                    app.pane(input.pane)?.attachment(&input.generation)?;
                    let token = rsi_ui::fresh_identity("export")?;
                    let cancellation = CancellationToken::new();
                    *attached.export.cancel.lock().map_err(error)? =
                        Some((token.clone(), cancellation.clone()));
                    let source = Box::pin(async_stream::try_stream! {
                        loop {
                            let item = tokio::select! { biased;
                                () = cancellation.cancelled() => Some(Err(rsi_session_protocol::SessionError::ShuttingDown)),
                                item = source.next() => item,
                            };
                            let Some(item) = item else { break; };
                            yield item?;
                        }
                    });
                    *attached.export.active.lock().await = Some(Active {
                        token: token.clone(),
                        source,
                    });
                    serde_json::to_string(
                        &serde_json::json!({"token":token,"filename":filename,"event":first}),
                    )
                    .map_err(error)
                }
                Operation::Next { token } | Operation::Cancel { token } if token.len() > 256 => {
                    Err("Invalid export token".into())
                }
                Operation::Cancel { token } => {
                    if let Some((id, cancellation)) =
                        attached.export.cancel.lock().map_err(error)?.as_ref()
                        && *id == token
                    {
                        cancellation.cancel();
                    }
                    let mut active = attached.export.active.lock().await;
                    if active.as_ref().is_some_and(|value| value.token == token) {
                        active.take();
                    }
                    Ok("null".into())
                }
                Operation::Next { token } => {
                    let mut active = attached
                        .export
                        .active
                        .try_lock()
                        .map_err(|_| "Export read already pending")?;
                    let value = active
                        .as_mut()
                        .filter(|value| value.token == token)
                        .ok_or("Export is no longer active")?;
                    let next = value.source.next().await;
                    if next.is_none() || next.as_ref().is_some_and(std::result::Result::is_err) {
                        active.take();
                    }
                    let event = next.transpose().map_err(error)?;
                    app.pane(input.pane)?.attachment(&input.generation)?;
                    serde_json::to_string(&event).map_err(error)
                }
            }
        })
    }
}
