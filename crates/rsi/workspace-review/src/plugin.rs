use crate::{WorkspaceReview, git::Git, owner::Observer, scratch_root::Root};
use async_trait::async_trait;
use rsi_api_protocol::{ApiError, ApiRegistrarContract, ApiRegistration, json_handler};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_storage_domain::{DomainFacilityContract, DomainSpec};
use rsi_workspace_review_api::{ConversationIdentity, Request};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(e.to_string())
}
/// Local owner of finite workspace execution evidence.
#[derive(Debug)]
pub struct WorkspaceReviewContract;
impl LocalContract for WorkspaceReviewContract {
    const KEY: &'static str = "rsi.workspace-review";
    type Service = WorkspaceReview;
}
/// Product plugin for isolated Git captures and durable summaries.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceReviewFactory;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    directory: PathBuf,
    program: PathBuf,
}
#[async_trait]
impl PluginFactory for WorkspaceReviewFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let input: Config = serde_json::from_value(config.clone()).map_err(meta)?;
        if !input.directory.is_absolute() || !input.program.is_absolute() {
            return Err(meta("review scratch and Git executable must be absolute"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<DomainFacilityContract>()
            .requiring_local::<rsi_process::ProcessContract>()
            .requiring_local::<rsi_sandbox::SandboxContract>()
            .requiring_local::<rsi_files_protocol::FilesContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let input: Config = serde_json::from_value(plan.config().as_ref().clone()).map_err(meta)?;
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: "rsi.workspace-review".into(),
                backend: "base".into(),
                version: 1,
                maximum_records: 8192,
                maximum_bytes: 64 * 1024 * 1024,
            })
            .await
            .map_err(meta)?;
        let root = Root::open(input.directory).await.map_err(meta)?;
        let git = Git {
            process: plan.local::<rsi_process::ProcessContract>()?,
            sandbox: plan.local::<rsi_sandbox::SandboxContract>()?,
            files: plan.local::<rsi_files_protocol::FilesContract>()?,
            program: input.program,
            quota: Arc::new(tokio::sync::Semaphore::new(1024 * 1024 * 1024)),
            tasks: TaskTracker::new(),
        };
        let owner = WorkspaceReview::open(
            domain,
            root,
            git,
            plan.context().runtime().execution().clone(),
        )
        .await
        .map_err(meta)?;
        let observer: Arc<dyn rsi_agent_turn_protocol::ExecutionObserver> =
            Arc::new(Observer(owner.clone()));
        let observation = plan
            .context()
            .provide_local::<rsi_agent_turn_protocol::ExecutionObserverContract>(observer)?;
        let supply = plan
            .context()
            .provide_local::<WorkspaceReviewContract>(owner.clone())?;
        plan.defer(
            "drain workspace review",
            Box::new(move || {
                Box::pin(async move {
                    drop((supply, observation));
                    owner.close().await;
                    Ok(())
                })
            }),
        )
    }
}
/// Read API adapter independently reauthorizing every requested source.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceReviewApiFactory;
#[derive(Serialize)]
enum Never {}
#[async_trait]
impl PluginFactory for WorkspaceReviewApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("workspace review API config must be null"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<WorkspaceReviewContract>()
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<rsi_workspace_protocol::WorkspaceRegistryContract>()
            .requiring_local::<rsi_agent_store_protocol::SessionStoreContract>()
            .requiring_local::<rsi_acp_protocol::service::ExternalConversationsContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let owner = plan.local::<WorkspaceReviewContract>()?;
        let store = plan.local::<rsi_agent_store_protocol::SessionStoreContract>()?;
        let workspaces = plan.local::<rsi_workspace_protocol::WorkspaceRegistryContract>()?;
        let external = plan.local::<rsi_acp_protocol::service::ExternalConversationsContract>()?;
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let mut registrations = Vec::new();
        for spec in rsi_workspace_review_api::operations() {
            let (owner, store, workspaces, external) = (
                owner.clone(),
                store.clone(),
                workspaces.clone(),
                external.clone(),
            );
            let expected = spec.clone();
            registrations.push(
                registrar
                    .register(
                        spec,
                        json_handler(move |_, request: Request| {
                            let (owner, store, workspaces, external, expected) = (
                                owner.clone(),
                                store.clone(),
                                workspaces.clone(),
                                external.clone(),
                                expected.clone(),
                            );
                            async move {
                                request.validate()?;
                                if request.spec() != expected {
                                    return Err(ApiError::Invalid(
                                        "review operation mismatch".into(),
                                    ));
                                }
                                let scope = request.scope();
                                let workspace = workspaces
                                    .get(&scope.workspace)
                                    .await
                                    .map_err(|_| ApiError::Unavailable)?;
                                let cwd = match &scope.conversation {
                                    ConversationIdentity::Native(id) => store
                                        .header(id)
                                        .await
                                        .map_err(|_| ApiError::Unavailable)?
                                        .canonical_cwd()
                                        .to_owned(),
                                    ConversationIdentity::External(id) => {
                                        external
                                            .view(id)
                                            .await
                                            .map_err(|_| ApiError::Unavailable)?
                                            .snapshot
                                            .cwd
                                    }
                                };
                                if workspace.path.to_str() != Some(&cwd) {
                                    return Err(ApiError::Invalid(
                                        "review source outside workspace".into(),
                                    ));
                                }
                                owner
                                    .read(request, CancellationToken::new())
                                    .await
                                    .map(Ok::<_, Never>)
                            }
                        }),
                    )
                    .map_err(meta)?,
            );
        }
        plan.defer(
            "withdraw workspace review API",
            Box::new(move || {
                Box::pin(async move {
                    futures_util::future::join_all(
                        registrations.into_iter().map(ApiRegistration::close),
                    )
                    .await;
                    Ok(())
                })
            }),
        )
    }
}
