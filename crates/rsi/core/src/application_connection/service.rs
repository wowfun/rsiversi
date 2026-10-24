use super::ConnectionFactory;
use crate::application_services::{ServingService, ServingServiceContract};
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiDispatch, ApiError, ApiInvocation, AuthenticatedDevice, CallOrigin, DeviceAdministration,
    DeviceAuthentication, DeviceId, DeviceRecord, OperationCall, OperationSpec, RegisteredDevice,
};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::watch;
#[cfg(target_os = "linux")]
use tokio_util::sync::CancellationToken;

/// Product composition, without owning HTTP or process-signal policy.
#[derive(Debug)]
pub(super) struct ServiceFactory(pub Arc<ConnectionFactory>, pub bool);

#[async_trait]
impl PluginFactory for ServiceFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        self.0.prepare(desired)
    }
    #[allow(clippy::too_many_lines)] // Keep capability publication and ordered owner teardown together.
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (profile, composition) = self.0.compose(&mut plan).await?;
        if self.1 && composition.host_launcher.is_some() {
            return crate::local_acp::attach(plan, &composition)
                .await
                .map_err(|error| self.0.diagnosed(error));
        }
        if let Some(launcher) = &composition.host_launcher {
            launcher
                .ensure_started(composition.paths())
                .await
                .map_err(|e| self.0.diagnosed(e))?;
            let owner = rsi_service_host::ServiceHostPaths::from_host_paths(composition.paths())
                .and_then(|paths| paths.read_metadata())
                .map_err(|e| self.0.diagnosed(e))?
                .ok_or_else(|| self.0.diagnosed("resident Service is unavailable"))?;
            let supply = plan
                .context()
                .provide_local::<crate::application_services::RemoteServiceContract>(Arc::new(
                    owner,
                ))?;
            return plan.defer(
                "withdraw remote Service identity",
                Box::new(move || {
                    Box::pin(async move {
                        drop(supply);
                        Ok(())
                    })
                }),
            );
        }
        let profile =
            profile.ok_or_else(|| self.0.diagnosed("embedded service needs a Profile"))?;
        let paths = rsi_service_host::ServiceHostPaths::from_host_paths(self.0.composition.paths())
            .map_err(|error| self.0.diagnosed(error))?;
        let lease = rsi_service_host::HostOwnerLease::try_acquire(paths)
            .map_err(|error| self.0.diagnosed(error))?;
        #[cfg(target_os = "linux")]
        let (running, stopped) = {
            let daemon = crate::StandardServiceDaemon::start_in(
                composition,
                &profile,
                lease,
                plan.context(),
            )
            .await
            .map_err(|error| self.0.diagnosed(error))?;
            let running = daemon.running();
            let stop = CancellationToken::new();
            let guard = stop.clone().drop_guard();
            let (sender, stopped) = watch::channel(None);
            let task = plan.context().runtime().execution().spawn(async move {
                let result = daemon.run(stop).await.map_err(|error| error.to_string());
                sender.send_replace(Some(result.clone()));
                result
            });
            plan.defer(
                "drain application service",
                Box::new(move || {
                    Box::pin(async move {
                        drop(guard);
                        task.await.map_err(|error| error.to_string())?
                    })
                }),
            )?;
            (running, stopped)
        };
        #[cfg(windows)]
        let (running, stopped) = {
            let epoch =
                rsi_api_protocol::HostEpoch::generate().map_err(|error| self.0.diagnosed(error))?;
            let composition = composition
                .with_service_owner(Arc::new(lease), epoch)
                .map_err(|error| self.0.diagnosed(error))?;
            let running = Arc::new(
                crate::RunningRsi::boot_host_profile_in(composition, &profile, plan.context())
                    .await
                    .map_err(|error| self.0.diagnosed(error))?,
            );
            let (sender, stopped) = watch::channel(None);
            let owned = running.clone();
            plan.defer(
                "drain embedded application Service",
                Box::new(move || {
                    Box::pin(async move {
                        let result = if owned.shutdown().await.is_clean() {
                            Ok(())
                        } else {
                            Err("embedded application Service cleanup failed".to_owned())
                        };
                        sender.send_replace(Some(result.clone()));
                        result
                    })
                }),
            )?;
            (running, stopped)
        };
        let dispatch = running
            .api_dispatch()
            .map_err(|error| self.0.diagnosed(error))?;
        running
            .device_authentication()
            .map_err(|error| self.0.diagnosed(error))?;
        running
            .device_administration()
            .map_err(|error| self.0.diagnosed(error))?;
        let description = running
            .connection_description()
            .map_err(|error| self.0.diagnosed(error))?;
        self.0
            .composition
            .addons()
            .publish_domains(&mut plan, crate::addon::DomainLookup::Service(&running))?;
        let context = plan.context();
        let service = Arc::new(Service {
            inputs: [
                rsi_api_protocol::OperationClass::Control,
                rsi_api_protocol::OperationClass::Data,
                rsi_api_protocol::OperationClass::Subscription,
            ]
            .map(|class| dispatch.input_budget(class)),
            running: Arc::downgrade(&running),
            stopped,
            execution: context.runtime().execution().clone(),
            launches: tokio_util::task::TaskTracker::new(),
            launch_closed: Mutex::new(false),
            launch_device: Arc::default(),
        });
        let supplies = vec![
            context.provide_local::<rsi_api_protocol::ApiDispatchContract>(service.clone())?,
            context
                .provide_local::<rsi_api_protocol::DeviceAdministrationContract>(service.clone())?,
            context
                .provide_local::<rsi_api_protocol::DeviceAuthenticationContract>(service.clone())?,
            context
                .provide_local::<rsi_api_protocol::ConnectionDescriptionContract>(description)?,
            context
                .provide_local::<crate::application_services::LocalBrowserAdministrationContract>(
                    service.clone(),
                )?,
            context.provide_local::<ServingServiceContract>(service.clone())?,
        ];
        if self.1 {
            let backend = crate::acp_owner::backend(running.as_ref())
                .map_err(|error| self.0.diagnosed(error))?;
            let supply = context
                .provide_local::<crate::application_services::AcpAttachmentContract>(Arc::new(
                    crate::application_services::AcpAttachment::Backend(backend.clone()),
                ))?;
            plan.defer(
                "close native ACP backend",
                Box::new(move || {
                    Box::pin(async move {
                        let result = backend
                            .shutdown()
                            .await
                            .map_err(|_| "native ACP cleanup failed".to_owned());
                        drop(supply);
                        result
                    })
                }),
            )?;
        }
        plan.defer(
            "withdraw application service",
            Box::new(move || {
                Box::pin(async move {
                    drop(supplies);
                    service.close_launches().await;
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct Service {
    inputs: [rsi_api_protocol::ByteBudget; 3],
    running: Weak<crate::RunningRsi>,
    stopped: watch::Receiver<Option<Result<(), String>>>,
    execution: rsi_meta::Execution,
    launches: tokio_util::task::TaskTracker,
    launch_closed: Mutex<bool>,
    launch_device: Arc<tokio::sync::OnceCell<RegisteredDevice>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dispatch_lookup_failure_preserves_input_pool_and_is_not_an_empty_catalog() {
        let pool = rsi_api_protocol::ByteBudget::new(1024).unwrap();
        let service = Service {
            inputs: std::array::from_fn(|_| pool.clone()),
            running: Weak::new(),
            stopped: watch::channel(None).1,
            execution: rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            launches: tokio_util::task::TaskTracker::new(),
            launch_closed: Mutex::new(false),
            launch_device: Arc::default(),
        };
        assert!(matches!(service.operations(), Err(ApiError::Unavailable)));
        let retained = service
            .input_budget(rsi_api_protocol::OperationClass::Data)
            .copy(b"{}")
            .unwrap();
        assert_eq!(pool.used(), 2);
        drop(retained);
        assert_eq!(pool.used(), 0);
    }
}
impl Service {
    async fn close_launches(&self) {
        {
            let mut closed = self.launch_closed.lock().expect("browser launch poisoned");
            *closed = true;
            self.launches.close();
        }
        self.launches.wait().await;
    }
    fn current<C: rsi_meta::LocalContract>(&self) -> rsi_api_protocol::Result<Arc<C::Service>> {
        self.running
            .upgrade()
            .and_then(|running| running.host.lookup_local::<C>())
            .ok_or(ApiError::Unavailable)
    }
}
impl ApiDispatch for Service {
    fn admit(
        &self,
        operation: &OperationCall,
        origin: CallOrigin,
    ) -> rsi_api_protocol::Result<Box<dyn ApiInvocation>> {
        self.current::<rsi_api_protocol::ApiDispatchContract>()?
            .admit(operation, origin)
    }
    fn operations(&self) -> rsi_api_protocol::Result<Vec<OperationSpec>> {
        self.current::<rsi_api_protocol::ApiDispatchContract>()?
            .operations()
    }
    fn operation(
        &self,
        key: &rsi_api_protocol::OperationKey,
        origin: &CallOrigin,
    ) -> rsi_api_protocol::Result<OperationSpec> {
        self.current::<rsi_api_protocol::ApiDispatchContract>()?
            .operation(key, origin)
    }
    fn input_budget(
        &self,
        class: rsi_api_protocol::OperationClass,
    ) -> rsi_api_protocol::ByteBudget {
        self.inputs[match class {
            rsi_api_protocol::OperationClass::Control => 0,
            rsi_api_protocol::OperationClass::Data => 1,
            rsi_api_protocol::OperationClass::Subscription => 2,
        }]
        .clone()
    }
}
impl DeviceAuthentication for Service {
    fn authenticate(&self, token: &SecretValue) -> rsi_api_protocol::Result<AuthenticatedDevice> {
        self.current::<rsi_api_protocol::DeviceAuthenticationContract>()?
            .authenticate(token)
    }
}
#[async_trait]
impl DeviceAdministration for Service {
    async fn register(&self, label: &str) -> rsi_api_protocol::Result<RegisteredDevice> {
        self.current::<rsi_api_protocol::DeviceAdministrationContract>()?
            .register(label)
            .await
    }
    async fn rotate_managed(
        &self,
        slot: &str,
        label: &str,
    ) -> rsi_api_protocol::Result<RegisteredDevice> {
        self.current::<rsi_api_protocol::DeviceAdministrationContract>()?
            .rotate_managed(slot, label)
            .await
    }
    fn managed_device(&self, slot: &str) -> rsi_api_protocol::Result<Option<DeviceRecord>> {
        self.current::<rsi_api_protocol::DeviceAdministrationContract>()?
            .managed_device(slot)
    }
    async fn rotate_managed_if(
        &self,
        slot: &str,
        label: &str,
        expected: Option<&DeviceId>,
    ) -> rsi_api_protocol::Result<RegisteredDevice> {
        self.current::<rsi_api_protocol::DeviceAdministrationContract>()?
            .rotate_managed_if(slot, label, expected)
            .await
    }
    async fn revoke_credential(&self, device: &RegisteredDevice) -> rsi_api_protocol::Result<bool> {
        self.current::<rsi_api_protocol::DeviceAdministrationContract>()?
            .revoke_credential(device)
            .await
    }
    async fn revoke(&self, id: &DeviceId) -> rsi_api_protocol::Result<bool> {
        self.current::<rsi_api_protocol::DeviceAdministrationContract>()?
            .revoke(id)
            .await
    }
    fn list(&self) -> rsi_api_protocol::Result<Vec<DeviceRecord>> {
        self.current::<rsi_api_protocol::DeviceAdministrationContract>()?
            .list()
    }
}
#[async_trait]
impl ServingService for Service {
    async fn stopped(&self) -> Result<(), String> {
        let mut receiver = self.stopped.clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| "service stopped without a result".to_owned())?;
        }
    }
    async fn reload(&self) -> Result<(), String> {
        self.running
            .upgrade()
            .ok_or("service has stopped")?
            .reload()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[async_trait]
impl crate::application_services::LocalBrowserAdministration for Service {
    async fn launch(&self) -> rsi_api_protocol::Result<RegisteredDevice> {
        let running = self.running.upgrade().ok_or(ApiError::ShuttingDown)?;
        let administration = running
            .lookup_addon::<rsi_api_protocol::DeviceAdministrationContract>()
            .ok_or(ApiError::Unavailable)?;
        let configuration = running
            .lookup_addon::<rsi_configuration_access::ConfigurationAccessContract>()
            .ok_or(ApiError::Unavailable)?;
        let task = {
            let closed = self.launch_closed.lock().expect("browser launch poisoned");
            if *closed {
                return Err(ApiError::ShuttingDown);
            }
            let device = self.launch_device.clone();
            self.execution.spawn(self.launches.track_future(async move {
                launch_once(&device, async move {
                    let _running = running;
                    let snapshot = configuration.snapshot().await?;
                    authorize_browser(administration.as_ref(), move |id| async move {
                        configuration
                            .set_grant(&CallOrigin::Local, id, &snapshot.revision, true)?
                            .await
                            .map(|_| ())
                    })
                    .await
                })
                .await
            }))
        };
        task.await.map_err(|_| ApiError::OutcomeUnknown)?
    }
}

async fn launch_once(
    cell: &tokio::sync::OnceCell<RegisteredDevice>,
    create: impl std::future::Future<Output = rsi_api_protocol::Result<RegisteredDevice>>,
) -> rsi_api_protocol::Result<RegisteredDevice> {
    let device = cell.get_or_try_init(|| create).await?;
    Ok(RegisteredDevice {
        record: device.record.clone(),
        token: device.token.clone(),
    })
}

async fn authorize_browser<F: std::future::Future<Output = rsi_api_protocol::Result<()>>>(
    administration: &dyn DeviceAdministration,
    grant: impl FnOnce(rsi_api_protocol::DeviceId) -> F,
) -> rsi_api_protocol::Result<RegisteredDevice> {
    if let Some(previous) = administration.managed_device("local-web")? {
        grant(previous.id.clone()).await?;
        return administration
            .rotate_managed_if("local-web", "Local Web browser", Some(&previous.id))
            .await;
    }
    let device = administration
        .rotate_managed_if("local-web", "Local Web browser", None)
        .await?;
    if let Err(error) = grant(device.record.id.clone()).await {
        if administration.revoke_credential(&device).await.is_err() {
            return Err(ApiError::Backend("local browser grant failed; credential cleanup also failed; inspect local device administration before retrying".into()));
        }
        return Err(error);
    }
    Ok(device)
}

#[derive(Debug)]
pub(crate) struct BrowserAuthorityFactory;
#[derive(Debug)]
struct BrowserAuthority {
    device: tokio::sync::OnceCell<RegisteredDevice>,
    administration: Arc<dyn DeviceAdministration>,
    configuration: Arc<rsi_configuration_access::ConfigurationAccess>,
}
#[async_trait]
impl crate::application_services::LocalBrowserAdministration for BrowserAuthority {
    async fn launch(&self) -> rsi_api_protocol::Result<RegisteredDevice> {
        launch_once(&self.device, async {
            let snapshot = self.configuration.snapshot().await?;
            authorize_browser(self.administration.as_ref(), |id| async move {
                self.configuration
                    .set_grant(&CallOrigin::Local, id, &snapshot.revision, true)?
                    .await
                    .map(|_| ())
            })
            .await
        })
        .await
    }
}
#[async_trait]
impl PluginFactory for BrowserAuthorityFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(rsi_meta::MetaError::InvalidInput(
                "browser authority takes no configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<rsi_api_protocol::DeviceAdministrationContract>()
            .requiring_local::<rsi_configuration_access::ConfigurationAccessContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<crate::application_services::LocalBrowserAdministrationContract>(
            Arc::new(BrowserAuthority {
                device: tokio::sync::OnceCell::new(),
                administration: plan.local::<rsi_api_protocol::DeviceAdministrationContract>()?,
                configuration: plan
                    .local::<rsi_configuration_access::ConfigurationAccessContract>()?,
            }),
        )?;
        plan.defer(
            "withdraw browser launch authority",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn overlapping_launches_share_one_authorized_credential_and_failed_attempts_retry() {
        let cell = tokio::sync::OnceCell::new();
        assert!(
            launch_once(&cell, async { Err(ApiError::Capacity) })
                .await
                .is_err()
        );
        let creates = AtomicUsize::new(0);
        let create = || async {
            creates.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            authorize_browser(&Administration::default(), |_| async { Ok(()) }).await
        };
        let (one, two) = tokio::join!(launch_once(&cell, create()), launch_once(&cell, create()));
        let one = one.unwrap();
        let two = two.unwrap();
        assert_eq!(one.record.id, two.record.id);
        assert_eq!(one.token.expose_secret(), two.token.expose_secret());
        launch_once(&cell, async {
            panic!("completed launch must not rotate again")
        })
        .await
        .unwrap();
        assert_eq!(creates.load(Ordering::SeqCst), 1);
    }

    #[derive(Debug, Default)]
    struct Administration {
        cleanup_fails: bool,
        cleanups: AtomicUsize,
        rotations: AtomicUsize,
        existing: bool,
    }
    #[async_trait]
    impl DeviceAdministration for Administration {
        async fn register(&self, _: &str) -> rsi_api_protocol::Result<RegisteredDevice> {
            panic!("launch must use the managed slot")
        }
        fn managed_device(&self, _: &str) -> rsi_api_protocol::Result<Option<DeviceRecord>> {
            Ok(self.existing.then(|| DeviceRecord {
                id: DeviceId::from_bytes([1; 16]),
                label: "old".into(),
            }))
        }
        async fn rotate_managed_if(
            &self,
            slot: &str,
            label: &str,
            expected: Option<&DeviceId>,
        ) -> rsi_api_protocol::Result<RegisteredDevice> {
            assert_eq!(expected.is_some(), self.existing);
            self.rotations.fetch_add(1, Ordering::SeqCst);
            self.rotate_managed(slot, label).await
        }
        async fn rotate_managed(
            &self,
            slot: &str,
            _: &str,
        ) -> rsi_api_protocol::Result<RegisteredDevice> {
            assert_eq!(slot, "local-web");
            Ok(RegisteredDevice {
                record: DeviceRecord {
                    id: DeviceId::from_bytes([1; 16]),
                    label: "fixture".into(),
                },
                token: SecretValue::new("a".repeat(64)).unwrap(),
            })
        }
        async fn revoke_credential(
            &self,
            device: &RegisteredDevice,
        ) -> rsi_api_protocol::Result<bool> {
            assert_eq!(device.record.id, DeviceId::from_bytes([1; 16]));
            self.cleanups.fetch_add(1, Ordering::SeqCst);
            if self.cleanup_fails {
                Err(ApiError::Unavailable)
            } else {
                Ok(true)
            }
        }
        async fn revoke(&self, _: &DeviceId) -> rsi_api_protocol::Result<bool> {
            panic!("cleanup must not revoke a newer credential by stable device ID")
        }
        fn list(&self) -> rsi_api_protocol::Result<Vec<DeviceRecord>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn failed_grant_revokes_exact_credential_and_reports_cleanup_failure() {
        let administration = Administration::default();
        let result =
            authorize_browser(&administration, |_| async { Err(ApiError::Unavailable) }).await;
        assert!(matches!(result, Err(ApiError::Unavailable)));
        assert_eq!(administration.cleanups.load(Ordering::SeqCst), 1);
        let administration = Administration {
            cleanup_fails: true,
            ..Administration::default()
        };
        let result =
            authorize_browser(&administration, |_| async { Err(ApiError::Unavailable) }).await;
        assert!(
            matches!(result, Err(ApiError::Backend(message)) if message.contains("credential cleanup also failed"))
        );
        assert_eq!(administration.cleanups.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn successful_grant_publishes_credential_without_cleanup() {
        let administration = Administration::default();
        let result = authorize_browser(&administration, |id| async move {
            assert_eq!(id, DeviceId::from_bytes([1; 16]));
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(result.record.id, DeviceId::from_bytes([1; 16]));
        assert_eq!(administration.cleanups.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn rejected_grants_preserve_existing_credentials_across_retries() {
        let administration = Administration {
            existing: true,
            ..Administration::default()
        };
        for _ in 0..3 {
            assert!(
                authorize_browser(&administration, |_| async { Err(ApiError::Unavailable) })
                    .await
                    .is_err()
            );
        }
        assert_eq!(administration.rotations.load(Ordering::SeqCst), 0);
        assert_eq!(administration.cleanups.load(Ordering::SeqCst), 0);
        authorize_browser(&administration, |_| async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(administration.rotations.load(Ordering::SeqCst), 1);
    }
}
