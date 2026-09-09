use super::{Arc, Attachment, Result, WebApplication, error};
use crate::details::UiDetail;
use rsi_ui::{ActionInput, BoundView, UiReference};

impl WebApplication {
    fn ui_selection(&self, index: u8, generation: &str) -> Result<(Arc<Attachment>, u64)> {
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
            pane: index,
            generation: generation.into(),
            ticket: revision.to_string(),
            view: None,
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
            Ok(view) if self.ui.is_current(&view.reference) => detail.view = Some(view),
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
            .and_then(|detail| detail.view.as_ref())
            .is_some_and(|view| !self.ui.is_current(&view.reference))
        {
            let _ = details.begin();
        }
    }
    pub(crate) fn ui_surface(
        &self,
        index: u8,
        generation: &str,
        reference: &UiReference,
    ) -> Result<()> {
        let (attached, revision) = self.ui_selection(index, generation)?;
        let result = if self.ui.matches_target(&attached.ui_target, reference) {
            self.ui.surface(reference).map_err(error)
        } else {
            Err("This UI contribution belongs to another or retired surface".into())
        };
        self.ui_result(revision, result);
        Ok(())
    }
    pub(crate) fn ui_block(&self, index: u8, generation: &str, key: &str) -> Result<()> {
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
        reference: UiReference,
        input: ActionInput,
    ) -> Result<()> {
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
            if view.actions.get(&reference.name) != Some(&reference) {
                return Err("This action is not part of the displayed card".into());
            }
            (detail.pane, detail.generation.clone())
        };
        let pane = self.pane(selected.0)?;
        let (revision, stop) = {
            let current = pane.current.lock().expect("Web pane poisoned");
            let attached = current
                .as_ref()
                .filter(|attached| attached.generation.to_string() == selected.1)
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
