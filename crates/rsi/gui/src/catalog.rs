#[path = "settings.rs"]
mod settings;
pub(crate) use settings::PreferenceRefreshDelay;

use crate::application::{Command, GuiApplication, Result, error};

impl GuiApplication {
    pub(crate) async fn refresh(&self, command: Command) -> Result<()> {
        let _work = self.catalog_work.lock().await;
        if matches!(command, Command::Refresh)
            && let Some(api) = &self.api
        {
            let result = api.operations().await.map_err(error)?;
            *self.capabilities.lock().expect("GUI capabilities") =
                crate::application::Capabilities::new(Some(result));
        }
        if matches!(command, Command::Refresh | Command::WorkspacesNext)
            && self.available(&[rsi_workspace_api::Operation::List.spec()])
        {
            let after = if matches!(command, Command::WorkspacesNext) {
                self.catalog
                    .lock()
                    .expect("Web catalog poisoned")
                    .workspace_cursor
            } else {
                None
            };
            let page = self.workspace.list(after, 64).await.map_err(error)?;
            let seed = if self.available(&[rsi_workspace_api::Operation::OrderSeed.spec()]) {
                Some(self.workspace.order_seed().await.map_err(error)?)
            } else {
                None
            };
            let mut catalog = self.catalog.lock().expect("Web catalog poisoned");
            catalog.workspaces_more = page.next.is_some();
            catalog.workspace_cursor = page.next;
            catalog.workspaces = page.records;
            catalog.workspace_order_seed = seed;
        }
        if matches!(command, Command::Refresh | Command::ModelsNext)
            && self.available(&[rsi_ai_models_api::operation()])
        {
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
        if matches!(command, Command::Refresh) {
            self.clear_unavailable_catalog();
        }
        if matches!(command, Command::Refresh)
            && self.external.is_some()
            && self.available(&[
                rsi_acp_api::Operation::Endpoints.spec(),
                rsi_acp_api::Operation::Residents.spec(),
                rsi_acp_api::Operation::List.spec(),
            ])
        {
            self.refresh_external(false).await?;
        }
        Ok(())
    }

    fn clear_unavailable_catalog(&self) {
        let mut catalog = self.catalog.lock().expect("Web catalog poisoned");
        if !self.available(&[rsi_workspace_api::Operation::List.spec()]) {
            catalog.workspaces.clear();
            catalog.workspaces_more = false;
            catalog.workspace_order_seed = None;
        }
        if !self.available(&[rsi_ai_models_api::operation()]) {
            catalog.models.clear();
            catalog.models_more = false;
        }
    }
}

// Add coordinate keys at the presentation boundary without retaining another map.
pub(crate) fn serialize_workspaces<S: serde::Serializer>(
    records: &[rsi_workspace_protocol::WorkspaceRecord],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    #[derive(serde::Serialize)]
    struct Workspace<'a> {
        #[serde(flatten)]
        record: &'a rsi_workspace_protocol::WorkspaceRecord,
        key: rsi_workspace_protocol::WorkspaceKey,
    }
    serializer.collect_seq(records.iter().map(|record| Workspace {
        record,
        key: rsi_workspace_protocol::WorkspaceKey::from_coordinates(&record.coordinates),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_choices_expose_distinct_registration_and_history_identities() {
        use rsi_workspace_protocol::{
            ExecutionCoordinates, ExecutionLocation, WorkspaceId, WorkspaceKey, WorkspaceRecord,
        };
        let coordinates = ExecutionCoordinates::new(ExecutionLocation::Local, "/project").unwrap();
        let key = WorkspaceKey::from_coordinates(&coordinates);
        let id = WorkspaceId::from_bytes([0xaa; 32]);
        let catalog = crate::application::Catalog {
            workspaces: vec![WorkspaceRecord::new(id.clone(), coordinates)],
            ..Default::default()
        };
        let value = serde_json::to_value(catalog).unwrap();
        assert_eq!(value["workspaces"][0]["id"], id.as_str());
        assert_eq!(value["workspaces"][0]["key"], key.as_str());
        assert_ne!(value["workspaces"][0]["id"], value["workspaces"][0]["key"]);
    }
}
