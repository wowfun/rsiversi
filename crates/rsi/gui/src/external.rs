use super::{GuiApplication, Pane, Result, error};
use rsi_acp_protocol::{
    observation::{ConversationId, Snapshot},
    service::Endpoint,
};
use rsi_client::{ExternalCommand, ExternalController};
use serde::Serialize;
use std::sync::{Arc, Mutex, atomic::Ordering};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct ExternalCatalog {
    endpoints: Vec<Endpoint>,
    conversations: Vec<Snapshot>,
    more: bool,
    #[serde(skip)]
    after: Option<ConversationId>,
}
#[derive(Debug)]
pub(super) struct Attachment {
    pub focus: Mutex<Option<rsi_navigation_api::attention::Target>>,
    pub generation: u64,
    pub controller: Arc<ExternalController>,
    pub source: Mutex<Option<SourceWindow>>,
    stop: CancellationToken,
}
#[derive(Serialize, Debug)]
pub(super) struct SourceWindow {
    pub(super) source: rsi_conversation::ExternalSource,
    start: usize,
    end: usize,
    text: String,
    more: bool,
}
impl Pane {
    pub(super) async fn detach_external(&self) {
        let external = self.external.lock().expect("external pane").take();
        if let Some(external) = external {
            external.stop.cancel();
            external.controller.retire().await;
        }
    }
    fn external_attachment(&self, generation: &str) -> Result<Arc<Attachment>> {
        self.external
            .lock()
            .expect("external pane")
            .as_ref()
            .filter(|current| current.generation.to_string() == generation)
            .cloned()
            .ok_or_else(|| "This conversation changed; use its current controls".into())
    }
}
impl GuiApplication {
    pub(crate) async fn start_external(
        self: &Arc<Self>,
        pane: crate::SurfaceId,
        id: ConversationId,
        endpoint: &str,
    ) -> Result<()> {
        self.pane(pane)?;
        self.external
            .as_ref()
            .ok_or("External conversations unavailable")?
            .start(id.clone(), endpoint)
            .await
            .map_err(error)?;
        self.open_external(pane, id).await?;
        self.refresh_external(false).await
    }

    pub(crate) async fn refresh_external(&self, next: bool) -> Result<()> {
        let service = self
            .external
            .as_ref()
            .ok_or("External conversations unavailable")?;
        let after = if next {
            self.external_catalog
                .lock()
                .expect("external catalog")
                .after
                .clone()
        } else {
            None
        };
        let endpoints = service.endpoints().await.map_err(error)?;
        let conversations = service.list(after).await.map_err(error)?;
        let more = conversations.len() == 64;
        let after = conversations.last().map(|value| value.id.clone());
        *self.external_catalog.lock().expect("external catalog") = ExternalCatalog {
            endpoints,
            conversations,
            more,
            after,
        };
        Ok(())
    }
    pub(crate) async fn open_external(
        self: &Arc<Self>,
        index: crate::SurfaceId,
        id: ConversationId,
    ) -> Result<()> {
        let pane = self.pane(index)?;
        let _switching = pane.switching.lock().await;
        if pane.closed.load(Ordering::Acquire) {
            return Err("Surface is retiring".into());
        }
        let service = self
            .external
            .as_ref()
            .ok_or("External conversations unavailable")?
            .clone();
        let generation = self
            .attachment_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| "Surface generation exhausted")?
            + 1;
        let controller = ExternalController::attach(service, self.execution.clone(), id)
            .await
            .map_err(error)?;
        let mut changes = controller.changes();
        let stop = self.stop.child_token();
        let current = Arc::new(Attachment {
            focus: Mutex::new(None),
            generation,
            controller,
            source: Mutex::new(None),
            stop: stop.clone(),
        });
        pane.detach_external().await;
        let old = pane.current.lock().expect("native pane").take();
        if let Some(old) = old {
            let detached = self
                .details
                .lock()
                .expect("GUI details")
                .detach(index, &old.generation.to_string());
            let closed = old.close().await;
            if let Err(error) = detached.and(closed) {
                current.controller.retire().await;
                return Err(error);
            }
        }
        *pane.external.lock().expect("external pane") = Some(current);
        pane.selection.fetch_add(1, Ordering::AcqRel);
        pane.changed();
        self.changed();
        let app = Arc::downgrade(self);
        drop(self.execution.spawn(self.tasks.track_future(async move {
            loop {tokio::select! {biased;()=stop.cancelled()=>break,result=changes.changed()=>{if result.is_err(){break;}
if let Some(app)=app.upgrade(){app.changed();}else{break;}}}}
        })));
        Ok(())
    }
    pub(crate) async fn external_control(
        &self,
        pane: crate::SurfaceId,
        generation: &str,
        command: ExternalCommand,
    ) -> Result<()> {
        self.pane(pane)?
            .external_attachment(generation)?
            .controller
            .command(command)
            .await
            .map_err(error)
    }
    pub(crate) async fn external_source(
        &self,
        index: crate::SurfaceId,
        generation: &str,
        source: rsi_conversation::ExternalSource,
        start: usize,
    ) -> Result<()> {
        let pane = self.pane(index)?;
        let current = pane.external_attachment(generation)?;
        let bytes = current
            .controller
            .source(&source, start)
            .await
            .map_err(error)?;
        let next = pane.external_attachment(generation)?;
        if !Arc::ptr_eq(&current, &next)
            || current.controller.view().observed.snapshot.epoch != source.epoch()
        {
            return Err("Source conversation changed".into());
        }
        *current.source.lock().expect("external source") = Some(SourceWindow {
            source,
            start,
            end: start.saturating_add(bytes.len()),
            more: bytes.len() == 64 * 1024,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        });
        pane.changed();
        Ok(())
    }
}
