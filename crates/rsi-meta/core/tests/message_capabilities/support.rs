use super::*;
use rsi_meta::{FiberHandle, FiberState};
use tokio_util::sync::CancellationToken;

pub(super) const V1: ContractVersion = ContractVersion(1);

fn contract(key: &str) -> String {
    format!("test.{key}")
}

#[derive(Debug)]
pub(super) struct ProviderFactory {
    _identity: FactoryIdentity,
    key: &'static str,
    endpoint: Arc<dyn ServiceEndpoint>,
}

impl ProviderFactory {
    pub(super) fn new(
        name: &'static str,
        key: &'static str,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Self {
        Self {
            _identity: FactoryIdentity::linked(name, "1"),
            key,
            endpoint,
        }
    }
}

#[async_trait]
impl PluginFactory for ProviderFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context()
            .provide(self.key, contract(self.key), V1, Arc::clone(&self.endpoint))?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(super) struct Capture {
    pub(super) context: Context,
    pub(super) capabilities: Vec<Capability>,
}

#[derive(Debug)]
struct CaptureFactory {
    _identity: FactoryIdentity,
    keys: Vec<&'static str>,
    capture: Arc<Mutex<Option<Capture>>>,
}

#[async_trait]
impl PluginFactory for CaptureFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(self
            .keys
            .iter()
            .fold(PreparedActivation::new(desired.clone()), |prepared, key| {
                prepared.requiring(Requirement::new(*key, contract(key), V1))
            }))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let capabilities = self
            .keys
            .iter()
            .map(|key| {
                plan.inject(key)
                    .expect("prepared capability must be injected")
                    .clone()
            })
            .collect();
        *self.capture.lock().expect("capability capture poisoned") = Some(Capture {
            context: plan.context().clone(),
            capabilities,
        });
        Ok(())
    }
}

pub(super) async fn install_provider(
    runtime: &Runtime,
    name: &'static str,
    key: &'static str,
    endpoint: Arc<dyn ServiceEndpoint>,
) -> FiberHandle {
    let handle = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(ProviderFactory::new(name, key, endpoint))),
            Value::Null,
        )
        .await
        .expect("provider installation failed");
    wait_active(&handle).await;
    handle
}

pub(super) async fn install_consumer(
    runtime: &Runtime,
    name: &'static str,
    keys: Vec<&'static str>,
) -> (FiberHandle, Capture) {
    let capture = Arc::new(Mutex::new(None));
    let handle = runtime
        .root()
        .apply(
            crate::resolved(Arc::new(CaptureFactory {
                _identity: FactoryIdentity::linked(name, "1"),
                keys,
                capture: Arc::clone(&capture),
            })),
            Value::Null,
        )
        .await
        .expect("consumer installation failed");
    wait_active(&handle).await;
    let captured = capture
        .lock()
        .expect("capability capture poisoned")
        .take()
        .expect("consumer activation captured its Context and capabilities");
    (handle, captured)
}

async fn wait_active(handle: &FiberHandle) {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        handle.wait_active(&CancellationToken::new()),
    )
    .await
    .expect("fiber activation timed out")
    .expect("fiber should activate");
    assert!(matches!(handle.snapshot().state, FiberState::Active));
}
