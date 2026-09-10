use crate::wire::{Failure, IdRequest, ListRequest, Operation, RegisterRequest};
use async_trait::async_trait;
use rsi_api_protocol::{ApiClient, ApiClientContract, ApiError, call_json};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_workspace_protocol::{
    MAXIMUM_WORKSPACES_PER_PAGE, Result, WorkspaceCursor, WorkspaceError, WorkspaceId,
    WorkspacePage, WorkspaceRecord, WorkspaceRegistry, WorkspaceRegistryContract, WorkspaceStatus,
    validate_workspace_path,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::Path, sync::Arc};

/// Workspace proxy retaining one negotiated generic API client generation.
#[derive(Debug)]
pub struct WorkspaceClient {
    api: Arc<dyn ApiClient>,
}
impl WorkspaceClient {
    /// Requires every exact domain operation before publishing a usable Workspace proxy.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if Operation::ALL
            .iter()
            .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self { api })
    }
    async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: Operation,
        input: &I,
    ) -> Result<O> {
        call_json::<_, O, Failure>(self.api.as_ref(), &operation.spec(), input)
            .await
            .map_err(WorkspaceError::Api)?
            .map_err(WorkspaceError::from)
    }
}
#[async_trait]
impl WorkspaceRegistry for WorkspaceClient {
    async fn get(&self, id: &WorkspaceId) -> Result<WorkspaceRecord> {
        let record: WorkspaceRecord = self
            .call(Operation::Get, &IdRequest { id: id.clone() })
            .await?;
        record.validate()?;
        if record.id != *id {
            return Err(WorkspaceError::Corrupt(
                "Workspace response changed the requested identity".into(),
            ));
        }
        Ok(record)
    }
    async fn list(&self, after: Option<WorkspaceCursor>, limit: usize) -> Result<WorkspacePage> {
        if !(1..=MAXIMUM_WORKSPACES_PER_PAGE).contains(&limit) {
            return Err(WorkspaceError::InvalidInput(
                "workspace page limit must be 1..=256".into(),
            ));
        }
        let page: WorkspacePage = self
            .call(Operation::List, &ListRequest { after, limit })
            .await?;
        page.validate(after, limit)?;
        Ok(page)
    }
    async fn get_or_create(&self, path: &Path) -> Result<WorkspaceRecord> {
        validate_workspace_path(path)?;
        let record: WorkspaceRecord = self
            .call(Operation::Register, &RegisterRequest { path: path.into() })
            .await?;
        record.validate().map_err(crate::wire::invalid_record)?;
        Ok(record)
    }
    async fn status(&self, id: &WorkspaceId) -> Result<WorkspaceStatus> {
        self.call(Operation::Status, &IdRequest { id: id.clone() })
            .await
    }
    async fn delete_registration(&self, id: &WorkspaceId) -> Result<bool> {
        self.call(Operation::Delete, &IdRequest { id: id.clone() })
            .await
    }
}

/// Ordinary Meta client plugin, independent of rendering or native Workspace providers.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceClientFactory;
#[async_trait]
impl PluginFactory for WorkspaceClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        crate::empty(config)?;
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let client = WorkspaceClient::new(plan.local::<ApiClientContract>()?)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let supply = plan
            .context()
            .provide_local::<WorkspaceRegistryContract>(Arc::new(client))?;
        plan.defer(
            "withdraw Workspace client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
