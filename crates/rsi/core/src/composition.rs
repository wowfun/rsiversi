use crate::settings::{AgentSettingsContract, AgentSettingsFactory, SETTINGS_FACTORY};
use async_trait::async_trait;
use rsi_agent_presets::{
    EXECUTOR_FACTORY, HeadlessAgentConfig, KERNEL_FACTORY, SQLITE_STORE_FACTORY, headless_fragment,
};
use rsi_agent_store_protocol::SessionStoreContract;
use rsi_agent_turn_protocol::{
    TurnExecutionContract, TurnFinalizationContract, TurnFinalizationError, TurnFinalizer,
    TurnServiceContract,
};
use rsi_ai_protocol::{ImageCallContract, LanguageCallContract};
use rsi_ai_provider::{ImageRegistrarContract, LanguageRegistrarContract};
use rsi_approval_protocol::{
    ApprovalAnswerer, ApprovalAnswerersContract, ApprovalContract, ApprovalDecision,
    ApprovalOutcome, ApprovalRequest,
};
use rsi_commands_protocol::CommandRuntimeContract;
use rsi_credentials_local::{CredentialsLocalFactory, KeyringSecretStore, SecretStore};
use rsi_credentials_protocol::{CredentialsAdminContract, CredentialsResolveContract, SecretValue};
use rsi_host::{Host, HostBuilder, HostPaths, ProfileEntry, ProfileFragment};
use rsi_jobs::{JobScope, Jobs, JobsContract};
use rsi_media_protocol::{MediaBackendContract, MediaContract, MediaReadContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, UpdateMode,
};
use rsi_permission_presets::PermissionPresetsContract;
use rsi_projection::ProjectionRegistryContract;
use rsi_sandbox::SandboxContract;
use rsi_settings_protocol::{SettingsContract, SettingsProviderContract};
use rsi_storage::StorageHubContract;
use rsi_storage_domain::DomainFacilityContract;
use rsi_tools_protocol::ToolRuntimeContract;
use rsi_workspace::WorkspaceRegistryContract;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) const OPENAI_FACTORY: &str = "rsi.ai.provider.openai";
pub(crate) const OPENAI_COMPATIBLE_FACTORY: &str = "rsi.ai.provider.openai-compatible";
pub(crate) const DEEPSEEK_FACTORY: &str = "rsi.ai.provider.deepseek";

const STORAGE_FACTORY: &str = "rsi.storage";
const STORAGE_SQLITE_FACTORY: &str = "rsi.storage.sqlite";
const STORAGE_DOMAIN_FACTORY: &str = "rsi.storage.domain";
const SETTINGS_LOCAL_FACTORY: &str = "rsi.settings.local";
const SETTINGS_CORE_FACTORY: &str = "rsi.settings";
const CREDENTIALS_FACTORY: &str = "rsi.credentials.local";
const MEDIA_LOCAL_FACTORY: &str = "rsi.media.local";
const MEDIA_FACTORY: &str = "rsi.media";
const APPROVAL_FACTORY: &str = "rsi.approval";
const DENY_APPROVAL_FACTORY: &str = "rsi.headless.approval.deny";
const PERMISSIONS_FACTORY: &str = "rsi.permission-presets";
const SANDBOX_FACTORY: &str = "rsi.sandbox.local";
const COMMANDS_FACTORY: &str = "rsi.commands";
const JOBS_FACTORY: &str = "rsi.jobs.local";
const JOBS_FINALIZER_FACTORY: &str = "rsi.headless.jobs-finalizer";
const PROJECTION_FACTORY: &str = "rsi.projection";
const WORKSPACE_FACTORY: &str = "rsi.workspace";
const TOOLS_FACTORY: &str = "rsi.tools";
const LANGUAGE_FACTORY: &str = "rsi.ai.language";
const IMAGE_FACTORY: &str = "rsi.ai.image";

/// Frozen inputs used to construct the standard linked catalog and fragments.
#[derive(Clone, Debug)]
pub struct StandardComposition {
    paths: HostPaths,
    captured_environment: BTreeMap<String, SecretValue>,
    credential_store: Arc<dyn SecretStore>,
}

