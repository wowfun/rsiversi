use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, CallerEffect, CallerView, Capability, Cleanup, ConfigValue, ContextExtension,
    ContractVersion, DispatchMode, EventHandler, EventOptions, EventOutcome, FactoryIdentity,
    FiberGeneration, FiberId, FiberState, InvocationContext, Message, MetaError, PluginFactory,
    PreparedActivation, ProviderChannel, Requirement, Result, Runtime, RuntimeLimits,
    ServiceEndpoint, TopologyLimits,
};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const V1: ContractVersion = ContractVersion(1);

struct CallerLabel;

impl ContextExtension for CallerLabel {
    type Value = String;
}

#[derive(Debug)]
struct ProviderFactory {
    identity: FactoryIdentity,
    endpoint: Arc<dyn ServiceEndpoint>,
}

#[async_trait]
impl PluginFactory for ProviderFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().provide(
            "caller-effect",
            "test.caller-effect",
            V1,
            Arc::clone(&self.endpoint),
        )?;
        Ok(())
    }
}

#[derive(Debug)]
struct ConsumerFactory {
    identity: FactoryIdentity,
    service: Arc<Mutex<Option<Capability>>>,
}

#[async_trait]
impl PluginFactory for ConsumerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                "caller-effect",
                "test.caller-effect",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.service.lock().expect("service capture poisoned") = Some(
            plan.inject("caller-effect")
                .expect("prepared caller-effect requirement must be injected")
                .clone(),
        );
        Ok(())
    }
}

fn captured_service(service: &Arc<Mutex<Option<Capability>>>) -> Capability {
    service
        .lock()
        .expect("service capture poisoned")
        .take()
        .expect("consumer captured its service")
}

async fn wait_unloading(handle: &rsi_meta::FiberHandle) {
    let mut snapshots = handle.subscribe();
    loop {
        if matches!(snapshots.borrow().state, FiberState::Unloading) {
            return;
        }
        snapshots
            .changed()
            .await
            .expect("Fiber must remain observable until retirement completes");
    }
}

