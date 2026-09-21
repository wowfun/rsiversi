//! Independently linked language addon, using only public author contracts.
use rsi::{AddonScope, StandardAddonBuilder, StandardAddonSet};
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::UpdateMode;
use std::sync::Arc;
pub fn addons() -> rsi_host::Result<StandardAddonSet> {
    let mut builder = StandardAddonBuilder::new("example.language");
    for (id, factory) in [
        (
            "example.process",
            Arc::new(rsi_process_local::ProcessLocalFactory) as Arc<dyn rsi_meta::PluginFactory>,
        ),
        (
            "example.sandbox",
            Arc::new(rsi_sandbox_local::SandboxLocalFactory::default()),
        ),
        ("example.files", Arc::new(rsi_files::FilesFactory)),
        ("example.language", Arc::new(rsi_lsp::LanguageFactory)),
        (
            "example.language.ui",
            Arc::new(rsi_lsp_ui::LanguageUiFactory),
        ),
    ] {
        builder.register_factory(
            AddonScope::Service,
            id,
            "1",
            UpdateMode::Replayable,
            factory,
        )?;
    }
    builder.register_factory(
        AddonScope::Agent,
        "example.language.tools",
        "1",
        UpdateMode::Replayable,
        Arc::new(rsi_lsp::LanguageToolsFactory),
    )?;
    builder.register_local_contract::<rsi_process::DuplexProcessContract>()?;
    builder.register_local_contract::<rsi_process::ProcessContract>()?;
    builder.register_local_contract::<rsi_sandbox::SandboxContract>()?;
    builder.register_local_contract::<rsi_files_protocol::FilesContract>()?;
    builder.register_local_contract::<rsi_lsp::LanguageContract>()?;
    builder.register_local_contract::<rsi_ui::UiContract>()?;
    builder.register_local_contract_at::<rsi_tools_protocol::ToolRegistrarContract>(
        AddonScope::Agent,
    )?;
    builder.register_local_contract_at::<rsi_lsp::LanguageContract>(AddonScope::Agent)?;
    StandardAddonSet::new([builder.build()?])
}
pub fn program(config: rsi_lsp::Config) -> ProfileProgram {
    ProfileProgram::from_profile(Profile::new([
        ProfileEntry::new("process", "example.process", serde_json::json!({})),
        ProfileEntry::new(
            "sandbox",
            "example.sandbox",
            if cfg!(target_os = "linux") {
                serde_json::json!({"bubblewrap":["/usr/bin/bwrap"],"landlock":[]})
            } else {
                serde_json::json!({})
            },
        ),
        ProfileEntry::new("files", "example.files", serde_json::Value::Null),
        ProfileEntry::new(
            "language",
            "example.language",
            serde_json::to_value(config).unwrap(),
        ),
    ]))
}
pub fn host() -> rsi_host::Result<rsi_host::Host> {
    let mut builder = HostBuilder::without_paths(std::env::consts::OS);
    addons()?.register_into(&mut builder, AddonScope::Service)?;
    builder.build()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn public_testkit_replaces_provider_and_retires_retained_generation() {
        let config = |revision| rsi_lsp::Config {
            program: if cfg!(windows) {
                "C:\\fixture\\server.exe".into()
            } else {
                "/fixture/server".into()
            },
            arguments: vec![],
            environment: Default::default(),
            languages: [(".rs".into(), "rust".into())].into(),
            initialization_options: serde_json::json!({"revision":revision}),
            configuration: serde_json::Value::Null,
        };
        rsi_addon_testkit::assert_addon_generations::<rsi_lsp::LanguageContract>(
            &addons().unwrap(),
            AddonScope::Service,
            [program(config(1)), program(config(2))],
            |generation, service| {
                assert_eq!(
                    service.retired(),
                    matches!(generation, rsi_addon_testkit::GenerationProbe::Retained)
                );
            },
        )
        .await;
    }
}
