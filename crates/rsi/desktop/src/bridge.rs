use rsi_api_http::AssetType;
use rsi_api_http::HttpAssets;
use rsi_application::ApplicationLifetime;
use rsi_gui::GuiApplication;
use rsi_web_assets::{BundleLease, WebAssetControl};
use serde::Deserialize;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type Result<T> = std::result::Result<T, String>;
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[derive(Debug, Default)]
struct Frames {
    awaiting: Option<(String, CancellationToken)>,
    revision: Option<u64>,
    asset_revision: Option<String>,
    pending: Option<BundleLease>,
    accepted: Option<BundleLease>,
    rejected: Option<String>,
    offer: Option<serde_json::Value>,
}

#[derive(Debug)]
pub(crate) struct Bridge {
    pub app: Arc<GuiApplication>,
    pub assets: Arc<WebAssetControl>,
    pub identity: String,
    pub lifetime: Arc<ApplicationLifetime>,
    pub failed: Arc<AtomicBool>,
    pub stop: CancellationToken,
    pub tasks: TaskTracker,
    pub slots: Arc<Semaphore>,
    pub frame_slot: Arc<Semaphore>,
    pub control_slot: Arc<Semaphore>,
    close_attempt: Mutex<Option<CancellationToken>>,
    frames: Mutex<Frames>,
}
impl Bridge {
    pub fn new(
        app: Arc<GuiApplication>,
        assets: Arc<WebAssetControl>,
        identity: String,
        lifetime: Arc<ApplicationLifetime>,
        failed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            app,
            assets,
            identity,
            lifetime,
            failed,
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(8)),
            frame_slot: Arc::new(Semaphore::new(1)),
            control_slot: Arc::new(Semaphore::new(1)),
            close_attempt: Mutex::new(None),
            frames: Mutex::new(Frames::default()),
        }
    }
    pub async fn close(&self) {
        self.cancel_document_close();
        self.stop.cancel();
        self.slots.close();
        self.frame_slot.close();
        self.tasks.close();
        self.tasks.wait().await;
        *self.frames.lock().expect("desktop frames poisoned") = Frames::default();
    }
    pub fn begin_document_close(&self) -> Option<CancellationToken> {
        let mut attempt = self
            .close_attempt
            .lock()
            .expect("desktop close attempt poisoned");
        if attempt.is_some() {
            return None;
        }
        let token = CancellationToken::new();
        *attempt = Some(token.clone());
        Some(token)
    }
    pub fn cancel_document_close(&self) {
        if let Some(attempt) = self
            .close_attempt
            .lock()
            .expect("desktop close attempt poisoned")
            .take()
        {
            attempt.cancel();
        }
    }
    pub async fn frame(self: &Arc<Self>, base: Option<String>) -> Result<Vec<u8>> {
        let (previous, old_asset) = {
            let frames = self.frames.lock().expect("desktop frames poisoned");
            if frames.awaiting.is_some() {
                return Err("A document acknowledgement is pending".into());
            }
            (frames.revision, frames.asset_revision.clone())
        };
        let mut changed = self.app.changes();
        let mut assets_changed = self.assets.changes();
        loop {
            let revision = *changed.borrow_and_update();
            let asset_revision = assets_changed.borrow_and_update().clone();
            if base.is_none()
                || base != self.app.frame_id()
                || previous != Some(revision)
                || old_asset.as_ref() != Some(&asset_revision)
            {
                let mut frames = self.frames.lock().expect("desktop frames poisoned");
                if frames.pending.is_none()
                    && frames.accepted.as_ref().map(BundleLease::revision)
                        != Some(asset_revision.as_str())
                    && frames.rejected.as_ref() != Some(&asset_revision)
                {
                    let candidate = self.assets.acquire(&asset_revision).map_err(error)?;
                    frames.offer = Some(
                        serde_json::json!({"revision": candidate.revision(), "catalog": candidate.catalog()}),
                    );
                    frames.pending = Some(candidate);
                }
                let frame = self.app.next_frame(base.as_deref()).map_err(error)?;
                let id = self.app.frame_id().ok_or("Frame identity is absent")?;
                let offer =
                    serde_json::to_vec(frames.offer.as_ref().ok_or("Renderer offer is absent")?)
                        .map_err(error)?;
                let mut bytes = Vec::with_capacity(frame.len() + offer.len() + 32);
                bytes.extend_from_slice(b"{\"view\":");
                bytes.extend_from_slice(frame.as_bytes());
                bytes.extend_from_slice(b",\"assets\":");
                bytes.extend_from_slice(&offer);
                bytes.push(b'}');
                let ack = CancellationToken::new();
                frames.awaiting = Some((id, ack.clone()));
                frames.revision = Some(revision);
                frames.asset_revision = Some(asset_revision);
                let bridge = self.clone();
                tokio::spawn(self.tasks.track_future(async move {
                    tokio::select! { biased;
                        () = bridge.stop.cancelled() => {},
                        () = ack.cancelled() => {},
                        () = tokio::time::sleep(ACK_TIMEOUT) => { eprintln!("desktop: document acknowledgement timed out"); bridge.failed.store(true, Ordering::Release); bridge.lifetime.request_stop(); }
                    }
                }));
                return Ok(bytes);
            }
            tokio::select! { biased;
                () = self.stop.cancelled() => return Err("Native bridge is closed".into()),
                () = self.app.closed() => return Err("GUI application is closed".into()),
                result = changed.changed() => result.map_err(error)?,
                result = assets_changed.changed() => result.map_err(error)?,
            }
        }
    }
    pub fn ack(&self, source: &[u8]) -> Result<()> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Ack {
            frame_id: String,
            #[serde(default)]
            renderer: Option<Renderer>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Renderer {
            revision: String,
            accept: bool,
        }
        if source.len() > 1024 {
            return Err("Acknowledgement exceeds its limit".into());
        }
        let ack: Ack = serde_json::from_slice(source).map_err(error)?;
        let mut frames = self.frames.lock().expect("desktop frames poisoned");
        if frames.awaiting.as_ref().map(|(id, _)| id) != Some(&ack.frame_id) {
            return Err("Stale document acknowledgement".into());
        }
        if let Some(renderer) = ack.renderer {
            if frames.pending.as_ref().map(BundleLease::revision)
                != Some(renderer.revision.as_str())
            {
                return Err("Stale renderer acknowledgement".into());
            }
            let candidate = frames.pending.take();
            if renderer.accept {
                frames.accepted = candidate;
                frames.rejected = None;
            } else {
                frames.rejected = Some(renderer.revision);
            }
        }
        frames.awaiting.take().expect("checked ACK").1.cancel();
        Ok(())
    }
    pub fn asset(&self, path: &str) -> Result<(Vec<u8>, &'static str)> {
        let asset = self
            .assets
            .get(path)
            .map_err(error)?
            .ok_or("Asset is unavailable")?;
        let mime = match asset.kind {
            AssetType::Html => "text/html; charset=utf-8",
            AssetType::JavaScript => "text/javascript; charset=utf-8",
            AssetType::Css => "text/css; charset=utf-8",
            AssetType::Wasm => "application/wasm",
            AssetType::Json => "application/json",
            AssetType::Png => "image/png",
        };
        Ok((asset.bytes.as_bytes().to_vec(), mime))
    }
    pub async fn call(&self, method: &str, source: &[u8]) -> Result<Vec<u8>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Submission {
            pane: String,
            generation: String,
            opaque: String,
            mode: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Source {
            ticket: String,
            name: String,
            offset: u64,
            maximum: usize,
        }
        if source.len() > 17 * 1024 * 1024 {
            return Err("Native request exceeds its limit".into());
        }
        match method {
            "connect" => {
                self.app.command(r#"{"action":"refresh"}"#).await?;
                Ok(self.identity.as_bytes().to_vec())
            }
            "command" => {
                self.app.command(text(source)?).await?;
                Ok(Vec::new())
            }
            "restore_session" => Ok(self.app.restore_session(text(source)?).await?.into_bytes()),
            "prepare_submission" => Ok(self
                .app
                .prepare_submission(text(source)?)
                .await?
                .into_bytes()),
            "dispatch_submission" => {
                let value: Submission = serde_json::from_slice(source).map_err(error)?;
                Ok(self
                    .app
                    .dispatch_submission(
                        rsi_gui::SurfaceId::parse(&value.pane)?,
                        &value.generation,
                        &value.opaque,
                        &value.mode,
                    )
                    .await?
                    .into_bytes())
            }
            "read_image" => Ok(self.app.read_image(text(source)?).await?.bytes.to_vec()),
            "ui_source" => {
                if source.len() > 1024 {
                    return Err("Source selection exceeds its limit".into());
                }
                let source: Source = serde_json::from_slice(source).map_err(error)?;
                Ok(self
                    .app
                    .read_ui_source(&source.ticket, &source.name, source.offset, source.maximum)
                    .await?
                    .as_bytes()
                    .to_vec())
            }
            // Binary upload prefix is bounded JSON followed by a newline and exact source bytes.
            "import_image" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Import {
                    pane: String,
                    generation: String,
                }
                let boundary = source
                    .iter()
                    .take(256)
                    .position(|b| *b == b'\n')
                    .ok_or("Invalid image selection")?;
                let selection: Import =
                    serde_json::from_slice(&source[..boundary]).map_err(error)?;
                let bytes = &source[boundary + 1..];
                if bytes.is_empty() || bytes.len() > rsi_gui::MAXIMUM_UPLOAD_BYTES {
                    return Err("Image source exceeds its limit".into());
                }
                let value = self
                    .app
                    .import_image(
                        rsi_gui::SurfaceId::parse(&selection.pane)?,
                        &selection.generation,
                        bytes.to_vec().into(),
                    )
                    .await?;
                serde_json::to_vec(&value).map_err(error)
            }
            _ => Err("Unknown native input".into()),
        }
    }
}
fn text(source: &[u8]) -> Result<&str> {
    std::str::from_utf8(source).map_err(error)
}
pub(crate) fn error(error: impl std::fmt::Display) -> String {
    rsi_gui::display_error(error)
}
