use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use rsi_meta_scope::{ScopeRoot, ScopedContributions};
use std::sync::{Arc, Mutex};

fn values(snapshot: &[Arc<usize>]) -> Vec<usize> {
    snapshot.iter().map(|value| **value).collect()
}

#[tokio::test]
async fn ordered_contributions_reuse_snapshots_across_unrelated_changes_and_owner_rebuild() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let scopes = ScopeRoot::new(16).unwrap();
    let table = ScopedContributions::new(runtime.identity(), scopes.clone(), 4).unwrap();
    let a = root.child_position().unwrap();
    let b = root.child_position().unwrap();
    let first = scopes
        .create(&root.with_child_position(&a).unwrap())
        .await
        .unwrap();
    let second = scopes
        .create(&root.with_child_position(&b).unwrap())
        .await
        .unwrap();
    let credential = |scope: &rsi_meta_scope::ScopeHandle| {
        scope.context().meta().registration_context().unwrap()
    };
    let _b = table
        .register(&credential(&second), None, Arc::new(2))
        .unwrap();
    let first_lease = table
        .register(&credential(&first), None, Arc::new(1))
        .unwrap();
    let before = table.snapshot(None).unwrap();
    assert_eq!(values(&before), [1, 2]);
    assert!(Arc::ptr_eq(&before, &table.snapshot(None).unwrap()));
    let unrelated = scopes.create(&root).await.unwrap();
    assert!(Arc::ptr_eq(&before, &table.snapshot(None).unwrap()));
    let unrelated_lease = table
        .register(&credential(&unrelated), Some(unrelated.key()), Arc::new(3))
        .unwrap();
    assert!(Arc::ptr_eq(&before, &table.snapshot(None).unwrap()));
    drop(unrelated_lease);
    assert!(Arc::ptr_eq(&before, &table.snapshot(None).unwrap()));
    root.reorder_children(&[b.clone(), a.clone()]).unwrap();
    assert_eq!(values(&table.snapshot(None).unwrap()), [2, 1]);
    assert_eq!(values(&before), [1, 2]);
    assert!(first.dispose().await.is_clean());
    let replacement = scopes
        .create(&root.with_child_position(&a).unwrap())
        .await
        .unwrap();
    let _replacement = table
        .register(&credential(&replacement), None, Arc::new(11))
        .unwrap();
    drop(first_lease);
    assert_eq!(values(&table.snapshot(None).unwrap()), [2, 11]);
    assert!(runtime.shutdown().await.is_clean());
    assert!(table.snapshot(None).unwrap().is_empty());
    assert_eq!(values(&before), [1, 2]);
}

#[tokio::test]
async fn scoped_order_keeps_ancestor_precedence_and_reparenting_changes_only_new_snapshots() {
    let runtime = Runtime::default();
    let root = runtime.root();
    let scopes = ScopeRoot::new(16).unwrap();
    let table = ScopedContributions::new(runtime.identity(), scopes.clone(), 4).unwrap();
    let parent = scopes.create(&root).await.unwrap();
    let (child, binding) = scopes.create_child(&root, parent.key()).await.unwrap();
    let other = scopes.create(&root).await.unwrap();
    let credential = |scope: &rsi_meta_scope::ScopeHandle| {
        scope.context().meta().registration_context().unwrap()
    };
    let _child = table
        .register(&credential(&child), Some(child.key()), Arc::new(3))
        .unwrap();
    let _parent = table
        .register(&credential(&parent), Some(parent.key()), Arc::new(2))
        .unwrap();
    let _global = table
        .register(&credential(&child), None, Arc::new(1))
        .unwrap();
    let _other = table
        .register(&credential(&other), Some(other.key()), Arc::new(4))
        .unwrap();
    let before = table.snapshot(Some(child.key())).unwrap();
    assert_eq!(values(&before), [1, 2, 3]);
    binding.rebind(other.key()).unwrap();
    assert_eq!(
        values(&table.snapshot(Some(child.key())).unwrap()),
        [1, 4, 3]
    );
    assert_eq!(values(&before), [1, 2, 3]);
    assert_eq!(values(&table.snapshot(None).unwrap()), [1]);
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct RegisterOnLoad {
    table: Arc<ScopedContributions<usize>>,
    retained: Arc<Mutex<Option<rsi_meta::RegistrationLease>>>,
}
#[async_trait]
impl PluginFactory for RegisterOnLoad {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = self
            .table
            .register(&plan.context().registration_context()?, None, Arc::new(1))
            .unwrap();
        *self.retained.lock().unwrap() = Some(lease);
        Err(rsi_meta::MetaError::Activation("rollback fixture".into()))
    }
}

#[tokio::test]
async fn contribution_admission_owns_loading_rollback_and_bounds_all_scopes_together() {
    let runtime = Runtime::default();
    let scopes = ScopeRoot::new(16).unwrap();
    assert!(ScopedContributions::<usize>::new(runtime.identity(), scopes.clone(), 0).is_err());
    let table = Arc::new(ScopedContributions::new(runtime.identity(), scopes.clone(), 1).unwrap());
    let retained = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "fixture.loading",
                "1",
                UpdateMode::Replayable,
                Arc::new(RegisterOnLoad {
                    table: table.clone(),
                    retained: retained.clone(),
                }),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert!(table.snapshot(None).unwrap().is_empty());
    assert!(retained.lock().unwrap().is_some());
    let owner = scopes.create(&runtime.root()).await.unwrap();
    let credential = owner.context().meta().registration_context().unwrap();
    let lease = table.register(&credential, None, Arc::new(2)).unwrap();
    assert!(
        table
            .register(&credential, Some(owner.key()), Arc::new(3))
            .is_err()
    );
    drop(lease);
    let _scoped = table
        .register(&credential, Some(owner.key()), Arc::new(3))
        .unwrap();
    let other = Runtime::default();
    let foreign = scopes.create(&other.root()).await.unwrap();
    assert!(
        table
            .register(
                &foreign.context().meta().registration_context().unwrap(),
                None,
                Arc::new(4)
            )
            .is_err()
    );
    let foreign_root = ScopeRoot::new(16).unwrap();
    let foreign_scope = foreign_root.create(&runtime.root()).await.unwrap();
    assert!(table.snapshot(Some(foreign_scope.key())).is_err());
    assert!(
        table
            .register(&credential, Some(foreign_scope.key()), Arc::new(4))
            .is_err()
    );
    assert!(owner.dispose().await.is_clean());
    assert!(table.register(&credential, None, Arc::new(4)).is_err());
    assert!(runtime.shutdown().await.is_clean());
    assert!(other.shutdown().await.is_clean());
}
