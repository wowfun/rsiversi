use rsi_meta::{IsolationId, Runtime};
use rsi_meta_scope::{ScopeError, ScopeRoot};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{process::Command, thread};

const TEST_MAXIMUM_ANCESTRY_DEPTH: usize = 64;

fn new_scope_root() -> ScopeRoot {
    ScopeRoot::new(TEST_MAXIMUM_ANCESTRY_DEPTH).unwrap()
}

#[test]
fn roots_require_a_valid_explicit_ancestry_depth() {
    assert!(matches!(
        ScopeRoot::new(0),
        Err(ScopeError::InvalidAncestryDepth { .. })
    ));
    assert!(matches!(
        ScopeRoot::new(ScopeRoot::MAXIMUM_ANCESTRY_DEPTH + 1),
        Err(ScopeError::InvalidAncestryDepth { .. })
    ));
    assert!(ScopeRoot::new(1).is_ok());
    assert!(ScopeRoot::new(ScopeRoot::MAXIMUM_ANCESTRY_DEPTH).is_ok());
}

#[tokio::test]
async fn deepest_key_release_is_stack_safe_on_a_64_kib_thread() {
    const CHILD: &str = "RSI_META_SCOPE_DEEP_DROP_CHILD";
    const DEPTH: usize = 4_096;
    if std::env::var_os(CHILD).is_some() {
        let runtime = Runtime::default();
        let root = ScopeRoot::new(DEPTH).unwrap();
        let mut scopes = Vec::with_capacity(DEPTH);
        let mut bindings = Vec::with_capacity(DEPTH - 1);
        scopes.push(root.create(&runtime.root()).await.unwrap());
        for _ in 1..DEPTH {
            let child = root.create(&runtime.root()).await.unwrap();
            bindings.push(
                root.bind_parent(child.key(), scopes.last().unwrap().key())
                    .unwrap(),
            );
            scopes.push(child);
        }
        for scope in scopes.iter().rev() {
            assert!(scope.dispose().await.is_clean());
        }
        let deepest = scopes.last().unwrap().key().clone();
        drop(bindings);
        drop(scopes);

        thread::Builder::new()
            .name("scope-deep-drop".to_owned())
            .stack_size(64 * 1_024)
            .spawn(move || drop(deepest))
            .unwrap()
            .join()
            .unwrap();
        return;
    }

    let output = Command::new(std::env::current_exe().unwrap())
        .env(CHILD, "1")
        .args([
            "--exact",
            "deepest_key_release_is_stack_safe_on_a_64_kib_thread",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "deep scope release crashed the child process:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[tokio::test]
async fn bind_and_rebind_reject_whole_subtrees_beyond_the_root_depth() {
    let runtime = Runtime::default();
    let root = ScopeRoot::new(4).unwrap();

    let old_root = root.create(&runtime.root()).await.unwrap();
    let old_parent = root.create(&runtime.root()).await.unwrap();
    let old_parent_binding = root.bind_parent(old_parent.key(), old_root.key()).unwrap();
    let moved = root.create(&runtime.root()).await.unwrap();
    let moved_binding = root.bind_parent(moved.key(), old_parent.key()).unwrap();
    let descendant = root.create(&runtime.root()).await.unwrap();
    let descendant_binding = root.bind_parent(descendant.key(), moved.key()).unwrap();

    let deep_root = root.create(&runtime.root()).await.unwrap();
    let deep_middle = root.create(&runtime.root()).await.unwrap();
    let deep_middle_binding = root
        .bind_parent(deep_middle.key(), deep_root.key())
        .unwrap();
    let deep_parent = root.create(&runtime.root()).await.unwrap();
    let deep_parent_binding = root
        .bind_parent(deep_parent.key(), deep_middle.key())
        .unwrap();

    assert!(matches!(
        moved_binding.rebind(deep_parent.key()),
        Err(ScopeError::AncestryDepthExceeded { maximum: 4 })
    ));
    assert_eq!(
        root.parent_of(moved.key()).unwrap(),
        Some(old_parent.key().clone())
    );

    let unbound_subtree = root.create(&runtime.root()).await.unwrap();
    let unbound_descendant = root.create(&runtime.root()).await.unwrap();
    let unbound_descendant_binding = root
        .bind_parent(unbound_descendant.key(), unbound_subtree.key())
        .unwrap();
    assert!(matches!(
        root.bind_parent(unbound_subtree.key(), deep_parent.key()),
        Err(ScopeError::AncestryDepthExceeded { maximum: 4 })
    ));
    assert_eq!(root.parent_of(unbound_subtree.key()).unwrap(), None);

    let shallow_parent = root.create(&runtime.root()).await.unwrap();
    moved_binding.rebind(shallow_parent.key()).unwrap();
    assert_eq!(
        root.chain(descendant.key()).unwrap(),
        vec![
            descendant.key().clone(),
            moved.key().clone(),
            shallow_parent.key().clone()
        ]
    );
    let boundary_leaf = root.create(&runtime.root()).await.unwrap();
    let boundary_binding = root
        .bind_parent(boundary_leaf.key(), descendant.key())
        .unwrap();
    assert_eq!(root.chain(boundary_leaf.key()).unwrap().len(), 4);
    let overflow = root.create(&runtime.root()).await.unwrap();
    assert!(matches!(
        root.bind_parent(overflow.key(), boundary_leaf.key()),
        Err(ScopeError::AncestryDepthExceeded { maximum: 4 })
    ));

    drop(boundary_binding);
    drop(unbound_descendant_binding);
    drop(deep_parent_binding);
    drop(deep_middle_binding);
    drop(descendant_binding);
    drop(moved_binding);
    drop(old_parent_binding);
    for scope in [
        overflow,
        boundary_leaf,
        shallow_parent,
        unbound_descendant,
        unbound_subtree,
        deep_parent,
        deep_middle,
        deep_root,
        descendant,
        moved,
        old_parent,
        old_root,
    ] {
        assert!(scope.dispose().await.is_clean());
    }
}

#[tokio::test]
async fn scopes_are_active_child_fibers_with_inherited_root_local_context_keys() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let baseline = runtime.resource_snapshot().fibers.current;

    let outer = root.create(&runtime.root()).await.unwrap();
    assert_eq!(runtime.resource_snapshot().fibers.current, baseline + 1);
    assert_eq!(root.scope_of(outer.context()).unwrap(), outer.key().clone());

    let derived = outer
        .context()
        .clone()
        .isolate("probe", IsolationId(7))
        .unwrap();
    assert_eq!(root.scope_of(&derived).unwrap(), outer.key().clone());

    let (inner, _binding) = root
        .create_child(outer.context().meta(), outer.key())
        .await
        .unwrap();
    assert_eq!(runtime.resource_snapshot().fibers.current, baseline + 2);
    assert_eq!(
        root.chain(inner.key()).unwrap(),
        vec![inner.key().clone(), outer.key().clone()]
    );

    assert!(inner.dispose().await.is_clean());
    assert!(outer.dispose().await.is_clean());
    assert_eq!(runtime.resource_snapshot().fibers.current, baseline);
}

#[tokio::test]
async fn parent_authority_is_unique_root_local_and_cycle_checked() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let foreign = new_scope_root();
    let first = root.create(&runtime.root()).await.unwrap();
    let second = root.create(&runtime.root()).await.unwrap();
    let foreign_scope = foreign.create(&runtime.root()).await.unwrap();
    let (child, binding) = root
        .create_child(&runtime.root(), first.key())
        .await
        .unwrap();

    assert!(matches!(
        root.bind_parent(child.key(), second.key()),
        Err(ScopeError::ParentAlreadyBound)
    ));
    assert!(matches!(
        root.bind_parent(first.key(), foreign_scope.key()),
        Err(ScopeError::ForeignRoot)
    ));
    assert!(matches!(
        root.bind_parent(first.key(), first.key()),
        Err(ScopeError::ParentCycle)
    ));

    binding.rebind(second.key()).unwrap();
    assert_eq!(
        root.parent_of(child.key()).unwrap(),
        Some(second.key().clone())
    );
    assert_eq!(
        root.chain(child.key()).unwrap(),
        vec![child.key().clone(), second.key().clone()]
    );

    let (grandchild, grandchild_binding) = root
        .create_child(&runtime.root(), child.key())
        .await
        .unwrap();
    assert!(matches!(
        binding.rebind(grandchild.key()),
        Err(ScopeError::ParentCycle)
    ));
    // An unrelated binding cannot move the child's edge.
    grandchild_binding.rebind(first.key()).unwrap();
    assert_eq!(
        root.parent_of(child.key()).unwrap(),
        Some(second.key().clone())
    );

    assert!(foreign_scope.dispose().await.is_clean());
    assert!(grandchild.dispose().await.is_clean());
    assert!(child.dispose().await.is_clean());
    assert!(second.dispose().await.is_clean());
    assert!(first.dispose().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebind_snapshots_are_atomically_old_or_new() {
    let runtime = Runtime::default();
    let root = new_scope_root();
    let old_parent = root.create(&runtime.root()).await.unwrap();
    let new_parent = root.create(&runtime.root()).await.unwrap();
    let (child, binding) = root
        .create_child(&runtime.root(), old_parent.key())
        .await
        .unwrap();
    let old_chain = vec![child.key().clone(), old_parent.key().clone()];
    let new_chain = vec![child.key().clone(), new_parent.key().clone()];
    let reads = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|threads| {
        threads.spawn(|| {
            for index in 0..2_000 {
                let parent = if index % 2 == 0 {
                    new_parent.key()
                } else {
                    old_parent.key()
                };
                binding.rebind(parent).unwrap();
            }
        });
        for _ in 0..2 {
            let reads = Arc::clone(&reads);
            let root = &root;
            let child_key = child.key();
            let old_chain = &old_chain;
            let new_chain = &new_chain;
            threads.spawn(move || {
                for _ in 0..2_000 {
                    let observed = root.chain(child_key).unwrap();
                    assert!(observed == *old_chain || observed == *new_chain);
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(reads.load(Ordering::Relaxed), 4_000);

    assert!(child.dispose().await.is_clean());
    assert!(new_parent.dispose().await.is_clean());
    assert!(old_parent.dispose().await.is_clean());
}
