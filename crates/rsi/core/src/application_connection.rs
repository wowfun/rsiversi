mod http;
#[cfg(target_os = "linux")]
mod operator;
#[cfg(target_os = "linux")]
mod service;
use crate::{
    AgentPresetManager, HostProfileDocument, HostProfileId, ProfileCatalog, RsiError,
    StandardCodingTools, StandardComposition,
};
use async_trait::async_trait;
use rsi_credentials_protocol::SecretValue;
use rsi_host::{Host, HostBuilder, HostPaths};
use rsi_meta::{
    ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, UpdateMode,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

const CONNECTION: &str = "rsi.application.connection";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    host_profile: HostProfileId,
}

#[derive(Debug)]
struct ConnectionFactory {
    paths: HostPaths,
    environment: BTreeMap<String, SecretValue>,
    coding_tools: Option<StandardCodingTools>,
    diagnostic: Mutex<Option<RsiError>>,
}
impl ConnectionFactory {
    async fn compose(
        &self,
        plan: &mut ActivationPlan,
    ) -> rsi_meta::Result<(HostProfileDocument, StandardComposition)> {
        let host = plan.take_state::<HostProfileDocument>()?;
        let system_root = crate::standard_agent_preset_root(&self.paths)
            .map_err(|error| self.diagnosed(error))?;
        let presets = AgentPresetManager::open_standard_in(
            plan.context(),
            self.paths.clone(),
            system_root,
            self.coding_tools.is_some(),
        )
        .await
        .map_err(|error| self.diagnosed(error))?;
        let composition = StandardComposition::new(
            self.paths.clone(),
            self.environment.clone(),
            self.coding_tools.clone(),
        )
        .with_agent_presets(presets.catalog().clone());
        plan.defer(
            "close application preset Profile",
            Box::new(move || {
                Box::pin(async move {
                    if presets.shutdown().await.is_clean() {
                        Ok(())
                    } else {
                        Err("application preset cleanup failed".into())
                    }
                })
            }),
        )?;
        Ok((host, composition))
    }

