use crate::{McpContract, McpError, McpService, ServerStatus};
use async_trait::async_trait;
use rsi_agent_composition_protocol::AgentGenerationSeed;
use rsi_credentials_protocol::CredentialsResolveContract;
use rsi_mcp_protocol::{McpConfig, ServerConfig, TransportConfig};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_process::DuplexProcessContract;
use rsi_sandbox::SandboxContract;
use rsi_settings_protocol::{
    SettingsApply, SettingsContract, SettingsError, SettingsMetadata, SettingsScope, SettingsSpec,
    ValidateWith,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
/// Owner-local HTTP Settings namespace; stdio remains Local Profile configuration.
pub const SETTINGS_NAMESPACE: &str = "rsi.mcp";
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
fn http(value: &Value) -> std::result::Result<McpConfig, String> {
    let config: McpConfig = serde_json::from_value(value.clone())
        .map_err(|_| "Invalid HTTP MCP settings".to_owned())?;
    config.validate()?;
    if config
        .servers
        .iter()
        .any(|server| !matches!(server.transport, TransportConfig::StreamableHttp { .. }))
    {
        return Err(
            "MCP Settings admit HTTP endpoints only; stdio requires Local Profile configuration"
                .into(),
        );
    }
    Ok(config)
}
/// Settings-aware owner. Current input capture is synchronous and performs no discovery.
#[derive(Debug)]
pub struct McpOwner {
    service: Arc<McpService>,
    settings: Arc<dyn SettingsScope>,
    stdio: Vec<ServerConfig>,
    applied: Mutex<Option<Value>>,
    refresh: Semaphore,
    stop: CancellationToken,
    tasks: TaskTracker,
}
impl McpOwner {
    /// Whether saved HTTP settings have been applied to actual endpoint connections.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the owner state lock.
    pub fn settings_pending(&self) -> bool {
        self.settings.get().map_or(true, |snapshot| {
            self.applied.lock().expect("MCP settings poisoned").as_ref() != Some(&snapshot.value)
        })
    }
    /// Reads actual bounded endpoint observations without endpoint addresses or launch paths.
    pub fn status(&self) -> Vec<ServerStatus> {
        self.service.status()
    }
    /// Captures bounded overall readiness separately from each endpoint's last observation.
    pub fn observation(&self) -> rsi_mcp_protocol::McpStatus {
        let settings_pending = self.settings_pending();
        let ready = if settings_pending {
            Err(McpError::CatalogChanged)
        } else {
            self.service.readiness()
        };
        rsi_mcp_protocol::McpStatus {
            settings_pending,
            fresh_ready: ready.is_ok(),
            fresh_error: ready.err(),
            servers: self.status(),
        }
    }
    /// Captures the complete verified Domain input for fresh composition.
    pub fn seed(&self) -> std::result::Result<AgentGenerationSeed, McpError> {
        if self.settings_pending() {
            return Err(McpError::CatalogChanged);
        }
        self.service.generation_seed()
    }
    /// Reads saved HTTP endpoint configuration for an explicitly granted configuration flow.
    pub fn http_config(&self) -> std::result::Result<McpConfig, McpError> {
        http(&self.settings.get().map_err(|_| McpError::Protocol)?.value)
            .map_err(|_| McpError::Protocol)
    }
    /// Whether this exact configured endpoint is Local stdio.
    pub fn is_stdio(&self, id: &str) -> bool {
        self.stdio.iter().any(|server| server.id == id)
    }
    /// Applies saved HTTP settings and explicitly verifies selected endpoints.
    /// `None` refreshes HTTP endpoints; explicit stdio refresh requires a Local caller.
    pub async fn refresh(
        &self,
        id: Option<&str>,
        cancellation: CancellationToken,
    ) -> std::result::Result<(), McpError> {
        self.refresh_selected(id, false, cancellation).await
    }
    async fn refresh_selected(
        &self,
        id: Option<&str>,
        all_stdio: bool,
        cancellation: CancellationToken,
    ) -> std::result::Result<(), McpError> {
        let _permit = self.refresh.try_acquire().map_err(|_| McpError::Busy)?;
        let snapshot = self.settings.get().map_err(|_| McpError::Protocol)?;
        let mut config = http(&snapshot.value).map_err(|_| McpError::Protocol)?;
        config.servers.extend(self.stdio.clone());
        config.servers.sort_by(|one, two| one.id.cmp(&two.id));
        config.validate().map_err(|_| McpError::Protocol)?;
        if id.is_some_and(|id| {
            !config
                .servers
                .iter()
                .any(|server| server.id == id && server.enabled)
        }) {
            return Err(McpError::NotFound);
        }
        let targets = config
            .servers
            .iter()
            .filter(|server| {
                server.enabled
                    && id.map_or(
                        all_stdio
                            || matches!(server.transport, TransportConfig::StreamableHttp { .. }),
                        |id| server.id == id,
                    )
            })
            .map(|server| server.id.clone())
            .collect::<Vec<_>>();
        self.service.configure(config).await?;
        *self.applied.lock().expect("MCP settings poisoned") = Some(snapshot.value);
        let mut failure = None;
        // Per-endpoint service work owns cancellation and retirement; all eight may progress independently.
        let results = futures_util::future::join_all(
            targets
                .iter()
                .map(|id| self.service.refresh(id, cancellation.child_token())),
        )
        .await;
        for result in results {
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    /// Retires startup refresh and all protocol/process ownership.
    pub async fn shutdown(&self) -> crate::error::Result<()> {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
        self.service.shutdown().await
    }
}
/// Local owner capability for product status, configuration and composition wiring.
#[derive(Debug)]
pub struct McpOwnerContract;
impl LocalContract for McpOwnerContract {
    const KEY: &'static str = "rsi.mcp.owner";
    type Service = McpOwner;
}
/// Ordinary disabled-by-default Host integration, with Local-only stdio configuration.
#[derive(Debug, Default)]
pub struct McpFactory;
#[async_trait]
impl PluginFactory for McpFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config = if config.is_null() {
            json!({"servers":[]})
        } else {
            config.clone()
        };
        let stdio: McpConfig = serde_json::from_value(config.clone())
            .map_err(|_| meta("Invalid Local MCP configuration"))?;
        stdio.validate().map_err(meta)?;
        if stdio
            .servers
            .iter()
            .any(|server| !matches!(server.transport, TransportConfig::Stdio { .. }))
        {
            return Err(meta(
                "Local MCP plugin configuration owns stdio entries; use rsi.mcp Settings for HTTP",
            ));
        }
        Ok(PreparedActivation::new(config)
            .requiring_local::<SettingsContract>()
            .requiring_local::<CredentialsResolveContract>()
            .requiring_local::<DuplexProcessContract>()
            .requiring_local::<SandboxContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let stdio: McpConfig =
            serde_json::from_value(plan.config().as_ref().clone()).map_err(meta)?;
        let registration = plan.local::<SettingsContract>()?.register(SettingsSpec {
            namespace: SETTINGS_NAMESPACE.into(), defaults: json!({"servers":[]}), base: json!({}),
            metadata: SettingsMetadata { applies: SettingsApply::Live, description: "Save HTTP endpoints, then refresh their connections in Plugins. New Sessions capture verified catalogs; existing Sessions retain their original schemas. Configure stdio only in the Local Host Profile. Store secrets through Credentials, never in URL or settings text.".into(), schema: json!({"type":"object","properties":{"servers":{"type":"array","maxItems":8,"items":{"type":"object","required":["id","transport"],"properties":{"id":{"type":"string","maxLength":64},"enabled":{"type":"boolean","default":false},"tools":{"type":"array","maxItems":64,"items":{"type":"string"}},"transport":{"type":"object","required":["kind","url"],"properties":{"kind":{"const":"streamable_http"},"url":{"type":"string"},"credential":{"type":"object","required":["owner","slot"],"properties":{"owner":{"const":"rsi.mcp"},"slot":{"type":"string"}},"additionalProperties":false}},"additionalProperties":false}},"additionalProperties":false}}},"additionalProperties":false}), sensitive_fields: vec![] },
            validator: Arc::new(ValidateWith(|value: &Value| http(value).map(|_| ()).map_err(SettingsError::InvalidInput))),
        }).map_err(meta)?;
        let service = Arc::new(McpService::new(
            plan.local::<CredentialsResolveContract>()?,
            plan.local::<DuplexProcessContract>()?,
            plan.local::<SandboxContract>()?,
        ));
        let owner = Arc::new(McpOwner {
            service: service.clone(),
            settings: registration.scope.clone(),
            stdio: stdio.servers,
            applied: Mutex::new(None),
            refresh: Semaphore::new(1),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
        });
        let cleanup = owner.clone();
        plan.defer(
            "retire MCP owner and settings",
            Box::new(move || {
                Box::pin(async move {
                    let result = cleanup.shutdown().await;
                    drop(registration);
                    result.map_err(|error| error.to_string())
                })
            }),
        )?;
        // Publish configured disabled state synchronously; enabled targets remain unavailable until verified.
        let initial = owner.settings.get().map_err(meta)?;
        let mut config = http(&initial.value).map_err(meta)?;
        config.servers.extend(owner.stdio.clone());
        config.servers.sort_by(|a, b| a.id.cmp(&b.id));
        service.configure(config).await.map_err(meta)?;
        *owner.applied.lock().expect("MCP settings poisoned") = Some(initial.value);
        plan.context().provide_local::<McpContract>(service)?;
        plan.context()
            .provide_local::<McpOwnerContract>(owner.clone())?;
        if owner.status().iter().any(|server| server.enabled) {
            let active = owner.clone();
            owner.tasks.spawn(async move {
                let _ = active
                    .refresh_selected(None, true, active.stop.child_token())
                    .await;
            });
        }
        Ok(())
    }
}
