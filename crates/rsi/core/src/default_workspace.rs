//! Product-owned first-use directory policy; Workspace remains a registry.
use async_trait::async_trait;
use rsi_agent_store_protocol::{SessionStore, SessionStoreContract};
use rsi_api_protocol::{ApiError, ApiRegistrarContract, CallOrigin, json_handler};
use rsi_configuration_access::{ConfigurationAccess, ConfigurationAccessContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage_domain::{Domain, DomainFacilityContract, DomainSpec};
use rsi_workspace_protocol::{
    WorkspaceError, WorkspaceIngress, WorkspaceIngressContract, WorkspaceRecord, WorkspaceRegistry,
    WorkspaceRegistryContract,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex;

#[cfg(test)]
mod tests;

fn create_default_directory(documents: &std::path::Path) -> std::io::Result<()> {
    use rsi_files_native_fs::{
        DirectoryCreationError, create_directory_no_follow, open_absolute_directory_no_follow,
        open_relative_directory_no_follow,
    };
    let mut parent = open_absolute_directory_no_follow(documents)?;
    for name in ["RSIversi", "default-workspace"] {
        let child = match create_directory_no_follow(&parent, std::ffi::OsStr::new(name)) {
            Ok(child) => {
                child.sync_all()?;
                parent.sync_all()?;
                child
            }
            Err(DirectoryCreationError::NotCreated(error))
                if error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                open_relative_directory_no_follow(&parent, std::path::Path::new(name))?
            }
            Err(
                DirectoryCreationError::NotCreated(error) | DirectoryCreationError::Created(error),
            ) => return Err(error),
        };
        parent = child;
    }
    Ok(())
}

fn read_failure(error: impl std::fmt::Display) -> ApiError {
    let mut message = error.to_string();
    let mut end = message.len().min(512);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    ApiError::Backend(message)
}

#[derive(Debug)]
pub(crate) struct Factory {
    pub documents: Option<PathBuf>,
}
struct Owner {
    documents: Option<PathBuf>,
    domain: Arc<dyn Domain>,
    workspace: Arc<dyn WorkspaceRegistry>,
    ingress: Arc<dyn WorkspaceIngress>,
    store: Arc<dyn SessionStore>,
    access: Arc<ConfigurationAccess>,
    receipt: Arc<Mutex<Option<WorkspaceRecord>>>,
    closed: AtomicBool,
    execution: rsi_meta::Execution,
}
impl Owner {
    async fn initialize(
        self: Arc<Self>,
        origin: CallOrigin,
    ) -> rsi_api_protocol::Result<std::result::Result<Option<WorkspaceRecord>, String>> {
        let grant = self.access.admit(&origin)?;
        let guard = self.receipt.clone().lock_owned().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(ApiError::ShuttingDown);
        }
        let execution = self.execution.clone();
        execution
            .spawn(async move {
                let (_grant, mut receipt) = (grant, guard);
                let selected = self.ingress.scoped(origin);
                if let Some(saved) = receipt.as_ref() {
                    return match selected.get(&saved.id).await {
                        Ok(record) => Ok(Ok(Some(record))),
                        Err(WorkspaceError::Unknown(_)) => Ok(Ok(None)),
                        Err(error) => Ok(Err(error.to_string())),
                    };
                }
                let Some(documents) = self.documents.as_ref() else {
                    return Ok(Ok(None));
                };
                let path = documents.join("RSIversi/default-workspace");
                let existing = self.workspace.list(None, 2).await.map_err(read_failure)?;
                let canonical = tokio::fs::canonicalize(&path)
                    .await
                    .ok()
                    .map(rsi_workspace_protocol::canonical_native_path);
                // A prior registration whose receipt was interrupted is recoverable by identity.
                let candidate = existing
                    .records
                    .iter()
                    .find(|record| {
                        record.coordinates.location()
                            == &rsi_workspace_protocol::ExecutionLocation::Local
                            && canonical.as_deref()
                                == Some(std::path::Path::new(record.coordinates.path()))
                    })
                    .cloned();
                if candidate.is_none()
                    && (!existing.records.is_empty()
                        || !self
                            .store
                            .list_sessions(None, 1)
                            .await
                            .map_err(read_failure)?
                            .sessions
                            .is_empty())
                {
                    return Ok(Ok(None));
                }
                let record = if let Some(record) = candidate {
                    record
                } else {
                    let directory = documents.clone();
                    let created =
                        tokio::task::spawn_blocking(move || create_default_directory(&directory))
                            .await
                            .map_err(|_| ApiError::OutcomeUnknown)?;
                    if let Err(error) = created {
                        return Ok(Err(format!(
                            "Default workspace directory could not be opened: {}",
                            error.kind()
                        )));
                    }
                    // Recheck registry and history after filesystem work, before registration.
                    if !self
                        .workspace
                        .list(None, 1)
                        .await
                        .map_err(read_failure)?
                        .records
                        .is_empty()
                    {
                        return Ok(Ok(None));
                    }
                    if !self
                        .store
                        .list_sessions(None, 1)
                        .await
                        .map_err(read_failure)?
                        .sessions
                        .is_empty()
                    {
                        return Ok(Ok(None));
                    }
                    match selected.get_or_create(&path).await {
                        Ok(record) => record,
                        Err(error) => return Ok(Err(error.to_string())),
                    }
                };
                let value = serde_json::to_value(&record).map_err(read_failure)?;
                self.domain
                    .put("initialized", value)
                    .await
                    .map_err(|_| ApiError::OutcomeUnknown)?;
                *receipt = Some(record.clone());
                Ok(Ok(Some(record)))
            })
            .await
            .map_err(|_| ApiError::OutcomeUnknown)?
    }
}
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if config != &serde_json::json!({"documents":self.documents}) {
            return Err(MetaError::InvalidInput(
                "Default workspace policy must match captured Documents directory".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<DomainFacilityContract>()
            .requiring_local::<WorkspaceRegistryContract>()
            .requiring_local::<WorkspaceIngressContract>()
            .requiring_local::<SessionStoreContract>()
            .requiring_local::<ConfigurationAccessContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: "rsi.workspace-default".into(),
                backend: "base".into(),
                version: 1,
                maximum_records: 1,
                maximum_bytes: 128 * 1024,
            })
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let mut snapshot = domain
            .snapshot()
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        if snapshot.len() > 1 || snapshot.keys().any(|key| key != "initialized") {
            return Err(MetaError::Activation(
                "Invalid default workspace receipt".into(),
            ));
        }
        let receipt: Option<WorkspaceRecord> = snapshot
            .remove("initialized")
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| MetaError::Activation("Invalid default workspace receipt".into()))?;
        if let Some(record) = &receipt {
            record
                .validate()
                .map_err(|error| MetaError::Activation(error.to_string()))?;
            if record.coordinates.location() != &rsi_workspace_protocol::ExecutionLocation::Local {
                return Err(MetaError::Activation(
                    "Default workspace must be local".into(),
                ));
            }
        }
        let owner = Arc::new(Owner {
            documents: self.documents.clone(),
            domain,
            workspace: plan.local::<WorkspaceRegistryContract>()?,
            ingress: plan.local::<WorkspaceIngressContract>()?,
            store: plan.local::<SessionStoreContract>()?,
            access: plan.local::<ConfigurationAccessContract>()?,
            receipt: Arc::new(Mutex::new(receipt)),
            closed: AtomicBool::new(false),
            execution: plan.context().runtime().execution().clone(),
        });
        let service = owner.clone();
        let registration = plan
            .local::<ApiRegistrarContract>()?
            .register(
                rsi_workspace_default_api::operation(),
                json_handler(move |context, _: rsi_workspace_default_api::Initialize| {
                    service.clone().initialize(context.origin)
                }),
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "drain default workspace initialization",
            Box::new(move || {
                Box::pin(async move {
                    drop(registration);
                    owner.closed.store(true, Ordering::Release);
                    let _settled = owner.receipt.lock().await;
                    Ok(())
                })
            }),
        )
    }
}
