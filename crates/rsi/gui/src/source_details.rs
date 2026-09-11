use crate::{
    application::{GuiApplication, Result, error},
    details::{SOURCE_PAGE_BYTES, SourceDetail},
};
use rsi_conversation::SourceRef;

impl GuiApplication {
    pub(super) fn inspect_block(
        &self,
        index: crate::SurfaceId,
        generation: &str,
        key: &str,
    ) -> Result<()> {
        let pane = self.pane(index)?;
        let current = pane.current.lock().expect("Web pane poisoned");
        let attached = current
            .as_ref()
            .filter(|current| current.generation.to_string() == generation)
            .ok_or("This pane changed; reopen its source list")?;
        let sources = {
            let state = attached
                .renderer
                .state
                .lock()
                .expect("Web renderer poisoned");
            let transcript = state.history.as_ref().unwrap_or(&state.transcript);
            transcript
                .blocks
                .iter()
                .find(|block| block.key == key)
                .ok_or("This block is no longer in the retained view")?
                .sources()
        };
        let mut details = self.details.lock().expect("Web details poisoned");
        let ticket = details.begin()?.to_string();
        details.block_sources = Some(crate::details::BlockSources::new(
            index,
            generation.into(),
            ticket,
            sources,
        ));
        Ok(())
    }

    pub(super) fn block_sources_page(&self, ticket: &str, forward: bool) -> Result<()> {
        let mut details = self.details.lock().expect("Web details poisoned");
        if details
            .block_sources
            .as_ref()
            .is_none_or(|sources| sources.ticket != ticket)
        {
            return Ok(());
        }
        let mut sources = details.block_sources.take().expect("current source list");
        let ticket = details.begin()?.to_string();
        sources.page(ticket, forward);
        details.block_sources = Some(sources);
        Ok(())
    }

    pub(super) async fn source_page(&self, ticket: &str, forward: bool) -> Result<()> {
        let selected = {
            let details = self.details.lock().expect("Web details poisoned");
            let Some(detail) = details
                .source
                .as_ref()
                .filter(|detail| detail.ticket == ticket)
            else {
                return Ok(());
            };
            let Some(window) = &detail.window else {
                return Ok(());
            };
            let start = if forward {
                if !window.more {
                    return Ok(());
                }
                window.end
            } else {
                window.start.saturating_sub(SOURCE_PAGE_BYTES)
            };
            (detail.pane, detail.generation.clone(), detail.source, start)
        };
        self.inspect_source(
            selected.0,
            &selected.1,
            selected.2,
            selected.3,
            Some(ticket),
        )
        .await
    }

    pub(super) async fn inspect_source(
        &self,
        index: crate::SurfaceId,
        generation: &str,
        source: SourceRef,
        start: usize,
        ticket: Option<&str>,
    ) -> Result<()> {
        let pane = self.pane(index)?;
        // The same lock order as pane replacement makes selection and invalidation atomic.
        let (attachment, revision, stop) = {
            let current = pane.current.lock().expect("Web pane poisoned");
            let mut details = self.details.lock().expect("Web details poisoned");
            if ticket.is_some_and(|ticket| {
                details
                    .source
                    .as_ref()
                    .is_none_or(|detail| detail.ticket != ticket)
            }) {
                return Ok(());
            }
            let attached = current
                .as_ref()
                .filter(|current| current.generation.to_string() == generation)
                .cloned()
                .ok_or("This pane changed; retry the action in the current conversation")?;
            let revision = details.begin()?;
            details.source = Some(SourceDetail {
                pane: index,
                generation: generation.into(),
                source,
                ticket: revision.to_string(),
                window: None,
                error: None,
            });
            (attached, revision, details.stop.clone())
        };
        self.changed();
        let result = attachment
            .controller
            .source_window(source, start, SOURCE_PAGE_BYTES, stop)
            .await
            .map_err(error);
        let current = pane.current.lock().expect("Web pane poisoned");
        if current
            .as_ref()
            .is_some_and(|current| std::sync::Arc::ptr_eq(current, &attachment))
        {
            self.details
                .lock()
                .expect("Web details poisoned")
                .source_result(revision, result);
        }
        // A source error belongs to this detail; superseded reads never replace a global notice.
        Ok(())
    }
}
