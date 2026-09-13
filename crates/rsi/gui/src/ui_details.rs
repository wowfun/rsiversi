use super::{Arc, Attachment, GuiApplication, Result, error};
use crate::details::UiDetail;
use rsi_ui::{ActionInput, BoundView, UiReference};

impl GuiApplication {
    pub(super) fn ui_selection(
        &self,
        index: crate::SurfaceId,
        generation: &str,
    ) -> Result<(Arc<Attachment>, u64)> {
        let pane = self.pane(index)?;
        let current = pane.current.lock().expect("Web pane poisoned");
        let attached = current
            .as_ref()
            .filter(|attached| attached.generation.to_string() == generation)
            .cloned()
            .ok_or("This pane changed; reopen its card")?;
        let mut details = self.details.lock().expect("Web details poisoned");
        let revision = details.begin()?;
        details.ui = Some(UiDetail {
            pane: Some(index),
            generation: Some(generation.into()),
            ticket: revision.to_string(),
            view: None,
            lease: None,
            snapshot: None,
            remote: None,
            binding: None,
            model: None,
            error: None,
            busy: false,
        });
        Ok((attached, revision))
    }
    fn ui_result(&self, revision: u64, result: Result<BoundView>) {
        let mut details = self.details.lock().expect("Web details poisoned");
        if details.revision != revision || details.stop.is_cancelled() {
            return;
        }
        let Some(detail) = &mut details.ui else {
            return;
        };
        detail.busy = false;
        match result {
            Ok(view) if self.ui.is_current(&view.reference) => {
                match rsi_ui::UiModel::standard(view.view.clone()) {
                    Ok(model) => {
                        detail.model = Some(model);
                        detail.binding = Some(view.reference.clone());
                        detail.view = Some(view);
                    }
                    Err(failure) => {
                        detail.error = Some(error(failure));
                    }
                }
            }
            Ok(_) => {
                let _ = details.begin();
            }
            Err(error) => detail.error = Some(error),
        }
        drop(details);
        self.changed();
    }
    pub(crate) fn prune_ui(&self) {
        let mut details = self.details.lock().expect("Web details poisoned");
        if details
            .ui
            .as_ref()
            .filter(|detail| detail.remote.is_none())
            .and_then(|detail| detail.binding.as_ref())
            .is_some_and(|reference| !self.ui.is_current(reference))
        {
            let _ = details.begin();
        }
    }
    pub(crate) async fn ui_surface(
        self: &Arc<Self>,
        index: crate::SurfaceId,
        generation: &str,
        reference: &UiReference,
    ) -> Result<()> {
        let (attached, revision) = self.ui_selection(index, generation)?;
        if !self.ui.matches_target(&attached.ui_target, reference) {
            self.ui_result(
                revision,
                Err("This UI contribution belongs to another or retired surface".into()),
            );
            return Ok(());
        }
        self.present_ui(revision, reference).await
    }
    pub(crate) async fn application_ui_surface(
        self: &Arc<Self>,
        reference: &UiReference,
    ) -> Result<()> {
        let target = self
            .application_target
            .as_ref()
            .ok_or("Application UI target is unavailable")?;
        if !self.ui.matches_target(target, reference) {
            return Err("This UI contribution belongs to another or retired target".into());
        }
        let revision = {
            let mut details = self.details.lock().expect("GUI details poisoned");
            let revision = details.begin()?;
            details.ui = Some(UiDetail {
                pane: None,
                generation: None,
                ticket: revision.to_string(),
                view: None,
                lease: None,
                snapshot: None,
                remote: None,
                binding: None,
                model: None,
                error: None,
                busy: false,
            });
            revision
        };
        self.present_ui(revision, reference).await
    }
    async fn present_ui(self: &Arc<Self>, revision: u64, reference: &UiReference) -> Result<()> {
        let lease = Arc::new(self.ui.present(reference).map_err(error)?);
        self.present_lease(revision, lease).await
    }
    async fn present_lease(
        self: &Arc<Self>,
        revision: u64,
        lease: Arc<rsi_ui::PresentationLease>,
    ) -> Result<()> {
        let stop = {
            let mut details = self.details.lock().expect("Web details poisoned");
            if details.revision != revision {
                return Err("UI selection was replaced".into());
            }
            let detail = details.ui.as_mut().expect("selected UI detail");
            detail.binding = Some(lease.identity().reference.clone());
            detail.lease = Some(lease.clone());
            details.stop.clone()
        };
        self.changed();
        let ready = tokio::select! { biased;
            () = stop.cancelled() => Err("UI detail closed".into()),
            result = lease.ready() => result.map_err(error),
        };
        self.model_result(&lease, ready, false);
        let app = self.clone();
        let task = self.tasks.token();
        self.execution.spawn(async move {
            let _task = task;
            let mut changed = lease.changes();
            loop {
                if stop.is_cancelled() {
                    break;
                }
                let status = changed.borrow_and_update().clone();
                if status.stopped {
                    break;
                }
                match lease.snapshot() {
                    Ok(Some(snapshot)) => app.model_result(&lease, Ok(snapshot), false),
                    Err(failure) => {
                        app.model_result(&lease, Err(error(failure)), false);
                        break;
                    }
                    Ok(None) => {}
                }
                tokio::select! { biased;
                    () = stop.cancelled() => break,
                    result = changed.changed() => { if result.is_err() { break; } }
                }
            }
            if let Err(failure) = lease.close().await {
                *app.notice.lock().expect("Web notice poisoned") = error(failure);
                app.changed();
            }
        });
        Ok(())
    }
    fn model_result(
        &self,
        lease: &Arc<rsi_ui::PresentationLease>,
        result: Result<rsi_ui::SnapshotPin>,
        settled: bool,
    ) {
        let mut details = self.details.lock().expect("Web details poisoned");
        if details.stop.is_cancelled() {
            return;
        }
        let Some(detail) = details.ui.as_mut().filter(|detail| {
            detail
                .lease
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, lease))
        }) else {
            return;
        };
        if settled {
            detail.busy = false;
        }
        match result {
            Ok(snapshot) => {
                if !settled
                    && detail
                        .snapshot
                        .as_ref()
                        .is_some_and(|previous| previous.revision() == snapshot.revision())
                {
                    return;
                }
                detail.model = Some(snapshot.model().model);
                detail.snapshot = Some(snapshot);
                detail.error = None;
            }
            Err(failure) => detail.error = Some(failure),
        }
        drop(details);
        self.changed();
    }
    pub(crate) async fn ui_block(
        self: &Arc<Self>,
        index: crate::SurfaceId,
        generation: &str,
        key: &str,
    ) -> Result<()> {
        let (attached, revision) = self.ui_selection(index, generation)?;
        let block = {
            let state = attached
                .renderer
                .state
                .lock()
                .expect("Web renderer poisoned");
            state
                .history
                .as_ref()
                .unwrap_or(&state.transcript)
                .blocks
                .iter()
                .find(|block| block.key == key)
                .cloned()
        };
        if let Some(block) = &block {
            let sources = block.sources();
            if let Some(lease) = self
                .ui
                .present_inline(
                    &attached.ui_target,
                    &rsi_ui::BlockInput {
                        key: &block.key,
                        text: &block.text,
                        tool: block.tool.as_ref(),
                        sources: &sources,
                    },
                )
                .map_err(error)?
            {
                return self.present_lease(revision, Arc::new(lease)).await;
            }
        }
        // Plugin code runs after releasing both application and renderer locks.
        let result = block
            .ok_or_else(|| "This block is no longer retained".to_owned())
            .and_then(|block| {
                let sources = block.sources();
                self.ui
                    .block(
                        &attached.ui_target,
                        &rsi_ui::BlockInput {
                            key: &block.key,
                            text: &block.text,
                            tool: block.tool.as_ref(),
                            sources: &sources,
                        },
                    )
                    .map_err(error)?
                    .ok_or_else(|| "No active contribution renders this block".into())
            });
        self.ui_result(revision, result);
        Ok(())
    }
    pub(crate) async fn ui_invoke(
        &self,
        ticket: &str,
        name: String,
        input: ActionInput,
    ) -> Result<()> {
        if ticket.starts_with("inline:") {
            return self.inline_invoke(ticket, name, input).await;
        }
        if self
            .details
            .lock()
            .expect("Web details poisoned")
            .ui
            .as_ref()
            .is_some_and(|detail| detail.remote.is_some())
        {
            return self.remote_ui_invoke(ticket, name, input).await;
        }
        self.local_ui_invoke(ticket, name, input).await
    }

    async fn local_ui_invoke(&self, ticket: &str, name: String, input: ActionInput) -> Result<()> {
        if let Some((lease, snapshot)) = {
            let details = self.details.lock().expect("Web details poisoned");
            details
                .ui
                .as_ref()
                .filter(|detail| detail.ticket == ticket && !detail.busy)
                .and_then(|detail| Some((detail.lease.clone()?, detail.snapshot.clone()?)))
        } {
            let admitted = {
                let mut details = self.details.lock().expect("Web details poisoned");
                let next = details
                    .revision
                    .checked_add(1)
                    .ok_or("Detail generation exhausted")?;
                let Some(detail) = details
                    .ui
                    .as_mut()
                    .filter(|detail| detail.ticket == ticket && !detail.busy)
                else {
                    return Ok(());
                };
                let invocation = lease.invoke(
                    &rsi_ui::PresentationAction {
                        presentation: snapshot.identity().clone(),
                        revision: snapshot.revision(),
                        action: name,
                    },
                    input,
                );
                detail.busy = true;
                detail.ticket = next.to_string();
                detail.error = None;
                details.revision = next;
                invocation
            };
            self.changed();
            let result = admitted.await.map_err(error);
            self.model_result(&lease, result, true);
            return Ok(());
        }
        let selected = {
            let details = self.details.lock().expect("Web details poisoned");
            let Some(detail) = details
                .ui
                .as_ref()
                .filter(|detail| detail.ticket == ticket && !detail.busy)
            else {
                return Ok(());
            };
            let Some(view) = &detail.view else {
                return Ok(());
            };
            let reference = view
                .actions
                .get(&name)
                .cloned()
                .ok_or("This action is not part of the displayed card")?;
            (detail.pane, detail.generation.clone(), reference)
        };
        let reference = selected.2;
        let pane = self.pane(selected.0.ok_or("Legacy card requires a Session target")?)?;
        let (revision, stop) = {
            let current = pane.current.lock().expect("Web pane poisoned");
            let attached = current
                .as_ref()
                .filter(|attached| Some(attached.generation.to_string()) == selected.1)
                .ok_or("This pane changed; reopen its card")?;
            if !self.ui.matches_target(&attached.ui_target, &reference) {
                return Err("This UI action has retired".into());
            }
            let mut details = self.details.lock().expect("Web details poisoned");
            if details
                .ui
                .as_ref()
                .is_none_or(|detail| detail.ticket != ticket || detail.busy)
            {
                return Ok(());
            }
            let mut detail = details.ui.take().expect("selected UI detail");
            let revision = details.begin()?;
            detail.ticket = revision.to_string();
            detail.busy = true;
            detail.error = None;
            details.ui = Some(detail);
            (revision, details.stop.clone())
        };
        self.changed();
        let result = self
            .ui
            .invoke_in_view(&reference, input, stop.clone())
            .await
            .map_err(error);
        self.ui_result(revision, result);
        Ok(())
    }
}

