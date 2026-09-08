use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Context, Emit, EmitEventHandler, LocalEvent, LocalEventOptions, PluginFactory,
    PreparedActivation, Result, Runtime, RuntimeLimits,
};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

#[path = "support/resolver.rs"]
mod resolver;
use resolver::resolved;

struct Notice;
impl LocalEvent for Notice {
    const KEY: &'static str = "test.composition-order";
    type Value = ();
    type Error = Infallible;
    type Mode = Emit;
}
#[derive(Debug)]
struct Record(&'static str, Arc<Mutex<Vec<&'static str>>>);
impl EmitEventHandler<Notice> for Record {
    fn handle(&self, (): &()) {
        self.1.lock().unwrap().push(self.0);
    }
}
#[derive(Debug)]
struct Capture(Arc<Mutex<Option<Context>>>);
#[async_trait]
impl PluginFactory for Capture {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        *self.0.lock().unwrap() = Some(plan.context().clone());
        Ok(())
    }
}
async fn owner(parent: &Context) -> (rsi_meta::FiberHandle, Context) {
    let captured = Arc::new(Mutex::new(None));
    let fiber = parent
        .apply(resolved(Arc::new(Capture(captured.clone()))), Value::Null)
        .await
        .unwrap();
    let context = captured.lock().unwrap().take().unwrap();
    (fiber, context)
}