impl StandardComposition {
    /// Creates a standard composition from explicit Host paths and captured secrets.
    pub fn new(paths: HostPaths, captured_environment: BTreeMap<String, SecretValue>) -> Self {
        Self {
            paths,
            captured_environment,
            credential_store: Arc::new(KeyringSecretStore),
        }
    }

    /// Replaces the credential store implementation for an explicit embedder.
    #[must_use]
    pub fn with_credential_store(mut self, store: Arc<dyn SecretStore>) -> Self {
        self.credential_store = store;
        self
    }

    /// Returns the frozen paths used by this candidate.
    pub const fn paths(&self) -> &HostPaths {
        &self.paths
    }

    /// Builds the generic Host without reading a Profile or activating plugins.
    pub fn build(self) -> rsi_host::Result<Host> {
        let mut builder = HostBuilder::new(self.paths.clone());
        register_contracts(&mut builder)?;
        register_factories(
            &mut builder,
            self.credential_store,
            self.captured_environment,
        )?;
        builder.register_fragment(base_fragment(&self.paths))?;
        let agent = HeadlessAgentConfig::new(self.paths.state().join("agent"))
            .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?;
        builder.register_fragment(headless_fragment(&agent))?;
        builder.build()
    }
}

fn register_factories(
    builder: &mut HostBuilder,
    credential_store: Arc<dyn SecretStore>,
    captured_environment: BTreeMap<String, SecretValue>,
) -> rsi_host::Result<()> {
    register(
        builder,
        STORAGE_FACTORY,
        UpdateMode::Replayable,
        rsi_storage::StorageFactory,
    )?;
    register(
        builder,
        STORAGE_SQLITE_FACTORY,
        UpdateMode::RestartRequired,
        rsi_storage_sqlite::SqliteStorageFactory,
    )?;
    register(
        builder,
        STORAGE_DOMAIN_FACTORY,
        UpdateMode::Replayable,
        rsi_storage_domain::DomainFactory,
    )?;
    register(
        builder,
        SETTINGS_LOCAL_FACTORY,
        UpdateMode::RestartRequired,
        rsi_settings_local::LocalSettingsFactory,
    )?;
    register(
        builder,
        SETTINGS_CORE_FACTORY,
        UpdateMode::Replayable,
        rsi_settings::SettingsFactory,
    )?;
    register(
        builder,
        SETTINGS_FACTORY,
        UpdateMode::Replayable,
        AgentSettingsFactory,
    )?;
    register(
        builder,
        CREDENTIALS_FACTORY,
        UpdateMode::RestartRequired,
        CredentialsLocalFactory::with_store(credential_store, captured_environment),
    )?;
    register_runtime_factories(builder)?;
    register_agent_ai_factories(builder)
}

fn register_runtime_factories(builder: &mut HostBuilder) -> rsi_host::Result<()> {
    register(
        builder,
        MEDIA_LOCAL_FACTORY,
        UpdateMode::RestartRequired,
        rsi_media_local::LocalMediaBackendFactory,
    )?;
    register(
        builder,
        MEDIA_FACTORY,
        UpdateMode::Replayable,
        rsi_media::MediaFactory,
    )?;
    register(
        builder,
        APPROVAL_FACTORY,
        UpdateMode::Replayable,
        rsi_approval::ApprovalFactory,
    )?;
    register(
        builder,
        DENY_APPROVAL_FACTORY,
        UpdateMode::Replayable,
        DenyApprovalFactory,
    )?;
    register(
        builder,
        PERMISSIONS_FACTORY,
        UpdateMode::Replayable,
        rsi_permission_presets::PermissionPresetsFactory,
    )?;
    register(
        builder,
        SANDBOX_FACTORY,
        UpdateMode::RestartRequired,
        rsi_sandbox_local::SandboxLocalFactory::default(),
    )?;
    register(
        builder,
        COMMANDS_FACTORY,
        UpdateMode::Replayable,
        rsi_commands::CommandsFactory,
    )?;
    register(
        builder,
        JOBS_FACTORY,
        UpdateMode::Replayable,
        rsi_jobs_local::JobsLocalFactory,
    )?;
    register(
        builder,
        JOBS_FINALIZER_FACTORY,
        UpdateMode::Replayable,
        HeadlessJobsFinalizerFactory,
    )?;
    register(
        builder,
        PROJECTION_FACTORY,
        UpdateMode::Replayable,
        rsi_projection::ProjectionFactory,
    )?;
    register(
        builder,
        WORKSPACE_FACTORY,
        UpdateMode::Replayable,
        rsi_workspace::WorkspaceFactory,
    )?;
    register(
        builder,
        TOOLS_FACTORY,
        UpdateMode::Replayable,
        rsi_tools::ToolsFactory,
    )?;
    Ok(())
}

