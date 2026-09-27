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
        self.composition
            .preflight_service(crate::service_program(&host), None)
            .map_err(|error| self.diagnosed(error))?;
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
        let host = ProfileCatalog::new(
            self.composition.paths().clone(),
            self.composition.application_metadata().clone(),
        )
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
        let api = connection.api_client();
        let external =
            Arc::new(rsi_acp_api::Client::new(api.clone()).map_err(|error| self.diagnosed(error))?);
        let workspace = connection.workspace_registry();
        let models = connection.language_models();
        let output = connection.output_cache();
        let media = connection.media_service();
        let files = connection.session_files();
        let settings = connection.settings_access();
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
            context.provide_local::<rsi_api_protocol::ApiClientContract>(api)?,
            context.provide_local::<rsi_acp_protocol::service::ExternalConversationsContract>(
                external,
            )?,
            context.provide_local::<rsi_session_protocol::SessionContract>(session)?,
            context.provide_local::<rsi_session_files::SessionFilesContract>(files)?,
            context.provide_local::<rsi_settings_protocol::SettingsAccessContract>(settings)?,
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

/// Ordered, consuming application bootstrap diagnostics.
#[derive(Debug)]
pub struct ApplicationDiagnostics(Vec<Arc<dyn rsi_application::ApplicationDiagnostic>>);
impl ApplicationDiagnostics {
    /// Returns the first actionable owner diagnostic.
    pub fn take(&self) -> Option<RsiError> {
        self.0.iter().find_map(|owner| owner.take_diagnostic())
    }
}
impl rsi_application::ApplicationDiagnostic for ConnectionFactory {
    fn take_diagnostic(&self) -> Option<RsiError> {
        self.diagnostic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// Freezes the explicitly supplied application catalog without activating a backend.
pub fn standard_application_host(
    composition: crate::ApplicationComposition,
    arguments: Vec<std::ffi::OsString>,
) -> crate::Result<(Host, ApplicationDiagnostics)> {
    let crate::ApplicationComposition {
        service,
        catalog,
        extras,
    } = composition;
    let application = catalog.build(&service, arguments)?;
    let actual: std::collections::BTreeSet<_> = application
        .addons
        .descriptions()
        .map(|entry| entry.plugin.clone())
        .collect();
    if &actual != catalog.metadata().plugins() {
        return Err(boot(
            "application catalog factories differ from reserved metadata",
        ));
    }
    application
        .addons
        .validate_application_only()
        .map_err(boot)?;
    let addons = service
        .addons()
        .merged_set(&application.addons)
        .map_err(boot)?
        .merged_set(&extras)
        .map_err(boot)?;
    addons
        .validate_platform(&format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
        .map_err(boot)?;
    let mut host = HostBuilder::new(service.paths().clone());
    addons
        .register_into(&mut host, crate::AddonScope::Application)
        .map_err(boot)?;
    Ok((
        host.build().map_err(boot)?,
        ApplicationDiagnostics(application.diagnostics),
    ))
}

/// IDs of core-owned application connection adapters; no factory is instantiated.
pub const BASE_APPLICATION_PLUGINS: &[&str] = &[
    CONNECTION,
    "rsi.directory-picker.client",
    "rsi.credentials.local",
    "rsi.application.http",
    #[cfg(target_os = "linux")]
    "rsi.application.service",
    #[cfg(target_os = "linux")]
    "rsi.application.acp-service",
    #[cfg(target_os = "linux")]
    "rsi.application.operator",
];

/// Encapsulated connection and Service adapters consumed by an application catalog.
pub fn base_application_catalog(
    composition: StandardComposition,
) -> crate::Result<crate::ApplicationCatalog> {
    let connection = Arc::new(ConnectionFactory {
        composition,
        diagnostic: Mutex::new(None),
    });
    let mut builder = crate::StandardAddonBuilder::new("rsi.application.services");
    register_contracts(&mut builder)?;
    for (id, factory) in [
        (CONNECTION, connection.clone() as Arc<dyn PluginFactory>),
        (
            "rsi.directory-picker.client",
            Arc::new(rsi_directory_picker_api::ClientFactory),
        ),
        (
            "rsi.credentials.local",
            Arc::new(connection.composition.credentials_factory()),
        ),
        (
            "rsi.application.http",
            Arc::new(http::HttpFactory(connection.clone())),
        ),
        #[cfg(target_os = "linux")]
        (
            "rsi.application.service",
            Arc::new(service::ServiceFactory(connection.clone(), false)),
        ),
        #[cfg(target_os = "linux")]
        (
            "rsi.application.acp-service",
            Arc::new(service::ServiceFactory(connection.clone(), true)),
        ),
        #[cfg(target_os = "linux")]
        (
            "rsi.application.operator",
            Arc::new(operator::OperatorFactory(connection.clone())),
        ),
    ] {
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
    Ok(crate::ApplicationCatalog {
        addons: crate::StandardAddonSet::new([builder.build().map_err(boot)?]).map_err(boot)?,
        diagnostics: vec![connection],
    })
}
fn boot(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}

fn register_contracts(builder: &mut crate::StandardAddonBuilder) -> crate::Result<()> {
    let scope = crate::AddonScope::Application;
    builder
        .register_local_contract_at::<rsi_directory_picker_api::ClientContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<rsi_acp_agent::AgentBackendContract>(scope)
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
        .register_local_contract_at::<rsi_acp_protocol::service::ExternalConversationsContract>(
            scope,
        )
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
        .register_local_contract_at::<crate::application_services::LocalBrowserAdministrationContract>(scope)
        .map_err(boot)?;
    builder
        .register_local_contract_at::<crate::application_services::ServingServiceContract>(scope)
        .map_err(boot)?;
    Ok(())
}
