use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, CleanupReport, ConfigValue, Context, ContextExtension, FactoryIdentity,
    FiberHandle, MetaError, PluginFactory, PreparedActivation, Result as MetaResult,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use thiserror::Error;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

mod topology;

use topology::{attach_child, detach_child, set_subtree_depths, validate_attachment};

/// Failure to construct or modify a scope tree.
#[derive(Debug, Error)]
pub enum ScopeError {
    /// The underlying Runtime rejected the scope Fiber operation.
    #[error(transparent)]
    Runtime(#[from] MetaError),
    /// A key from a different [`ScopeRoot`] was supplied.
    #[error("scope key belongs to a different scope root")]
    ForeignRoot,
    /// The child already has a parent and its authority cannot be minted again.
    #[error("scope child already has a parent binding")]
    ParentAlreadyBound,
    /// The requested edge would make the parent relation cyclic.
    #[error("scope parent edge would form a cycle")]
    ParentCycle,
    /// A root was configured outside the supported complete-chain range.
    #[error("scope ancestry depth {requested} is outside the supported range 1..={maximum}")]
    InvalidAncestryDepth {
        /// Rejected configured depth.
        requested: usize,
        /// Implementation-safe hard ceiling.
        maximum: usize,
    },
    /// A parent mutation would exceed this root's configured complete-chain depth.
    #[error("scope ancestry would exceed the configured depth limit of {maximum}")]
    AncestryDepthExceeded {
        /// Configured complete-chain depth ceiling.
        maximum: usize,
    },
    /// The root exhausted its non-repeating key space.
    #[error("scope key identity space is exhausted")]
    IdentityExhausted,
    /// Activation completed without returning the child generation Context.
    #[error("scope Fiber did not return its active Context")]
    MissingActiveContext,
}

struct ParentEdge {
    parent: Arc<ScopeNode>,
    authority: Weak<ParentAuthority>,
}

#[derive(Default)]
struct TreeState {
    next_key: u64,
    #[cfg(test)]
    topology_node_visits: usize,
}

pub(crate) struct ScopeTree {
    state: Mutex<TreeState>,
    maximum_ancestry_depth: usize,
}

pub(crate) struct ScopeNode {
    id: u64,
    parent: Mutex<Option<ParentEdge>>,
    children: Mutex<HashMap<u64, Weak<ScopeNode>>>,
    depth: AtomicUsize,
    has_ever_been_parent: AtomicBool,
}

/// One independent tree of opaque scope identities and parent links.
#[derive(Clone)]
pub struct ScopeRoot {
    tree: Arc<ScopeTree>,
}

impl ScopeRoot {
    /// Largest implementation-safe complete ancestry depth for one root.
    pub const MAXIMUM_ANCESTRY_DEPTH: usize = 4_096;

    /// Creates an empty root whose keys cannot be used with another root.
    ///
    /// The key itself counts toward `maximum_ancestry_depth`.
    pub fn new(maximum_ancestry_depth: usize) -> Result<Self, ScopeError> {
        if !(1..=Self::MAXIMUM_ANCESTRY_DEPTH).contains(&maximum_ancestry_depth) {
            return Err(ScopeError::InvalidAncestryDepth {
                requested: maximum_ancestry_depth,
                maximum: Self::MAXIMUM_ANCESTRY_DEPTH,
            });
        }
        Ok(Self {
            tree: Arc::new(ScopeTree {
                state: Mutex::new(TreeState::default()),
                maximum_ancestry_depth,
            }),
        })
    }

    /// Creates one active root scope backed by an ordinary no-op child Fiber.
    pub async fn create(&self, context: &Context) -> Result<ScopeHandle, ScopeError> {
        let key = self.mint_key()?;
        create_scope_fiber(context, key).await
    }

    /// Creates one active child scope and its unique parent-edge authority.
    ///
    /// The initial edge is installed before the child Fiber can activate, so
    /// callers never observe the returned scope without its requested parent.
    pub async fn create_child(
        &self,
        context: &Context,
        parent: &ScopeKey,
    ) -> Result<(ScopeHandle, ScopeParentBinding), ScopeError> {
        self.ensure_local(parent)?;
        let key = self.mint_key()?;
        let binding = self.bind_parent(&key, parent)?;
        let scope = create_scope_fiber(context, key).await?;
        Ok((scope, binding))
    }

