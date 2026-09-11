//! Ordinary workbench feature plugins over bounded typed domain APIs.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use futures_util::future::BoxFuture;
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, LocalContract, MetaError, PluginFactory,
    PreparedActivation,
};
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, watch};
use tokio_util::task::TaskTracker;
mod navigation;
mod setup;
pub use navigation::{
    NavigationCommand, NavigationFeature, NavigationFeatureContract, NavigationFeatureFactory,
};
pub use setup::{SetupCommand, SetupFeature, SetupFeatureContract, SetupFeatureFactory};

type Result<T> = std::result::Result<T, String>;
#[derive(Debug)]
struct Work {
    execution: Execution,
    tasks: TaskTracker,
    slot: Arc<Semaphore>,
    admission: Mutex<bool>,
    changed: watch::Sender<u64>,
}
impl Work {
    fn new(execution: Execution) -> Self {
        Self {
            execution,
            tasks: TaskTracker::new(),
            slot: Arc::new(Semaphore::new(1)),
            admission: Mutex::new(true),
            changed: watch::channel(0).0,
        }
    }
    fn run<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = Result<T>> + Send + 'static,
    ) -> BoxFuture<'static, Result<T>> {
        let open = self.admission.lock().expect("workbench admission poisoned");
        if !*open {
            return Box::pin(async { Err("Workbench feature is closed".into()) });
        }
        let Ok(permit) = self.slot.clone().try_acquire_owned() else {
            return Box::pin(async { Err("This workbench operation is still running".into()) });
        };
        let changed = self.changed.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            let result = future.await;
            changed.send_modify(|revision| *revision = revision.wrapping_add(1));
            result
        }));
        drop(open);
        Box::pin(async move {
            task.await
                .map_err(|_| "Workbench operation outcome is unknown".to_owned())?
        })
    }
    async fn close(&self) {
        {
            let mut open = self.admission.lock().expect("workbench admission poisoned");
            *open = false;
            self.slot.close();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
fn prepare(config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !config.is_null() {
        return Err(MetaError::InvalidInput(
            "workbench feature configuration must be null".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null)
        .requiring_local::<rsi_api_protocol::ApiClientContract>()
        .requiring_local::<rsi_ui::UiContract>())
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
fn error(value: impl std::fmt::Display) -> String {
    let mut value = value.to_string();
    if value.len() > 4096 {
        let mut end = 4096;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}
fn contribute(
    plan: &ActivationPlan,
    name: &str,
    title: &str,
    renderer: Arc<dyn rsi_ui::SurfaceRenderer>,
) -> rsi_meta::Result<()> {
    let lease = plan
        .local::<rsi_ui::UiContract>()?
        .register(
            plan,
            rsi_ui::Contributions {
                name: name.into(),
                surfaces: vec![rsi_ui::SurfaceContribution {
                    name: "status".into(),
                    title: title.into(),
                    target: rsi_ui::TargetKind::Application,
                    renderer,
                }],
                actions: vec![],
                renderers: vec![],
            },
        )
        .map_err(meta)?;
    plan.defer(
        "withdraw workbench contribution",
        Box::new(move || {
            Box::pin(async move {
                if lease.dispose().await.is_clean() {
                    Ok(())
                } else {
                    Err("workbench contribution cleanup failed".into())
                }
            })
        }),
    )
}
/// Registers the independent workbench feature factories and ordered entries.
pub fn register(
    builder: &mut rsi_host::HostBuilder,
    entries: &mut Vec<rsi_host::ProfileEntry>,
) -> rsi_host::Result<()> {
    builder.register_local_contract::<SetupFeatureContract>()?;
    builder.register_local_contract::<NavigationFeatureContract>()?;
    for (name, factory) in [
        (
            "rsi.workbench.setup",
            Arc::new(SetupFeatureFactory) as Arc<dyn PluginFactory>,
        ),
        (
            "rsi.workbench.navigation",
            Arc::new(NavigationFeatureFactory),
        ),
    ] {
        builder.register_linked(
            name,
            env!("CARGO_PKG_VERSION"),
            rsi_meta::UpdateMode::RestartRequired,
            factory,
        )?;
        entries.push(rsi_host::ProfileEntry::new(name, name, ConfigValue::Null));
    }
    Ok(())
}
