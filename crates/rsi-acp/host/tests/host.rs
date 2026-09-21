#![cfg(unix)]
use async_trait::async_trait;
use rsi_acp_protocol::{
    observation::{ConversationId, Status},
    service::{Error, ExternalConversations, ExternalConversationsContract, Setup},
};
use rsi_credentials_protocol::{
    CredentialRef, CredentialSource, CredentialsResolve, CredentialsResolveContract,
    ResolvedCredential, SecretValue,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use serde_json::json;
use std::{path::Path, sync::Arc, time::Duration};

#[derive(Debug)]
struct Credentials;
#[async_trait]
impl CredentialsResolve for Credentials {
    async fn resolve(
        &self,
        reference: &CredentialRef,
    ) -> rsi_credentials_protocol::Result<ResolvedCredential> {
        assert_eq!(reference.owner.as_str(), "rsi.acp");
        Ok(ResolvedCredential {
            secret: SecretValue::new(if reference.slot == "dsh-live" {
                std::env::var("RSI_DSH_LIVE_KEY").expect("explicit opt-in DSH live credential")
            } else {
                "private-fixture-secret".into()
            })
            .unwrap(),
            source: CredentialSource::File,
        })
    }
}
#[async_trait]
impl PluginFactory for Credentials {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<CredentialsResolveContract>(Arc::new(Self))?;
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
            ResolvedFactory::linked(
                id,
                "fixture",
                UpdateMode::RestartRequired,
                Arc::new(factory),
            ),
            config,
        )
        .await
        .unwrap()
}
async fn fixture(
    root: &Path,
    mode: &str,
) -> (
    Runtime,
    rsi_meta::FiberHandle,
    Arc<dyn ExternalConversations>,
    ConfigValue,
) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/rsi/acp/agent.py");
    fixture_command(
        root,
        "/usr/bin/python3",
        vec![
            "-u".into(),
            script.to_str().unwrap().into(),
            root.to_str().unwrap().into(),
            mode.into(),
        ],
    )
    .await
}
async fn fixture_command(
    root: &Path,
    program: &str,
    arguments: Vec<String>,
) -> (
    Runtime,
    rsi_meta::FiberHandle,
    Arc<dyn ExternalConversations>,
    ConfigValue,
) {
    let runtime = Runtime::default();
    activate(
        &runtime,
        "process",
        rsi_process_local::ProcessLocalFactory,
        json!({}),
    )
    .await;
    activate(
        &runtime,
        "sandbox",
        rsi_sandbox_local::SandboxLocalFactory::default(),
        json!({}),
    )
    .await;
    activate(&runtime, "credentials", Credentials, json!(null)).await;
    let slow = arguments.last().is_some_and(|mode| mode == "slow-phases");
    let launch = json!({"program":program, "arguments":arguments, "environment":{"FIXTURE_SECRET":{"kind":"credential","reference":{"owner":"rsi.acp","slot":"fixture"}}}});
    let mut config = json!({"directory":root.join("journal"),"endpoints":[{"id":"configured", "enabled":true,"cwd":root,"sandbox":"danger-full-access","launch":launch,"mcp_servers":[{"name":"private-mcp","launch":{"program":"/usr/bin/python3","environment":{"MCP_SECRET":{"kind":"credential","reference":{"owner":"rsi.acp","slot":"fixture"}}}}}]}]});
    if slow {
        config["endpoints"][0]["session_options"] = json!([{"id":"mode","value":"selected"}]);
    }
    let fiber = activate(&runtime, "acp", rsi_acp_host::Factory, config.clone()).await;
    let service = runtime
        .root()
        .lookup_local::<ExternalConversationsContract>()
        .unwrap();
    (runtime, fiber, service, config)
}
fn id(text: &str) -> ConversationId {
    ConversationId::new(text).unwrap()
}
fn assert_reaped(root: &Path) {
    for pid in std::fs::read_to_string(root.join("peer-pids"))
        .unwrap()
        .lines()
    {
        #[cfg(target_os = "linux")]
        assert!(!Path::new("/proc").join(pid).exists(), "peer {pid} remains");
    }
}
async fn settle(service: &dyn ExternalConversations, id: &ConversationId, expected: Status) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if service.view(id).await.unwrap().snapshot.status == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn host_owns_peers_across_detach_reconciles_start_and_reopens_after_retirement() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, fiber, service, config) = fixture(root.path(), "normal").await;
    assert_eq!(
        serde_json::to_value(service.endpoints().await.unwrap()).unwrap(),
        json!([{"id":"configured","enabled":true}])
    );
    let identity = id("direct");
    let snapshot = service.start(identity.clone(), "configured").await.unwrap();
    assert_eq!(snapshot.status, Status::Ready);
    assert_eq!(
        service.start(identity.clone(), "configured").await.unwrap(),
        snapshot
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("new-count")).unwrap(),
        "new\n"
    );
    assert_eq!(
        service.start(identity.clone(), "other").await.unwrap_err(),
        Error::Stale
    );
    let detached = service.clone();
    drop(detached);
    service.submit(&identity, "work").await.unwrap();
    settle(service.as_ref(), &identity, Status::Completed).await;
    let projection = service.page(&identity, snapshot.epoch, 0).await.unwrap();
    assert_eq!(projection.records.len(), 2);
    assert_eq!(
        service.close(&identity).await.unwrap().status,
        Status::Closed
    );
    assert_reaped(root.path());
    assert_eq!(
        service
            .reconnect(&identity, Setup::Resume)
            .await
            .unwrap()
            .epoch,
        1
    );
    assert_eq!(
        service.page(&identity, 1, 0).await.unwrap().records.len(),
        2
    );
    service.close(&identity).await.unwrap();
    assert_eq!(
        service
            .reconnect(&identity, Setup::Load)
            .await
            .unwrap()
            .epoch,
        2
    );
    assert_eq!(
        service.page(&identity, 2, 0).await.unwrap().records.len(),
        1
    );
    assert!(fiber.dispose().await.is_clean());
    assert_reaped(root.path());
    assert!(service.list(None).await.is_err());
    // Retained obsolete service handles do not keep the writer lease after retirement.
    let replacement = activate(&runtime, "acp-replacement", rsi_acp_host::Factory, config).await;
    let current = runtime
        .root()
        .lookup_local::<ExternalConversationsContract>()
        .unwrap();
    assert_eq!(current.list(None).await.unwrap().len(), 1);
    assert!(current.residents().await.unwrap().is_empty());
    assert!(replacement.dispose().await.is_clean());
    let stored = std::fs::read(root.path().join("journal/observed.sqlite3")).unwrap();
    assert!(
        !stored
            .windows(b"private-fixture-secret".len())
            .any(|window| window == b"private-fixture-secret")
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn eight_resident_limit_includes_idle_peers_and_shutdown_cancels_running_prompt() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, fiber, service, _) = fixture(root.path(), "normal").await;
    for index in 0..8 {
        service
            .start(id(&format!("external-{index}")), "configured")
            .await
            .unwrap();
    }
    assert_eq!(service.residents().await.unwrap().len(), 8);
    assert_eq!(
        service.start(id("ninth"), "configured").await.unwrap_err(),
        Error::Busy
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("peer-pids"))
            .unwrap()
            .lines()
            .count(),
        8
    );
    service.submit(&id("external-0"), "wait").await.unwrap();
    assert!(fiber.dispose().await.is_clean());
    assert_reaped(root.path());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn abandoned_setup_and_stubborn_process_are_reaped_on_provider_retirement() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, fiber, service, _) = fixture(root.path(), "stall").await;
    let setup = tokio::spawn({
        let service = service.clone();
        async move { service.start(id("preparing"), "configured").await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("peer-pids").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    setup.abort();
    assert!(setup.await.unwrap_err().is_cancelled());
    assert!(fiber.dispose().await.is_clean());
    assert_reaped(root.path());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn missing_workspace_does_not_leak_resident_capacity_or_fail_retirement() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, fiber, _, mut config) = fixture(root.path(), "normal").await;
    assert!(fiber.dispose().await.is_clean());
    config["endpoints"][0]["cwd"] = json!(root.path().join("absent"));
    let fiber = activate(&runtime, "missing-workspace", rsi_acp_host::Factory, config).await;
    let service = runtime
        .root()
        .lookup_local::<ExternalConversationsContract>()
        .unwrap();
    for index in 0..16 {
        assert_eq!(
            service
                .start(id(&format!("missing-{index}")), "configured")
                .await
                .unwrap_err(),
            Error::Launch
        );
    }
    assert!(service.residents().await.unwrap().is_empty());
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[ignore = "opt-in independent Node ACP SDK 1.4.0; set RSI_ACP_SDK_NODE to its absolute executable"]
async fn independent_sdk_agent_new_permissions_resume_full_load_and_cancel() {
    let node = std::env::var("RSI_ACP_SDK_NODE").expect("explicit absolute Node executable");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/rsi/acp/agent.mjs")
        .canonicalize()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let (runtime, fiber, service, _) = fixture_command(
        root.path(),
        &node,
        vec![
            script.to_str().unwrap().into(),
            root.path().to_str().unwrap().into(),
        ],
    )
    .await;
    let identity = id("sdk-independent");
    let snapshot = service.start(identity.clone(), "configured").await.unwrap();
    assert_eq!(snapshot.remote.as_deref(), Some("sdk-remote"));
    service.submit(&identity, "permission").await.unwrap();
    let permission = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(permission) = service
                .view(&identity)
                .await
                .unwrap()
                .permissions
                .into_iter()
                .next()
            {
                break permission;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(permission.options.len(), 4);
    service
        .answer(
            &identity,
            snapshot.generation,
            &permission.id,
            "sdk-exact-always",
        )
        .await
        .unwrap();
    settle(service.as_ref(), &identity, Status::Completed).await;
    assert_eq!(
        service.page(&identity, 1, 0).await.unwrap().records.len(),
        3
    );
    service.close(&identity).await.unwrap();
    let resumed = service.reconnect(&identity, Setup::Resume).await.unwrap();
    assert_eq!(resumed.epoch, 1);
    assert_eq!(
        service.page(&identity, 1, 0).await.unwrap().records.len(),
        3
    );
    service.close(&identity).await.unwrap();
    let loaded = service.reconnect(&identity, Setup::Load).await.unwrap();
    let mut after = 0;
    let mut count = 0;
    loop {
        let page = service.page(&identity, loaded.epoch, after).await.unwrap();
        count += page.records.len();
        after = page.records.last().unwrap().sequence;
        if !page.has_more {
            break;
        }
    }
    assert_eq!(count, 1200);
    service.submit(&identity, "wait").await.unwrap();
    assert!(matches!(
        service.cancel(&identity).await.unwrap().status,
        Status::Cancelled | Status::Discarded
    ));
    service.close(&identity).await.unwrap();
    assert_reaped(root.path());
    assert!(fiber.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    println!(
        "{}",
        json!({"sdk":"@agentclientprotocol/sdk@1.4.0","role":"independent-agent-to-rsi-host-client","new":true,"permission_options":4,"resume_replay_records":0,"load_records":count,"processes_reaped":true})
    );
}

#[path = "host/dsh.rs"]
mod dsh;

#[tokio::test]
#[ignore = "opt-in real-process phase timing regression; takes about 90 seconds"]
async fn host_does_not_shorten_client_setup_and_close_phase_budgets() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, fiber, service, _) = fixture(root.path(), "slow-phases").await;
    let identity = id("slow");
    let started = std::time::Instant::now();
    assert_eq!(
        service
            .start(identity.clone(), "configured")
            .await
            .unwrap()
            .status,
        Status::Ready
    );
    assert!(started.elapsed() >= Duration::from_secs(45));
    service.submit(&identity, "wait").await.unwrap();
    let closing = std::time::Instant::now();
    assert_eq!(
        service.close(&identity).await.unwrap().status,
        Status::Closed
    );
    assert!(closing.elapsed() >= Duration::from_secs(35));
    assert!(fiber.dispose().await.is_clean());
    assert_reaped(root.path());
    assert!(runtime.shutdown().await.is_clean());
}
