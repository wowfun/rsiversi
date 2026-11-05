use crate::{Connection, Handler};
use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_process::{DuplexProcessContract, DuplexProcessSpec};
use rsi_sandbox::{ProcessRequest, ProcessStdio, SandboxContract, SandboxMode};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Fully explicit trusted process launch. Secrets must not be stored in Profiles.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    /// Installed executable; command-shell shims are not accepted.
    pub program: PathBuf,
    /// Literal arguments with no shell parsing.
    pub arguments: Vec<String>,
    /// Absolute working directory.
    pub cwd: PathBuf,
    /// Complete environment, never merged with ambient values.
    pub environment: BTreeMap<String, String>,
    /// Exact immutable generation identity.
    pub generation: String,
}
impl Launch {
    /// Checks the executable, working directory, environment and generation boundary.
    pub fn validate(&self) -> rsi_meta::Result<()> {
        if !self.program.is_absolute()
            || !self.cwd.is_absolute()
            || self.program.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat")
            })
        {
            return Err(MetaError::InvalidInput(
                "extension requires absolute executable and cwd".into(),
            ));
        }
        crate::Frame {
            generation: self.generation.clone(),
            message: crate::Message::Cancel { id: 1 },
        }
        .validate()
        .map_err(meta)?;
        rsi_process::validate_environment(
            &self
                .environment
                .iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect::<Vec<_>>(),
        )
        .map_err(meta)
    }
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
/// Exact process-generation connection published by an ordinary plugin.
#[derive(Debug)]
pub struct ExtensionContract;
impl LocalContract for ExtensionContract {
    const KEY: &'static str = "rsi.extension.connection";
    type Service = Connection;
}
/// Factory with explicitly supplied inbound authority.
#[derive(Debug)]
pub struct ExtensionFactory {
    handler: Arc<dyn Handler>,
    resources: Option<Arc<dyn crate::LaunchResources>>,
}
impl ExtensionFactory {
    /// Binds a family adapter; it is not acquired from untrusted request data.
    pub fn new(handler: Arc<dyn Handler>) -> Self {
        Self {
            handler,
            resources: None,
        }
    }
    /// Binds immutable launch resources supplied by the owning product.
    #[must_use]
    pub fn retaining(mut self, resources: Arc<dyn crate::LaunchResources>) -> Self {
        self.resources = Some(resources);
        self
    }
}
#[async_trait]
impl PluginFactory for ExtensionFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let launch: Launch = serde_json::from_value(config.clone()).map_err(meta)?;
        launch.validate()?;
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<DuplexProcessContract>()
            .requiring_local::<SandboxContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let launch: Launch =
            serde_json::from_value(plan.config().as_ref().clone()).map_err(meta)?;
        self.activate_launch(plan, launch).await
    }
}
impl ExtensionFactory {
    /// Starts a product-resolved launch through the same admission and cleanup path.
    pub async fn activate_launch(
        &self,
        plan: ActivationPlan,
        launch: Launch,
    ) -> rsi_meta::Result<()> {
        launch.validate()?;
        let confined = plan
            .local::<SandboxContract>()?
            .confine(ProcessRequest {
                stdio: ProcessStdio::Pipes,
                mode: SandboxMode::DangerFullAccess,
                program: launch.program,
                arguments: launch.arguments,
                cwd: launch.cwd.clone(),
                workspace: launch.cwd,
            })
            .await
            .map_err(meta)?;
        let process = plan
            .local::<DuplexProcessContract>()?
            .spawn(DuplexProcessSpec {
                process: confined,
                environment: launch
                    .environment
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
                stdout_buffer_bytes: 64 * 1024,
                stderr_max_bytes: 64 * 1024,
                termination_grace_ms: 1000,
            })
            .await
            .map_err(meta)?;
        let connection = Arc::new(
            Connection::new(process, launch.generation, self.handler.clone()).map_err(meta)?,
        );
        if let Some(resources) = &self.resources {
            connection.retain_until_settled(resources.clone());
        }
        let cleanup = connection.clone();
        plan.defer(
            "settle extension process",
            Box::new(move || {
                Box::pin(async move { cleanup.shutdown().await.map_err(|error| error.to_string()) })
            }),
        )?;
        let hello = tokio::time::timeout(
            Duration::from_secs(10),
            connection.request(
                "extension.hello",
                serde_json::json!({"protocol":1}),
                CancellationToken::new(),
            ),
        )
        .await
        .map_err(meta)?
        .map_err(meta)?;
        if hello != serde_json::json!({"protocol":1}) {
            return Err(meta("extension handshake mismatch"));
        }
        connection.start_watchdog();
        plan.context()
            .provide_local::<ExtensionContract>(connection)?;
        Ok(())
    }
}
