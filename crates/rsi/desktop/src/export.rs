use crate::bridge::Bridge;
use serde::Deserialize;
use std::sync::Arc;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Active {
    id: String,
    stop: CancellationToken,
    claimed: CancellationToken,
}
#[derive(Debug, Default)]
pub(crate) struct State(Option<Active>);
impl State {
    fn reserve(&mut self, stop: CancellationToken) -> Result<(String, CancellationToken), String> {
        if self.0.is_some() {
            return Err("A native export is already active".into());
        }
        let id = rsi_ui::fresh_identity("export")?;
        let claimed = CancellationToken::new();
        self.0 = Some(Active {
            id: id.clone(),
            stop,
            claimed: claimed.clone(),
        });
        Ok((id, claimed))
    }
    fn claim(&mut self, id: &str) -> Result<CancellationToken, String> {
        let active = self
            .0
            .as_ref()
            .filter(|active| active.id == id && !active.claimed.is_cancelled())
            .ok_or("Native export reservation is unavailable")?;
        active.claimed.cancel();
        Ok(active.stop.clone())
    }
    fn cancel(&mut self, id: &str) {
        if let Some(active) = &self.0
            && active.id == id
        {
            active.stop.cancel();
            if !active.claimed.is_cancelled() {
                self.0.take();
            }
        }
    }
    fn expire(&mut self, id: &str) {
        if self
            .0
            .as_ref()
            .is_some_and(|active| active.id == id && !active.claimed.is_cancelled())
        {
            self.cancel(id);
        }
    }
    fn clear(&mut self, id: &str) {
        if self.0.as_ref().is_some_and(|active| active.id == id) {
            self.0.take();
        }
    }
}
struct Clear<'a> {
    owner: &'a Bridge,
    id: String,
}
impl Drop for Clear<'_> {
    fn drop(&mut self) {
        self.owner
            .export
            .lock()
            .expect("desktop export poisoned")
            .clear(&self.id);
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Token {
    token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    token: String,
    source: String,
}

impl Bridge {
    pub(crate) fn open_export(self: &Arc<Self>) -> Result<Vec<u8>, String> {
        let stop = self.stop.child_token();
        let mut state = self.export.lock().expect("desktop export poisoned");
        if self.stop.is_cancelled() {
            return Err("Native application is closing".into());
        }
        let (id, claimed) = state.reserve(stop.clone())?;
        let owner = self.clone();
        let reserved = id.clone();
        self.tasks.spawn(async move {
            if reservation_expires(claimed, stop).await {
                owner
                    .export
                    .lock()
                    .expect("desktop export poisoned")
                    .expire(&reserved);
            }
        });
        serde_json::to_vec(&serde_json::json!({"token":id})).map_err(|e| e.to_string())
    }
    pub(crate) fn cancel_export(&self, source: &str) -> Result<Vec<u8>, String> {
        if source.len() > 256 {
            return Err("Invalid export token".into());
        }
        let input: Token = serde_json::from_str(source).map_err(|_| "Invalid export token")?;
        self.export
            .lock()
            .map_err(|e| e.to_string())?
            .cancel(&input.token);
        Ok(b"null".to_vec())
    }
    pub(crate) async fn save_export(self: &Arc<Self>, source: &str) -> Result<Vec<u8>, String> {
        if source.len() > 36 * 1024 {
            return Err("Export input exceeds its bound".into());
        }
        let input: Save = serde_json::from_str(source).map_err(|_| "Invalid export input")?;
        let (stop, task) = {
            let mut active = self.export.lock().expect("desktop export poisoned");
            if self.stop.is_cancelled() {
                return Err("Native application is closing".into());
            }
            let stop = active.claim(&input.token)?;
            let owner = self.clone();
            let token = stop.clone();
            let task = self.tasks.spawn(async move {
                let _clear = Clear {
                    owner: &owner,
                    id: input.token,
                };
                let (stream, filename) = tokio::select! { biased;
                    () = token.cancelled() => return Err("Export cancelled".into()),
                    result = owner.app.open_export(&input.source) => result?,
                };
                if token.is_cancelled() {
                    return Err("Export cancelled".into());
                }
                let app = owner
                    .app_handle
                    .lock()
                    .expect("desktop dialog poisoned")
                    .clone()
                    .ok_or("Native save dialog unavailable")?;
                let (sender, receiver) = tokio::sync::oneshot::channel();
                app.dialog()
                    .file()
                    .set_parent(
                        &app.get_webview_window("main")
                            .ok_or("Native window is closed")?,
                    )
                    .set_title("Export session")
                    .set_file_name(&filename)
                    .save_file(move |path| {
                        let _ = sender.send(path);
                    });
                // Native chooser API has no dismiss operation: retain callback and slot.
                let selected = receiver.await.map_err(|_| "Native save dialog closed")?;
                if token.is_cancelled() {
                    return Err("Export cancelled".into());
                }
                let path = selected
                    .ok_or("Export cancelled")?
                    .into_path()
                    .map_err(|e| e.to_string())?;
                let bytes = rsi_session_export::write_file(stream, &path, token)
                    .await
                    .map_err(|e| e.to_string())?;
                serde_json::to_vec(
                    &serde_json::json!({"filename":filename,"bytes":bytes.to_string()}),
                )
                .map_err(|e| e.to_string())
            });
            (stop, task)
        };
        let _cancel = stop.drop_guard();
        task.await.map_err(|e| e.to_string())?
    }
}

async fn reservation_expires(claimed: CancellationToken, stop: CancellationToken) -> bool {
    tokio::select! { biased;
        () = claimed.cancelled() => false,
        () = stop.cancelled() => true,
        () = tokio::time::sleep(std::time::Duration::from_secs(30)) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_is_fenced_before_claim_and_across_replacements() {
        let mut state = State::default();
        let (a, _) = state.reserve(CancellationToken::new()).unwrap();
        state.cancel(&a);
        assert!(state.claim(&a).is_err());
        let (b, _) = state.reserve(CancellationToken::new()).unwrap();
        let stop = state.claim(&b).unwrap();
        state.cancel(&a);
        state.clear(&a);
        assert!(!stop.is_cancelled());
        assert!(state.reserve(CancellationToken::new()).is_err());
        state.cancel(&b);
        assert!(stop.is_cancelled());
        assert!(state.reserve(CancellationToken::new()).is_err());
        state.clear(&b);
        assert!(state.reserve(CancellationToken::new()).is_ok());
    }
    #[tokio::test(start_paused = true)]
    async fn reservation_watchdog_expires_at_thirty_seconds() {
        let mut state = State::default();
        let stop = CancellationToken::new();
        let (id, claimed) = state.reserve(stop.clone()).unwrap();
        let mut watchdog = Box::pin(reservation_expires(claimed, stop.clone()));
        assert!(futures_util::poll!(&mut watchdog).is_pending());
        tokio::time::advance(std::time::Duration::from_secs(29)).await;
        assert!(futures_util::poll!(&mut watchdog).is_pending());
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(watchdog.await);
        state.expire(&id);
        assert!(stop.is_cancelled());
        assert!(state.claim(&id).is_err());
        assert!(state.reserve(CancellationToken::new()).is_ok());
    }
    #[tokio::test(start_paused = true)]
    async fn claim_wins_simultaneous_deadline_and_late_expiry_rechecks_state() {
        for claim_before_poll in [true, false] {
            let mut state = State::default();
            let stop = CancellationToken::new();
            let (id, claimed) = state.reserve(stop.clone()).unwrap();
            let mut watchdog = Box::pin(reservation_expires(claimed, stop.clone()));
            assert!(futures_util::poll!(&mut watchdog).is_pending());
            tokio::time::advance(std::time::Duration::from_secs(30)).await;
            if claim_before_poll {
                state.claim(&id).unwrap();
            }
            assert_eq!(watchdog.await, !claim_before_poll);
            if !claim_before_poll {
                state.claim(&id).unwrap();
            }
            state.expire(&id);
            assert!(!stop.is_cancelled());
            assert!(state.reserve(CancellationToken::new()).is_err());
        }
    }
    #[tokio::test(start_paused = true)]
    async fn stopped_or_stale_watchdog_cannot_expire_a_replacement() {
        let mut state = State::default();
        let old_stop = CancellationToken::new();
        let (old, claimed) = state.reserve(old_stop.clone()).unwrap();
        let mut watchdog = Box::pin(reservation_expires(claimed, old_stop));
        assert!(futures_util::poll!(&mut watchdog).is_pending());
        state.cancel(&old);
        assert!(watchdog.await);
        let next_stop = CancellationToken::new();
        let (next, _) = state.reserve(next_stop.clone()).unwrap();
        state.expire(&old);
        assert!(!next_stop.is_cancelled());
        assert!(state.claim(&next).is_ok());
    }
}
