use super::{
    NativeAddonInspection, NativeAddonManager, NativeAddonRefresh, NativeAddonUpdateError, Result,
};
use crate::{NativeAddonStore, StandardAddonSet};
use async_trait::async_trait;
use rsi_agent_composition::AgentCompositionSourceContract;
use rsi_agent_presets::AgentPresetCatalog;
use rsi_host::HostPaths;
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, LocalContract, MetaError, PluginFactory,
    PreparedActivation, Task,
};
use rsi_meta_native_loader::{CatalogOptions, NativeCatalog};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Maximum pending explicit refresh requests behind the single staging worker.
pub const MAXIMUM_NATIVE_ADDON_REFRESH_REQUESTS: usize = 16;

/// Local native management operations owned by the ordinary manager Fiber.
#[async_trait]
pub trait NativeAddonControl: std::fmt::Debug + Send + Sync + 'static {
    /// Bounded selection and actual Loader resource observations.
    fn inspect(&self) -> NativeAddonInspection;
    /// Explicitly retries the current selection through the owned staging worker.
    async fn refresh(&self) -> Result<NativeAddonRefresh>;
}
/// Ordinary Local management supply; it grants no remote operation authority.
#[derive(Debug)]
pub struct NativeAddonControlContract;
impl LocalContract for NativeAddonControlContract {
    const KEY: &'static str = "rsi.native-addons.control";
    type Service = dyn NativeAddonControl;
}

#[derive(Clone)]
pub(crate) struct NativeAddonFactory {
    pub(crate) paths: HostPaths,
    pub(crate) linux_tools: bool,
    pub(crate) presets: AgentPresetCatalog,
    pub(crate) base: StandardAddonSet,
}
impl std::fmt::Debug for NativeAddonFactory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeAddonFactory")
            .finish_non_exhaustive()
    }
}
impl NativeAddonFactory {
    fn open(self) -> rsi_meta::Result<Arc<NativeAddonManager>> {
        let roots = |path: std::path::PathBuf| {
            rsi_files_native_fs::resolve_absolute_root_alias(&path, true).map_err(activation_error)
        };
        let store = NativeAddonStore::open(roots(self.paths.config().join("native-addons"))?)
            .map_err(activation_error)?;
        let loader = NativeCatalog::new(CatalogOptions::new(roots(
            self.paths.cache().join("native-addons"),
        )?))
        .map_err(activation_error)?;
        NativeAddonManager::new(
            Arc::new(store),
            loader,
            self.paths,
            self.linux_tools,
            self.presets,
            self.base,
        )
        .map(Arc::new)
        .map_err(activation_error)
    }
}
#[async_trait]
impl PluginFactory for NativeAddonFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "native addon manager configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<rsi_service_host::ServiceOwnerContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let _owner_lease = plan.local::<rsi_service_host::ServiceOwnerContract>()?;
        let owner = Arc::new(Worker {
            stop: CancellationToken::new(),
            task: Mutex::new(None),
            manager: Mutex::new(None),
        });
        let cleanup = Arc::clone(&owner);
        // The activation's root EffectTxn keeps its setup window open until
        // this function returns or is cancelled. Reserve undo before spawning;
        // install the task synchronously before the first await closes setup.
        plan.defer(
            "stop native addon staging worker",
            Box::new(move || {
                Box::pin(async move { cleanup.shutdown().await.map_err(|error| error.to_string()) })
            }),
        )?;
        let execution = plan.context().runtime().execution().clone();
        let (ready, receive) = oneshot::channel();
        let (requests, queue) = mpsc::channel(MAXIMUM_NATIVE_ADDON_REFRESH_REQUESTS);
        let task = execution.spawn(run(
            self.clone(),
            execution.clone(),
            Arc::clone(&owner),
            queue,
            ready,
        ));
        *owner
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        let manager = receive.await.map_err(|_| {
            MetaError::Activation("native addon worker did not initialize".into())
        })??;
        plan.context()
            .provide_local::<AgentCompositionSourceContract>(manager.clone())?;
        plan.context()
            .provide_local::<NativeAddonControlContract>(Arc::new(Control {
                manager,
                requests,
                stop: owner.stop.clone(),
            }))?;
        Ok(())
    }
}

