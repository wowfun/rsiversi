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
#[allow(dead_code)]
#[path = "../../../../fixtures/rsi/execution/metadata.rs"]
mod execution_fixture;

#[tokio::test]
async fn ssh_refresh_does_not_nest_use_permits_and_revocation_precedes_credentials() {
    use std::sync::atomic::Ordering;
    let credentials = Arc::new(Credentials::default());
    let service = rsi_mcp::McpService::new(
        credentials.clone(),
        Arc::new(NoProcess),
        Arc::new(TestSandbox),
    );
    let target = rsi_execution::ExecutionTargetId::parse("a".repeat(32)).unwrap();
    let config = serde_json::from_value(json!({"servers":[{
        "id":"remote","enabled":true,"transport":{"kind":"ssh_stdio","target":target,"command":"fixture","cwd":"/tmp",
        "environment":{"TOKEN":{"kind":"credential","reference":{"owner":"rsi.mcp","slot":"fixture"}}}}
    }]})).unwrap();
    service.configure(config).await.unwrap();
    let gate = Arc::new(execution_fixture::Gate {
        maximum: Some(1),
        ..Default::default()
    });
    let lease = execution_fixture::lease(
        rsi_execution::ExecutionLocation::Ssh { target },
        gate.clone(),
        1,
    );
    // The inert backend rejects target-program resolution as Unsupported. Reaching
    // that boundary proves the sole permit was not consumed by an outer refresh.
    assert_eq!(
        service
            .refresh("remote", Some(lease.clone()), CancellationToken::new())
            .await
            .unwrap_err(),
        rsi_mcp::McpError::ProcessUnavailable
    );
    assert_eq!(credentials.resolutions.load(Ordering::SeqCst), 1);
    assert_eq!(gate.active.load(Ordering::SeqCst), 0);
    gate.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        service
            .refresh("remote", Some(lease), CancellationToken::new())
            .await
            .unwrap_err(),
        rsi_mcp::McpError::ProcessUnavailable
    );
    assert_eq!(credentials.resolutions.load(Ordering::SeqCst), 1);
    service.shutdown().await.unwrap();
}
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
    let mcp = activate(&runtime, "mcp", McpFactory, stdio.clone()).await;
    let owner = runtime.root().lookup_local::<McpOwnerContract>().unwrap();
    assert!(owner.seed().is_ok());
    assert!(owner.is_stdio("private-local"));
    check_unchanged_remote_keeps_seed(&owner, rsi_mcp::McpConfig::default()).await;
    check_remote_rejection_and_abandonment(&runtime, &owner, stdio).await;
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

async fn check_unchanged_remote_keeps_seed(owner: &rsi_mcp::McpOwner, config: rsi_mcp::McpConfig) {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let before = owner.seed().unwrap();
    let mut apply = Box::pin(owner.set_remote_stdio(config));
    match apply.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => {
            assert!(!owner.settings_pending(), "unchanged inputs became pending");
            assert_eq!(owner.seed().unwrap().sha256(), before.sha256());
            apply.await.unwrap();
        }
    }
    assert_eq!(owner.seed().unwrap().sha256(), before.sha256());
}

async fn check_remote_rejection_and_abandonment(
    runtime: &Runtime,
    owner: &rsi_mcp::McpOwner,
    stdio: ConfigValue,
) {
    let remote = |id: &str| {
        serde_json::from_value(json!({"servers":[{
            "id":id,"enabled":false,"transport":{"kind":"ssh_stdio",
            "target":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","command":"fixture","cwd":"/tmp"}
        }]}))
        .unwrap()
    };
    owner.set_remote_stdio(remote("remote-good")).await.unwrap();
    assert!(!owner.settings_pending());
    check_unchanged_remote_keeps_seed(owner, remote("remote-good")).await;
    assert_eq!(
        owner.set_remote_stdio(remote("private-local")).await,
        Err(rsi_mcp::McpError::Protocol)
    );
    assert!(owner.is_ssh_stdio("remote-good"));
    assert!(!owner.is_ssh_stdio("private-local"));
    assert!(!owner.settings_pending());
    owner.refresh(None, CancellationToken::new()).await.unwrap();
    {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let service = runtime
            .root()
            .lookup_local::<rsi_mcp::McpContract>()
            .unwrap();
        let mut busy: rsi_mcp::McpConfig = serde_json::from_value(stdio).unwrap();
        busy.servers.extend(remote("remote-good").servers);
        let mut configure = Box::pin(service.configure(busy));
        assert!(matches!(
            configure
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        assert_eq!(
            owner.set_remote_stdio(remote("remote-busy")).await,
            Err(rsi_mcp::McpError::Busy)
        );
        assert!(owner.is_ssh_stdio("remote-good"));
        assert!(!owner.is_ssh_stdio("remote-busy"));
        assert!(!owner.settings_pending());
        configure.await.unwrap();
        let mut abandoned = Box::pin(owner.set_remote_stdio(remote("remote-new")));
        assert!(matches!(
            abandoned
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        drop(abandoned);
        assert!(owner.is_ssh_stdio("remote-new"));
        assert!(owner.settings_pending());
        // Let the already accepted retirement release configuration admission.
        tokio::task::yield_now().await;
        let mut repeated = Box::pin(owner.set_remote_stdio(remote("remote-new")));
        assert!(matches!(
            repeated
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        assert!(owner.settings_pending());
        assert!(owner.seed().is_err());
        drop(repeated);
        tokio::task::yield_now().await;
        owner.refresh(None, CancellationToken::new()).await.unwrap();
        assert!(!owner.settings_pending());
        assert!(owner.is_ssh_stdio("remote-new"));
    }
}