    fn diagnosed(&self, error: impl std::fmt::Display) -> MetaError {
        let mut message = error.to_string();
        if message.len() > 4096 {
            let mut end = 4096;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        *self
            .diagnostic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(RsiError::Boot(message.clone()));
        MetaError::Activation(message)
    }
}
#[async_trait]
impl PluginFactory for ConnectionFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Configuration =
            serde_json::from_value(config.clone()).map_err(|error| self.diagnosed(error))?;
        let host = ProfileCatalog::new(self.paths.clone())
            .host(&config.host_profile)
            .map_err(|error| self.diagnosed(error))?;
        let retained = host.contents.len() + 4096;
        Ok(PreparedActivation::with_state(
            ConfigValue::Null,
            host,
            retained,
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (host, composition) = self.compose(&mut plan).await?;
        let connection = crate::connect_or_embed_service_host(plan.context(), composition, &host)
            .await
            .map_err(|error| self.diagnosed(error))?;
        let session = connection.session_service();
        let workspace = connection.workspace_registry();
        let models = connection.language_models();
        let output = connection.output_cache();
        let media = connection.media_service();
        let lifetime = match connection.mode() {
            crate::ServiceHostConnectionMode::Embedded => rsi_client::ConnectionLifetime::Embedded,
            crate::ServiceHostConnectionMode::Remote => rsi_client::ConnectionLifetime::Remote,
        };
        plan.defer(
            "close application service connection",
            Box::new(move || {
                Box::pin(async move {
                    connection
                        .shutdown()
                        .await
                        .map_err(|error| error.to_string())
                })
            }),
        )?;
        let context = plan.context();
        let supplies = vec![
            context.provide_local::<rsi_session_protocol::SessionContract>(session)?,
            context
                .provide_local::<rsi_workspace_protocol::WorkspaceRegistryContract>(workspace)?,
            context.provide_local::<rsi_ai_protocol::LanguageModelsContract>(models)?,
            context.provide_local::<rsi_process::ProcessOutputCacheContract>(output)?,
            context.provide_local::<rsi_media_protocol::MediaContract>(media)?,
            context.provide_local::<rsi_client::ConnectionLifetimeContract>(Arc::new(lifetime))?,
        ];
        plan.defer(
            "withdraw application connection capabilities",
            Box::new(move || {
                Box::pin(async move {
                    drop(supplies);
                    Ok(())
                })
            }),
        )
    }
}

/// Bounded diagnostics owned by the native application factories.
#[derive(Debug)]
pub struct ApplicationDiagnostics {
    connection: Arc<ConnectionFactory>,
    cli: Arc<rsi_terminal::CliFactory>,
    headless: Arc<rsi_terminal::HeadlessFactory>,
    tui: Arc<rsi_terminal::TuiFactory>,
    serve: Arc<rsi_serve::ServeFactory>,
    web_serve: Arc<rsi_serve::ServeFactory>,
    devices: Arc<rsi_terminal::DevicesFactory>,
}
impl ApplicationDiagnostics {
    /// Takes an actionable owner diagnostic after generic Profile bootstrap fails.
    pub fn take(&self) -> Option<RsiError> {
        self.cli
            .take_diagnostic()
            .or_else(|| self.headless.take_diagnostic())
            .or_else(|| self.tui.take_diagnostic())
            .or_else(|| self.serve.take_diagnostic())
            .or_else(|| self.web_serve.take_diagnostic())
            .or_else(|| self.devices.take_diagnostic())
            .or_else(|| {
                self.connection
                    .diagnostic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
            })
    }
}

/// Freezes the ordinary native application catalog without activating any backend.
pub fn standard_application_host(
    paths: HostPaths,
    arguments: Vec<std::ffi::OsString>,
    environment: BTreeMap<String, SecretValue>,
    coding_tools: Option<StandardCodingTools>,
) -> crate::Result<(Host, ApplicationDiagnostics)> {
    let diagnostics = ApplicationDiagnostics {
        connection: Arc::new(ConnectionFactory {
            paths: paths.clone(),
            environment,
            coding_tools,
            diagnostic: Mutex::new(None),
        }),
        cli: Arc::new(rsi_terminal::CliFactory::new(arguments.clone())),
        headless: Arc::new(rsi_terminal::HeadlessFactory::new(arguments.clone())),
        tui: Arc::new(rsi_terminal::TuiFactory::new(arguments.clone())),
        devices: Arc::new(rsi_terminal::DevicesFactory::new(arguments.clone())),
        web_serve: Arc::new(rsi_serve::ServeFactory::with_web_assets(arguments.clone())),
        serve: Arc::new(rsi_serve::ServeFactory::new(arguments)),
    };
    let mut builder = HostBuilder::new(paths);
    register_contracts(&mut builder)?;
    builder
        .register_linked(
            "rsi.credentials.local",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(rsi_credentials_local::CredentialsLocalFactory::with_store(
                Arc::new(rsi_credentials_local::KeyringSecretStore),
                diagnostics.connection.environment.clone(),
            )),
        )
        .map_err(boot)?;
    builder
        .register_linked(
            "rsi.application.http",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(http::HttpFactory(diagnostics.connection.clone())),
        )
        .map_err(boot)?;
    #[cfg(target_os = "linux")]
    builder
        .register_linked(
            "rsi.application.service",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(service::ServiceFactory(diagnostics.connection.clone())),
        )
        .map_err(boot)?;
    #[cfg(target_os = "linux")]
    builder
        .register_linked(
            "rsi.application.operator",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(operator::OperatorFactory(diagnostics.connection.clone())),
        )
        .map_err(boot)?;
    let factories: [(&str, Arc<dyn PluginFactory>); 8] = [
        ("rsi.application.serve-web", diagnostics.web_serve.clone()),
        ("rsi.web.assets", Arc::new(rsi_web_assets::WebAssetsFactory)),
        ("rsi.application.devices", diagnostics.devices.clone()),
        (CONNECTION, diagnostics.connection.clone()),
        ("rsi.application.cli", diagnostics.cli.clone()),
        ("rsi.application.headless", diagnostics.headless.clone()),
        ("rsi.application.tui", diagnostics.tui.clone()),
        ("rsi.application.serve", diagnostics.serve.clone()),
    ];
    for (id, factory) in factories {
        builder
            .register_linked(
                id,
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                factory,
            )
            .map_err(boot)?;
    }
    Ok((builder.build().map_err(boot)?, diagnostics))
}
fn boot(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}

fn register_contracts(builder: &mut HostBuilder) -> crate::Result<()> {
    builder
        .register_local_contract::<rsi_api_http::HttpAssetsContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_credentials_protocol::CredentialsResolveContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_credentials_protocol::CredentialsAdminContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_settings_protocol::SettingsAccessContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_media_protocol::MediaReadContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_api_protocol::ApiClientContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_application::ApplicationRunContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_client::ConnectionLifetimeContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_session_protocol::SessionContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_workspace_protocol::WorkspaceRegistryContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_ai_protocol::LanguageModelsContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_process::ProcessOutputCacheContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_media_protocol::MediaContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_api_protocol::DeviceAdministrationContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_api_http::HttpListenerContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_api_protocol::ApiDispatchContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_api_protocol::DeviceAuthenticationContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_api_protocol::ConnectionDescriptionContract>()
        .map_err(boot)?;
    builder
        .register_local_contract::<rsi_serve::ServingServiceContract>()
        .map_err(boot)?;
    Ok(())
}
