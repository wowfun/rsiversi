use super::SessionController;
use futures_util::future::BoxFuture;
use rsi_conversation::{FieldWindow, SourceRef, WindowError};
use rsi_session_protocol::SessionError;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Exact-source lookup failure, independent of renderer presentation.
#[derive(Debug, thiserror::Error)]
pub enum SourceReadError {
    /// Presentation or controller retirement cancelled this read.
    #[error("source read cancelled")]
    Cancelled,
    /// The exact Fact or selected semantic field is absent.
    #[error("exact Fact payload field is unavailable")]
    Unavailable,
    /// Session read admission or transport failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// Source-window bounds or serialization failed.
    #[error(transparent)]
    Window(#[from] WindowError),
}

impl SessionController {
    /// Admits an owned exact-source read bound to this controller's Session.
    /// Dropping the waiter alone leaves the read owned until completion or cancellation.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned the controller admission lock.
    pub fn source_window(
        self: &Arc<Self>,
        source: SourceRef,
        start: usize,
        maximum: usize,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<FieldWindow, SourceReadError>> {
        self.source_window_inner(source, None, start, maximum, stop)
    }

    /// Reads a validated subfield of the exact Tool value using the same owned source path.
    pub fn tool_value_window(
        self: &Arc<Self>,
        source: SourceRef,
        path: rsi_conversation::ToolValuePath,
        start: usize,
        maximum: usize,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<FieldWindow, SourceReadError>> {
        if source.field != rsi_conversation::FactField::ToolValue {
            return Box::pin(async { Err(SourceReadError::Unavailable) });
        }
        self.source_window_inner(source, Some(path), start, maximum, stop)
    }

    fn source_window_inner(
        self: &Arc<Self>,
        source: SourceRef,
        path: Option<rsi_conversation::ToolValuePath>,
        start: usize,
        maximum: usize,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<FieldWindow, SourceReadError>> {
        if let Err(error) = FieldWindow::text("", 0, maximum) {
            return Box::pin(async move { Err(error.into()) });
        }
        if source.seq == 0 {
            return Box::pin(async { Err(SourceReadError::Unavailable) });
        }
        let admission = self
            .admission
            .lock()
            .expect("controller admission poisoned");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err(SessionError::ShuttingDown.into()) });
        }
        let Ok(permit) = self.submissions.clone().try_acquire_owned() else {
            return Box::pin(async { Err(SessionError::Capacity.into()) });
        };
        let controller = self.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            let read = async {
                let page = crate::read_with_capacity_retry(&controller.execution, || {
                    controller
                        .handle
                        .history_before(source.seq.checked_add(1), 1)
                })
                .await?;
                let fact = page
                    .facts
                    .first()
                    .filter(|fact| fact.seq() == source.seq)
                    .ok_or(SourceReadError::Unavailable)?;
                let value = match &path {
                    Some(path) => rsi_conversation::select_tool_value_path(fact, source, path),
                    None => rsi_conversation::select_field(fact, source),
                }
                .ok_or(SourceReadError::Unavailable)?;
                let window = value.window(start, maximum)?;
                if stop.is_cancelled() || controller.stop.is_cancelled() {
                    return Err(SourceReadError::Cancelled);
                }
                Ok(window)
            };
            tokio::select! { biased;
                () = controller.stop.cancelled() => Err(SourceReadError::Cancelled),
                () = stop.cancelled() => Err(SourceReadError::Cancelled),
                result = read => result,
            }
        }));
        drop(admission);
        Box::pin(async move { task.await.unwrap_or(Err(SourceReadError::Cancelled)) })
    }
}
