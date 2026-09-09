use super::ConnectionFactory;
use async_trait::async_trait;
use rsi_api_http_client::{HttpClientConfig, HttpClientFactory};
use rsi_host::{Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation, UpdateMode,
};
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct HttpFactory(pub(super) Arc<ConnectionFactory>);
#[async_trait]
impl PluginFactory for HttpFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        HttpClientFactory
            .prepare(config)
            .map_err(|error| self.0.diagnosed(error))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.activate_inner(plan)
            .await
            .map_err(|error| self.0.diagnosed(error))
    }
}
impl HttpFactory {
    async fn activate_inner(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<HttpClientConfig>()?;
        let (mut builder, mut entries) =
            rsi_client_composition::domain_clients("native").map_err(activation)?;
        builder
            .register_linked(
                "rsi.connection.http",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(HttpClientFactory),
            )
            .map_err(activation)?;
        entries.insert(
            0,
            ProfileEntry::new(
                "connection",
                "rsi.connection.http",
                serde_json::to_value(config).map_err(activation)?,
            ),
        );
        builder
            .register_fragment(rsi_host::ProfileFragment::new(
                "rsi.standard.clients",
                entries,
            ))
            .map_err(activation)?;
        self.0
            .composition
            .addons()
            .register_into(&mut builder, crate::AddonScope::Client)
            .map_err(activation)?;
        // Credentials is inherited explicitly; each domain and API marker is isolated by the child Host catalog.
        let connection = Arc::new(
            crate::ProfileOwner::start_scoped(
                builder.build().map_err(activation)?,
                self.0.composition.paths().clone(),
                plan.context(),
                ProfileProgram::from_profile(Profile::default()),
            )
            .await
            .map_err(activation)?,
        );
        let cleanup = connection.clone();
        plan.defer(
            "close remote application connection",
            Box::new(move || {
                Box::pin(async move {
                    if cleanup.shutdown().await.is_clean() {
                        Ok(())
                    } else {
                        Err("remote application connection cleanup failed".into())
                    }
                })
            }),
        )?;
        self.0
            .composition
            .addons()
            .publish_domains(&mut plan, crate::addon::DomainLookup::Remote(&connection))?;
        macro_rules! facet {
            ($contract:ty) => {{
                let service = connection.lookup_local::<$contract>().ok_or_else(|| {
                    activation(concat!(
                        "remote Profile did not publish ",
                        stringify!($contract)
                    ))
                })?;
                plan.context().provide_local::<$contract>(service)?
            }};
        }
        let supplies = vec![
            facet!(rsi_session_protocol::SessionContract),
            facet!(rsi_session_files::SessionFilesContract),
            facet!(rsi_workspace_protocol::WorkspaceRegistryContract),
            facet!(rsi_ai_protocol::LanguageModelsContract),
            facet!(rsi_process::ProcessOutputCacheContract),
            facet!(rsi_settings_protocol::SettingsAccessContract),
            facet!(rsi_media_protocol::MediaContract),
            plan.context()
                .provide_local::<rsi_client::ConnectionLifetimeContract>(Arc::new(
                    rsi_client::ConnectionLifetime::Remote,
                ))?,
        ];
        plan.defer(
            "withdraw remote application capabilities",
            Box::new(move || {
                Box::pin(async move {
                    drop(supplies);
                    Ok(())
                })
            }),
        )
    }
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