#[tokio::test]
async fn local_listener_order_uses_composition_position_instead_of_registration_timing() {
    let runtime = Runtime::new(RuntimeLimits::default()).unwrap();
    let root = runtime.root();
    let (first, first_context) = owner(&root).await;
    let (second, second_context) = owner(&root).await;
    let log = Arc::new(Mutex::new(Vec::new()));
    second_context
        .on_emit::<Notice, _>(
            Arc::new(Record("second", log.clone())),
            LocalEventOptions::default(),
        )
        .unwrap();
    first_context
        .on_emit::<Notice, _>(
            Arc::new(Record("first", log.clone())),
            LocalEventOptions::default(),
        )
        .unwrap();
    root.dispatch_local::<Notice>(()).unwrap();
    assert_eq!(*log.lock().unwrap(), ["first", "second"]);
    assert!(second.dispose().await.is_clean());
    assert!(first.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

fn listen(
    context: &Context,
    label: &'static str,
    log: &Arc<Mutex<Vec<&'static str>>>,
    prepend: bool,
) {
    context
        .on_emit::<Notice, _>(
            Arc::new(Record(label, log.clone())),
            LocalEventOptions {
                prepend,
                once: false,
            },
        )
        .unwrap();
}
fn observed(root: &Context, log: &Arc<Mutex<Vec<&'static str>>>) -> Vec<&'static str> {
    root.dispatch_local::<Notice>(()).unwrap();
    std::mem::take(&mut *log.lock().unwrap())
}

#[tokio::test]
async fn reserved_positions_survive_rebuild_and_reorder_both_lanes_without_reactivation() {
    let runtime = Runtime::new(RuntimeLimits::default()).unwrap();
    let root = runtime.root();
    let a = root.child_position().unwrap();
    let b = root.child_position().unwrap();
    let (second, second_context) = owner(&root.with_child_position(&b).unwrap()).await;
    let (first, first_context) = owner(&root.with_child_position(&a).unwrap()).await;
    let log = Arc::new(Mutex::new(Vec::new()));
    listen(&second_context, "bp", &log, true);
    listen(&second_context, "ba", &log, false);
    listen(&first_context, "ap", &log, true);
    listen(&first_context, "aa", &log, false);
    assert_eq!(observed(&root, &log), ["bp", "ap", "aa", "ba"]);
    let before = [first.snapshot().generation, second.snapshot().generation];
    root.reorder_children(&[b.clone(), a.clone()]).unwrap();
    assert_eq!(observed(&root, &log), ["ap", "bp", "ba", "aa"]);
    assert_eq!(
        [first.snapshot().generation, second.snapshot().generation],
        before
    );
    assert!(first.dispose().await.is_clean());
    let (replacement, replacement_context) = owner(&root.with_child_position(&a).unwrap()).await;
    listen(&replacement_context, "a2p", &log, true);
    listen(&replacement_context, "a2a", &log, false);
    assert_eq!(observed(&root, &log), ["a2p", "bp", "ba", "a2a"]);
    assert_ne!(replacement.snapshot().generation, before[0]);
    assert_eq!(second.snapshot().generation, before[1]);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn positions_reject_foreign_parent_stale_owner_duplicate_order_and_double_occupancy() {
    let runtime = Runtime::new(RuntimeLimits::default()).unwrap();
    let foreign = Runtime::new(RuntimeLimits::default()).unwrap();
    let root = runtime.root();
    let (parent, context) = owner(&root).await;
    let position = context.child_position().unwrap();
    assert!(root.with_child_position(&position).is_err());
    assert!(
        foreign
            .root()
            .reorder_children(std::slice::from_ref(&position))
            .is_err()
    );
    assert!(
        context
            .reorder_children(&[position.clone(), position.clone()])
            .is_err()
    );
    let selected = context.with_child_position(&position).unwrap();
    let (child, _) = owner(&selected).await;
    let captured = Arc::new(Mutex::new(None));
    assert!(
        selected
            .apply(resolved(Arc::new(Capture(captured.clone()))), Value::Null)
            .await
            .is_err()
    );
    assert!(captured.lock().unwrap().is_none());
    assert!(child.dispose().await.is_clean());
    assert!(parent.dispose().await.is_clean());
    assert!(context.with_child_position(&position).is_err());
    assert!(context.reorder_children(&[]).is_err());
    assert!(context.child_position().is_err());
    assert!(runtime.shutdown().await.is_clean());
    assert!(foreign.shutdown().await.is_clean());
}

#[tokio::test]
async fn position_metadata_capacity_is_reusable_and_does_not_reserve_fibers_or_block_shutdown() {
    let limits = RuntimeLimits {
        topology: rsi_meta::TopologyLimits {
            maximum_fibers: 1,
            maximum_composition_positions: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let runtime = Runtime::new(limits).unwrap();
    let root = runtime.root();
    let a = root.child_position().unwrap();
    let b = root.child_position().unwrap();
    assert!(root.child_position().is_err());
    let (first, _) = owner(&root.with_child_position(&a).unwrap()).await;
    assert!(first.dispose().await.is_clean());
    let (second, _) = owner(&root.with_child_position(&b).unwrap()).await;
    assert!(second.dispose().await.is_clean());
    drop(a);
    let replacement = root.child_position().unwrap();
    assert!(root.child_position().is_err());
    assert!(runtime.shutdown().await.is_clean());
    assert!(root.with_child_position(&replacement).is_err());
}

#[tokio::test]
async fn nested_parent_reorder_and_compensation_leave_another_parents_order_intact() {
    let runtime = Runtime::new(RuntimeLimits::default()).unwrap();
    let root = runtime.root();
    let (_, left) = owner(&root).await;
    let (_, right) = owner(&root).await;
    let la = left.child_position().unwrap();
    let lb = left.child_position().unwrap();
    let ra = right.child_position().unwrap();
    let rb = right.child_position().unwrap();
    let (_, lac) = owner(&left.with_child_position(&la).unwrap()).await;
    let (_, lbc) = owner(&left.with_child_position(&lb).unwrap()).await;
    let (_, rac) = owner(&right.with_child_position(&ra).unwrap()).await;
    let (_, rbc) = owner(&right.with_child_position(&rb).unwrap()).await;
    let log = Arc::new(Mutex::new(Vec::new()));
    for (context, label) in [(&rbc, "rb"), (&lbc, "lb"), (&rac, "ra"), (&lac, "la")] {
        listen(context, label, &log, false);
    }
    assert_eq!(observed(&root, &log), ["la", "lb", "ra", "rb"]);
    left.reorder_children(&[lb.clone(), la.clone()]).unwrap();
    right.reorder_children(&[rb, ra]).unwrap();
    assert_eq!(observed(&root, &log), ["lb", "la", "rb", "ra"]);
    left.reorder_children(&[la, lb]).unwrap();
    assert_eq!(observed(&root, &log), ["la", "lb", "rb", "ra"]);
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct Registrar {
    next: std::sync::atomic::AtomicU64,
    entries: Mutex<std::collections::BTreeMap<u64, rsi_meta::RegistrationPosition>>,
}
impl Registrar {
    fn register(
        self: &Arc<Self>,
        context: &rsi_meta::RegistrationContext,
    ) -> Result<rsi_meta::RegistrationLease> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let weak = Arc::downgrade(self);
        let ((), lease) = context.register(
            "fixture contribution",
            move || {
                if let Some(registrar) = weak.upgrade() {
                    registrar.entries.lock().unwrap().remove(&id);
                }
                Ok(())
            },
            |position| {
                self.entries.lock().unwrap().insert(id, position);
                Ok(())
            },
        )?;
        Ok(lease)
    }
    fn live(&self) -> usize {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|entry| entry.is_admitting())
            .count()
    }
}

#[derive(Debug)]
struct RegisterOnLoad {
    registrar: Arc<Registrar>,
    retained: Arc<Mutex<Option<rsi_meta::RegistrationLease>>>,
    fail: bool,
}
#[async_trait]
impl PluginFactory for RegisterOnLoad {
    fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let lease = self
            .registrar
            .register(&plan.context().registration_context()?)?;
        assert_eq!(self.registrar.live(), 1);
        *self.retained.lock().unwrap() = Some(lease);
        if self.fail {
            return Err(rsi_meta::MetaError::Activation("fixture rollback".into()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn local_registration_joins_loading_rollback_even_when_its_lease_escapes() {
    let runtime = Runtime::default();
    let registrar = Arc::new(Registrar::default());
    let retained = Arc::new(Mutex::new(None));
    let fiber = runtime
        .root()
        .apply(
            resolved(Arc::new(RegisterOnLoad {
                registrar: registrar.clone(),
                retained: retained.clone(),
                fail: true,
            })),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        fiber.snapshot().state,
        rsi_meta::FiberState::Failed(_)
    ));
    assert_eq!(registrar.live(), 0);
    let escaped = retained.lock().unwrap().take().unwrap();
    assert!(escaped.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct RetainedRegistration {
    credential: Mutex<Option<rsi_meta::RegistrationContext>>,
    lease: Mutex<Option<rsi_meta::RegistrationLease>>,
}
#[async_trait]
impl PluginFactory for RetainedRegistration {
    fn prepare(&self, config: &Value) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let credential = plan.context().registration_context()?;
        if plan.config().as_bool() == Some(true) {
            let ((), lease) = credential.register("retained setup", || Ok(()), |_| Ok(()))?;
            *self.lease.lock().unwrap() = Some(lease);
        } else {
            *self.credential.lock().unwrap() = Some(credential);
        }
        Ok(())
    }
}

#[tokio::test]
async fn runtime_owned_registration_lease_does_not_form_a_last_owner_cycle() {
    for loading in [true, false] {
        let runtime = Runtime::default();
        let factory = Arc::new(RetainedRegistration::default());
        let weak = Arc::downgrade(&factory);
        let fiber = runtime
            .root()
            .apply(resolved(factory.clone()), Value::Bool(loading))
            .await
            .unwrap();
        if !loading {
            let credential = factory.credential.lock().unwrap().take().unwrap();
            let ((), lease) = credential
                .register("retained active", || Ok(()), |_| Ok(()))
                .unwrap();
            *factory.lease.lock().unwrap() = Some(lease);
        }
        drop(factory);
        drop(fiber);
        drop(runtime);
        assert!(
            weak.upgrade().is_none(),
            "registration lease retained its Runtime (loading={loading})"
        );
    }
}

#[derive(Debug)]
struct PanickingCleanupPayload;
impl Drop for PanickingCleanupPayload {
    fn drop(&mut self) {
        panic!("nested cleanup payload destructor");
    }
}

#[tokio::test]
async fn registration_cleanup_failure_is_bounded_retained_and_never_retried() {
    for panic in [false, true] {
        let mut limits = RuntimeLimits::default();
        limits.payloads.maximum_diagnostic_bytes = 128;
        let runtime = Runtime::new(limits).unwrap();
        let (_, context) = owner(&runtime.root()).await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let undo_calls = calls.clone();
        let ((), lease) = context
            .registration_context()
            .unwrap()
            .register(
                "failing contribution undo",
                move || {
                    undo_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if panic {
                        std::panic::panic_any(PanickingCleanupPayload);
                    }
                    Err("界".repeat(1024))
                },
                |_| Ok(()),
            )
            .unwrap();
        let report = lease.dispose().await;
        assert!(!report.is_clean());
        assert_eq!(report.failures().len(), 1);
        assert!(report.failures()[0].error.len() <= 128);
        if panic {
            assert!(
                report.failures()[0]
                    .error
                    .contains("payload destruction panicked")
            );
        }
        assert!(runtime.snapshot().terminal.is_some());
        assert!(!runtime.shutdown().await.is_clean());
        drop(lease);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn active_registration_drop_and_owner_retirement_share_exact_undo_and_reject_stale_credentials()
 {
    let runtime = Runtime::default();
    let root = runtime.root();
    assert!(root.registration_context().is_err());
    let (fiber, context) = owner(&root).await;
    let credential = context.registration_context().unwrap();
    let registrar = Arc::new(Registrar::default());
    let first = registrar.register(&credential).unwrap();
    let second = registrar.register(&credential).unwrap();
    assert_eq!(registrar.live(), 2);
    drop(first);
    assert_eq!(registrar.live(), 1);
    let token = registrar
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(token.is_admitting());
    assert!(fiber.dispose().await.is_clean());
    assert_eq!(registrar.live(), 0);
    assert!(!token.is_admitting());
    assert!(registrar.register(&credential).is_err());
    assert!(second.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn failed_registration_publication_undo_runs_once_and_does_not_replace_existing_entries() {
    let runtime = Runtime::default();
    let (_, context) = owner(&runtime.root()).await;
    let credential = context.registration_context().unwrap();
    let undone = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cleanup = undone.clone();
    let result: Result<((), rsi_meta::RegistrationLease)> = credential.register(
        "failed publication",
        move || {
            cleanup.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        |_| Err(rsi_meta::MetaError::InvalidInput("duplicate entry".into())),
    );
    assert!(result.is_err());
    assert_eq!(undone.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(undone.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn contribution_order_capture_is_consistent_across_reorder_and_rejects_other_runtimes() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let a = root.child_position().unwrap();
    let b = root.child_position().unwrap();
    let (_, ac) = owner(&root.with_child_position(&a).unwrap()).await;
    let (_, bc) = owner(&root.with_child_position(&b).unwrap()).await;
    let registrar = Arc::new(Registrar::default());
    let _b = registrar
        .register(&bc.registration_context().unwrap())
        .unwrap();
    let _a = registrar
        .register(&ac.registration_context().unwrap())
        .unwrap();
    let positions: Vec<_> = registrar
        .entries
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    let captured = rsi_meta::RegistrationOrderSnapshot::capture(&positions).unwrap();
    assert!(captured.is_current());
    assert!(captured.ranks()[1] < captured.ranks()[0]);
    root.reorder_children(&[b, a]).unwrap();
    assert!(!captured.is_current());
    assert!(captured.ranks()[1] < captured.ranks()[0]);
    let current = rsi_meta::RegistrationOrderSnapshot::capture(&positions).unwrap();
    assert!(current.ranks()[0] < current.ranks()[1]);
    let other = Runtime::default();
    let (_, other_context) = owner(&other.root()).await;
    let _other = registrar
        .register(&other_context.registration_context().unwrap())
        .unwrap();
    let mixed: Vec<_> = registrar
        .entries
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert!(rsi_meta::RegistrationOrderSnapshot::capture(&mixed).is_err());
    assert!(other.shutdown().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn business_order_can_replace_the_owner_local_registration_tie_break() {
    let runtime = Runtime::default();
    let (_, context) = owner(&runtime.root()).await;
    let registrar = Arc::new(Registrar::default());
    let credential = context.registration_context().unwrap();
    let _first = registrar.register(&credential).unwrap();
    let _second = registrar.register(&credential).unwrap();
    let positions = registrar
        .entries
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let order = rsi_meta::RegistrationOrderSnapshot::capture(&positions).unwrap();
    assert!(order.ranks()[0] < order.ranks()[1]);
    assert_eq!(
        order.ranks()[0].compare_position(&order.ranks()[1]),
        std::cmp::Ordering::Equal
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn active_publication_racing_retirement_cannot_reappear_after_cleanup() {
    let runtime = Runtime::default();
    let (fiber, context) = owner(&runtime.root()).await;
    let credential = context.registration_context().unwrap();
    let published = Arc::new(Mutex::new(None));
    let undo_target = published.clone();
    let target = published.clone();
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate_control = gate.clone();
    let (entered, entry) = tokio::sync::oneshot::channel();
    let registration = tokio::task::spawn_blocking(move || {
        credential.register(
            "racing publication",
            move || {
                *undo_target.lock().unwrap() = None;
                Ok(())
            },
            move |position| {
                let _ = entered.send(());
                let (released, ready) = &*gate;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = ready.wait(released).unwrap();
                }
                *target.lock().unwrap() = Some(position);
                Ok(())
            },
        )
    });
    entry.await.unwrap();
    let retiring = fiber.clone();
    let retirement = tokio::spawn(async move { retiring.dispose().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fiber.snapshot().state != rsi_meta::FiberState::Unloading {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(context.registration_context().is_err());
    *gate_control.0.lock().unwrap() = true;
    gate_control.1.notify_all();
    let ((), lease) = registration.await.unwrap().unwrap();
    assert!(retirement.await.unwrap().is_clean());
    assert!(published.lock().unwrap().is_none());
    assert!(lease.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
