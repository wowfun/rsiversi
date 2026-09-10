#![cfg(not(target_family = "wasm"))]

use async_trait::async_trait;
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileFragment};
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, ContractVersion, FactoryIdentity, LocalContract,
    Message, PluginFactory, PreparedActivation, ProviderChannel, Requirement, ServiceEndpoint,
    UpdateMode,
};
use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const V1: ContractVersion = ContractVersion(1);

#[derive(Debug)]
struct Upstream;

#[async_trait]
impl ServiceEndpoint for Upstream {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Provider;

#[async_trait]
impl PluginFactory for Provider {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide("upstream", "fixture.upstream", V1, Arc::new(Upstream))?;
        Ok(())
    }
}

enum Echo {}
impl LocalContract for Echo {
    const KEY: &'static str = "test.echo";
    type Service = Capability;
}

#[derive(Debug)]
struct Consumer;

#[async_trait]
impl PluginFactory for Consumer {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                "echo",
                "fixture.echo",
                V1,
            )),
        )
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<Echo>(Arc::new(plan.inject("echo").unwrap().clone()))?;
        Ok(())
    }
}

#[tokio::test]
async fn native_catalog_factory_runs_through_frozen_host_and_releases_after_last_owner() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let target = root.join("target/native-fixture-test");
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "--locked", "--manifest-path"])
        .arg(root.join("fixtures/rsi-meta/echo-bidi/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success());
    let artifact = target.join("debug").join(format!(
        "{}rsi_meta_fixture_echo_bidi{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    let expected = FactoryIdentity::native(
        "fixture.native-echo",
        hex::encode(Sha256::digest(std::fs::read(&artifact).unwrap())),
    );
    let cache = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(cache.path())).unwrap();
    let factory = catalog.load(&artifact).unwrap();
    assert_eq!(factory.identity(), &expected);
    let mut builder = HostBuilder::without_paths("native-test");
    builder.register_factory(factory).unwrap();
    builder
        .register_linked("upstream", "1", UpdateMode::Replayable, Arc::new(Provider))
        .unwrap();
    builder
        .register_linked("consumer", "1", UpdateMode::Replayable, Arc::new(Consumer))
        .unwrap();
    builder.register_local_contract::<Echo>().unwrap();
    builder
        .register_fragment(ProfileFragment::new(
            "base",
            [
                ProfileEntry::new("upstream", "upstream", Value::Null),
                ProfileEntry::new("native", "fixture.native-echo", json!({"prefix":"native:"})),
                ProfileEntry::new("consumer", "consumer", Value::Null),
            ],
        ))
        .unwrap();
    let host = builder.build().unwrap();
    assert_eq!(
        host.preview(Profile::default()).unwrap().leaves[1].identity,
        expected
    );
    let running = host.start(Profile::default()).await.unwrap();
    assert!(
        running
            .runtime_snapshot()
            .fibers
            .iter()
            .any(|fiber| fiber.factory == expected)
    );
    let echo = running.lookup_local::<Echo>().unwrap();
    let response = echo.invoke(Message::new(b"host".as_slice())).await.unwrap();
    assert_eq!(response.as_bytes(), b"native:host");
    drop(response);
    drop(echo);
    assert!(running.shutdown().await.is_clean());
    // Frozen catalog ownership still pins code after Runtime shutdown.
    assert!(catalog.snapshot().staging_bytes > 0);
    drop(running);
    tokio::time::timeout(Duration::from_secs(10), async {
        while catalog.snapshot().staging_bytes != 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let resources = catalog.snapshot();
    assert_eq!(resources.active_instances, 0);
    assert_eq!(resources.active_callbacks, 0);
    assert_eq!(resources.host_capabilities, 0);
    assert_eq!(resources.host_outputs, 0);
    assert_eq!(resources.retained_failed_finalizations, 0);
    // Durable content-addressed cache remains until explicit cache management.
    assert!(resources.cache_artifacts > 0);
}
