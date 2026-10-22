use rsi_session_protocol::export::{ExportEvent, ExportStream, ExportVerifier};
use std::sync::{Arc, Mutex};
use tauri::Manager as _;
use tauri_plugin_dialog::DialogExt as _;
use tokio::sync::{mpsc, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type Completion = oneshot::Receiver<Result<serde_json::Value, String>>;
type Input = Option<(ExportEvent, oneshot::Sender<()>)>;
struct Active {
    id: String,
    stop: CancellationToken,
    sender: mpsc::Sender<Input>,
    completion: Option<Completion>,
}
#[derive(Default)]
pub(super) struct Exports {
    active: Mutex<Option<Active>>,
    tasks: TaskTracker,
    stopped: CancellationToken,
}
impl Exports {
    pub async fn open(
        self: &Arc<Self>,
        app: tauri::AppHandle,
        first: ExportEvent,
    ) -> Result<serde_json::Value, String> {
        let mut verifier = ExportVerifier::default();
        verifier.accept(&first, None).map_err(crate::error)?;
        let ExportEvent::Start { filename, .. } = &first else {
            return Err("Export must start with metadata".into());
        };
        let filename = filename.clone();
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(crate::error)?;
        let id = hex::encode(random);
        let stop = self.stopped.child_token();
        let (sender, receiver) = mpsc::channel(1);
        let (completed, completion) = oneshot::channel();
        let (opened, opening) = oneshot::channel();
        {
            let mut active = self.active.lock().expect("export");
            if self.stopped.is_cancelled() || active.is_some() {
                return Err("A native export is active or closing".into());
            }
            *active = Some(Active {
                id: id.clone(),
                stop: stop.clone(),
                sender,
                completion: Some(completion),
            });
            let owner = self.clone();
            let token = id.clone();
            self.tasks.spawn(async move {
                let result =
                    save(app, first, filename, receiver, stop, opened, &owner.stopped).await;
                let _ = completed.send(result);
                let mut active = owner.active.lock().expect("export");
                if active.as_ref().is_some_and(|value| value.id == token) {
                    active.take();
                }
            });
        }
        opening.await.map_err(|_| "Native export did not open")??;
        Ok(serde_json::json!({ "token": id }))
    }
    pub async fn write(&self, id: &str, event: ExportEvent) -> Result<(), String> {
        let sender = {
            let active = self.active.lock().expect("export");
            let active = active
                .as_ref()
                .filter(|value| value.id == id && value.completion.is_some())
                .ok_or("Native export is unavailable")?;
            active.sender.clone()
        };
        // Bounded nonqueued admission also rejects concurrent document writers.
        let (accepted, acknowledgment) = oneshot::channel();
        sender
            .try_send(Some((event, accepted)))
            .map_err(|_| "Native export is busy or closed")?;
        acknowledgment
            .await
            .map_err(|_| "Native export rejected the event".into())
    }
    pub async fn finish(&self, id: &str) -> Result<serde_json::Value, String> {
        let (sender, completion) = {
            let mut active = self.active.lock().expect("export");
            let active = active
                .as_mut()
                .filter(|value| value.id == id)
                .ok_or("Native export is unavailable")?;
            (
                active.sender.clone(),
                active
                    .completion
                    .take()
                    .ok_or("Native export is already finishing")?,
            )
        };
        sender
            .send(None)
            .await
            .map_err(|_| "Native export failed before completion")?;
        completion
            .await
            .map_err(|_| "Native persistence result unavailable")?
    }
    pub fn cancel(&self, id: &str) {
        if let Some(active) = self
            .active
            .lock()
            .expect("export")
            .as_ref()
            .filter(|value| value.id == id)
        {
            active.stop.cancel();
        }
    }
    pub async fn shutdown(&self) {
        {
            let _active = self.active.lock().expect("export");
            self.stopped.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}

async fn save(
    app: tauri::AppHandle,
    first: ExportEvent,
    filename: String,
    mut receiver: mpsc::Receiver<Input>,
    stop: CancellationToken,
    opened: oneshot::Sender<Result<(), String>>,
    shutdown: &CancellationToken,
) -> Result<serde_json::Value, String> {
    let window = app
        .get_webview_window("main")
        .ok_or("Native window is closed")?;
    let (selected, selection) = oneshot::channel();
    app.dialog()
        .file()
        .set_parent(&window)
        .set_title("Export session")
        .set_file_name(&filename)
        .save_file(move |path| {
            let _ = selected.send(path);
        });
    // Ordinary cancellation keeps the chooser slot; shutdown must not wait for
    // a callback from a window that is being destroyed.
    let path = await_selection(selection, shutdown).await?;
    if stop.is_cancelled() || path.is_none() {
        let _ = opened.send(Err("Export cancelled".into()));
        return Err("Export cancelled".into());
    }
    let path = path.expect("checked").into_path().map_err(crate::error)?;
    if opened.send(Ok(())).is_err() {
        return Err("Export document disappeared".into());
    }
    let stream: ExportStream = Box::pin(async_stream::try_stream! {
        yield first;
        loop {
            // A vanished document cannot leave a temporary file and reservation forever.
            let event = tokio::time::timeout(std::time::Duration::from_secs(30), receiver.recv())
                .await.map_err(|_| rsi_session_protocol::SessionError::Invalid("Native export input timed out".into()))?
                .ok_or_else(|| rsi_session_protocol::SessionError::Invalid("Native export input closed".into()))?;
            let Some((event, accepted)) = event else { break; };
            yield event;
            let _ = accepted.send(());
        }
    });
    let bytes = rsi_session_export::write_file(stream, &path, stop)
        .await
        .map_err(crate::error)?;
    Ok(serde_json::json!({"filename":filename,"bytes":bytes.to_string()}))
}

async fn await_selection<T>(
    selection: oneshot::Receiver<T>,
    shutdown: &CancellationToken,
) -> Result<T, String> {
    tokio::select! { biased;
        () = shutdown.cancelled() => Err("Export cancelled during shutdown".into()),
        path = selection => path.map_err(|_| "Native save dialog closed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_releases_pending_chooser_and_rejects_late_selection() {
        let shutdown = CancellationToken::new();
        let (sender, receiver) = oneshot::channel::<String>();
        shutdown.cancel();
        assert!(await_selection(receiver, &shutdown).await.is_err());
        assert!(sender.send("late path".into()).is_err());
    }

    #[tokio::test]
    async fn active_chooser_delivers_selection_but_shutdown_wins_a_ready_callback() {
        let shutdown = CancellationToken::new();
        let (sender, receiver) = oneshot::channel();
        sender.send("selected").unwrap();
        assert_eq!(
            await_selection(receiver, &shutdown).await.unwrap(),
            "selected"
        );
        let (sender, receiver) = oneshot::channel();
        sender.send("late selection").unwrap();
        shutdown.cancel();
        assert!(await_selection(receiver, &shutdown).await.is_err());
    }
}
