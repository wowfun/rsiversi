#[path = "settings.rs"]
mod settings;

use crate::application::{Command, Recent, Result, WebApplication, error};

impl WebApplication {
    pub(crate) async fn refresh(&self, command: Command) -> Result<()> {
        let _work = self.catalog_work.lock().await;
        if matches!(command, Command::Refresh | Command::WorkspacesNext) {
            let after = if matches!(command, Command::WorkspacesNext) {
                self.catalog
                    .lock()
                    .expect("Web catalog poisoned")
                    .workspace_cursor
            } else {
                None
            };
            let page = self.workspace.list(after, 64).await.map_err(error)?;
            let mut catalog = self.catalog.lock().expect("Web catalog poisoned");
            catalog.workspaces_more = page.next.is_some();
            catalog.workspace_cursor = page.next;
            catalog.workspaces = page.records;
        }
        if matches!(command, Command::Refresh | Command::SessionsNext) {
            let after = if matches!(command, Command::SessionsNext) {
                self.catalog
                    .lock()
                    .expect("Web catalog poisoned")
                    .session_cursor
                    .clone()
            } else {
                None
            };
            let page = rsi_client::read_with_capacity_retry(&self.execution, || {
                self.session.list_recent(after.as_ref(), 20)
            })
            .await
            .map_err(error)?;
            let mut catalog = self.catalog.lock().expect("Web catalog poisoned");
            catalog.sessions_more = page.has_more;
            catalog.session_cursor = page
                .sessions
                .last()
                .map(rsi_session_protocol::SessionSummary::cursor);
            catalog.sessions = page
                .sessions
                .iter()
                .map(|session| Recent {
                    id: session.header.session_id().to_string(),
                    path: session.header.canonical_cwd().into(),
                })
                .collect();
        }
        if matches!(command, Command::Refresh | Command::ModelsNext) {
            let after = if matches!(command, Command::ModelsNext) {
                self.catalog
                    .lock()
                    .expect("Web catalog poisoned")
                    .models
                    .last()
                    .cloned()
            } else {
                None
            };
            let page = self
                .models
                .list_models(after.as_ref(), 64)
                .await
                .map_err(error)?;
            let mut catalog = self.catalog.lock().expect("Web catalog poisoned");
            catalog.models_more = page.has_more;
            catalog.models = page.models;
        }
        Ok(())
    }
}
