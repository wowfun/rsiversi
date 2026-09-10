#![allow(dead_code)] // Public-boundary test binaries select different fixture surfaces.
use async_trait::async_trait;
use rsi_api::ApiRegistry;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiContext, ApiDispatch, ApiError, ApiHandler, ApiMessage,
    ApiOutput, ApiRegistrar, ApiRegistration, ApiResponseCapacity, ByteBudget, CallOrigin,
    ConnectionDescription, EndpointId, HostEpoch, OperationAccess, OperationClass, OperationEffect,
    OperationId, OperationSpec, RequestEncoding, Result, RetainedBytes,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FiberHandle, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;

pub fn operation(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("fixture", name, 1).unwrap(),
        access: OperationAccess::Authenticated,
        class: if name == "stream" {
            OperationClass::Subscription
        } else {
            OperationClass::Data
        },
        effect: if name == "mutate" {
            OperationEffect::Mutation
        } else {
            OperationEffect::Read
        },
        encoding: RequestEncoding::Binary,
        maximum_request_bytes: 2 * 1024 * 1024,
        maximum_response_bytes: 2 * 1024 * 1024,
    }
}
#[derive(Debug)]
pub struct Behavior {
    pub mutations: AtomicUsize,
    pub finished: AtomicUsize,
    pub release: Semaphore,
}
#[derive(Debug)]
struct Handler {
    name: &'static str,
    behavior: Arc<Behavior>,
}
#[async_trait]
impl ApiHandler for Handler {
    async fn invoke(
        &self,
        _: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        if self.name == "stream" {
            let ApiResponseCapacity::Subscription { budget, maximum } = output else {
                panic!("stream budget")
            };
            return Ok(ApiOutput::Stream(Box::pin(async_stream::try_stream! {
                for index in 0..3 { let reservation = budget.reserve(maximum)?; yield ApiMessage { json: reservation.encode(&index)?, binary: None }; }
                futures_util::future::pending::<()>().await;
            })));
        }
        if self.name == "mutate" {
            self.behavior.mutations.fetch_add(1, Ordering::SeqCst);
            self.behavior.release.acquire().await.unwrap().forget();
            self.behavior.finished.fetch_add(1, Ordering::SeqCst);
        }
        let ApiResponseCapacity::Finite(mut capacity) = output else {
            panic!("finite budget")
        };
        if self.name == "domain" {
            return Err(ApiError::Domain(capacity.encode(
                &serde_json::json!({"rejected": "\u{0001}".repeat(20_000)}),
            )?));
        }
        let binary = capacity.split(input.len())?.copy(input.as_bytes())?;
        let json = capacity.encode(
            &serde_json::json!({"length": input.len(), "padding": "\u{0001}".repeat(20_000)}),
        )?;
        Ok(ApiOutput::Reply(ApiMessage {
            json,
            binary: Some(binary),
        }))
    }
}
#[derive(Debug)]
struct LocalClient {
    registry: Arc<ApiRegistry>,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    input: ByteBudget,
}
#[async_trait]
impl ApiClient for LocalClient {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.input.clone()
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        self.registry
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await
    }
}
#[derive(Debug)]
struct Source(Arc<LocalClient>);
#[async_trait]
impl PluginFactory for Source {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ApiClientContract>(self.0.clone())?;
        plan.defer(
            "source client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
pub async fn apply(
    context: &Context,
    id: &str,
    factory: impl PluginFactory,
    config: ConfigValue,
) -> FiberHandle {
    let handle = context
        .apply(
            ResolvedFactory::linked(id, "1", UpdateMode::Replayable, Arc::new(factory)),
            config,
        )
        .await
        .unwrap();
    assert_eq!(handle.snapshot().state, rsi_meta::FiberState::Active);
    handle
}
pub struct Fixture {
    pub runtime: Runtime,
    pub registry: Arc<ApiRegistry>,
    pub registrations: Vec<ApiRegistration>,
    pub client: Arc<dyn ApiClient>,
    pub importer: FiberHandle,
    pub exporter: FiberHandle,
    pub behavior: Arc<Behavior>,
}
impl Fixture {
    pub async fn new() -> Self {
        Self::with_runtime(Runtime::default()).await
    }
    pub async fn with_runtime(runtime: Runtime) -> Self {
        let registry = Arc::new(ApiRegistry::new(runtime.execution().clone()));
        let behavior = Arc::new(Behavior {
            mutations: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            release: Semaphore::new(0),
        });
        let mut registrations = Vec::new();
        for name in ["echo", "mutate", "stream", "domain", "hidden"] {
            registrations.push(
                registry
                    .register(
                        operation(name),
                        Arc::new(Handler {
                            name,
                            behavior: behavior.clone(),
                        }),
                    )
                    .unwrap(),
            );
        }
        let source = Arc::new(LocalClient {
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            operations: registry.operations(),
            registry: registry.clone(),
            input: ByteBudget::default(),
        });
        apply(&runtime.root(), "source", Source(source), ConfigValue::Null).await;
        let exporter = apply(&runtime.root(), "export", rsi_api_portable::PortableApiExportFactory, serde_json::json!({"service":"fixture.api","operations": [operation("echo").id,operation("mutate").id,operation("stream").id,operation("domain").id],"publish_local":true})).await;
        let (isolated, _) = runtime
            .root()
            .isolate_local_fresh::<ApiClientContract>()
            .unwrap();
        let importer = apply(
            &isolated,
            "import",
            rsi_api_portable::PortableApiClientFactory,
            serde_json::json!({"service":"fixture.api"}),
        )
        .await;
        let client = isolated.lookup_local::<ApiClientContract>().unwrap();
        Self {
            runtime,
            registry,
            registrations,
            client,
            importer,
            exporter,
            behavior,
        }
    }
    pub async fn close(self) {
        assert!(self.runtime.shutdown().await.is_clean());
        for registration in self.registrations {
            registration.close().await;
        }
        self.registry.close().await;
    }
}
pub async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