impl GuiApplication {
    /// Reads a model-local source from the exact displayed detail snapshot.
    ///
    /// # Panics
    /// Panics if a previous application panic poisoned detail storage.
    pub fn read_ui_source(
        &self,
        ticket: &str,
        name: &str,
        offset: u64,
        maximum: usize,
    ) -> futures_util::future::BoxFuture<'static, Result<rsi_api_protocol::RetainedBytes>> {
        if ticket.starts_with("inline:") {
            return self.inline_source(ticket, name, offset, maximum);
        }
        if self
            .details
            .lock()
            .expect("Web details poisoned")
            .ui
            .as_ref()
            .is_some_and(|detail| detail.remote.is_some())
        {
            return self.remote_ui_source(ticket, name, offset, maximum);
        }
        let selected = {
            let details = self.details.lock().expect("Web details poisoned");
            details
                .ui
                .as_ref()
                .filter(|detail| {
                    detail.ticket == ticket && !detail.busy && !details.stop.is_cancelled()
                })
                .and_then(|detail| {
                    Some((detail.lease.clone()?, detail.snapshot.as_ref()?.revision()))
                })
        };
        let Some((lease, revision)) = selected else {
            return Box::pin(async { Err("This model source has retired".into()) });
        };
        let read = lease.source(revision, name, offset, maximum);
        Box::pin(async move { read.await.map_err(error) })
    }
}
