use super::{ScopeError, ScopeNode, TreeState, record_topology_visit};
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub(super) struct SubtreeNode {
    node: Arc<ScopeNode>,
    distance_from_root: usize,
}

pub(super) fn validate_attachment(
    child: &Arc<ScopeNode>,
    parent: &Arc<ScopeNode>,
    state: &mut TreeState,
    maximum_ancestry_depth: usize,
) -> Result<Vec<SubtreeNode>, ScopeError> {
    if Arc::ptr_eq(child, parent) {
        record_topology_visit(state);
        return Err(ScopeError::ParentCycle);
    }
    // A key can occur in another key's ancestry only after serving as a
    // parent. This fact never reverts while the key remains live.
    if child.has_ever_been_parent.load(Ordering::Relaxed) {
        ensure_acyclic(child, parent, state)?;
    }

    let subtree = if child.has_ever_been_parent.load(Ordering::Relaxed) {
        collect_subtree(child, state)
    } else {
        vec![SubtreeNode {
            node: Arc::clone(child),
            distance_from_root: 0,
        }]
    };
    let subtree_height = subtree
        .iter()
        .map(|entry| entry.distance_from_root)
        .max()
        .unwrap_or(0);
    let resulting_depth = parent
        .depth
        .load(Ordering::Relaxed)
        .checked_add(1)
        .and_then(|depth| depth.checked_add(subtree_height));
    if resulting_depth.is_none_or(|depth| depth > maximum_ancestry_depth) {
        return Err(ScopeError::AncestryDepthExceeded {
            maximum: maximum_ancestry_depth,
        });
    }
    Ok(subtree)
}

fn ensure_acyclic(
    child: &Arc<ScopeNode>,
    parent: &Arc<ScopeNode>,
    state: &mut TreeState,
) -> Result<(), ScopeError> {
    let mut cursor = Some(Arc::clone(parent));
    while let Some(node) = cursor {
        record_topology_visit(state);
        if Arc::ptr_eq(&node, child) {
            return Err(ScopeError::ParentCycle);
        }
        cursor = node
            .parent
            .lock()
            .expect("scope node poisoned")
            .as_ref()
            .map(|edge| Arc::clone(&edge.parent));
    }
    Ok(())
}

fn collect_subtree(root: &Arc<ScopeNode>, state: &mut TreeState) -> Vec<SubtreeNode> {
    let mut result = Vec::new();
    let mut pending = vec![(Arc::clone(root), 0)];
    while let Some((node, distance_from_root)) = pending.pop() {
        record_topology_visit(state);
        for child in live_children(&node) {
            pending.push((child, distance_from_root + 1));
        }
        result.push(SubtreeNode {
            node,
            distance_from_root,
        });
    }
    result
}

fn live_children(node: &ScopeNode) -> Vec<Arc<ScopeNode>> {
    let mut result = Vec::new();
    node.children
        .lock()
        .expect("scope node poisoned")
        .retain(|_, child| {
            if let Some(child) = child.upgrade() {
                result.push(child);
                true
            } else {
                false
            }
        });
    result
}

pub(super) fn attach_child(parent: &ScopeNode, child: &Arc<ScopeNode>) {
    parent
        .children
        .lock()
        .expect("scope node poisoned")
        .insert(child.id, Arc::downgrade(child));
    parent.has_ever_been_parent.store(true, Ordering::Relaxed);
}

pub(super) fn detach_child(parent: &ScopeNode, child_id: u64) {
    parent
        .children
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&child_id);
}

pub(super) fn set_subtree_depths(subtree: &[SubtreeNode], root_depth: usize) {
    for entry in subtree {
        entry
            .node
            .depth
            .store(root_depth + entry.distance_from_root, Ordering::Relaxed);
    }
}
