mod http;
#[cfg(target_os = "linux")]
mod operator;
#[cfg(target_os = "linux")]
mod service;
use crate::{
    AgentPresetManager, HostProfileDocument, HostProfileId, ProfileCatalog, RsiError,
    StandardComposition,
};
use async_trait::async_trait;
use rsi_host::{Host, HostBuilder};
use rsi_meta::{
    ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, UpdateMode,
};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

const CONNECTION: &str = "rsi.application.connection";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    host_profile: HostProfileId,
}

#[derive(Debug)]
struct ConnectionFactory {
    composition: StandardComposition,
    diagnostic: Mutex<Option<RsiError>>,
}
impl ConnectionFactory {
    async fn compose(
        &self,
        plan: &mut ActivationPlan,
    ) -> rsi_meta::Result<(HostProfileDocument, StandardComposition)> {
        let host = plan.take_state::<HostProfileDocument>()?;
        let system_root = crate::standard_agent_preset_root(self.composition.paths())
            .map_err(|error| self.diagnosed(error))?;
        let presets =
            AgentPresetManager::open_standard_in(plan.context(), &self.composition, system_root)
                .await
                .map_err(|error| self.diagnosed(error))?;
        let composition = self
            .composition
            .clone()
            .with_agent_presets(&presets)
            .map_err(|error| self.diagnosed(error))?;
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
        let host = ProfileCatalog::new(self.composition.paths().clone())
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
        let exports = self
            .composition
            .addons()
            .publish_domains(&mut plan, crate::addon::DomainLookup::Local(&connection));
        let session = connection.session_service();
        let workspace = connection.workspace_registry();
        let models = connection.language_models();
        let output = connection.output_cache();
        let media = connection.media_service();
        let files = connection.session_files();
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
        exports.map_err(|error| self.diagnosed(error))?;
        let context = plan.context();
        let supplies = vec![
            context.provide_local::<rsi_session_protocol::SessionContract>(session)?,
            context.provide_local::<rsi_session_files::SessionFilesContract>(files)?,
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
    composition: StandardComposition,
    arguments: Vec<std::ffi::OsString>,
) -> crate::Result<(Host, ApplicationDiagnostics)> {
    let diagnostics = ApplicationDiagnostics {
        connection: Arc::new(ConnectionFactory {
            composition,
            diagnostic: Mutex::new(None),
        }),
        cli: Arc::new(rsi_terminal::CliFactory::new(arguments.clone())),
        headless: Arc::new(rsi_terminal::HeadlessFactory::new(arguments.clone())),
        tui: Arc::new(rsi_terminal::TuiFactory::new(arguments.clone())),
        devices: Arc::new(rsi_terminal::DevicesFactory::new(arguments.clone())),
        web_serve: Arc::new(rsi_serve::ServeFactory::with_web_assets(arguments.clone())),
        serve: Arc::new(rsi_serve::ServeFactory::new(arguments)),
    };
    let mut builder = crate::StandardAddonBuilder::new("rsi.standard.application");
    diagnostics
        .connection
        .composition
        .addons()
        .validate_platform(&format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
        .map_err(boot)?;
    register_contracts(&mut builder)?;
    builder
        .register_factory(
            crate::AddonScope::Application,
            "rsi.credentials.local",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(diagnostics.connection.composition.credentials_factory()),
        )
        .map_err(boot)?;
    builder
        .register_factory(
            crate::AddonScope::Application,
            "rsi.application.http",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(http::HttpFactory(diagnostics.connection.clone())),
        )
        .map_err(boot)?;
    #[cfg(target_os = "linux")]
    builder
        .register_factory(
            crate::AddonScope::Application,
            "rsi.application.service",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(service::ServiceFactory(diagnostics.connection.clone())),
        )
        .map_err(boot)?;
    #[cfg(target_os = "linux")]
    builder
        .register_factory(
            crate::AddonScope::Application,
            "rsi.application.operator",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(operator::OperatorFactory(diagnostics.connection.clone())),
        )
        .map_err(boot)?;
    for (id, factory) in application_factories(&diagnostics) {
        builder
            .register_factory(
                crate::AddonScope::Application,
                id,
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                factory,
            )
            .map_err(boot)?;
    }
    let addons = diagnostics
        .connection
        .composition
        .addons()
        .merged(builder.build().map_err(boot)?)
        .map_err(boot)?;
    let mut host = HostBuilder::new(diagnostics.connection.composition.paths().clone());
    addons
        .register_into(&mut host, crate::AddonScope::Application)
        .map_err(boot)?;
    Ok((host.build().map_err(boot)?, diagnostics))
}

fn boot(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}

fn register_contracts(builder: &mut crate::StandardAddonBuilder) -> crate::Result<()> {
    let scope = crate::AddonScope::Application;
    builder
        .register_local_contract_at::<rsi_ui::UiContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_ui::UiTargetContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_api_http::HttpAssetsContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_credentials_protocol::CredentialsResolveContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_credentials_protocol::CredentialsAdminContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_settings_protocol::SettingsAccessContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_media_protocol::MediaReadContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_api_protocol::ApiClientContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_application::ApplicationRunContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_client::ConnectionLifetimeContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_session_files::SessionFilesContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_session_protocol::SessionContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_workspace_protocol::WorkspaceRegistryContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_ai_protocol::LanguageModelsContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_process::ProcessOutputCacheContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_media_protocol::MediaContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_api_protocol::DeviceAdministrationContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_api_http::HttpListenerContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_api_protocol::ApiDispatchContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_api_protocol::DeviceAuthenticationContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_api_protocol::ConnectionDescriptionContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_serve::ServingServiceContract>(scope)
        .map_err(boot)?;
    Ok(())
}

fn application_factories(
    diagnostics: &ApplicationDiagnostics,
) -> [(&'static str, Arc<dyn PluginFactory>); 12] {
    [
        ("rsi.ui", Arc::new(rsi_ui::UiFactory)),
        ("rsi.ui.target", Arc::new(rsi_ui::UiTargetFactory)),
        ("rsi.session.ui", Arc::new(rsi_session_ui::SessionUiFactory)),
        (
            "rsi.session.files.ui",
            Arc::new(rsi_session_files_ui::FilesUiFactory),
        ),
        ("rsi.application.serve-web", diagnostics.web_serve.clone()),
        ("rsi.web.assets", Arc::new(rsi_web_assets::WebAssetsFactory)),
        ("rsi.application.devices", diagnostics.devices.clone()),
        (CONNECTION, diagnostics.connection.clone()),
        ("rsi.application.cli", diagnostics.cli.clone()),
        ("rsi.application.headless", diagnostics.headless.clone()),
        ("rsi.application.tui", diagnostics.tui.clone()),
        ("rsi.application.serve", diagnostics.serve.clone()),
    ]
}