fn register_agent_ai_factories(builder: &mut HostBuilder) -> rsi_host::Result<()> {
    register(
        builder,
        LANGUAGE_FACTORY,
        UpdateMode::Replayable,
        rsi_ai::LanguageRouterFactory,
    )?;
    register(
        builder,
        IMAGE_FACTORY,
        UpdateMode::Replayable,
        rsi_ai_image::ImageRouterFactory,
    )?;
    register(
        builder,
        SQLITE_STORE_FACTORY,
        UpdateMode::RestartRequired,
        rsi_agent_store_sqlite::SqliteStoreFactory,
    )?;
    register(
        builder,
        KERNEL_FACTORY,
        UpdateMode::Replayable,
        rsi_agent_kernel::KernelFactory,
    )?;
    register(
        builder,
        EXECUTOR_FACTORY,
        UpdateMode::Replayable,
        rsi_agent_executor::ExecutorFactory,
    )?;
    register(
        builder,
        OPENAI_FACTORY,
        UpdateMode::Replayable,
        rsi_ai_openai::OpenAiFactory::default(),
    )?;
    register(
        builder,
        OPENAI_COMPATIBLE_FACTORY,
        UpdateMode::Replayable,
        rsi_ai_openai_compatible::OpenAiCompatibleFactory::default(),
    )?;
    register(
        builder,
        DEEPSEEK_FACTORY,
        UpdateMode::Replayable,
        rsi_ai_deepseek::DeepSeekFactory::default(),
    )?;
    Ok(())
}

fn register(
    builder: &mut HostBuilder,
    id: &'static str,
    mode: UpdateMode,
    factory: impl PluginFactory,
) -> rsi_host::Result<()> {
    builder.register_linked(id, env!("CARGO_PKG_VERSION"), mode, Arc::new(factory))?;
    Ok(())
}

fn register_contracts(builder: &mut HostBuilder) -> rsi_host::Result<()> {
    builder.register_local_contract::<StorageHubContract>()?;
    builder.register_local_contract::<DomainFacilityContract>()?;
    builder.register_local_contract::<SettingsProviderContract>()?;
    builder.register_local_contract::<SettingsContract>()?;
    builder.register_local_contract::<AgentSettingsContract>()?;
    builder.register_local_contract::<CredentialsResolveContract>()?;
    builder.register_local_contract::<CredentialsAdminContract>()?;
    builder.register_local_contract::<MediaBackendContract>()?;
    builder.register_local_contract::<MediaContract>()?;
    builder.register_local_contract::<MediaReadContract>()?;
    builder.register_local_contract::<ApprovalContract>()?;
    builder.register_local_contract::<ApprovalAnswerersContract>()?;
    builder.register_local_contract::<PermissionPresetsContract>()?;
    builder.register_local_contract::<SandboxContract>()?;
    builder.register_local_contract::<CommandRuntimeContract>()?;
    builder.register_local_contract::<JobsContract>()?;
    builder.register_local_contract::<ProjectionRegistryContract>()?;
    builder.register_local_contract::<WorkspaceRegistryContract>()?;
    builder.register_local_contract::<ToolRuntimeContract>()?;
    builder.register_local_contract::<LanguageCallContract>()?;
    builder.register_local_contract::<ImageCallContract>()?;
    builder.register_local_contract::<LanguageRegistrarContract>()?;
    builder.register_local_contract::<ImageRegistrarContract>()?;
    builder.register_local_contract::<SessionStoreContract>()?;
    builder.register_local_contract::<TurnServiceContract>()?;
    builder.register_local_contract::<TurnExecutionContract>()?;
    builder.register_local_contract::<TurnFinalizationContract>()?;
    Ok(())
}

