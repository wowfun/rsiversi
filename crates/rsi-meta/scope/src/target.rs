use crate::scope::{ScopeContextKey, ScopeError, ScopeKey, ScopeRoot};
use rsi_meta::{EventTarget, ListenerView, MetaError, Result as MetaResult};
use std::collections::HashSet;
use std::fmt;

/// Immutable ancestor-chain selector for one scoped event dispatch.
///
/// Construct this value with [`ScopeRoot::target`]. The chain is captured
/// atomically with respect to parent rebinding, so every listener in one
/// dispatch is evaluated against the same scope topology.
#[derive(Clone)]
pub struct ScopeTarget {
    root: ScopeRoot,
    membership: HashSet<u64>,
}

impl ScopeRoot {
    /// Captures one local key's complete ancestor chain for event selection.
    pub fn target(&self, key: &ScopeKey) -> Result<ScopeTarget, ScopeError> {
        let membership = self.chain(key)?.into_iter().map(|key| key.id()).collect();
        Ok(ScopeTarget {
            root: self.clone(),
            membership,
        })
    }
}

impl ScopeTarget {
    fn contains(&self, key: &ScopeKey) -> bool {
        self.membership.contains(&key.id())
    }
}

impl EventTarget for ScopeTarget {
    fn select(&self, listener: &ListenerView) -> MetaResult<bool> {
        let Some(key) = listener.extension::<ScopeContextKey>() else {
            return Ok(true);
        };
        self.root.ensure_local(key.as_ref()).map_err(|_| {
            MetaError::Event("listener scope belongs to a different ScopeRoot".to_owned())
        })?;
        Ok(self.contains(key.as_ref()))
    }
}

impl fmt::Debug for ScopeTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopeTarget")
            .field("depth", &self.membership.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_meta::Runtime;

    fn assert_hash_set(_: &HashSet<u64>) {}

    #[tokio::test]
    async fn target_precomputes_membership_from_one_bounded_chain_walk() {
        const DEPTH: usize = 64;
        let runtime = Runtime::default();
        let root = ScopeRoot::new(DEPTH).unwrap();
        let mut scopes = Vec::with_capacity(DEPTH + 1);
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
        let outsider = root.create(&runtime.root()).await.unwrap();
        root.reset_topology_node_visits();
        let target = root.target(scopes.last().unwrap().key()).unwrap();
        assert_eq!(root.topology_node_visits(), DEPTH);

        assert_hash_set(&target.membership);
        root.reset_topology_node_visits();
        assert!(!target.contains(outsider.key()));
        assert_eq!(root.topology_node_visits(), 0);

        drop(bindings);
        assert!(outsider.dispose().await.is_clean());
        for scope in scopes.into_iter().rev() {
            assert!(scope.dispose().await.is_clean());
        }
    }
}
