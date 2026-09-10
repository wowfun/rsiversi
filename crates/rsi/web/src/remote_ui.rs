use super::{Arc, Result, WebApplication, error};
use crate::details::{RemoteCatalog, RemotePresentation};
use rsi_ui_api::{
    CatalogCursor, CatalogRequest, ExportScope, Invoke, Observe, Selection, Source, UiItem,
};

impl WebApplication {
    pub(crate) async fn remote_ui_list(
        &self,
        pane: u8,
        generation: &str,
        after: Option<CatalogCursor>,
    ) -> Result<()> {
        let client = self.remote_ui.as_ref().ok_or("Service UI is unavailable")?;
        let (attached, revision) = self.ui_selection(pane, generation)?;
        let scope = ExportScope {
            kind: "session".into(),
            key: attached.id.to_string(),
        };
        let stop = {
            let mut details = self.details.lock().expect("Web details poisoned");
            if details.revision != revision {
                return Err("UI selection changed".into());
            }
            details.ui = None;
            details.remote_catalog = Some(RemoteCatalog {
                pane,
                generation: generation.into(),
                ticket: revision.to_string(),
                page: None,
                error: None,
                scope: scope.clone(),
            });
            details.stop.clone()
        };
        self.changed();
        let request = CatalogRequest {
            scope,
            after,
            maximum: 64,
        };
        let result = tokio::select! { biased;
            () = stop.cancelled() => return Ok(()),
            result = client.catalog(&request) => result,
        };
        let mut details = self.details.lock().expect("Web details poisoned");
        if details.revision == revision
            && let Some(catalog) = &mut details.remote_catalog
        {
            match result {
                Ok(entries) => catalog.page = Some(entries),
                Err(failure) => catalog.error = Some(error(failure)),
            }
        }
        drop(details);
        self.changed();
        Ok(())
    }

    pub(crate) async fn remote_ui_next(&self, ticket: &str) -> Result<()> {
        let (pane, generation, after) = {
            let details = self.details.lock().expect("Web details poisoned");
            let catalog = details
                .remote_catalog
                .as_ref()
                .filter(|catalog| catalog.ticket == ticket)
                .ok_or("This extension catalog has retired")?;
            let after = catalog
                .page
                .as_ref()
                .and_then(|page| page.next.clone())
                .ok_or("No more extensions")?;
            (catalog.pane, catalog.generation.clone(), after)
        };
        self.remote_ui_list(pane, &generation, Some(after)).await
    }

    pub(crate) async fn remote_ui_surface(
        self: &Arc<Self>,
        ticket: &str,
        bundle: &str,
        surface: &str,
    ) -> Result<()> {
        let client = self
            .remote_ui
            .as_ref()
            .ok_or("Service UI is unavailable")?
            .clone();
        let (pane, generation, scope) = {
            let details = self.details.lock().expect("Web details poisoned");
            let catalog = details
                .remote_catalog
                .as_ref()
                .filter(|catalog| catalog.ticket == ticket)
                .ok_or("This extension catalog has retired")?;
            if !catalog.page.as_ref().is_some_and(|page| {
                page.entries
                    .iter()
                    .any(|entry| entry.bundle == bundle && entry.surface == surface)
            }) {
                return Err("This extension is not in the displayed catalog".into());
            }
            (
                catalog.pane,
                catalog.generation.clone(),
                catalog.scope.clone(),
            )
        };
        let (attached, revision) = self.ui_selection(pane, &generation)?;
        if attached.id.as_str() != scope.key {
            return Err("Extension target changed".into());
        }
        let application = rsi_ui::fresh_identity("web-ui")?;
        let stop = {
            let mut details = self.details.lock().expect("Web details poisoned");
            if details.revision != revision {
                return Err("UI selection changed".into());
            }
            let detail = details.ui.as_mut().expect("selected detail");
            detail.busy = true;
            detail.remote = Some(RemotePresentation {
                application: application.clone(),
                item: None,
                closed: false,
            });
            details.stop.clone()
        };
        self.changed();
        let request = Observe {
            application: application.clone(),
            selections: vec![Selection {
                scope,
                bundle: bundle.into(),
                surface: surface.into(),
            }],
        };
        let observed = tokio::select! { biased;
            () = stop.cancelled() => return Ok(()),
            result = client.observe(&request) => result,
        };
        let mut observed = match observed {
            Ok(observed) => observed,
            Err(failure) => {
                self.remote_ui_error(&application, error(failure), true);
                return Ok(());
            }
        };
        let app = self.clone();
        let task = self.tasks.token();
        self.execution.spawn(async move {
            let _task = task;
            loop {
                let result = tokio::select! { biased;
                    () = stop.cancelled() => break,
                    result = observed.next() => result,
                };
                match result {
                    Ok(Some(item)) => app.remote_ui_item(&application, item),
                    ended => {
                        app.remote_ui_error(
                            &application,
                            ended
                                .err()
                                .map_or_else(|| "Service extension closed".into(), error),
                            true,
                        );
                        break;
                    }
                }
            }
            // Dropping the connection-owned stream retires the server's target lease.
            drop(observed);
        });
        Ok(())
    }