fn base_fragment(paths: &HostPaths) -> ProfileFragment {
    ProfileFragment::new(
        "rsi.headless.base",
        [
            ProfileEntry::new("rsi-storage", STORAGE_FACTORY, Value::Null),
            ProfileEntry::new(
                "rsi-storage-sqlite",
                STORAGE_SQLITE_FACTORY,
                json!({ "name": "base", "path": paths.state().join("base.sqlite3") }),
            ),
            ProfileEntry::new("rsi-storage-domain", STORAGE_DOMAIN_FACTORY, Value::Null),
            ProfileEntry::new(
                "rsi-settings-local",
                SETTINGS_LOCAL_FACTORY,
                json!({ "path": paths.config().join("settings.json") }),
            ),
            ProfileEntry::new("rsi-settings", SETTINGS_CORE_FACTORY, Value::Null),
            ProfileEntry::new("rsi-headless-settings", SETTINGS_FACTORY, Value::Null),
            ProfileEntry::new(
                "rsi-credentials",
                CREDENTIALS_FACTORY,
                json!({
                    "service": "rsiversi",
                    "environment": [
                        { "reference": { "owner": OPENAI_FACTORY, "slot": "default" }, "variable": "OPENAI_API_KEY" },
                        { "reference": { "owner": OPENAI_COMPATIBLE_FACTORY, "slot": "default" }, "variable": "RSI_OPENAI_COMPATIBLE_API_KEY" },
                        { "reference": { "owner": DEEPSEEK_FACTORY, "slot": "default" }, "variable": "DEEPSEEK_API_KEY" }
                    ]
                }),
            ),
            ProfileEntry::new(
                "rsi-media-local",
                MEDIA_LOCAL_FACTORY,
                json!({ "root": paths.state().join("media") }),
            ),
            ProfileEntry::new("rsi-media", MEDIA_FACTORY, Value::Null),
            ProfileEntry::new("rsi-approval", APPROVAL_FACTORY, Value::Null),
            ProfileEntry::new("rsi-headless-deny", DENY_APPROVAL_FACTORY, Value::Null),
            ProfileEntry::new(
                "rsi-permission-presets",
                PERMISSIONS_FACTORY,
                json!({
                    "read-only": { "sandbox": "read-only", "require_approval": false },
                    "workspace-write": { "sandbox": "workspace-write", "require_approval": false },
                    "danger-full-access": { "sandbox": "danger-full-access", "require_approval": true }
                }),
            ),
            ProfileEntry::new(
                "rsi-sandbox",
                SANDBOX_FACTORY,
                json!({
                    "bubblewrap": ["/usr/bin/bwrap"],
                    "landlock": []
                }),
            ),
            ProfileEntry::new("rsi-commands", COMMANDS_FACTORY, Value::Null),
            ProfileEntry::new("rsi-jobs", JOBS_FACTORY, Value::Null),
            ProfileEntry::new(
                "rsi-headless-jobs-finalizer",
                JOBS_FINALIZER_FACTORY,
                Value::Null,
            ),
            ProfileEntry::new("rsi-projection", PROJECTION_FACTORY, Value::Null),
            ProfileEntry::new(
                "rsi-workspace",
                WORKSPACE_FACTORY,
                json!({ "backend": "base" }),
            ),
            ProfileEntry::new("rsi-tools", TOOLS_FACTORY, Value::Null),
            ProfileEntry::new("rsi-ai-language", LANGUAGE_FACTORY, Value::Null),
            ProfileEntry::new("rsi-ai-image", IMAGE_FACTORY, Value::Null),
        ],
    )
}

