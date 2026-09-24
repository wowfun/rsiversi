use crate::bridge::Bridge;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

struct Clear<'a>(&'a Bridge);
impl Drop for Clear<'_> {
    fn drop(&mut self) {
        self.0
            .export_cancel
            .lock()
            .expect("desktop export poisoned")
            .take();
    }
}

impl Bridge {
    pub(crate) fn cancel_export(&self) -> Result<Vec<u8>, String> {
        if let Some(stop) = self
            .export_cancel
            .lock()
            .map_err(|e| e.to_string())?
            .as_ref()
        {
            stop.cancel();
        }
        Ok(b"null".to_vec())
    }
    pub(crate) async fn save_export(&self, source: &str) -> Result<Vec<u8>, String> {
        let stop = self.stop.child_token();
        {
            let mut active = self.export_cancel.lock().expect("desktop export poisoned");
            if active.is_some() {
                return Err("A native export is already active".into());
            }
            *active = Some(stop.clone());
        }
        let _clear = Clear(self);
        tokio::select! { biased;
            () = stop.cancelled() => Err("Export cancelled".into()),
            result = async {
                let (source, filename) = self.app.open_export(source).await?;
                let app = self.app_handle.lock().expect("desktop dialog poisoned").clone().ok_or("Native save dialog unavailable")?;
                let (sender, receiver) = tokio::sync::oneshot::channel();
                app.dialog().file().set_parent(&app.get_webview_window("main").ok_or("Native window is closed")?).set_title("Export session").set_file_name(&filename).save_file(move |path| { let _ = sender.send(path); });
                let path = receiver.await.map_err(|_| "Native save dialog closed")?.ok_or("Export cancelled")?.into_path().map_err(|e|e.to_string())?;
                let bytes = rsi_session_export::write_file(source, &path).await.map_err(|e|e.to_string())?;
                serde_json::to_vec(&serde_json::json!({"filename":filename,"bytes":bytes.to_string()})).map_err(|e|e.to_string())
            } => result,
        }
    }
}