    fn remote_ui_item(&self, application: &str, item: UiItem) {
        let mut details = self.details.lock().expect("Web details poisoned");
        if details.stop.is_cancelled() {
            return;
        }
        let Some(revision) = details.revision.checked_add(1) else {
            return;
        };
        let Some(detail) = details.ui.as_mut().filter(|detail| {
            detail
                .remote
                .as_ref()
                .is_some_and(|remote| remote.application == application)
        }) else {
            return;
        };
        detail.ticket = revision.to_string();
        detail.binding = Some(item.item.snapshot.presentation.reference.clone());
        detail.model = Some(item.item.snapshot.model.clone());
        detail.busy = item.item.ticket.is_none();
        detail.error = None;
        detail.remote.as_mut().expect("remote detail").item = Some(item);
        details.revision = revision;
        drop(details);
        self.changed();
    }

    fn remote_ui_error(&self, application: &str, message: String, closed: bool) {
        let mut details = self.details.lock().expect("Web details poisoned");
        let Some(detail) = details.ui.as_mut().filter(|detail| {
            detail
                .remote
                .as_ref()
                .is_some_and(|remote| remote.application == application)
        }) else {
            return;
        };
        detail.error = Some(message);
        if closed {
            detail.busy = true;
            let remote = detail.remote.as_mut().expect("remote detail");
            remote.closed = true;
            if let Some(item) = &mut remote.item {
                item.item.ticket = None;
            }
        }
        drop(details);
        self.changed();
    }

    pub(super) async fn remote_ui_invoke(
        &self,
        ticket: &str,
        name: String,
        input: rsi_ui::ActionInput,
    ) -> Result<()> {
        let client = self.remote_ui.as_ref().ok_or("Service UI is unavailable")?;
        let request = {
            let mut details = self.details.lock().expect("Web details poisoned");
            let revision = details
                .revision
                .checked_add(1)
                .ok_or("Detail generation exhausted")?;
            let detail = details
                .ui
                .as_mut()
                .filter(|detail| detail.ticket == ticket && !detail.busy)
                .ok_or("This model input has retired")?;
            let remote = detail
                .remote
                .as_mut()
                .filter(|remote| !remote.closed)
                .ok_or("Service extension closed")?;
            let item = remote
                .item
                .as_mut()
                .ok_or("Service extension is not ready")?;
            let ticket = item
                .item
                .ticket
                .take()
                .ok_or("This model input was already consumed")?;
            let request = Invoke {
                application: remote.application.clone(),
                ticket,
                input,
                action: rsi_ui::PresentationAction {
                    presentation: item.item.snapshot.presentation.clone(),
                    revision: item.item.snapshot.revision,
                    action: name,
                },
            };
            detail.busy = true;
            detail.ticket = revision.to_string();
            detail.error = None;
            details.revision = revision;
            request
        };
        self.changed();
        if let Err(failure) = client.invoke(&request).await {
            self.remote_ui_error(&request.application, error(failure), false);
        }
        Ok(())
    }

    pub(super) fn remote_ui_source(
        &self,
        ticket: &str,
        name: &str,
        offset: u64,
        maximum: usize,
    ) -> futures_util::future::BoxFuture<'static, Result<rsi_api_protocol::RetainedBytes>> {
        let request: Result<_> = (|| {
            let details = self.details.lock().expect("Web details poisoned");
            if details.stop.is_cancelled() {
                return Err("Service extension closed".into());
            }
            let detail = details
                .ui
                .as_ref()
                .filter(|detail| detail.ticket == ticket && !detail.busy)
                .ok_or("This model source has retired")?;
            let remote = detail
                .remote
                .as_ref()
                .filter(|remote| !remote.closed)
                .ok_or("Service extension closed")?;
            let snapshot = &remote
                .item
                .as_ref()
                .ok_or("Service extension is not ready")?
                .item
                .snapshot;
            if maximum == 0
                || maximum > rsi_ui::MAXIMUM_INPUT_BYTES
                || !snapshot
                    .model
                    .sources
                    .iter()
                    .any(|source| source.name == name)
            {
                return Err("Invalid displayed source window".into());
            }
            let client = self
                .remote_ui
                .as_ref()
                .ok_or("Service UI is unavailable")?
                .clone();
            Ok((
                client,
                details.stop.clone(),
                Source {
                    application: remote.application.clone(),
                    presentation: snapshot.presentation.clone(),
                    revision: snapshot.revision,
                    name: name.into(),
                    offset,
                    maximum,
                },
            ))
        })();
        Box::pin(async move {
            let (client, stop, request) = request?;
            tokio::select! { biased;
                () = stop.cancelled() => Err("This model source has retired".into()),
                result = client.source(&request) => result.map_err(error),
            }
        })
    }
}
