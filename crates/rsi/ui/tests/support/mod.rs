use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FiberHandle, FiberState, LocalContract, PluginFactory,
    PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_ui::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Semaphore;

#[derive(Debug)]
struct Label;
impl LocalContract for Label {
    const KEY: &'static str = "test.ui.label";
    type Service = String;
}
#[derive(Debug)]
struct LabelFactory(&'static str);
#[async_trait]
impl PluginFactory for LabelFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<Label>(Arc::new(self.0.into()))?;
        plan.defer(
            "withdraw label",
            Box::new(move || {
                Box::pin(async {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Addon {
    name: &'static str,
    calls: AtomicUsize,
    gate: Semaphore,
    lease: Mutex<Option<Arc<ContributionLease>>>,
    bad_view: bool,
    read: bool,
    fail: bool,
}
impl Addon {
    fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            calls: AtomicUsize::new(0),
            gate: Semaphore::new(0),
            lease: Mutex::new(None),
            bad_view: false,
            read: false,
            fail: false,
        })
    }
    fn view(&self, target: &Context) -> UiView {
        UiView {
            title: self.name.into(),
            elements: vec![
                UiElement::Text {
                    text: target
                        .lookup_local::<Label>()
                        .map_or_else(|| "global".into(), |label| label.as_ref().clone()),
                },
                UiElement::Button {
                    action: if self.bad_view { "foreign" } else { "run" }.into(),
                    label: "Run once".into(),
                    value: serde_json::Value::Null,
                },
            ],
        }
    }
}
impl SurfaceRenderer for Addon {
    fn render(&self, context: &Context) -> Result<UiView> {
        Ok(self.view(context))
    }
}
impl BlockRenderer for Addon {
    fn render(&self, context: &Context, input: &BlockInput<'_>) -> Result<Option<UiView>> {
        Ok((input.text == "match").then(|| self.view(context)))
    }
}
#[derive(Debug)]
struct Action(Arc<Addon>);
impl UiAction for Action {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        let addon = self.0.clone();
        Box::pin(async move {
            if !input.value.is_null() || !input.fields.is_empty() {
                return Err(UiError::Invalid("action accepts no payload".into()));
            }
            // Capture the exact Local values before I/O. Meta intentionally rejects
            // new lookups from a retiring Context; already acquired values stay owned.
            let view = addon.view(target.context());
            addon.calls.fetch_add(1, Ordering::SeqCst);
            // A deliberately blocked admitted mutation does not abandon its side effect
            // merely because its contribution/target or response waiter goes away.
            let permit = if addon.read {
                tokio::select! { biased;
                    () = target.view_closed() => return Err(UiError::Retired),
                    () = target.cancelled() => return Err(UiError::Retired),
                    permit = addon.gate.acquire() => permit.unwrap(),
                }
            } else {
                addon.gate.acquire().await.unwrap()
            };
            permit.forget();
            Ok(view)
        })
    }
}
#[derive(Debug)]
struct AddonFactory(Arc<Addon>);
#[async_trait]
impl PluginFactory for AddonFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let addon = &self.0;
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: addon.name.into(),
                    surfaces: vec![SurfaceContribution {
                        name: "panel".into(),
                        title: addon.name.into(),
                        target: TargetKind::Surface,
                        renderer: addon.clone(),
                    }],
                    actions: vec![ActionContribution {
                        name: "run".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(Action(addon.clone())),
                    }],
                    renderers: vec![BlockRendererContribution {
                        name: "block".into(),
                        target: TargetKind::Surface,
                        renderer: addon.clone(),
                    }],
                },
            )
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        let lease = Arc::new(lease);
        *addon.lease.lock().unwrap() = Some(lease.clone());
        plan.defer(
            "release addon lease",
            Box::new(move || {
                Box::pin(async {
                    drop(lease);
                    Ok(())
                })
            }),
        )?;
        if addon.fail {
            return Err(rsi_meta::MetaError::Activation(
                "failure after publication".into(),
            ));
        }
        Ok(())
    }
}
async fn apply(
    context: &Context,
    name: &str,
    factory: Arc<dyn PluginFactory>,
    config: ConfigValue,
) -> FiberHandle {
    context
        .apply(
            ResolvedFactory::linked(name, "test", UpdateMode::Replayable, factory),
            config,
        )
        .await
        .unwrap()
}
async fn fixture(execution: rsi_meta::Execution) -> (Runtime, Arc<Ui>) {
    let runtime = Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution).unwrap();
    let fiber = apply(
        &runtime.root(),
        "ui",
        Arc::new(UiFactory),
        ConfigValue::Null,
    )
    .await;
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    let ui = runtime.root().lookup_local::<UiContract>().unwrap();
    (runtime, ui)
}
async fn target(root: &Context, label: &'static str) -> (FiberHandle, Arc<UiTarget>) {
    let context = root
        .clone()
        .isolate_local_fresh::<UiTargetContract>()
        .unwrap()
        .0
        .isolate_local_fresh::<Label>()
        .unwrap()
        .0;
    apply(
        &context,
        label,
        Arc::new(LabelFactory(label)),
        ConfigValue::Null,
    )
    .await;
    let fiber = apply(
        &context,
        "target",
        Arc::new(UiTargetFactory),
        serde_json::json!("surface"),
    )
    .await;
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    (fiber, context.lookup_local::<UiTargetContract>().unwrap())
}
fn action(ui: &Ui, target: &UiTarget) -> UiReference {
    let menu = ui.surfaces(target).unwrap();
    ui.surface(&menu[0].reference).unwrap().actions["run"].clone()
}
async fn entered(execution: &rsi_meta::Execution, addon: &Addon, count: usize) {
    execution
        .deadline_after(Duration::from_secs(2))
        .timeout(async {
            while addon.calls.load(Ordering::SeqCst) < count {
                execution.sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
}
pub async fn declaration_reorder_drives_surfaces_and_renderers_with_actual_target_mappings(
    execution: rsi_meta::Execution,
) {
    let (runtime, ui) = fixture(execution.clone()).await;
    let root = runtime.root();
    let first = root.child_position().unwrap();
    let second = root.child_position().unwrap();
    let b = Addon::new("b");
    let a = Addon::new("a");
    apply(
        &root.with_child_position(&second).unwrap(),
        "b",
        Arc::new(AddonFactory(b)),
        ConfigValue::Null,
    )
    .await;
    apply(
        &root.with_child_position(&first).unwrap(),
        "a",
        Arc::new(AddonFactory(a)),
        ConfigValue::Null,
    )
    .await;
    let (_, left) = target(&root, "left").await;
    let (_, right) = target(&root, "right").await;
    assert_eq!(
        ui.surfaces(&left)
            .unwrap()
            .iter()
            .map(|entry| entry.title.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    let sources = rsi_conversation::SourceIndex::default();
    let input = BlockInput {
        key: "key",
        text: "match",
        tool: None,
        sources: &sources,
    };
    let view = ui.block(&right, &input).unwrap().unwrap();
    assert_eq!(view.view.title, "a");
    assert!(matches!(&view.view.elements[0], UiElement::Text { text } if text == "right"));
    root.reorder_children(&[second, first]).unwrap();
    assert_eq!(ui.surfaces(&left).unwrap()[0].title, "b");
    assert_eq!(ui.block(&left, &input).unwrap().unwrap().view.title, "b");
    assert!(runtime.shutdown().await.is_clean());
    assert!(ui.surfaces(&left).is_err());
}
pub async fn dropped_waiter_and_contribution_retirement_drain_admitted_mutation_once(
    execution: rsi_meta::Execution,
) {
    let (runtime, ui) = fixture(execution.clone()).await;
    let addon = Addon::new("addon");
    let fiber = apply(
        &runtime.root(),
        "addon",
        Arc::new(AddonFactory(addon.clone())),
        ConfigValue::Null,
    )
    .await;
    let (_, target) = target(&runtime.root(), "one").await;
    let reference = action(&ui, &target);
    drop(ui.invoke(&reference, ActionInput::default()));
    entered(&execution, &addon, 1).await;
    let mut disposal = Box::pin(fiber.dispose());
    assert!(
        execution
            .deadline_after(Duration::from_millis(30))
            .timeout(disposal.as_mut())
            .await
            .is_err()
    );
    assert!(matches!(
        ui.invoke(&reference, ActionInput::default()).await,
        Err(UiError::Retired)
    ));
    assert!(ui.surfaces(&target).unwrap().is_empty());
    addon.gate.add_permits(1);
    assert!(
        execution
            .deadline_after(Duration::from_secs(2))
            .timeout(disposal)
            .await
            .unwrap()
            .is_clean()
    );
    assert_eq!(addon.calls.load(Ordering::SeqCst), 1);
    assert!(runtime.shutdown().await.is_clean());
}
pub async fn replacement_target_foreign_application_and_business_payload_are_fenced(
    execution: rsi_meta::Execution,
) {
    let (runtime, ui) = fixture(execution.clone()).await;
    let addon = Addon::new("addon");
    apply(
        &runtime.root(),
        "addon",
        Arc::new(AddonFactory(addon.clone())),
        ConfigValue::Null,
    )
    .await;
    let (fiber, old) = target(&runtime.root(), "old").await;
    let reference = action(&ui, &old);
    let work = ui.invoke(&reference, ActionInput::default());
    entered(&execution, &addon, 1).await;
    let mut disposal = Box::pin(fiber.dispose());
    assert!(
        execution
            .deadline_after(Duration::from_millis(30))
            .timeout(disposal.as_mut())
            .await
            .is_err()
    );
    let (_, new) = target(&runtime.root(), "new").await;
    let new_reference = action(&ui, &new);
    assert_ne!(reference.target, new_reference.target);
    assert!(matches!(
        ui.invoke(&reference, ActionInput::default()).await,
        Err(UiError::Retired)
    ));
    let mut foreign = new_reference.clone();
    foreign.application.push('x');
    assert!(matches!(
        ui.invoke(&foreign, ActionInput::default()).await,
        Err(UiError::Retired)
    ));
    assert!(
        ui.invoke(
            &new_reference,
            ActionInput {
                value: serde_json::json!("unexpected"),
                ..ActionInput::default()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(addon.calls.load(Ordering::SeqCst), 1);
    addon.gate.add_permits(1);
    let result = work.await.unwrap();
    assert!(matches!(&result.view.elements[0], UiElement::Text { text } if text == "old"));
    assert!(disposal.await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
pub async fn action_capacity_is_not_released_when_response_waiters_are_dropped(
    execution: rsi_meta::Execution,
) {
    let (runtime, ui) = fixture(execution.clone()).await;
    let addon = Addon::new("addon");
    apply(
        &runtime.root(),
        "addon",
        Arc::new(AddonFactory(addon.clone())),
        ConfigValue::Null,
    )
    .await;
    let (_, target) = target(&runtime.root(), "one").await;
    let reference = action(&ui, &target);
    for _ in 0..MAXIMUM_ACTIONS {
        drop(ui.invoke(&reference, ActionInput::default()));
    }
    entered(&execution, &addon, MAXIMUM_ACTIONS).await;
    assert!(matches!(
        ui.invoke(&reference, ActionInput::default()).await,
        Err(UiError::Capacity)
    ));
    let oversized = ActionInput {
        value: serde_json::json!("x".repeat(MAXIMUM_INPUT_BYTES)),
        ..ActionInput::default()
    };
    assert!(matches!(
        ui.invoke(&reference, oversized).await,
        Err(UiError::Invalid(_))
    ));
    addon.gate.add_permits(MAXIMUM_ACTIONS);
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(addon.calls.load(Ordering::SeqCst), MAXIMUM_ACTIONS);
}
pub async fn duplicate_and_failed_activation_preserve_existing_bundle_and_reject_foreign_buttons(
    execution: rsi_meta::Execution,
) {
    let (runtime, ui) = fixture(execution.clone()).await;
    let a = Addon::new("same");
    apply(
        &runtime.root(),
        "a",
        Arc::new(AddonFactory(a)),
        ConfigValue::Null,
    )
    .await;
    let b = apply(
        &runtime.root(),
        "b",
        Arc::new(AddonFactory(Addon::new("same"))),
        ConfigValue::Null,
    )
    .await;
    assert_ne!(b.snapshot().state, FiberState::Active);
    let mut fail = Addon::new("failed");
    Arc::get_mut(&mut fail).unwrap().fail = true;
    let failed = apply(
        &runtime.root(),
        "fail",
        Arc::new(AddonFactory(fail)),
        ConfigValue::Null,
    )
    .await;
    assert_ne!(failed.snapshot().state, FiberState::Active);
    let (_, target) = target(&runtime.root(), "one").await;
    assert_eq!(ui.surfaces(&target).unwrap().len(), 1);
    let mut bad = Addon::new("bad");
    Arc::get_mut(&mut bad).unwrap().bad_view = true;
    apply(
        &runtime.root(),
        "bad",
        Arc::new(AddonFactory(bad)),
        ConfigValue::Null,
    )
    .await;
    let menu = ui.surfaces(&target).unwrap();
    assert!(matches!(
        ui.surface(
            &menu
                .iter()
                .find(|entry| entry.title == "bad")
                .unwrap()
                .reference
        ),
        Err(UiError::Invalid(_))
    ));
    assert!(
        ui.surface(
            &menu
                .iter()
                .find(|entry| entry.title == "same")
                .unwrap()
                .reference
        )
        .is_ok()
    );
    assert!(runtime.shutdown().await.is_clean());
}

pub async fn independent_registry_and_reactivated_bundle_never_reuse_old_references(
    execution: rsi_meta::Execution,
) {
    let (runtime, ui) = fixture(execution).await;
    let root = runtime.root();
    let addon = Addon::new("addon");
    let old_fiber = apply(
        &root,
        "addon",
        Arc::new(AddonFactory(addon.clone())),
        ConfigValue::Null,
    )
    .await;
    let (_, first) = target(&root, "first").await;
    let old = action(&ui, &first);
    let foreign_root = root.clone().isolate_local_fresh::<UiContract>().unwrap().0;
    apply(
        &foreign_root,
        "foreign-ui",
        Arc::new(UiFactory),
        ConfigValue::Null,
    )
    .await;
    let foreign = foreign_root.lookup_local::<UiContract>().unwrap();
    let (_, foreign_target) = target(&foreign_root, "foreign").await;
    assert!(ui.surfaces(&foreign_target).is_err());
    assert!(foreign.surfaces(&first).is_err());
    assert!(!foreign.is_current(&old));
    assert!(old_fiber.dispose().await.is_clean());
    apply(
        &root,
        "replacement",
        Arc::new(AddonFactory(Addon::new("addon"))),
        ConfigValue::Null,
    )
    .await;
    let current = action(&ui, &first);
    assert_ne!(old.contribution, current.contribution);
    assert!(!ui.is_current(&old));
    assert!(ui.is_current(&current));
    assert!(matches!(
        ui.invoke(&old, ActionInput::default()).await,
        Err(UiError::Retired)
    ));
    assert_eq!(addon.calls.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}

pub async fn presentation_close_signals_reads_and_preserves_admitted_mutations(
    execution: rsi_meta::Execution,
) {
    let (runtime, ui) = fixture(execution.clone()).await;
    let mutation = Addon::new("mutation");
    let fiber = apply(
        &runtime.root(),
        "mutation",
        Arc::new(AddonFactory(mutation.clone())),
        ConfigValue::Null,
    )
    .await;
    let (_, target) = target(&runtime.root(), "one").await;
    let reference = action(&ui, &target);
    let stop = tokio_util::sync::CancellationToken::new();
    let mut pending = ui.invoke_in_view(&reference, ActionInput::default(), stop.clone());
    entered(&execution, &mutation, 1).await;
    stop.cancel();
    assert!(
        execution
            .deadline_after(Duration::from_millis(30))
            .timeout(&mut pending)
            .await
            .is_err()
    );
    mutation.gate.add_permits(1);
    pending.await.unwrap();
    assert!(fiber.dispose().await.is_clean());
    let mut read = Addon::new("read");
    Arc::get_mut(&mut read).unwrap().read = true;
    apply(
        &runtime.root(),
        "read",
        Arc::new(AddonFactory(read.clone())),
        ConfigValue::Null,
    )
    .await;
    let reference = action(&ui, &target);
    let stop = tokio_util::sync::CancellationToken::new();
    let pending = ui.invoke_in_view(&reference, ActionInput::default(), stop.clone());
    entered(&execution, &read, 1).await;
    stop.cancel();
    assert!(matches!(
        execution
            .deadline_after(Duration::from_secs(2))
            .timeout(pending)
            .await
            .unwrap(),
        Err(UiError::Retired)
    ));
    assert!(ui.is_current(&reference));
    read.gate.add_permits(1);
    ui.invoke(&reference, ActionInput::default()).await.unwrap();
    assert_eq!(read.calls.load(Ordering::SeqCst), 2);
    assert!(runtime.shutdown().await.is_clean());
}