/// Captures only the standard allowlisted credential environment variables.
pub fn capture_standard_environment() -> crate::Result<BTreeMap<String, SecretValue>> {
    let mut captured = BTreeMap::new();
    for name in [
        "OPENAI_API_KEY",
        "RSI_OPENAI_COMPATIBLE_API_KEY",
        "DEEPSEEK_API_KEY",
    ] {
        if let Some(value) = std::env::var_os(name) {
            let value = value.into_string().map_err(|_| {
                crate::RsiError::Boot(format!(
                    "credential environment variable `{name}` is not UTF-8"
                ))
            })?;
            captured.insert(
                name.into(),
                SecretValue::new(value)
                    .map_err(|error| crate::RsiError::Boot(error.to_string()))?,
            );
        }
    }
    Ok(captured)
}

#[derive(Debug)]
struct DenyApprovalFactory;

#[derive(Debug)]
struct DenyAnswerer;

#[derive(Debug)]
struct HeadlessJobsFinalizerFactory;

#[derive(Debug)]
struct HeadlessJobsFinalizer {
    jobs: Arc<dyn Jobs>,
}

#[async_trait]
impl TurnFinalizer for HeadlessJobsFinalizer {
    async fn finalize(
        &self,
        session_id: &rsi_agent_session_protocol::SessionId,
        turn_id: &rsi_agent_session_protocol::TurnId,
    ) -> rsi_agent_turn_protocol::FinalizationResult<()> {
        let scope = agent_turn_job_scope(session_id, turn_id).map_err(|error| {
            TurnFinalizationError::Failed {
                code: "jobs.scope".into(),
                message: error.to_string(),
            }
        })?;
        self.jobs
            .cancel_scope(&scope)
            .await
            .map_err(|error| TurnFinalizationError::Failed {
                code: match error {
                    rsi_jobs::JobsError::CancellationTimeout => "jobs.cancellation_timeout",
                    _ => "jobs.finalization",
                }
                .into(),
                message: error.to_string(),
            })
    }
}

pub(crate) fn agent_turn_job_scope(
    session_id: &rsi_agent_session_protocol::SessionId,
    turn_id: &rsi_agent_session_protocol::TurnId,
) -> rsi_jobs::Result<JobScope> {
    JobScope::new("rsi.agent.turn", [session_id.as_str(), turn_id.as_str()])
}

#[async_trait]
impl PluginFactory for HeadlessJobsFinalizerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Headless Jobs finalizer configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null)
            .requiring_local::<JobsContract>()
            .requiring_local::<TurnFinalizationContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let finalizer = Arc::new(HeadlessJobsFinalizer {
            jobs: plan.local::<JobsContract>()?,
        });
        let lease = plan
            .local::<TurnFinalizationContract>()?
            .register("rsi.headless.jobs".into(), finalizer)
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw Headless Jobs finalizer",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}

#[async_trait]
impl ApprovalAnswerer for DenyAnswerer {
    async fn answer(
        &self,
        _request: ApprovalRequest,
        cancellation: CancellationToken,
    ) -> rsi_approval_protocol::Result<Option<ApprovalOutcome>> {
        if cancellation.is_cancelled() {
            return Err(rsi_approval_protocol::ApprovalError::Cancelled);
        }
        Ok(Some(ApprovalOutcome {
            decision: ApprovalDecision::Deny,
            answerer: "rsi.headless.deny".into(),
            reason: Some("Headless mode never reads stdin for approval".into()),
        }))
    }
}

#[async_trait]
impl PluginFactory for DenyApprovalFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(rsi_meta::MetaError::InvalidInput(
                "Headless deny answerer configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null).requiring_local::<ApprovalAnswerersContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<ApprovalAnswerersContract>()?
            .register(Arc::new(DenyAnswerer))
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw Headless deny answerer",
            Box::new(move || {
                Box::pin(async move {
                    drop(lease);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_unconfined_preset_requires_approval() {
        let paths = HostPaths::new("/config", "/state", "/cache").unwrap();
        let fragment = base_fragment(&paths);
        let permissions = fragment
            .entries()
            .iter()
            .find(|entry| entry.plugin().as_str() == PERMISSIONS_FACTORY)
            .expect("standard permission preset registration");
        assert_eq!(
            permissions.config()["danger-full-access"]["require_approval"],
            true
        );
    }
}