#[derive(Debug)]
struct CallerOwnedCleanupEndpoint {
    observed_owner: Arc<Mutex<Option<(FiberId, FiberGeneration)>>>,
    observed_extension: Arc<Mutex<Option<String>>>,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl ServiceEndpoint for CallerOwnedCleanupEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        *self.observed_owner.lock().expect("owner capture poisoned") =
            invocation.caller().owner()?;
        *self
            .observed_extension
            .lock()
            .expect("extension capture poisoned") = invocation
            .caller()
            .extension::<CallerLabel>()?
            .as_deref()
            .cloned();
        let cleanups = Arc::clone(&self.cleanups);
        let cleanup: Cleanup = Box::new(move || {
            Box::pin(async move {
                cleanups.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
        });
        invocation
            .caller_effect()
            .expect("a service call has an owned caller generation")
            .defer("caller-owned cleanup", cleanup)?;
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn provider_can_register_cleanup_only_on_the_exact_caller_generation() {
    let runtime = Runtime::default();
    let observed_owner = Arc::new(Mutex::new(None));
    let observed_extension = Arc::new(Mutex::new(None));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("caller-effect-provider", "1"),
                endpoint: Arc::new(CallerOwnedCleanupEndpoint {
                    observed_owner: Arc::clone(&observed_owner),
                    observed_extension: Arc::clone(&observed_extension),
                    cleanups: Arc::clone(&cleanups),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .with_extension::<CallerLabel>("caller-scope".to_owned())
        .unwrap()
        .apply(
            Arc::new(ConsumerFactory {
                identity: FactoryIdentity::builtin("caller-effect-consumer", "1"),
                service: Arc::clone(&service),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let caller_snapshot = caller.snapshot();
    assert!(matches!(caller_snapshot.state, FiberState::Active));

    captured_service(&service)
        .invoke(Message::new(b"register".to_vec()))
        .await
        .unwrap();

    assert_eq!(
        *observed_owner.lock().expect("owner capture poisoned"),
        Some((caller.id(), caller_snapshot.generation))
    );
    assert_eq!(
        observed_extension
            .lock()
            .expect("extension capture poisoned")
            .as_deref(),
        Some("caller-scope")
    );
    assert_eq!(cleanups.load(Ordering::Acquire), 0);
    assert!(caller.dispose().await.is_clean());
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    assert!(matches!(provider.snapshot().state, FiberState::Active));

    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct RetainingEndpoint {
    retained: Arc<Mutex<Option<(CallerView, CallerEffect)>>>,
}

struct DeferOnDropFuture {
    effect: Option<CallerEffect>,
    result: Arc<Mutex<Option<Result<()>>>>,
    cleanups: Arc<AtomicUsize>,
}

impl std::future::Future for DeferOnDropFuture {
    type Output = Result<()>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl Drop for DeferOnDropFuture {
    fn drop(&mut self) {
        let cleanups = Arc::clone(&self.cleanups);
        let result = self
            .effect
            .take()
            .expect("future retains exact caller effect")
            .defer(
                "provider future drop cleanup",
                Box::new(move || {
                    Box::pin(async move {
                        cleanups.fetch_add(1, Ordering::AcqRel);
                        Ok(())
                    })
                }),
            );
        *self.result.lock().expect("drop result poisoned") = Some(result);
    }
}

#[derive(Debug)]
struct DeferOnFutureDropEndpoint {
    result: Arc<Mutex<Option<Result<()>>>>,
    cleanups: Arc<AtomicUsize>,
}

impl ServiceEndpoint for DeferOnFutureDropEndpoint {
    fn serve<'life0, 'life1, 'async_trait>(
        &'life0 self,
        invocation: InvocationContext,
        _: ProviderChannel<'life1>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(DeferOnDropFuture {
            effect: Some(
                invocation
                    .caller_effect()
                    .expect("owned caller has callback effect")
                    .clone(),
            ),
            result: Arc::clone(&self.result),
            cleanups: Arc::clone(&self.cleanups),
        })
    }
}

#[tokio::test]
async fn provider_future_drop_retains_exact_caller_effect_authority() {
    let runtime = Runtime::default();
    let result = Arc::new(Mutex::new(None));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("future-drop-provider", "1"),
                endpoint: Arc::new(DeferOnFutureDropEndpoint {
                    result: Arc::clone(&result),
                    cleanups: Arc::clone(&cleanups),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .apply(
            Arc::new(ConsumerFactory {
                identity: FactoryIdentity::builtin("future-drop-consumer", "1"),
                service: Arc::clone(&service),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    let mut call = captured_service(&service).open().unwrap();
    assert!(call.recv().await.unwrap().is_none());
    assert_eq!(
        result
            .lock()
            .expect("drop result poisoned")
            .take()
            .expect("provider future Drop attempted registration"),
        Ok(()),
    );
    assert_eq!(cleanups.load(Ordering::Acquire), 0);
    assert!(caller.dispose().await.is_clean());
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[async_trait]
impl ServiceEndpoint for RetainingEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        *self
            .retained
            .lock()
            .expect("caller authority capture poisoned") = Some((
            invocation.caller().clone(),
            invocation
                .caller_effect()
                .expect("a service call has an owned caller generation")
                .clone(),
        ));
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn retained_caller_authority_is_stale_after_callback_terminal() {
    let runtime = Runtime::default();
    let retained = Arc::new(Mutex::new(None));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("retained-caller-provider", "1"),
                endpoint: Arc::new(RetainingEndpoint {
                    retained: Arc::clone(&retained),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .with_extension::<CallerLabel>("retained-scope".to_owned())
        .unwrap()
        .apply(
            Arc::new(ConsumerFactory {
                identity: FactoryIdentity::builtin("retained-caller-consumer", "1"),
                service: Arc::clone(&service),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let generation = caller.snapshot().generation;
    captured_service(&service)
        .invoke(Message::new(b"retain".to_vec()))
        .await
        .unwrap();

    let (view, effect) = retained
        .lock()
        .expect("caller authority capture poisoned")
        .clone()
        .expect("provider retained the callback-lifetime authorities");
    let stale = MetaError::StaleContext {
        fiber: caller.id(),
        generation,
    };
    assert_eq!(view.owner().unwrap_err(), stale);
    assert_eq!(view.extension::<CallerLabel>().unwrap_err(), stale);
    let resources = runtime.resource_snapshot();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let cleanup_count = Arc::clone(&cleanups);
    let cleanup: Cleanup = Box::new(move || {
        Box::pin(async move {
            cleanup_count.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    });
    assert_eq!(effect.defer("too late", cleanup).unwrap_err(), stale);
    assert_eq!(runtime.resource_snapshot(), resources);
    assert_eq!(cleanups.load(Ordering::Acquire), 0);

    assert!(caller.dispose().await.is_clean());
    drop(service.lock().expect("service capture poisoned").take());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());

    let cleanup_count = Arc::clone(&cleanups);
    let cleanup: Cleanup = Box::new(move || {
        Box::pin(async move {
            cleanup_count.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    });
    assert_eq!(effect.defer("closed Runtime", cleanup).unwrap_err(), stale);
    assert_eq!(cleanups.load(Ordering::Acquire), 0);
}

#[derive(Debug)]
struct LoadingCleanupEndpoint {
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl ServiceEndpoint for LoadingCleanupEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        let cleanups = Arc::clone(&self.cleanups);
        invocation
            .caller_effect()
            .expect("a Loading service caller has caller-effect authority")
            .defer(
                "loading caller cleanup",
                Box::new(move || {
                    Box::pin(async move {
                        cleanups.fetch_add(1, Ordering::AcqRel);
                        Ok(())
                    })
                }),
            )?;
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct FailingLoadingConsumer {
    identity: FactoryIdentity,
}

#[async_trait]
impl PluginFactory for FailingLoadingConsumer {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(
            PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                "caller-effect",
                "test.caller-effect",
                V1,
            )),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.inject("caller-effect")
            .expect("prepared caller-effect requirement must be injected")
            .invoke(Message::new(b"loading".to_vec()))
            .await?;
        Err(MetaError::Activation(
            "fail after caller-owned registration".to_owned(),
        ))
    }
}

#[tokio::test]
async fn loading_caller_effect_joins_the_activation_root_rollback() {
    let runtime = Runtime::default();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("loading-caller-provider", "1"),
                endpoint: Arc::new(LoadingCleanupEndpoint {
                    cleanups: Arc::clone(&cleanups),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let before = runtime.resource_snapshot();
    let caller = runtime
        .root()
        .apply(
            Arc::new(FailingLoadingConsumer {
                identity: FactoryIdentity::builtin("failing-loading-caller", "1"),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    assert!(matches!(caller.snapshot().state, FiberState::Failed(_)));
    assert_eq!(cleanups.load(Ordering::Acquire), 1);
    let after = runtime.resource_snapshot();
    assert_eq!(after.effects.current, before.effects.current);
    assert_eq!(
        after.effect_transactions.current,
        before.effect_transactions.current
    );

    assert!(caller.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

fn record_cleanup(order: &Arc<Mutex<Vec<&'static str>>>, label: &'static str) -> Cleanup {
    let order = Arc::clone(order);
    Box::new(move || {
        Box::pin(async move {
            order.lock().expect("cleanup order poisoned").push(label);
            Ok(())
        })
    })
}

#[derive(Debug)]
struct ActiveOrderedEndpoint {
    order: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl ServiceEndpoint for ActiveOrderedEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        let effect = invocation
            .caller_effect()
            .expect("an Active service caller has caller-effect authority");
        effect.defer("active first", record_cleanup(&self.order, "first"))?;
        effect.defer("active second", record_cleanup(&self.order, "second"))?;
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn active_caller_effects_commit_individually_and_retire_in_lifo_order() {
    let runtime = Runtime::default();
    let order = Arc::new(Mutex::new(Vec::new()));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("active-caller-provider", "1"),
                endpoint: Arc::new(ActiveOrderedEndpoint {
                    order: Arc::clone(&order),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let provider_only = runtime.resource_snapshot();
    let service = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .apply(
            Arc::new(ConsumerFactory {
                identity: FactoryIdentity::builtin("active-caller-consumer", "1"),
                service: Arc::clone(&service),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let before_call = runtime.resource_snapshot();

    captured_service(&service)
        .invoke(Message::new(b"active".to_vec()))
        .await
        .unwrap();

    let registered = runtime.resource_snapshot();
    assert_eq!(registered.effects.current, before_call.effects.current + 2);
    assert_eq!(
        registered.effect_transactions.current,
        before_call.effect_transactions.current + 2
    );
    assert!(order.lock().expect("cleanup order poisoned").is_empty());

    assert!(caller.dispose().await.is_clean());
    assert_eq!(
        *order.lock().expect("cleanup order poisoned"),
        vec!["second", "first"]
    );
    let retired = runtime.resource_snapshot();
    assert_eq!(retired.effects.current, provider_only.effects.current);
    assert_eq!(
        retired.effect_transactions.current,
        provider_only.effect_transactions.current
    );

    assert!(provider.dispose().await.is_clean());
    let empty = runtime.resource_snapshot();
    assert_eq!(empty.effects.current, 0);
    assert_eq!(empty.effect_transactions.current, 0);
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct RejectedCleanupEndpoint {
    error: Arc<Mutex<Option<MetaError>>>,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl ServiceEndpoint for RejectedCleanupEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        let cleanups = Arc::clone(&self.cleanups);
        let error = invocation
            .caller_effect()
            .expect("an Active service caller has caller-effect authority")
            .defer(
                "rejected caller cleanup",
                Box::new(move || {
                    Box::pin(async move {
                        cleanups.fetch_add(1, Ordering::AcqRel);
                        Ok(())
                    })
                }),
            )
            .expect_err("the Runtime-wide effect budget is already exhausted");
        *self.error.lock().expect("registration error poisoned") = Some(error);
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn rejected_active_registration_leaves_no_effect_wrapper() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_effects_per_fiber: 1,
            maximum_effects: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let error = Arc::new(Mutex::new(None));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("rejected-caller-provider", "1"),
                endpoint: Arc::new(RejectedCleanupEndpoint {
                    error: Arc::clone(&error),
                    cleanups: Arc::clone(&cleanups),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .apply(
            Arc::new(ConsumerFactory {
                identity: FactoryIdentity::builtin("rejected-caller-consumer", "1"),
                service: Arc::clone(&service),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let before = runtime.resource_snapshot();

    captured_service(&service)
        .invoke(Message::new(b"reject".to_vec()))
        .await
        .unwrap();

    assert_eq!(
        error.lock().expect("registration error poisoned").as_ref(),
        Some(&MetaError::CapacityExhausted {
            resource: "effects"
        })
    );
    assert_eq!(
        runtime.resource_snapshot().effects.current,
        before.effects.current
    );
    assert_eq!(
        runtime.resource_snapshot().effect_transactions.current,
        before.effect_transactions.current
    );
    assert_eq!(cleanups.load(Ordering::Acquire), 0);

    assert!(caller.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct AuthorityAttempt {
    view: Result<Option<(FiberId, FiberGeneration)>>,
    effect: Result<()>,
}

#[derive(Debug)]
struct BlockedAuthorityEndpoint {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    attempt: Arc<Mutex<Option<AuthorityAttempt>>>,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl ServiceEndpoint for BlockedAuthorityEndpoint {
    async fn serve(
        &self,
        invocation: InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        let view = invocation.caller().clone();
        let effect = invocation
            .caller_effect()
            .expect("a service caller has caller-effect authority")
            .clone();
        self.entered.notify_one();
        self.release.notified().await;
        let cleanups = Arc::clone(&self.cleanups);
        *self.attempt.lock().expect("authority attempt poisoned") = Some(AuthorityAttempt {
            view: view.owner(),
            effect: effect.defer(
                "retired caller cleanup",
                Box::new(move || {
                    Box::pin(async move {
                        cleanups.fetch_add(1, Ordering::AcqRel);
                        Ok(())
                    })
                }),
            ),
        });
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn caller_disposal_fences_authority_while_the_callback_is_still_live() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let attempt = Arc::new(Mutex::new(None));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("disposed-caller-provider", "1"),
                endpoint: Arc::new(BlockedAuthorityEndpoint {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                    attempt: Arc::clone(&attempt),
                    cleanups: Arc::clone(&cleanups),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .apply(
            Arc::new(ConsumerFactory {
                identity: FactoryIdentity::builtin("disposed-caller-consumer", "1"),
                service: Arc::clone(&service),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let generation = caller.snapshot().generation;
    let handle = captured_service(&service);
    let call = tokio::spawn(async move { handle.invoke(Message::new(b"dispose".to_vec())).await });
    entered.notified().await;

    let retiring_caller = caller.clone();
    let disposal = tokio::spawn(async move { retiring_caller.dispose().await });
    wait_unloading(&caller).await;
    release.notify_one();
    call.await.unwrap().unwrap();
    assert!(disposal.await.unwrap().is_clean());

    let stale = MetaError::StaleContext {
        fiber: caller.id(),
        generation,
    };
    let attempt = attempt
        .lock()
        .expect("authority attempt poisoned")
        .take()
        .expect("provider attempted both authorities");
    assert_eq!(attempt.view.unwrap_err(), stale);
    assert_eq!(attempt.effect.unwrap_err(), stale);
    assert_eq!(cleanups.load(Ordering::Acquire), 0);

    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn caller_reconfiguration_fences_authority_to_the_replaced_generation() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let attempt = Arc::new(Mutex::new(None));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("reconfigured-caller-provider", "1"),
                endpoint: Arc::new(BlockedAuthorityEndpoint {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                    attempt: Arc::clone(&attempt),
                    cleanups: Arc::clone(&cleanups),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .apply(
            Arc::new(ConsumerFactory {
                identity: FactoryIdentity::builtin("reconfigured-caller-consumer", "1"),
                service: Arc::clone(&service),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let old_generation = caller.snapshot().generation;
    let handle = captured_service(&service);
    let call =
        tokio::spawn(async move { handle.invoke(Message::new(b"reconfigure".to_vec())).await });
    entered.notified().await;

    let reconfigured_caller = caller.clone();
    let reconfiguration =
        tokio::spawn(async move { reconfigured_caller.reconfigure(Value::Null).await });
    wait_unloading(&caller).await;
    release.notify_one();
    call.await.unwrap().unwrap();
    let replacement = reconfiguration.await.unwrap().unwrap();
    assert_ne!(replacement.generation, old_generation);

    let stale = MetaError::StaleContext {
        fiber: caller.id(),
        generation: old_generation,
    };
    let attempt = attempt
        .lock()
        .expect("authority attempt poisoned")
        .take()
        .expect("provider attempted both authorities");
    assert_eq!(attempt.view.unwrap_err(), stale);
    assert_eq!(attempt.effect.unwrap_err(), stale);
    assert_eq!(cleanups.load(Ordering::Acquire), 0);

    assert!(caller.dispose().await.is_clean());
    drop(service.lock().expect("service capture poisoned").take());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct CancelledAuthorityEndpoint {
    entered: Arc<Notify>,
    attempt: Arc<Mutex<Option<AuthorityAttempt>>>,
    cleanups: Arc<AtomicUsize>,
}

#[async_trait]
impl ServiceEndpoint for CancelledAuthorityEndpoint {
    async fn serve(&self, invocation: InvocationContext, _: ProviderChannel<'_>) -> Result<()> {
        let view = invocation.caller().clone();
        let effect = invocation
            .caller_effect()
            .expect("a service caller has caller-effect authority")
            .clone();
        let cancellation = invocation.cancellation();
        self.entered.notify_one();
        cancellation.cancelled().await;
        let cleanups = Arc::clone(&self.cleanups);
        *self.attempt.lock().expect("authority attempt poisoned") = Some(AuthorityAttempt {
            view: view.owner(),
            effect: effect.defer(
                "cancelled caller cleanup",
                Box::new(move || {
                    Box::pin(async move {
                        cleanups.fetch_add(1, Ordering::AcqRel);
                        Ok(())
                    })
                }),
            ),
        });
        Ok(())
    }
}

#[tokio::test]
async fn call_cancellation_fences_caller_view_and_effect_before_callback_terminal() {
    let runtime = Runtime::default();
    let entered = Arc::new(Notify::new());
    let attempt = Arc::new(Mutex::new(None));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let provider = runtime
        .root()
        .apply(
            Arc::new(ProviderFactory {
                identity: FactoryIdentity::builtin("cancelled-caller-provider", "1"),
                endpoint: Arc::new(CancelledAuthorityEndpoint {
                    entered: Arc::clone(&entered),
                    attempt: Arc::clone(&attempt),
                    cleanups: Arc::clone(&cleanups),
                }),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let service = Arc::new(Mutex::new(None));
    let caller = runtime
        .root()
        .apply(
            Arc::new(ConsumerFactory {
                identity: FactoryIdentity::builtin("cancelled-caller-consumer", "1"),
                service: Arc::clone(&service),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let generation = caller.snapshot().generation;
    let mut call = captured_service(&service).open().unwrap();
    call.send(Message::new(b"cancel".to_vec())).await.unwrap();
    entered.notified().await;
    let before = runtime.resource_snapshot();

    call.cancel();
    assert_eq!(call.recv().await.unwrap_err(), MetaError::Cancelled);

    let stale = MetaError::StaleContext {
        fiber: caller.id(),
        generation,
    };
    let attempt = attempt
        .lock()
        .expect("authority attempt poisoned")
        .take()
        .expect("provider attempted both authorities");
    assert_eq!(attempt.view.unwrap_err(), stale);
    assert_eq!(attempt.effect.unwrap_err(), stale);
    assert_eq!(
        runtime.resource_snapshot().effects.current,
        before.effects.current
    );
    assert_eq!(
        runtime.resource_snapshot().effect_transactions.current,
        before.effect_transactions.current
    );
    assert_eq!(cleanups.load(Ordering::Acquire), 0);

    assert!(caller.dispose().await.is_clean());
    assert!(provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[derive(Debug)]
struct RetainedRootInvocationHandler {
    retained: Arc<Mutex<Option<InvocationContext>>>,
}

#[async_trait]
impl EventHandler for RetainedRootInvocationHandler {
    async fn handle(
        &self,
        invocation: InvocationContext,
        value: Arc<Value>,
    ) -> Result<EventOutcome> {
        assert!(invocation.caller_effect().is_none());
        assert_eq!(invocation.caller().owner()?, None);
        *self.retained.lock().expect("event invocation poisoned") = Some(invocation.clone());
        Ok(EventOutcome::Continue(value.as_ref().clone()))
    }
}

#[derive(Debug)]
struct RootInvocationListenerFactory {
    identity: FactoryIdentity,
    retained: Arc<Mutex<Option<InvocationContext>>>,
}

#[async_trait]
impl PluginFactory for RootInvocationListenerFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context().on(
            "root-caller-authority",
            Arc::new(RetainedRootInvocationHandler {
                retained: Arc::clone(&self.retained),
            }),
            EventOptions::default(),
        )?;
        Ok(())
    }
}

#[tokio::test]
async fn retained_root_event_invocation_has_no_caller_effect_after_callback() {
    let runtime = Runtime::default();
    let retained = Arc::new(Mutex::new(None));
    let listener = runtime
        .root()
        .apply(
            Arc::new(RootInvocationListenerFactory {
                identity: FactoryIdentity::builtin("root-invocation-listener", "1"),
                retained: Arc::clone(&retained),
            }),
            Value::Null,
        )
        .await
        .unwrap();

    runtime
        .root()
        .dispatch("root-caller-authority", DispatchMode::Emit, Value::Null)
        .await
        .unwrap();

    let invocation = retained
        .lock()
        .expect("event invocation poisoned")
        .take()
        .expect("handler retained its invocation");
    assert!(invocation.caller_effect().is_none());
    assert_eq!(
        invocation.caller().owner().unwrap_err(),
        MetaError::Cancelled
    );

    assert!(listener.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}
