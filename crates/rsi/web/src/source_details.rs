use crate::{
    application::{Result, WebApplication, error},
    details::{SOURCE_PAGE_BYTES, SourceDetail},
};
use rsi_conversation::SourceRef;

impl WebApplication {
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
        index: u8,
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
