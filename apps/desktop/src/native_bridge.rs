use rsi_desktop_bridge::Operation;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Manager as _, Url, Webview};
use tauri_plugin_clipboard_manager::ClipboardExt as _;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(super) struct Bridge {
    pub origin: Url,
    pub stop: CancellationToken,
    pub finished: CancellationToken,
    pub failed: AtomicBool,
    pub tasks: TaskTracker,
    pub exports: Arc<crate::save::Exports>,
    runtime: tokio::runtime::Handle,
    close: Mutex<CloseState>,
    slots: tokio::sync::Semaphore,
}

#[derive(Default)]
struct CloseState {
    ready: bool,
    next: u64,
    active: Option<(u64, CancellationToken)>,
}
impl CloseState {
    fn begin(&mut self) -> Option<(u64, CancellationToken)> {
        if self.active.is_some() {
            return None;
        }
        self.next = self.next.checked_add(1)?;
        let attempt = (self.next, CancellationToken::new());
        self.active = Some(attempt.clone());
        Some(attempt)
    }
    fn ready(&mut self) -> Option<u64> {
        if std::mem::replace(&mut self.ready, true) {
            return None;
        }
        self.active.as_ref().map(|(attempt, _)| *attempt)
    }
    fn acknowledge(&mut self, attempt: u64) -> Result<(), String> {
        if self.active.as_ref().is_none_or(|value| value.0 != attempt) {
            return Err("Stale native close attempt".into());
        }
        self.active.take().expect("checked").1.cancel();
        Ok(())
    }
}
impl Bridge {
    pub fn new(origin: Url, runtime: tokio::runtime::Handle) -> Self {
        Self {
            origin,
            stop: CancellationToken::new(),
            finished: CancellationToken::new(),
            failed: AtomicBool::new(false),
            tasks: TaskTracker::new(),
            exports: Arc::new(crate::save::Exports::default()),
            runtime,
            close: Mutex::default(),
            slots: tokio::sync::Semaphore::new(8),
        }
    }
    pub fn request_close(self: &Arc<Self>, app: &AppHandle) {
        let mut close = self.close.lock().expect("close state");
        if self.stop.is_cancelled() {
            return;
        }
        let Some((attempt, cancelled)) = close.begin() else {
            return;
        };
        if close.ready {
            self.deliver_close(app, attempt);
        }
        self.watch_close(cancelled);
    }
    fn deliver_close(&self, app: &AppHandle, attempt: u64) {
        let script = format!(
            "window.dispatchEvent(new CustomEvent('rsi-native-close',{{detail:{attempt}}}))"
        );
        if app
            .get_webview_window("main")
            .is_none_or(|window| window.eval(script).is_err())
        {
            self.failed.store(true, Ordering::Release);
            self.stop.cancel();
        }
    }
    fn watch_close(self: &Arc<Self>, cancelled: CancellationToken) {
        let owner = self.clone();
        self.tasks.spawn_on(
            async move {
                tokio::select! { biased;
                    () = owner.stop.cancelled() => {},
                    () = cancelled.cancelled() => {},
                    () = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                        eprintln!("desktop: document drain timed out");
                        owner.failed.store(true, Ordering::Release);
                        owner.stop.cancel();
                    },
                }
            },
            &self.runtime,
        );
    }
    pub async fn drain(&self) {
        {
            let _close = self.close.lock().expect("close state");
            self.tasks.close();
        }
        self.exports.shutdown().await;
        self.tasks.wait().await;
    }
}
pub(super) fn admits_root(expected: &Url, actual: &Url) -> bool {
    admits_navigation(expected, actual) && actual.path() == "/"
}

pub(super) fn admits_navigation(expected: &Url, actual: &Url) -> bool {
    actual.origin() == expected.origin()
        && matches!(
            actual.path(),
            "/" | "/preview-local.html" | "/preview-online.html"
        )
        && actual.query().is_none()
        && actual.username().is_empty()
        && actual.password().is_none()
}

