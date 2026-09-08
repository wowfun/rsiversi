use crate::application::{Command, Recent, Result, SettingsEditor, WebApplication, error};

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
    pub(crate) async fn read_settings(&self, namespace: &str) -> Result<()> {
        if namespace.len() > 256 {
            return Err("Setting namespace exceeds its limit".into());
        }
        let revision = self.details.lock().expect("Web details poisoned").begin()?;
        self.changed();
        let snapshot = self.settings.read(namespace).await.map_err(error)?;
        let text = serde_json::to_string_pretty(&snapshot.value).map_err(error)?;
        if text.len() > 8 * 1024 * 1024 {
            return Err("Setting is too large for this editor".into());
        }
        let ticket = crate::identity::allocate("settings")?;
        self.details.lock().expect("Web details poisoned").settings(
            revision,
            SettingsEditor {
                namespace: namespace.into(),
                text,
                ticket,
                version: snapshot.version(),
            },
        );
        Ok(())
    }
    pub(crate) async fn save_settings(&self, ticket: &str, text: &str) -> Result<()> {
        let (namespace, version) = {
            let details = self.details.lock().expect("Web details poisoned");
            let editor = details
                .editor
                .as_ref()
                .filter(|editor| editor.ticket == ticket)
                .ok_or("Settings view changed; read it again before saving")?;
            (editor.namespace.clone(), editor.version.clone())
        };
        let value = serde_json::from_str(text).map_err(|_| "Settings must contain valid JSON")?;
        let snapshot = self
            .settings
            .replace(&namespace, &version, value)
            .await
            .map_err(error)?;
        let mut details = self.details.lock().expect("Web details poisoned");
        if details
            .editor
            .as_ref()
            .is_some_and(|editor| editor.ticket == ticket)
        {
            details.editor = Some(SettingsEditor {
                namespace,
                text: serde_json::to_string_pretty(&snapshot.value).map_err(error)?,
                ticket: crate::identity::allocate("settings")?,
                version: snapshot.version(),
            });
        }
        Ok(())
    }
}
