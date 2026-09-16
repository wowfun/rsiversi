#[allow(dead_code)]
mod support;
use async_trait::async_trait;
use rsi_mcp::{McpFactory, McpOwnerContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use rsi_settings_protocol::{SettingsAccessContract, SettingsContract};
use serde_json::json;
use std::sync::Arc;
use support::*;
use tokio_util::sync::CancellationToken;
#[derive(Debug)]
struct Capabilities;
#[async_trait]
impl PluginFactory for Capabilities {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<rsi_credentials_protocol::CredentialsResolveContract>(Arc::new(
                Credentials::default(),
            ))?;
        plan.context()
            .provide_local::<rsi_sandbox::SandboxContract>(Arc::new(TestSandbox))?;
        plan.context()
            .provide_local::<rsi_process::DuplexProcessContract>(Arc::new(NoProcess))?;
        Ok(())
    }
}
async fn activate(
    runtime: &Runtime,
    id: &str,
    factory: impl PluginFactory,
    config: ConfigValue,
) -> rsi_meta::FiberHandle {
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(id, "fixture", UpdateMode::Replayable, Arc::new(factory)),
            config,
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn saved_http_settings_require_explicit_verification_and_stdio_is_never_exported_in_settings()
{
    let fixture = HttpFixture::start(Mode::default()).await;
    let runtime = Runtime::default();
    let provider = activate(
        &runtime,
        "settings.provider",
        rsi_settings_testkit::MemorySettingsProviderFactory::new(json!({})),
        json!(null),
    )
    .await;
    let settings = activate(
        &runtime,
        "settings",
        rsi_settings::SettingsFactory,
        json!(null),
    )
    .await;
    let capabilities = activate(&runtime, "capabilities", Capabilities, json!(null)).await;
    let stdio = json!({"servers":[{"id":"private-local","enabled":false,"transport":{"kind":"stdio","program":"/private/local-program","cwd":"/private/local-cwd","arguments":["private-launch-argument"],"environment":{}}}]});
    let mcp = activate(&runtime, "mcp", McpFactory, stdio).await;
    let owner = runtime.root().lookup_local::<McpOwnerContract>().unwrap();
    assert!(owner.seed().is_ok());
    assert!(owner.is_stdio("private-local"));
    let access = runtime
        .root()
        .lookup_local::<SettingsAccessContract>()
        .unwrap();
    let actual = access.read("rsi.mcp").await.unwrap();
    assert_eq!(actual.value, json!({"servers":[]}));
    let description = access.describe("rsi.mcp").await.unwrap();
    assert!(
        !serde_json::to_string(&description)
            .unwrap()
            .contains("/private/")
    );
    let scope = runtime
        .root()
        .lookup_local::<SettingsContract>()
        .unwrap()
        .scope("rsi.mcp")
        .unwrap();
    let saved = scope
        .replace(
            actual.version().revision,
            serde_json::to_value(fixture.config()).unwrap(),
        )
        .await
        .unwrap();
    assert!(owner.settings_pending());
    assert!(owner.seed().is_err());
    owner
        .refresh(Some("fixture"), CancellationToken::new())
        .await
        .unwrap();
    assert!(!owner.settings_pending());
    assert!(owner.seed().is_ok());
    assert!(scope.replace(saved.version().revision,json!({"servers":[{"id":"escape","transport":{"kind":"stdio","program":"/bin/sh","cwd":"/tmp"}}]})).await.is_err());
    assert!(mcp.dispose().await.is_clean());
    drop(owner);
    assert!(capabilities.dispose().await.is_clean());
    assert!(settings.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    fixture.shutdown().await;
}