#[tauri::command]
pub(super) async fn rsi_bridge(
    payload: String,
    webview: Webview,
    owner: tauri::State<'_, Arc<Bridge>>,
) -> Result<serde_json::Value, String> {
    let owner = owner.inner().clone();
    // Tauri capabilities admit an Origin, not an invoking document path. Preview
    // frames are isolated by their mandatory HTTP sandbox CSP (opaque Origin).
    // This check fences the current top-level document and window generation.
    if webview.label() != "main"
        || !admits_root(&owner.origin, &webview.url().map_err(crate::error)?)
    {
        return Err("Desktop bridge is restricted to the current main document".into());
    }
    let operation = rsi_desktop_bridge::decode(&payload).map_err(str::to_owned)?;
    let _slot = owner
        .slots
        .try_acquire()
        .map_err(|_| "Desktop bridge is busy")?;
    if owner.stop.is_cancelled() {
        return Err("Desktop is closing".into());
    }
    let app = webview.app_handle();
    match operation {
        Operation::Hello {} => {
            let mut close = owner.close.lock().expect("close state");
            if let Some(attempt) = close.ready() {
                owner.deliver_close(app, attempt);
            }
            return Ok(serde_json::json!({}));
        }
        Operation::RequestClose {} => owner.request_close(app),
        Operation::Clipboard { text } => app.clipboard().write_text(text).map_err(crate::error)?,
        Operation::ExportOpen { event } => return owner.exports.open(app.clone(), event).await,
        Operation::ExportWrite { token, event } => owner.exports.write(&token, event).await?,
        Operation::ExportFinish { token } => return owner.exports.finish(&token).await,
        Operation::ExportCancel { token } => owner.exports.cancel(&token),
        Operation::Close { attempt, ready } => {
            owner
                .close
                .lock()
                .expect("close state")
                .acknowledge(attempt)?;
            if ready {
                owner.stop.cancel();
            }
        }
    }
    Ok(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_observer_can_start_outside_a_tokio_runtime_context() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let bridge = Arc::new(Bridge::new(
            Url::parse("http://127.0.0.1:1234/").unwrap(),
            runtime.handle().clone(),
        ));
        // Both the Tauri main thread and launcher EOF observer lack an entered
        // Tokio context. Their admitted close work must use the owned runtime.
        bridge.watch_close(CancellationToken::new());
        bridge.stop.cancel();
        runtime.block_on(bridge.drain());
        assert!(bridge.tasks.is_empty());
    }
    #[test]
    fn exact_origin_and_root_exclude_previews_other_ports_and_credentials() {
        let root = Url::parse("http://127.0.0.1:1234/").unwrap();
        assert!(admits_root(&root, &root.join("#launch=one-use").unwrap()));
        for url in [
            "http://127.0.0.1:1235/",
            "http://localhost:1234/",
            "http://127.0.0.1:1234/preview-local.html",
            "http://127.0.0.1:1234/?x",
            "http://user@127.0.0.1:1234/",
            "https://127.0.0.1:1234/",
        ] {
            assert!(!admits_root(&root, &Url::parse(url).unwrap()), "{url}");
        }
    }
    #[test]
    fn sandbox_preview_navigation_never_grants_native_bridge_access() {
        let root = Url::parse("http://127.0.0.1:1234/").unwrap();
        for path in ["/preview-local.html", "/preview-online.html"] {
            let preview = root.join(path).unwrap();
            assert!(admits_navigation(&root, &preview));
            assert!(!admits_root(&root, &preview));
        }
        for url in [
            "http://127.0.0.1:1234/preview-local.html?x",
            "http://127.0.0.1:1235/preview-local.html",
            "http://user@127.0.0.1:1234/preview-local.html",
            "http://127.0.0.1:1234/arbitrary.html",
            "https://example.invalid/preview-local.html",
        ] {
            assert!(
                !admits_navigation(&root, &Url::parse(url).unwrap()),
                "{url}"
            );
        }
    }
    #[test]
    fn early_close_is_delivered_once_after_the_document_hello() {
        let mut state = CloseState::default();
        let attempt = state.begin().unwrap();
        assert!(!state.ready);
        assert_eq!(state.ready(), Some(attempt.0));
        assert_eq!(state.ready(), None);
        assert!(!attempt.1.is_cancelled());
        state.acknowledge(attempt.0).unwrap();
        assert!(attempt.1.is_cancelled());
    }
    #[test]
    fn cancelled_close_does_not_authorize_a_later_attempt() {
        let mut state = CloseState::default();
        let first = state.begin().unwrap();
        assert!(state.begin().is_none());
        state.acknowledge(first.0).unwrap();
        assert!(first.1.is_cancelled());
        let second = state.begin().unwrap();
        assert!(state.acknowledge(first.0).is_err());
        assert!(!second.1.is_cancelled());
    }
}