    /// Installs the first parent edge for an existing local child.
    ///
    /// A second call for the same child is rejected. Only the returned
    /// [`ScopeParentBinding`] may subsequently move this edge.
    pub fn bind_parent(
        &self,
        child: &ScopeKey,
        parent: &ScopeKey,
    ) -> Result<ScopeParentBinding, ScopeError> {
        self.ensure_local(child)?;
        self.ensure_local(parent)?;
        let authority = Arc::new(ParentAuthority);
        let mut state = self.tree.state.lock().expect("scope tree poisoned");
        if child
            .node
            .parent
            .lock()
            .expect("scope node poisoned")
            .is_some()
        {
            return Err(ScopeError::ParentAlreadyBound);
        }
        let subtree = validate_attachment(
            &child.node,
            &parent.node,
            &mut state,
            self.tree.maximum_ancestry_depth,
        )?;
        attach_child(&parent.node, &child.node);
        *child.node.parent.lock().expect("scope node poisoned") = Some(ParentEdge {
            parent: Arc::clone(&parent.node),
            authority: Arc::downgrade(&authority),
        });
        set_subtree_depths(&subtree, parent.node.depth.load(Ordering::Relaxed) + 1);
        Ok(ScopeParentBinding {
            tree: Arc::clone(&self.tree),
            child: Arc::clone(&child.node),
            authority,
        })
    }

    /// Returns the exact parent of one local key.
    pub fn parent_of(&self, key: &ScopeKey) -> Result<Option<ScopeKey>, ScopeError> {
        self.ensure_local(key)?;
        let _state = self.tree.state.lock().expect("scope tree poisoned");
        Ok(key
            .node
            .parent
            .lock()
            .expect("scope node poisoned")
            .as_ref()
            .map(|edge| ScopeKey::new(Arc::clone(&self.tree), Arc::clone(&edge.parent))))
    }

    /// Returns one local key's parent chain nearest-first, including the key.
    pub fn chain(&self, key: &ScopeKey) -> Result<Vec<ScopeKey>, ScopeError> {
        self.ensure_local(key)?;
        let mut state = self.tree.state.lock().expect("scope tree poisoned");
        let mut result = Vec::new();
        let mut cursor = Some(Arc::clone(&key.node));
        while let Some(node) = cursor {
            if result.len() == self.tree.maximum_ancestry_depth {
                return Err(ScopeError::AncestryDepthExceeded {
                    maximum: self.tree.maximum_ancestry_depth,
                });
            }
            record_topology_visit(&mut state);
            cursor = node
                .parent
                .lock()
                .expect("scope node poisoned")
                .as_ref()
                .map(|edge| Arc::clone(&edge.parent));
            result.push(ScopeKey::new(Arc::clone(&self.tree), node));
        }
        Ok(result)
    }

    /// Reads the nearest scope key inherited by a Context.
    ///
    /// An absent extension is an unscoped Context. A key minted by another
    /// root is rejected instead of being silently treated as local.
    pub fn scope_of(&self, context: &Context) -> Result<Option<ScopeKey>, ScopeError> {
        let Some(key) = context.extension::<ScopeContextKey>() else {
            return Ok(None);
        };
        self.ensure_local(&key)?;
        Ok(Some(key.as_ref().clone()))
    }

    fn mint_key(&self) -> Result<ScopeKey, ScopeError> {
        let mut state = self.tree.state.lock().expect("scope tree poisoned");
        let id = state
            .next_key
            .checked_add(1)
            .ok_or(ScopeError::IdentityExhausted)?;
        state.next_key = id;
        Ok(ScopeKey::new(
            Arc::clone(&self.tree),
            Arc::new(ScopeNode {
                id,
                parent: Mutex::new(None),
                children: Mutex::new(HashMap::new()),
                depth: AtomicUsize::new(1),
                has_ever_been_parent: AtomicBool::new(false),
            }),
        ))
    }

    pub(crate) fn ensure_local(&self, key: &ScopeKey) -> Result<(), ScopeError> {
        if Arc::ptr_eq(&self.tree, &key.tree) {
            Ok(())
        } else {
            Err(ScopeError::ForeignRoot)
        }
    }

    #[cfg(test)]
    pub(crate) fn reset_topology_node_visits(&self) {
        self.tree
            .state
            .lock()
            .expect("scope tree poisoned")
            .topology_node_visits = 0;
    }