struct Worker {
    stop: CancellationToken,
    task: Mutex<Option<Task<rsi_meta::Result<()>>>>,
    manager: Mutex<Option<Arc<NativeAddonManager>>>,
}
impl Worker {
    async fn shutdown(&self) -> rsi_meta::Result<()> {
        self.stop.cancel();
        if let Some(manager) = self
            .manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            manager.close();
        }
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let result = match task {
            Some(task) => task
                .await
                .map_err(|_| MetaError::Activation("native addon worker ended unexpectedly".into()))
                .and_then(|result| result),
            None => Ok(()),
        };
        self.manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        result
    }
}
struct RefreshRequest(oneshot::Sender<Result<NativeAddonRefresh>>);
struct CloseOnDrop(Arc<NativeAddonManager>);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}
#[derive(Debug)]
struct Control {
    manager: Arc<NativeAddonManager>,
    requests: mpsc::Sender<RefreshRequest>,
    stop: CancellationToken,
}
#[async_trait]
impl NativeAddonControl for Control {
    fn inspect(&self) -> NativeAddonInspection {
        self.manager.inspect()
    }
    async fn refresh(&self) -> Result<NativeAddonRefresh> {
        if self.stop.is_cancelled() {
            return Err(NativeAddonUpdateError::Closed);
        }
        let (reply, result) = oneshot::channel();
        self.requests
            .try_send(RefreshRequest(reply))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => NativeAddonUpdateError::Busy,
                mpsc::error::TrySendError::Closed(_) => NativeAddonUpdateError::Closed,
            })?;
        tokio::select! {
            biased;
            () = self.stop.cancelled() => Err(NativeAddonUpdateError::Closed),
            result = result => result.unwrap_or(Err(NativeAddonUpdateError::Closed)),
        }
    }
}

async fn run(
    factory: NativeAddonFactory,
    execution: Execution,
    owner: Arc<Worker>,
    mut requests: mpsc::Receiver<RefreshRequest>,
    ready: oneshot::Sender<rsi_meta::Result<Arc<NativeAddonManager>>>,
) -> rsi_meta::Result<()> {
    let initialized = execution
        .prepare(move || factory.open())
        .await
        .map_err(|_| {
            MetaError::Activation("native addon initialization ended unexpectedly".into())
        })?;
    let manager = match initialized {
        Ok(manager) => manager,
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
    *owner
        .manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(manager.clone());
    let _admission = CloseOnDrop(manager.clone());
    if owner.stop.is_cancelled() {
        manager.close();
        return Ok(());
    }
    let _initial_attempt = refresh(&execution, &manager, false).await?;
    if ready.send(Ok(manager.clone())).is_err() {
        manager.close();
        return Ok(());
    }
    let result = async {
        loop {
            tokio::select! {
                biased;
                () = owner.stop.cancelled() => return Ok(()),
                request = requests.recv() => {
                    let Some(RefreshRequest(reply)) = request else { return Ok(()); };
                    if !reply.is_closed() {
                        let result = refresh(&execution, &manager, true).await?;
                        let result = result.and_then(|receipt| receipt.ok_or(NativeAddonUpdateError::Selection("explicit refresh did not run")));
                        let _ = reply.send(result);
                    }
                }
                () = execution.sleep(Duration::from_secs(1)) => { let _attempt = refresh(&execution, &manager, false).await?; }
            }
        }
    }.await;
    manager.close();
    result
}

async fn refresh(
    execution: &Execution,
    manager: &Arc<NativeAddonManager>,
    explicit: bool,
) -> rsi_meta::Result<Result<Option<NativeAddonRefresh>>> {
    let manager = Arc::clone(manager);
    execution
        .prepare(move || {
            if explicit {
                manager.refresh().map(Some)
            } else {
                manager.refresh_changed()
            }
        })
        .await
        .map_err(|_| MetaError::Activation("native addon staging worker ended unexpectedly".into()))
}
fn activation_error(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
