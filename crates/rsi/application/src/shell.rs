use crate::{ApplicationError, Result, ScopedProfile};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_host::{Host, ProfileControl, ProfileProgram, ReloadOutcome};
use rsi_meta::{
    ActivationPlan, CleanupReport, ConfigValue, Context, LocalContract, MetaError, PluginFactory,
    PreparedActivation,
};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// One surface's capabilities and explicit disposal request, without Runtime ownership.
#[derive(Debug)]
pub struct Surface {
    context: Context,
    control: Arc<dyn ProfileControl>,
    closing: CancellationToken,
    closed: Option<oneshot::Receiver<CleanupReport>>,
}
impl Surface {
    /// Looks up a surface-local capability using its fixed Context mappings.
    pub fn lookup_local<C: LocalContract>(&self) -> Option<Arc<C::Service>> {
        self.context.lookup_local::<C>()
    }
    /// Reloads this surface's existing Profile.
    pub async fn reload(&self) -> rsi_host::Result<ReloadOutcome> {
        self.control.reload().await.map_err(Into::into)
    }
    /// Requests disposal and waits for the Shell-owned cleanup to finish.
    pub async fn close(mut self) -> Result<CleanupReport> {
        self.closing.cancel();
        self.closed
            .take()
            .ok_or(ApplicationError::TaskStopped)?
            .await
            .map_err(|_| ApplicationError::TaskStopped)
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        self.closing.cancel();
    }
}

/// Session-free bounded owner of ordinary surface Profiles in one Meta subtree.
#[derive(Debug)]
pub struct Shell {
    catalog: Arc<Host>,
    parent: Context,
    slots: Arc<Semaphore>,
    stop: CancellationToken,
    tasks: TaskTracker,
    admission: Mutex<()>,
}
impl Shell {
    /// Admits an owned open; dropped waiters cannot leave an unowned active surface.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned Shell admission.
    pub fn open(self: &Arc<Self>, program: ProfileProgram) -> BoxFuture<'static, Result<Surface>> {
        let admission = self.admission.lock().expect("Shell admission poisoned");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err(ApplicationError::ShuttingDown) });
        }
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            return Box::pin(async { Err(ApplicationError::Capacity) });
        };
        let (sender, receiver) = oneshot::channel();
        let shell = self.clone();
        let work = self.tasks.track_future(async move {
            shell.drive(program, permit, sender).await;
        });
        drop(admission);
        drop(self.parent.runtime().execution().spawn(work));
        Box::pin(async move { receiver.await.map_err(|_| ApplicationError::TaskStopped)? })
    }

    async fn drive(
        &self,
        program: ProfileProgram,
        permit: OwnedSemaphorePermit,
        sender: oneshot::Sender<Result<Surface>>,
    ) {
        let profile = match ScopedProfile::start(&self.catalog, &self.parent, program).await {
            Ok(profile) => profile,
            Err(error) => {
                drop(permit);
                let _ = sender.send(Err(error));
                return;
            }
        };
        if self.stop.is_cancelled() {
            let _ = profile.shutdown().await;
            drop(permit);
            let _ = sender.send(Err(ApplicationError::ShuttingDown));
            return;
        }
        let closing = CancellationToken::new();
        let (completed, closed) = oneshot::channel();
        let surface = Surface {
            context: profile.context(),
            control: profile.control(),
            closing: closing.clone(),
            closed: Some(closed),
        };
        // A failed send drops Surface and requests its own disposal.
        let _ = sender.send(Ok(surface));
        tokio::select! { biased;
            () = self.stop.cancelled() => {},
            () = closing.cancelled() => {},
        }
        let report = profile.shutdown().await;
        drop(permit);
        let _ = completed.send(report);
    }

    fn retire(&self) {
        let _admission = self.admission.lock().expect("Shell admission poisoned");
        self.stop.cancel();
        self.slots.close();
        self.tasks.close();
    }
    async fn close(&self) {
        self.retire();
        self.tasks.wait().await;
    }
}

/// Nominal Local surface-host capability, independent of every business domain.
#[derive(Debug)]
pub struct ShellContract;
impl LocalContract for ShellContract {
    const KEY: &'static str = "rsi.application.shell";
    type Service = Shell;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    #[serde(default = "default_surfaces")]
    maximum_surfaces: usize,
}
const fn default_surfaces() -> usize {
    8
}

/// Ordinary application shell with an explicit frozen catalog for its surfaces.
#[derive(Debug)]
pub struct ShellFactory {
    catalog: Arc<Host>,
}
impl ShellFactory {
    /// Supplies only surface factories and the Local markers they must isolate.
    pub fn new(catalog: Host) -> Self {
        Self {
            catalog: Arc::new(catalog),
        }
    }
}
#[async_trait]
impl PluginFactory for ShellFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Configuration = if desired.is_null() {
            Configuration {
                maximum_surfaces: default_surfaces(),
            }
        } else {
            serde_json::from_value(desired.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?
        };
        if !(1..=16).contains(&config.maximum_surfaces) {
            return Err(MetaError::InvalidInput(
                "Shell maximum_surfaces must be within 1..=16".into(),
            ));
        }
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            std::mem::size_of::<Configuration>(),
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Configuration>()?;
        let shell = Arc::new(Shell {
            catalog: self.catalog.clone(),
            parent: plan.context().clone(),
            slots: Arc::new(Semaphore::new(config.maximum_surfaces)),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            admission: Mutex::new(()),
        });
        let cleanup = shell.clone();
        plan.defer(
            "drain Shell surfaces",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<ShellContract>(shell.clone())?;
        plan.defer(
            "withdraw Shell",
            Box::new(move || {
                Box::pin(async move {
                    shell.retire();
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
