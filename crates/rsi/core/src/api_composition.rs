use rsi_host::{ProfileEntry, ProfileFragment};
use rsi_meta::UpdateMode;
use serde_json::{Value, json};

pub(crate) fn register(builder: &mut crate::StandardAddonBuilder) -> rsi_host::Result<()> {
    use rsi_api_protocol::{
        ApiDispatchContract, ApiRegistrarContract, ConnectionDescriptionContract,
        DeviceAdministrationContract, DeviceAuthenticationContract, EndpointIdentityContract,
    };
    builder.register_local_contract::<ApiDispatchContract>()?;
    builder.register_local_contract::<ApiRegistrarContract>()?;
    builder.register_local_contract::<ConnectionDescriptionContract>()?;
    builder.register_local_contract::<EndpointIdentityContract>()?;
    builder.register_local_contract::<DeviceAdministrationContract>()?;
    builder.register_local_contract::<DeviceAuthenticationContract>()?;
    #[cfg(unix)]
    {
        builder.register_local_contract::<rsi_service_host::LocalApiListenerContract>()?;
        builder.register_linked(
            "rsi.api.local",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            std::sync::Arc::new(rsi_service_host::LocalApiFactory),
        )?;
    }
    let factories: [(&str, std::sync::Arc<dyn rsi_meta::PluginFactory>); 11] = [
        ("rsi.api", std::sync::Arc::new(rsi_api::ApiFactory)),
        (
            "rsi.service.identity",
            std::sync::Arc::new(rsi_service_host::ServiceIdentityFactory),
        ),
        (
            "rsi.api.connection",
            std::sync::Arc::new(rsi_api::ConnectionApiFactory),
        ),
        (
            "rsi.api.devices",
            std::sync::Arc::new(rsi_api_auth::DeviceAuthFactory),
        ),
        (
            "rsi.api.devices.operations",
            std::sync::Arc::new(rsi_api_device_api::DeviceApiFactory),
        ),
        (
            "rsi.workspace.api",
            std::sync::Arc::new(rsi_workspace_api::WorkspaceApiFactory),
        ),
        (
            "rsi.models.api",
            std::sync::Arc::new(rsi_ai_models_api::ModelsApiFactory),
        ),
        (
            "rsi.settings.api",
            std::sync::Arc::new(rsi_settings_api::SettingsApiFactory),
        ),
        (
            "rsi.media.api",
            std::sync::Arc::new(rsi_media_api::MediaApiFactory),
        ),
        (
            "rsi.output.api",
            std::sync::Arc::new(rsi_process_output_api::OutputApiFactory),
        ),
        (
            "rsi.session.api",
            std::sync::Arc::new(rsi_session_api::SessionApiFactory),
        ),
    ];
    let mut entries = Vec::with_capacity(factories.len());
    for (id, factory) in factories {
        builder.register_linked(
            id,
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            factory,
        )?;
        let config = if matches!(id, "rsi.service.identity" | "rsi.api.devices") {
            json!({"backend": "base"})
        } else {
            Value::Null
        };
        entries.push(ProfileEntry::new(id, id, config));
    }
    builder.register_fragment(ProfileFragment::new("rsi.standard.api", entries))?;
    Ok(())
}
