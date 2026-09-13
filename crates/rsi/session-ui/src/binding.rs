use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_agent_session_protocol::SessionId;
use rsi_api_protocol::{
    ApiDispatchContract, ApiError, CallOrigin, ConnectionDescriptionContract, Result,
};
use rsi_application::ScopedProfile;
use rsi_client::{
    ObservationFailure, ObservationKind, ObservationSink, ObservationSinkContract,
    SessionControllerContract, SessionControllerFactory,
};
use rsi_host::{Host, HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, LocalContract, PluginFactory, PreparedActivation,
    UpdateMode,
};
use rsi_session_protocol::SessionContract;
use rsi_ui::{ContributionLease, TargetKind, UiContract, UiTargetContract};
use rsi_ui_api::{ExportScope, UiBinding, UiBindingOwner, UiTargetBinder, UiTargetBinderContract};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug)]
struct TargetLease;
impl LocalContract for TargetLease {
    const KEY: &'static str = "rsi.session.ui.binding.lease";
    type Service = ContributionLease;
}
#[derive(Debug)]
struct Target {
    origin: CallOrigin,
    invalidation: Arc<Invalidation>,
}
#[async_trait]
impl PluginFactory for Target {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(super::no_config(config)?
            .requiring_local::<UiContract>()
            .requiring_local::<SessionControllerContract>()
            .requiring_local::<ApiDispatchContract>()
            .requiring_local::<ConnectionDescriptionContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let session = plan
            .local::<SessionControllerContract>()?
            .session_id()
            .clone();
        let api = rsi_session_api::SessionTargetClient::from_dispatch(
            plan.local::<ApiDispatchContract>()?,
            plan.local::<ConnectionDescriptionContract>()?
                .as_ref()
                .clone(),
            self.origin.clone(),
            session.clone(),
        )
        .map_err(super::meta)?;
        let business = plan
            .context()
            .provide_local::<rsi_ui::UiBusinessApiContract>(Arc::new(rsi_ui::UiBusinessApi {
                scope: ExportScope {
                    kind: "session".into(),
                    key: session.to_string(),
                },
                client: Arc::new(api),
            }))?;
        let (target, lease) = plan
            .local::<UiContract>()?
            .register_target(&plan, TargetKind::Surface)
            .map_err(super::meta)?;
        let lease = Arc::new(lease);
        let watching = super::watch_target(&plan, &lease)?;
        *self
            .invalidation
            .0
            .lock()
            .expect("Session UI invalidation poisoned") = Arc::downgrade(&lease);
        let supplies = [
            plan.context().provide_local::<UiTargetContract>(target)?,
            plan.context().provide_local::<TargetLease>(lease.clone())?,
        ];
        plan.defer(
            "close exported Session target",
            Box::new(move || {
                Box::pin(async move {
                    drop(business);
                    drop(supplies);
                    watching.await?;
                    let report = lease.dispose().await;
                    if report.is_clean() {
                        Ok(())
                    } else {
                        Err("Session UI target cleanup failed".into())
                    }
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Sink(Arc<Invalidation>);
#[derive(Debug, Default)]
struct Invalidation(Mutex<Weak<ContributionLease>>);
impl Invalidation {
    fn invalidate(&self) {
        if let Some(lease) = self
            .0
            .lock()
            .expect("Session UI invalidation poisoned")
            .upgrade()
        {
            let _ = lease.invalidate();
        }
    }
}
#[async_trait]
impl ObservationSink for Sink {
    async fn observation(
        &self,
        _: rsi_agent_turn_protocol::SessionObservation,
    ) -> std::result::Result<(), ObservationFailure> {
        self.0.invalidate();
        Ok(())
    }
    async fn interactions(
        &self,
        _: rsi_session_protocol::InteractionSnapshot,
    ) -> std::result::Result<(), ObservationFailure> {
        self.0.invalidate();
        Ok(())
    }
    async fn projections(
        &self,
        _: rsi_session_protocol::ProjectionSnapshot,
    ) -> std::result::Result<(), ObservationFailure> {
        self.0.invalidate();
        Ok(())
    }
    async fn reconnecting(
        &self,
        _: ObservationKind,
        _: &ObservationFailure,
    ) -> std::result::Result<(), ObservationFailure> {
        Ok(())
    }
    async fn stopped(&self, _: ObservationKind, _: &ObservationFailure) {
        self.0.invalidate();
    }
}
#[derive(Debug)]
struct SinkFactory(Arc<Invalidation>);
#[async_trait]
impl PluginFactory for SinkFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        super::no_config(config)
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ObservationSinkContract>(Arc::new(Sink(self.0.clone())))?;
        plan.defer(
            "withdraw exported Session observation",
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
struct Owner {
    profile: Arc<ScopedProfile>,
    lease: Arc<ContributionLease>,
    slot: Option<OwnedSemaphorePermit>,
    failed: Arc<AtomicBool>,
    closed: AtomicBool,
    execution: rsi_meta::Execution,
    tasks: TaskTracker,
}
#[async_trait]
impl UiBindingOwner for Owner {
    fn retire(&self) {
        self.lease.retire();
    }
    async fn close(&self) -> Result<()> {
        let clean = self.profile.shutdown().await.is_clean();
        self.closed.store(true, Ordering::Release);
        if clean {
            Ok(())
        } else {
            self.failed.store(true, Ordering::Release);
            Err(ApiError::Backend("Session UI target cleanup failed".into()))
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.lease.retire();
        let profile = self.profile.clone();
        let failed = self.failed.clone();
        let slot = self.slot.take();
        self.execution.spawn(self.tasks.track_future(async move {
            let _slot = slot;
            let result = std::panic::AssertUnwindSafe(profile.shutdown())
                .catch_unwind()
                .await;
            if !matches!(result,Ok(report) if report.is_clean()) {
                failed.store(true, Ordering::Release);
            }
        }));
    }
}
#[derive(Debug)]
struct Binder {
    parent: Context,
    tasks: TaskTracker,
    stop: CancellationToken,
    slots: Arc<Semaphore>,
    failed: Arc<AtomicBool>,
}
impl Binder {
    async fn start(
        &self,
        id: SessionId,
        origin: CallOrigin,
        stop: CancellationToken,
        slot: OwnedSemaphorePermit,
    ) -> Result<UiBinding> {
        let program = ProfileProgram::from_profile(Profile::new(vec![
            ProfileEntry::new("sink", "rsi.session.ui.sink", ConfigValue::Null),
            ProfileEntry::new(
                "controller",
                "rsi.client.session-controller",
                serde_json::json!({"session_id": id}),
            ),
            ProfileEntry::new("target", "rsi.session.ui.target", ConfigValue::Null),
        ]));
        let host = host(origin).map_err(|_| ApiError::Unavailable)?;
        let profile = ScopedProfile::start(&host, &self.parent, program)
            .await
            .map_err(|_| ApiError::Unavailable)?;
        if stop.is_cancelled() || self.stop.is_cancelled() {
            if !profile.shutdown().await.is_clean() {
                self.failed.store(true, Ordering::Release);
            }
            return Err(ApiError::ShuttingDown);
        }
        let ui = self.parent.lookup_local::<UiContract>();
        let target = profile.lookup_local::<UiTargetContract>();
        let lease = profile.lookup_local::<TargetLease>();
        if let (Some(ui), Some(target), Some(lease)) = (ui, target, lease) {
            Ok(UiBinding::new(
                ui,
                target,
                Arc::new(Owner {
                    profile: Arc::new(profile),
                    lease,
                    slot: Some(slot),
                    failed: self.failed.clone(),
                    closed: AtomicBool::new(false),
                    execution: self.parent.runtime().execution().clone(),
                    tasks: self.tasks.clone(),
                }),
            ))
        } else {
            if !profile.shutdown().await.is_clean() {
                self.failed.store(true, Ordering::Release);
            }
            Err(ApiError::Unavailable)
        }
    }
}
#[derive(Debug)]
struct Binding(Arc<Binder>);
#[async_trait]
impl UiTargetBinder for Binding {
    async fn bind(
        &self,
        origin: CallOrigin,
        scope: ExportScope,
        stop: CancellationToken,
    ) -> Result<UiBinding> {
        if scope.kind != "session" {
            return Err(ApiError::Unauthorized);
        }
        if let CallOrigin::Device(device) = &origin
            && device.revoked.is_cancelled()
        {
            return Err(ApiError::Unauthorized);
        }
        let id = SessionId::new(scope.key)
            .map_err(|_| ApiError::Invalid("invalid Session scope".into()))?;
        if self.0.stop.is_cancelled() || stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let slot = self
            .0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let binder = self.0.clone();
        let token = binder.tasks.token();
        let (sender, receiver) = oneshot::channel();
        let execution = binder.parent.runtime().execution().clone();
        execution.spawn(async move {
            let _token = token;
            let result = binder.start(id, origin, stop, slot).await;
            if let Err(Ok(binding)) = sender.send(result) {
                let _ = binding.close().await;
            }
        });
        receiver.await.map_err(|_| ApiError::Unavailable)?
    }
}
fn host(origin: CallOrigin) -> rsi_host::Result<Host> {
    let mut builder = HostBuilder::without_paths(std::env::consts::OS);
    let invalidation = Arc::new(Invalidation::default());
    builder.register_local_contract::<ObservationSinkContract>()?;
    builder.register_local_contract::<SessionControllerContract>()?;
    builder.register_local_contract::<UiTargetContract>()?;
    builder.register_local_contract::<TargetLease>()?;
    builder.register_local_contract::<rsi_ui::UiBusinessApiContract>()?;
    let factories: [(&str, Arc<dyn PluginFactory>); 3] = [
        (
            "rsi.session.ui.sink",
            Arc::new(SinkFactory(invalidation.clone())),
        ),
        (
            "rsi.client.session-controller",
            Arc::new(SessionControllerFactory),
        ),
        (
            "rsi.session.ui.target",
            Arc::new(Target {
                origin,
                invalidation,
            }),
        ),
    ];
    for (id, factory) in factories {
        builder.register_linked(
            id,
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            factory,
        )?;
    }
    builder.build()
}
/// Explicit authenticated Session scope binder for the generic UI API.
#[derive(Clone, Debug, Default)]
pub struct SessionUiBinderFactory;
#[async_trait]
impl PluginFactory for SessionUiBinderFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(super::no_config(config)?
            .requiring_local::<UiContract>()
            .requiring_local::<SessionContract>()
            .requiring_local::<ApiDispatchContract>()
            .requiring_local::<ConnectionDescriptionContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let binder = Arc::new(Binder {
            parent: plan.context().clone(),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
            slots: Arc::new(Semaphore::new(16)),
            failed: Arc::new(AtomicBool::new(false)),
        });
        let supply = plan
            .context()
            .provide_local::<UiTargetBinderContract>(Arc::new(Binding(binder.clone())))?;
        plan.defer(
            "close Session UI target binding",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    binder.stop.cancel();
                    binder.tasks.close();
                    binder.tasks.wait().await;
                    if binder.failed.load(Ordering::Acquire) {
                        Err("Session UI binding cleanup failed".into())
                    } else {
                        Ok(())
                    }
                })
            }),
        )
    }
}
