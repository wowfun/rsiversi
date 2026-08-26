use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, DispatchMode, EventHandler, EventOptions, EventOutcome,
    FactoryIdentity, InvocationContext, PluginFactory, PreparedActivation, Result, Runtime,
};
use rsi_meta_scope::ScopeRoot;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn new_scope_root() -> ScopeRoot {
    ScopeRoot::new(64).unwrap()
}

#[derive(Debug)]
struct CountingHandler(Arc<AtomicUsize>);

#[async_trait]
impl EventHandler for CountingHandler {
    async fn handle(&self, _: InvocationContext, value: Arc<Value>) -> Result<EventOutcome> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(EventOutcome::Continue(value.as_ref().clone()))
    }
}

#[derive(Debug)]
struct CaptureFactory {
    identity: FactoryIdentity,
    sender: Mutex<Option<tokio::sync::oneshot::Sender<Context>>>,
}

#[async_trait]
impl PluginFactory for CaptureFactory {
    fn identity(&self) -> FactoryIdentity {
        self.identity.clone()
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        if let Some(sender) = self.sender.lock().expect("context capture poisoned").take() {
            let _ = sender.send(plan.context().clone());
        }
        Ok(())
    }
}

async fn unscoped_context(runtime: &Runtime) -> Context {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                identity: FactoryIdentity::builtin("scope-target.unscoped", "1"),
                sender: Mutex::new(Some(sender)),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    receiver.await.unwrap()
}

fn listen(context: &Context, event: &str, count: &Arc<AtomicUsize>) {
    context
        .on(
            event,
            Arc::new(CountingHandler(Arc::clone(count))),
            EventOptions::default(),
        )
        .unwrap();
}

#[tokio::test]
async fn scope_target_routes_to_exact_scope_ancestors_and_unscoped_listeners() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let parent = root.create(&runtime.root()).await.unwrap();
    let (child, _binding) = root
        .create_child(&runtime.root(), parent.key())
        .await
        .unwrap();
    let sibling = root.create(&runtime.root()).await.unwrap();
    let unscoped = unscoped_context(&runtime).await;
    let parent_count = Arc::new(AtomicUsize::new(0));
    let child_count = Arc::new(AtomicUsize::new(0));
    let sibling_count = Arc::new(AtomicUsize::new(0));
    let unscoped_count = Arc::new(AtomicUsize::new(0));
    listen(parent.context(), "scope-target", &parent_count);
    listen(child.context(), "scope-target", &child_count);
    listen(sibling.context(), "scope-target", &sibling_count);
    listen(&unscoped, "scope-target", &unscoped_count);

    let receipt = runtime
        .root()
        .dispatch_targeted(
            "scope-target",
            DispatchMode::Emit,
            Value::Null,
            Arc::new(root.target(child.key()).unwrap()),
        )
        .await
        .unwrap();

    assert_eq!(receipt.invoked, 3);
    assert_eq!(parent_count.load(Ordering::Acquire), 1);
    assert_eq!(child_count.load(Ordering::Acquire), 1);
    assert_eq!(sibling_count.load(Ordering::Acquire), 0);
    assert_eq!(unscoped_count.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn scope_target_rejects_foreign_listener_before_any_callback_starts() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let foreign = new_scope_root();
    let local_scope = root.create(&runtime.root()).await.unwrap();
    let foreign_scope = foreign.create(&runtime.root()).await.unwrap();
    let local_count = Arc::new(AtomicUsize::new(0));
    let foreign_count = Arc::new(AtomicUsize::new(0));
    listen(local_scope.context(), "scope-target-foreign", &local_count);
    listen(
        foreign_scope.context(),
        "scope-target-foreign",
        &foreign_count,
    );

    let error = runtime
        .root()
        .dispatch_targeted(
            "scope-target-foreign",
            DispatchMode::Emit,
            Value::Null,
            Arc::new(root.target(local_scope.key()).unwrap()),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("different ScopeRoot"));
    assert_eq!(local_count.load(Ordering::Acquire), 0);
    assert_eq!(foreign_count.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn scope_target_captures_one_parent_chain_before_rebind() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let old_parent = root.create(&runtime.root()).await.unwrap();
    let new_parent = root.create(&runtime.root()).await.unwrap();
    let (child, binding) = root
        .create_child(&runtime.root(), old_parent.key())
        .await
        .unwrap();
    let old_count = Arc::new(AtomicUsize::new(0));
    let new_count = Arc::new(AtomicUsize::new(0));
    let child_count = Arc::new(AtomicUsize::new(0));
    listen(old_parent.context(), "scope-target-snapshot", &old_count);
    listen(new_parent.context(), "scope-target-snapshot", &new_count);
    listen(child.context(), "scope-target-snapshot", &child_count);
    let old_target = root.target(child.key()).unwrap();

    binding.rebind(new_parent.key()).unwrap();
    runtime
        .root()
        .dispatch_targeted(
            "scope-target-snapshot",
            DispatchMode::Emit,
            Value::Null,
            Arc::new(old_target),
        )
        .await
        .unwrap();
    assert_eq!(old_count.load(Ordering::Acquire), 1);
    assert_eq!(new_count.load(Ordering::Acquire), 0);
    assert_eq!(child_count.load(Ordering::Acquire), 1);

    runtime
        .root()
        .dispatch_targeted(
            "scope-target-snapshot",
            DispatchMode::Emit,
            Value::Null,
            Arc::new(root.target(child.key()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(old_count.load(Ordering::Acquire), 1);
    assert_eq!(new_count.load(Ordering::Acquire), 1);
    assert_eq!(child_count.load(Ordering::Acquire), 2);
}