    #[cfg(test)]
    pub(crate) fn topology_node_visits(&self) -> usize {
        self.tree
            .state
            .lock()
            .expect("scope tree poisoned")
            .topology_node_visits
    }
}

impl Drop for ScopeNode {
    fn drop(&mut self) {
        let edge = self
            .parent
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(edge) = edge else {
            return;
        };
        let mut child_id = self.id;
        let mut parent = edge.parent;

        loop {
            detach_child(&parent, child_id);
            // Moving the uniquely owned node out lets its now-empty Drop run
            // without recursively destroying the next parent edge.
            match Arc::try_unwrap(parent) {
                Ok(mut node) => {
                    let next = node
                        .parent
                        .get_mut()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    let Some(next) = next else {
                        break;
                    };
                    child_id = node.id;
                    parent = next.parent;
                }
                Err(shared) => {
                    drop(shared);
                    break;
                }
            }
        }
    }
}

impl fmt::Debug for ScopeRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ScopeRoot").finish_non_exhaustive()
    }
}

fn record_topology_visit(state: &mut TreeState) {
    #[cfg(test)]
    {
        state.topology_node_visits += 1;
    }
    #[cfg(not(test))]
    let _ = state;
}

/// Opaque identity minted by exactly one [`ScopeRoot`].
#[derive(Clone)]
pub struct ScopeKey {
    tree: Arc<ScopeTree>,
    pub(crate) node: Arc<ScopeNode>,
}

impl ScopeKey {
    fn new(tree: Arc<ScopeTree>, node: Arc<ScopeNode>) -> Self {
        Self { tree, node }
    }

    pub(crate) fn id(&self) -> u64 {
        self.node.id
    }
}

impl PartialEq for ScopeKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
    }
}

impl Eq for ScopeKey {}

impl Hash for ScopeKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.node).hash(state);
    }
}

impl fmt::Debug for ScopeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ScopeKey")
            .field(&self.node.id)
            .finish()
    }
}

struct ParentAuthority;

/// Unique authority to move one already-bound parent edge.
///
/// This value is intentionally not cloneable. Rebinding validates root
/// identity, subtree depth, and cycles; the caller owns quiescence and product
/// notification.
pub struct ScopeParentBinding {
    tree: Arc<ScopeTree>,
    child: Arc<ScopeNode>,
    authority: Arc<ParentAuthority>,
}

impl ScopeParentBinding {
    /// Atomically replaces this binding's parent edge.
    pub fn rebind(&self, parent: &ScopeKey) -> Result<(), ScopeError> {
        if !Arc::ptr_eq(&self.tree, &parent.tree) {
            return Err(ScopeError::ForeignRoot);
        }
        let mut state = self.tree.state.lock().expect("scope tree poisoned");
        let current_parent = {
            let edge = self.child.parent.lock().expect("scope node poisoned");
            let owns_edge = edge
                .as_ref()
                .and_then(|edge| edge.authority.upgrade())
                .is_some_and(|actual| Arc::ptr_eq(&actual, &self.authority));
            if !owns_edge {
                return Err(ScopeError::ParentAlreadyBound);
            }
            Arc::clone(
                &edge
                    .as_ref()
                    .expect("validated parent edge remains present")
                    .parent,
            )
        };
        if Arc::ptr_eq(&current_parent, &parent.node) {
            return Ok(());
        }
        let subtree = validate_attachment(
            &self.child,
            &parent.node,
            &mut state,
            self.tree.maximum_ancestry_depth,
        )?;
        let old_parent = {
            let mut edge = self.child.parent.lock().expect("scope node poisoned");
            std::mem::replace(
                &mut edge
                    .as_mut()
                    .expect("validated parent edge remains present")
                    .parent,
                Arc::clone(&parent.node),
            )
        };
        detach_child(&old_parent, self.child.id);
        attach_child(&parent.node, &self.child);
        set_subtree_depths(&subtree, parent.node.depth.load(Ordering::Relaxed) + 1);
        drop(old_parent);
        drop(current_parent);
        Ok(())
    }
}

impl fmt::Debug for ScopeParentBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopeParentBinding")
            .field("child", &self.child.id)
            .finish_non_exhaustive()
    }
}

/// Active Context and Fiber ownership for one scope identity.
#[derive(Clone)]
pub struct ScopeHandle {
    key: ScopeKey,
    context: Context,
    fiber: FiberHandle,
}

impl ScopeHandle {
    /// Returns this scope's opaque root-local identity.
    pub fn key(&self) -> &ScopeKey {
        &self.key
    }

    /// Returns the active child-generation Context carrying this scope key.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Idempotently disposes the backing Fiber and every scope-owned effect.
    pub async fn dispose(&self) -> CleanupReport {
        self.fiber.dispose().await
    }
}

impl fmt::Debug for ScopeHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopeHandle")
            .field("key", &self.key)
            .field("fiber", &self.fiber)
            .finish_non_exhaustive()
    }
}

