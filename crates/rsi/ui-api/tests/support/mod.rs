use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_api::ApiRegistry;
use rsi_api_protocol::{ApiDispatch, ApiError, ApiOutput, CallOrigin, Result};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, PluginFactory, PreparedActivation, ResolvedFactory,
    Runtime, UpdateMode,
};
use rsi_meta_scope::{ScopeHandle, ScopeRoot};
use rsi_ui::*;
use rsi_ui_api::{
    ExportScope, Observe, Selection, UiApi, UiBinding, UiBindingOwner, UiTargetBinder,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct Source {
    pub fail_refresh: AtomicBool,
    pub refreshes: AtomicUsize,
    pub entered: AtomicUsize,
    pub completed: AtomicUsize,
    pub gate: Semaphore,
}
impl Source {
    fn view() -> UiView {
        UiView {
            title: "Exact target".into(),
            elements: vec![UiElement::Button {
                action: "run".into(),
                label: "Run once".into(),
                value: ConfigValue::Null,
            }],
        }
    }
}
impl SurfaceRenderer for Source {
    fn model(&self, _: Context) -> BoxFuture<'_, rsi_ui::Result<UiModel>> {
        Box::pin(async {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            if self.fail_refresh.load(Ordering::SeqCst) {
                return Err(UiError::Action("fixture refresh failed".into()));
            }
            let mut model = UiModel::standard(Self::view())?;
            model.sources.push(ModelSource {
                name: "raw".into(),
                title: "Source".into(),
                media_type: "application/octet-stream".into(),
            });
            Ok(model)
        })
    }
    fn source(
        &self,
        _: ActionTarget,
        _: String,
        offset: u64,
        maximum: usize,
    ) -> BoxFuture<'static, rsi_ui::Result<Vec<u8>>> {
        Box::pin(async move {
            Ok(b"\0\xffsource"
                .get(usize::try_from(offset).unwrap_or(usize::MAX)..)
                .unwrap_or_default()
                .iter()
                .take(maximum)
                .copied()
                .collect())
        })
    }
}
#[derive(Debug)]
struct Action(Arc<Source>);
impl UiAction for Action {
    fn invoke(
        &self,
        _: ActionTarget,
        _: ActionInput,
    ) -> BoxFuture<'static, rsi_ui::Result<UiView>> {
        let source = self.0.clone();
        Box::pin(async move {
            source.entered.fetch_add(1, Ordering::SeqCst);
            source.gate.acquire().await.unwrap().forget();
            source.completed.fetch_add(1, Ordering::SeqCst);
            Ok(Source::view())
        })
    }
}
#[derive(Debug)]
struct ContributionsFactory(Arc<Source>);
#[async_trait]
impl PluginFactory for ContributionsFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "fixture".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "panel".into(),
                        title: "Panel".into(),
                        target: TargetKind::Surface,
                        renderer: self.0.clone(),
                    }],
                    actions: vec![ActionContribution {
                        name: "run".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(Action(self.0.clone())),
                    }],
                    renderers: vec![],
                },
            )
            .unwrap();
        plan.defer(
            "contribution",
            Box::new(move || {
                Box::pin(async move {
                    lease.dispose().await;
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Target(Arc<Mutex<Option<Arc<ContributionLease>>>>);
#[async_trait]
impl PluginFactory for Target {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (target, lease) = plan
            .local::<UiContract>()?
            .register_target(&plan, TargetKind::Surface)
            .unwrap();
        let lease = Arc::new(lease);
        *self.0.lock().unwrap() = Some(lease.clone());
        let supply = plan.context().provide_local::<UiTargetContract>(target)?;
        plan.defer(
            "target",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    lease.dispose().await;
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct BindingOwner {
    scope: ScopeHandle,
    lease: Arc<ContributionLease>,
    closed: Arc<AtomicUsize>,
    once: AtomicBool,
    failed: Arc<AtomicBool>,
}
#[async_trait]
impl UiBindingOwner for BindingOwner {
    fn retire(&self) {
        self.lease.retire();
    }
    async fn close(&self) -> Result<()> {
        assert!(self.scope.dispose().await.is_clean());
        if !self.once.swap(true, Ordering::SeqCst) {
            self.closed.fetch_add(1, Ordering::SeqCst);
        }
        if self.failed.load(Ordering::SeqCst) {
            Err(ApiError::Backend("fixture cleanup failure".into()))
        } else {
            Ok(())
        }
    }
}
#[derive(Debug)]
pub struct Binder {
    root: Context,
    scopes: ScopeRoot,
    pub closed: Arc<AtomicUsize>,
    pub bound: AtomicUsize,
    pub fail_close: Arc<AtomicBool>,
    pub origins: Mutex<Vec<CallOrigin>>,
}
#[async_trait]
impl UiTargetBinder for Binder {
    async fn bind(
        &self,
        origin: CallOrigin,
        requested: ExportScope,
        stop: CancellationToken,
    ) -> Result<UiBinding> {
        if requested.kind != "fixture" || requested.key != "allowed" {
            return Err(ApiError::Unauthorized);
        }
        if stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        self.origins.lock().unwrap().push(origin);
        let (context, _) = self
            .root
            .clone()
            .isolate_local_fresh::<UiTargetContract>()
            .unwrap();
        let scope = self.scopes.create(&context).await.unwrap();
        let capture = Arc::new(Mutex::new(None));
        apply(scope.context().meta(), "target", Target(capture.clone())).await;
        let lease = capture.lock().unwrap().take().unwrap();
        let target = scope
            .context()
            .meta()
            .lookup_local::<UiTargetContract>()
            .unwrap();
        self.bound.fetch_add(1, Ordering::SeqCst);
        Ok(UiBinding::new(
            self.root.lookup_local::<UiContract>().unwrap(),
            target,
            Arc::new(BindingOwner {
                scope,
                lease,
                closed: self.closed.clone(),
                once: AtomicBool::new(false),
                failed: self.fail_close.clone(),
            }),
        ))
    }
}
pub async fn apply(context: &Context, name: &str, factory: impl PluginFactory) {
    let fiber = context
        .apply(
            ResolvedFactory::linked(name, "1", UpdateMode::Replayable, Arc::new(factory)),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
}
pub struct Fixture {
    pub runtime: Runtime,
    pub registry: ApiRegistry,
    pub api: UiApi,
    pub binder: Arc<Binder>,
    pub source: Arc<Source>,
    pub ui: Arc<Ui>,
}
impl Fixture {
    pub async fn new() -> Self {
        let runtime = Runtime::default();
        apply(&runtime.root(), "ui", UiFactory).await;
        let source = Arc::new(Source {
            fail_refresh: AtomicBool::new(false),
            refreshes: AtomicUsize::new(0),
            entered: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            gate: Semaphore::new(0),
        });
        apply(
            &runtime.root(),
            "contribution",
            ContributionsFactory(source.clone()),
        )
        .await;
        let binder = Arc::new(Binder {
            root: runtime.root(),
            scopes: ScopeRoot::new(4).unwrap(),
            closed: Arc::new(AtomicUsize::new(0)),
            bound: AtomicUsize::new(0),
            fail_close: Arc::new(AtomicBool::new(false)),
            origins: Mutex::new(Vec::new()),
        });
        let registry = ApiRegistry::new(runtime.execution().clone());
        let api = UiApi::register(&registry, runtime.execution().clone(), binder.clone()).unwrap();
        let ui = runtime.root().lookup_local::<UiContract>().unwrap();
        Self {
            runtime,
            registry,
            api,
            binder,
            source,
            ui,
        }
    }
    pub async fn close(self) {
        self.api.close().await.unwrap();
        self.registry.close().await;
        assert!(self.runtime.shutdown().await.is_clean());
    }
    pub fn call(
        &self,
        name: &str,
        origin: CallOrigin,
        input: &impl serde::Serialize,
    ) -> BoxFuture<'static, Result<ApiOutput>> {
        let spec = rsi_ui_api::operations()
            .into_iter()
            .find(|op| op.id.name() == name)
            .unwrap();
        let invocation = self.registry.admit(&spec.id, origin).unwrap();
        let bytes = invocation
            .input_budget()
            .encode(input, spec.maximum_request_bytes)
            .unwrap();
        invocation.invoke(bytes)
    }
}
pub fn observe(application: &str, count: usize) -> Observe {
    Observe {
        application: application.into(),
        selections: vec![
            Selection {
                scope: ExportScope {
                    kind: "fixture".into(),
                    key: "allowed".into()
                },
                bundle: "fixture".into(),
                surface: "panel".into()
            };
            count
        ],
    }
}
pub async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
