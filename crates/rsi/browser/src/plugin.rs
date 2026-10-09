use crate::{NativeRuntime, RuntimeConfig, SessionBrowser};
use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use std::sync::{Arc, Mutex};
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(e.to_string())
}
type InstalledRuntime = (RuntimeConfig, Arc<NativeRuntime>, Option<u16>);
/// Exactly one installed native runtime and shared capacity per Service generation.
#[derive(Debug)]
pub struct RuntimePool {
    runtime: Mutex<Option<InstalledRuntime>>,
    process: Arc<dyn rsi_process::DuplexProcess>,
    sandbox: Arc<dyn rsi_sandbox::Sandbox>,
}
impl RuntimePool {
    /// # Errors
    /// Rejects a different runtime configuration or failed confinement preparation.
    pub async fn acquire(&self, config: RuntimeConfig) -> Result<Arc<NativeRuntime>, String> {
        self.acquire_inner(config, None).await
    }
    #[cfg(feature = "test-support")]
    /// # Errors
    /// Rejects mismatched runtime or fixture policy and failed preparation.
    pub async fn acquire_with_fixture(
        &self,
        config: RuntimeConfig,
        port: u16,
    ) -> Result<Arc<NativeRuntime>, String> {
        self.acquire_inner(config, Some(port)).await
    }
    async fn acquire_inner(
        &self,
        config: RuntimeConfig,
        fixture: Option<u16>,
    ) -> Result<Arc<NativeRuntime>, String> {
        let runtime = {
            let mut guard = self
                .runtime
                .lock()
                .map_err(|_| "browser runtime pool unavailable")?;
            if let Some((current, runtime, network)) = &*guard {
                if *network != fixture || current != &config {
                    return Err("Automation and Session browsers require the same pinned runtime and network policy".into());
                }
                runtime.clone()
            } else {
                let runtime =
                    NativeRuntime::new(config.clone(), self.process.clone(), self.sandbox.clone())?;
                #[cfg(feature = "test-support")]
                let runtime = if let Some(port) = fixture {
                    runtime.with_fixture_network(port)
                } else {
                    runtime
                };
                let runtime = Arc::new(runtime);
                *guard = Some((config, runtime.clone(), fixture));
                runtime
            }
        };
        runtime.prepare().await?;
        Ok(runtime)
    }
}
#[derive(Debug)]
pub struct RuntimePoolContract;
impl LocalContract for RuntimePoolContract {
    const KEY: &'static str = "rsi.browser.runtime-pool";
    type Service = RuntimePool;
}
#[derive(Debug, Default)]
pub struct RuntimePoolFactory;
#[async_trait]
impl PluginFactory for RuntimePoolFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(meta("browser runtime pool config must be null"));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<rsi_process::DuplexProcessContract>()
            .requiring_local::<rsi_sandbox::SandboxContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let pool = Arc::new(RuntimePool {
            runtime: Mutex::new(None),
            process: plan.local::<rsi_process::DuplexProcessContract>()?,
            sandbox: plan.local::<rsi_sandbox::SandboxContract>()?,
        });
        let supply = plan.context().provide_local::<RuntimePoolContract>(pool)?;
        plan.defer(
            "withdraw browser runtime pool",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
pub struct SessionBrowserContract;
impl LocalContract for SessionBrowserContract {
    const KEY: &'static str = "rsi.browser.session";
    type Service = SessionBrowser;
}
#[derive(Debug, Default)]
pub struct SessionBrowserFactory;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    runtime: RuntimeConfig,
}
#[async_trait]
impl PluginFactory for SessionBrowserFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let c: Config = serde_json::from_value(config.clone()).map_err(meta)?;
        c.runtime.validate().map_err(meta)?;
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<RuntimePoolContract>()
            .requiring_local::<rsi_media_protocol::MediaContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let c: Config = serde_json::from_value(plan.config().as_ref().clone()).map_err(meta)?;
        let runtime = plan
            .local::<RuntimePoolContract>()?
            .acquire(c.runtime)
            .await
            .map_err(meta)?;
        let owner =
            SessionBrowser::new(runtime, plan.local::<rsi_media_protocol::MediaContract>()?)
                .map_err(meta)?;
        let supply = plan
            .context()
            .provide_local::<SessionBrowserContract>(owner.clone())?;
        plan.defer(
            "retire Session browsers",
            Box::new(move || {
                Box::pin(async move {
                    owner.close().await;
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct NoNativeWork;
    #[async_trait]
    impl rsi_process::DuplexProcess for NoNativeWork {
        async fn spawn(
            &self,
            _: rsi_process::DuplexProcessSpec,
        ) -> rsi_process::Result<rsi_process::ManagedDuplexProcess> {
            panic!("failed artifact verification must not spawn a native process")
        }
    }
    #[async_trait]
    impl rsi_sandbox::Sandbox for NoNativeWork {
        async fn workspace_read(
            &self,
            _: rsi_sandbox::WorkspaceReadRequest,
        ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
            unreachable!()
        }
        async fn confine(
            &self,
            _: rsi_sandbox::ProcessRequest,
        ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
            unreachable!()
        }
    }
    #[tokio::test]
    async fn failed_preparation_keeps_one_pinned_config_and_capacity_owner() {
        let missing = tempfile::tempdir().unwrap();
        let config = RuntimeConfig {
            node: missing.path().join("missing-node"),
            chromium_directory: missing.path().join("chrome"),
            package_directory: missing.path().join("package"),
            systemd_run: "/usr/bin/systemd-run".into(),
            user_runtime_directory: missing.path().join("user-runtime"),
            artifact_digest: "a".repeat(64),
        };
        let pool = RuntimePool {
            runtime: Mutex::new(None),
            process: Arc::new(NoNativeWork),
            sandbox: Arc::new(NoNativeWork),
        };
        assert!(pool.acquire(config.clone()).await.is_err());
        let pinned = pool.runtime.lock().unwrap().as_ref().unwrap().1.clone();
        let mut different = config.clone();
        different.artifact_digest = "b".repeat(64);
        assert!(
            pool.acquire(different)
                .await
                .unwrap_err()
                .contains("same pinned runtime")
        );
        assert!(pool.acquire(config).await.is_err());
        assert!(Arc::ptr_eq(
            &pinned,
            &pool.runtime.lock().unwrap().as_ref().unwrap().1
        ));
    }
}