pub(crate) struct ScopeContextKey;

impl ContextExtension for ScopeContextKey {
    type Value = ScopeKey;
}

struct ScopeFiberFactory {
    key: ScopeKey,
    context: Mutex<Option<oneshot::Sender<Context>>>,
}

impl ScopeFiberFactory {
    fn new(key: ScopeKey, context: oneshot::Sender<Context>) -> Self {
        Self {
            key,
            context: Mutex::new(Some(context)),
        }
    }
}

impl fmt::Debug for ScopeFiberFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopeFiberFactory")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PluginFactory for ScopeFiberFactory {
    fn identity(&self) -> FactoryIdentity {
        FactoryIdentity::builtin("rsi-meta-scope.scope", "1")
    }

    fn prepare(&self, desired: &ConfigValue) -> MetaResult<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> MetaResult<()> {
        let context = plan
            .context()
            .clone()
            .with_extension::<ScopeContextKey>(self.key.clone())?;
        if let Some(sender) = self
            .context
            .lock()
            .expect("scope context sender poisoned")
            .take()
        {
            let _ = sender.send(context);
        }
        Ok(())
    }
}

async fn create_scope_fiber(parent: &Context, key: ScopeKey) -> Result<ScopeHandle, ScopeError> {
    let (sender, receiver) = oneshot::channel();
    let factory = Arc::new(ScopeFiberFactory::new(key.clone(), sender));
    let fiber = parent.apply(factory, Value::Null).await?;
    if let Err(error) = fiber.wait_active(&CancellationToken::new()).await {
        let _cleanup = fiber.dispose().await;
        return Err(ScopeError::Runtime(error));
    }
    let Ok(context) = receiver.await else {
        let _cleanup = fiber.dispose().await;
        return Err(ScopeError::MissingActiveContext);
    };
    Ok(ScopeHandle {
        key,
        context,
        fiber,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_only_chain_construction_performs_no_ancestor_walks() {
        const DEPTH: usize = 1_024;
        let root = ScopeRoot::new(DEPTH).unwrap();
        let mut parent = root.mint_key().unwrap();
        let mut bindings = Vec::with_capacity(DEPTH - 1);

        for _ in 1..DEPTH {
            let child = root.mint_key().unwrap();
            bindings.push(root.bind_parent(&child, &parent).unwrap());
            parent = child;
        }

        let visits = root
            .tree
            .state
            .lock()
            .expect("scope tree poisoned")
            .topology_node_visits;
        assert_eq!(visits, 0, "leaf attachment must not walk ancestors");
        drop(bindings);
    }

    #[test]
    fn closing_cycle_still_walks_and_rejects_the_parent_chain() {
        const DEPTH: usize = 1_024;
        let root = ScopeRoot::new(DEPTH).unwrap();
        let first = root.mint_key().unwrap();
        let mut parent = first.clone();
        let mut bindings = Vec::with_capacity(DEPTH - 1);

        for _ in 1..DEPTH {
            let child = root.mint_key().unwrap();
            bindings.push(root.bind_parent(&child, &parent).unwrap());
            parent = child;
        }
        root.tree
            .state
            .lock()
            .expect("scope tree poisoned")
            .topology_node_visits = 0;

        assert!(matches!(
            root.bind_parent(&first, &parent),
            Err(ScopeError::ParentCycle)
        ));
        let visits = root
            .tree
            .state
            .lock()
            .expect("scope tree poisoned")
            .topology_node_visits;
        assert_eq!(visits, DEPTH);
        drop(bindings);
    }

    #[test]
    fn root_does_not_retain_minted_key_nodes_or_unreachable_parent_chains() {
        let root = ScopeRoot::new(8).unwrap();
        let parent = root.mint_key().unwrap();
        let parent_node = Arc::downgrade(&parent.node);
        let child = root.mint_key().unwrap();
        let child_node = Arc::downgrade(&child.node);
        let binding = root.bind_parent(&child, &parent).unwrap();

        drop(parent);
        assert!(
            parent_node.upgrade().is_some(),
            "the live child retains its parent"
        );
        drop(child);
        assert!(
            child_node.upgrade().is_some(),
            "the unique rebind authority retains its child"
        );
        drop(binding);

        assert!(child_node.upgrade().is_none());
        assert!(parent_node.upgrade().is_none());

        let standalone = root.mint_key().unwrap();
        let standalone_node = Arc::downgrade(&standalone.node);
        drop(standalone);
        assert!(standalone_node.upgrade().is_none());
    }
}
