//! Host-authorized directory selection with bounded actual filesystem work.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use async_trait::async_trait;
use rsi_api_protocol::ApiError;
use rsi_api_protocol::{ApiRegistrarContract, CallOrigin, json_handler};
use rsi_configuration_access::{ConfigurationAccess, ConfigurationAccessContract};
use rsi_directory_picker_api::{
    CreateRequest, Created, Failure, ListRequest, Listing, Operation, Status,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::{path::PathBuf, sync::Arc};
#[cfg(unix)]
use std::{sync::Mutex, time::Duration};
#[cfg(unix)]
use tokio::sync::Semaphore;
#[cfg(unix)]
use tokio_util::{sync::CancellationToken, task::TaskTracker};
#[cfg(unix)]
mod native;
#[cfg(unix)]
#[derive(Debug)]
struct Work {
    closed: Mutex<bool>,
    slots: Arc<Semaphore>,
    tasks: TaskTracker,
    stop: CancellationToken,
    deadline: Duration,
}
#[cfg(unix)]
struct CancelOnDrop(CancellationToken);
#[cfg(unix)]
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
#[cfg(unix)]
impl Work {
    fn new(deadline: Duration) -> Self {
        Self {
            closed: Mutex::new(false),
            slots: Arc::new(Semaphore::new(2)),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
            deadline,
        }
    }
    async fn run<
        T: Send + 'static,
        G: Send + 'static,
        F: FnOnce(CancellationToken) -> rsi_directory_picker_api::Result<T> + Send + 'static,
    >(
        &self,
        guard: G,
        mutation: bool,
        work: F,
    ) -> rsi_api_protocol::Result<rsi_directory_picker_api::Result<T>> {
        let (task, cancel) = {
            let closed = self.closed.lock().expect("directory admission poisoned");
            if *closed {
                return Err(ApiError::ShuttingDown);
            }
            let slot = self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| ApiError::Capacity)?;
            let token = self.tasks.token();
            let stop = self.stop.child_token();
            let cancel = CancelOnDrop(stop.clone());
            let task = tokio::task::spawn_blocking(move || {
                let (_guard, _slot, _token) = (guard, slot, token);
                if stop.is_cancelled() {
                    return Err(Failure::Cancelled);
                }
                work(stop)
            });
            (task, cancel)
        };
        let result = match tokio::time::timeout(self.deadline, task).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(if mutation {
                Failure::OutcomeUnknown
            } else {
                Failure::Io {
                    message: "filesystem worker failed".into(),
                }
            }),
            Err(_) => Err(if mutation {
                Failure::OutcomeUnknown
            } else {
                Failure::TimedOut
            }),
        };
        drop(cancel);
        Ok(result)
    }
    async fn close(&self) {
        {
            let mut closed = self.closed.lock().expect("directory admission poisoned");
            *closed = true;
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
fn access_allowed(
    admission: rsi_api_protocol::Result<rsi_configuration_access::ConfigurationLease>,
) -> rsi_api_protocol::Result<bool> {
    match admission {
        Ok(_) => Ok(true),
        Err(ApiError::Unauthorized) => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    #[test]
    fn status_preserves_capacity_and_shutdown_instead_of_reporting_denied() {
        assert!(!access_allowed(Err(ApiError::Unauthorized)).unwrap());
        assert!(matches!(
            access_allowed(Err(ApiError::Capacity)),
            Err(ApiError::Capacity)
        ));
        assert!(matches!(
            access_allowed(Err(ApiError::ShuttingDown)),
            Err(ApiError::ShuttingDown)
        ));
    }
}

#[derive(Debug)]
struct Owner {
    access: Arc<ConfigurationAccess>,
    #[cfg(unix)]
    home: Option<PathBuf>,
    #[cfg(unix)]
    work: Work,
}
impl Owner {
    async fn list(
        &self,
        origin: &CallOrigin,
        request: ListRequest,
    ) -> rsi_api_protocol::Result<rsi_directory_picker_api::Result<Listing>> {
        let grant = self.access.admit(origin)?;
        #[cfg(unix)]
        {
            let home = self.home.clone();
            self.work
                .run(grant, false, move |stop| native::list(request, home, stop))
                .await
        }
        #[cfg(not(unix))]
        {
            let _ = (grant, request);
            Ok(Err(Failure::Unsupported))
        }
    }
    async fn create(
        &self,
        origin: &CallOrigin,
        request: CreateRequest,
    ) -> rsi_api_protocol::Result<rsi_directory_picker_api::Result<Created>> {
        let grant = self.access.admit(origin)?;
        #[cfg(unix)]
        {
            self.work
                .run(grant, true, move |stop| native::create(request, stop))
                .await
        }
        #[cfg(not(unix))]
        {
            let _ = (grant, request);
            Ok(Err(Failure::Unsupported))
        }
    }
}
/// Ordinary Service factory; no ambient home is read during activation.
#[derive(Clone, Debug)]
pub struct DirectoryPickerFactory {
    home: Option<PathBuf>,
}
impl DirectoryPickerFactory {
    /// Captures composition's explicit default browse directory.
    pub fn new(home: Option<PathBuf>) -> Self {
        Self { home }
    }
}
#[async_trait]
impl PluginFactory for DirectoryPickerFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let home = serde_json::to_value(&self.home)
            .map_err(|_| MetaError::InvalidInput("Host home is not UTF-8".into()))?;
        if config != &serde_json::json!({"home":home}) {
            return Err(MetaError::InvalidInput(
                "Directory picker config must match the captured Host home".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<ConfigurationAccessContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let owner = Arc::new(Owner {
            access: plan.local::<ConfigurationAccessContract>()?,
            #[cfg(unix)]
            home: self.home.clone(),
            #[cfg(unix)]
            work: Work::new(Duration::from_secs(5)),
        });
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let status_owner = owner.clone();
        let status = registrar
            .register(
                Operation::Status.spec(),
                json_handler(move |context, (): ()| {
                    let owner = status_owner.clone();
                    async move {
                        Ok(Ok::<_, Failure>(Status {
                            supported: cfg!(unix),
                            allowed: access_allowed(owner.access.admit(&context.origin))?,
                        }))
                    }
                }),
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let list_owner = owner.clone();
        let list = registrar
            .register(
                Operation::List.spec(),
                json_handler(move |context, request: ListRequest| {
                    let owner = list_owner.clone();
                    async move { owner.list(&context.origin, request).await }
                }),
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let create_owner = owner.clone();
        let create = registrar
            .register(
                Operation::Create.spec(),
                json_handler(move |context, request: CreateRequest| {
                    let owner = create_owner.clone();
                    async move { owner.create(&context.origin, request).await }
                }),
            )
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "drain actual directory work",
            Box::new(move || {
                Box::pin(async move {
                    drop((status, list, create));
                    #[cfg(unix)]
                    owner.work.close().await;
                    drop(owner);
                    Ok(())
                })
            }),
        )
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn deadlines_keep_actual_slots_and_guards_until_uninterruptible_work_finishes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let work = Arc::new(Work::new(Duration::from_millis(20)));
        let released = Arc::new(AtomicUsize::new(0));
        let mut unblockers = Vec::new();
        for mutation in [false, true] {
            let (send, receive) = std::sync::mpsc::channel::<()>();
            unblockers.push(send);
            let result = work
                .run(Guard(released.clone()), mutation, move |_| {
                    let _ = receive.recv();
                    Ok(())
                })
                .await
                .unwrap();
            assert_eq!(
                result,
                Err(if mutation {
                    Failure::OutcomeUnknown
                } else {
                    Failure::TimedOut
                })
            );
        }
        assert_eq!(released.load(Ordering::SeqCst), 0);
        assert_eq!(work.slots.available_permits(), 0);
        assert!(matches!(
            work.run((), false, |_| Ok(())).await,
            Err(ApiError::Capacity)
        ));
        let closing = work.clone();
        let close = tokio::spawn(async move { closing.close().await });
        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        drop(unblockers);
        close.await.unwrap();
        assert_eq!(released.load(Ordering::SeqCst), 2);
        assert_eq!(work.slots.available_permits(), 2);
    }
}
