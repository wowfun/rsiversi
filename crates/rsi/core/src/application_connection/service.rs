use super::ConnectionFactory;
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiDispatch, ApiError, ApiInvocation, AuthenticatedDevice, CallOrigin, DeviceAdministration,
    DeviceAuthentication, DeviceId, DeviceRecord, OperationId, OperationSpec, RegisteredDevice,
};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use rsi_serve::{ServingService, ServingServiceContract};
use std::sync::{Arc, Weak};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Product composition, without owning HTTP or process-signal policy.
#[derive(Debug)]
pub(super) struct ServiceFactory(pub Arc<ConnectionFactory>);

#[async_trait]
impl PluginFactory for ServiceFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        self.0.prepare(desired)
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (profile, composition) = self.0.compose(&mut plan).await?;
        let paths = rsi_service_host::ServiceHostPaths::from_host_paths(&self.0.paths)
            .map_err(|error| self.0.diagnosed(error))?;
        let owner = rsi_service_host::HostOwnerLease::try_acquire(paths)
            .map_err(|error| self.0.diagnosed(error))?;
        let daemon =
            crate::StandardServiceDaemon::start_in(composition, &profile, owner, plan.context())
                .await
                .map_err(|error| self.0.diagnosed(error))?;
        let running = daemon.running();
        running
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
        let context = plan.context();
        let service = Arc::new(Service {
            running: Arc::downgrade(&running),
            stopped,
        });
        let supplies = vec![
            context.provide_local::<rsi_api_protocol::ApiDispatchContract>(service.clone())?,
            context
                .provide_local::<rsi_api_protocol::DeviceAdministrationContract>(service.clone())?,
            context
                .provide_local::<rsi_api_protocol::DeviceAuthenticationContract>(service.clone())?,
            context
                .provide_local::<rsi_api_protocol::ConnectionDescriptionContract>(description)?,
            context.provide_local::<ServingServiceContract>(service)?,
        ];
        plan.defer(
            "withdraw application service",
            Box::new(move || {
                Box::pin(async move {
                    drop(supplies);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct Service {
    running: Weak<crate::RunningRsi>,
    stopped: watch::Receiver<Option<Result<(), String>>>,
}
impl Service {
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
        operation: &OperationId,
        origin: CallOrigin,
    ) -> rsi_api_protocol::Result<Box<dyn ApiInvocation>> {
        self.current::<rsi_api_protocol::ApiDispatchContract>()?
            .admit(operation, origin)
    }
    fn operations(&self) -> Vec<OperationSpec> {
        self.current::<rsi_api_protocol::ApiDispatchContract>()
            .map_or_else(|_| Vec::new(), |dispatch| dispatch.operations())
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
