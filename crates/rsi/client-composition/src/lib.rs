//! Stateless standard-product domain client composition shared by native and Web.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use rsi_host::{HostBuilder, ProfileEntry};
use rsi_meta::{PluginFactory, UpdateMode};
use std::sync::Arc;

/// Returns a fresh mutable factory/marker catalog and its ordered domain-only entries.
pub fn domain_clients(platform: &str) -> rsi_host::Result<(HostBuilder, Vec<ProfileEntry>)> {
    let mut builder = HostBuilder::without_paths(platform);
    builder.register_local_contract::<rsi_api_protocol::ApiClientContract>()?;
    builder.register_local_contract::<rsi_session_protocol::SessionContract>()?;
    builder.register_local_contract::<rsi_session_files::SessionFilesContract>()?;
    builder.register_local_contract::<rsi_workspace_protocol::WorkspaceRegistryContract>()?;
    builder.register_local_contract::<rsi_ai_protocol::LanguageModelsContract>()?;
    builder.register_local_contract::<rsi_process::ProcessOutputCacheContract>()?;
    builder.register_local_contract::<rsi_settings_protocol::SettingsAccessContract>()?;
    builder.register_local_contract::<rsi_media_protocol::MediaContract>()?;
    builder.register_local_contract::<rsi_media_protocol::MediaReadContract>()?;
    let factories: [(&str, Arc<dyn PluginFactory>); 7] = [
        (
            "rsi.session.files.client",
            Arc::new(rsi_session_files::SessionFilesClientFactory),
        ),
        (
            "rsi.session.client",
            Arc::new(rsi_session_api::SessionClientFactory),
        ),
        (
            "rsi.workspace.client",
            Arc::new(rsi_workspace_api::WorkspaceClientFactory),
        ),
        (
            "rsi.models.client",
            Arc::new(rsi_ai_models_api::ModelsClientFactory),
        ),
        (
            "rsi.output.client",
            Arc::new(rsi_process_output_api::OutputClientFactory),
        ),
        (
            "rsi.settings.client",
            Arc::new(rsi_settings_api::SettingsClientFactory),
        ),
        (
            "rsi.media.client",
            Arc::new(rsi_media_api::MediaClientFactory),
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
        entries.push(ProfileEntry::new(id, id, serde_json::Value::Null));
    }
    Ok((builder, entries))
}
