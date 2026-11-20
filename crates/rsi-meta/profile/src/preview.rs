use super::{ProfileCandidate, ProfileSnapshot, TreeNode};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// One origin in the compiler's effective source program.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "source", rename_all = "snake_case")]
pub enum ProfileLayer {
    /// An injected, named profile fragment.
    Fragment(String),
    /// Root supplied as an in-memory profile.
    Memory,
    /// Logical member of an immutable profile bundle.
    Bundle(std::path::PathBuf),
    /// Native root document or transitive include.
    File(std::path::PathBuf),
    /// Zero-based position in the frozen launch patches.
    LaunchPatch(usize),
}

/// Source-layer indices for a declared instance, including disabled instances.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NodeOrigin {
    /// Layer declaring this node.
    pub declared_by: usize,
    /// Layer supplying the effective configuration of a plugin leaf.
    pub config_from: Option<usize>,
    /// Last layer setting this node's own enabled flag.
    pub enabled_from: usize,
    /// First disabled node on the path to this instance, including itself.
    pub disabled_by: Option<String>,
}

/// Local provenance, separate from the compiled candidate and its identity.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ProfileOrigins {
    /// References shared by all nodes originating in a layer.
    pub layers: Vec<ProfileLayer>,
    /// Effective attribution by instance ID.
    pub nodes: BTreeMap<String, NodeOrigin>,
}

impl ProfileOrigins {
    pub(super) fn set_disabled(&mut self, nodes: &[TreeNode], inherited: Option<&str>) {
        for node in nodes {
            let enabled = match node {
                TreeNode::Plugin(plugin) => plugin.enabled,
                TreeNode::Group(group) => group.enabled,
            };
            let disabled = inherited.or_else(|| (!enabled).then(|| node.id()));
            self.nodes
                .get_mut(node.id())
                .expect("registered node")
                .disabled_by = disabled.map(str::to_owned);
            if let TreeNode::Group(group) = node {
                self.set_disabled(&group.children, disabled);
            }
        }
    }
}

/// Redacted reason that an existing effective node differs.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NodeChangeAspect {
    /// Group and plugin kinds differ.
    Kind,
    /// The selected plugin key differs.
    Plugin,
    /// The immediate parent group differs.
    Parent,
    /// The position among siblings differs.
    Order,
    /// The node's own evaluated enabled state differs.
    Enabled,
    /// The complete evaluated plugin configuration differs.
    Configuration,
    /// The group's own isolation declaration differs.
    Isolation,
}
/// One node's difference from an earlier compiled tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NodeChangeKind {
    /// The node exists only in the new tree.
    Added,
    /// The node exists only in the old tree.
    Removed,
    /// Unique changed aspects, in declaration order.
    Modified(Vec<NodeChangeAspect>),
}
/// A redacted change keyed by stable all-tree node identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeChange {
    /// Stable node identity; no configuration values are included.
    pub id: String,
    /// Added, removed, or modified effective behavior.
    pub kind: NodeChangeKind,
}

impl ProfileCandidate {
    /// Returns the complete redacted desired tree at pure-preview revision zero.
    pub fn snapshot(&self) -> ProfileSnapshot {
        ProfileSnapshot::from_candidate(self)
    }

    /// Reports effective changes from a previous candidate without exposing configs.
    pub fn changes_from(&self, previous: &Self) -> Vec<NodeChange> {
        let current = positions(&self.tree);
        let previous = positions(&previous.tree);
        let ids = current
            .keys()
            .chain(previous.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        ids.into_iter()
            .filter_map(|id| {
                let kind = match (previous.get(id), current.get(id)) {
                    (None, Some(_)) => NodeChangeKind::Added,
                    (Some(_), None) => NodeChangeKind::Removed,
                    (Some(before), Some(after)) => {
                        let aspects = changed_aspects(before, after);
                        if aspects.is_empty() {
                            return None;
                        }
                        NodeChangeKind::Modified(aspects)
                    }
                    (None, None) => unreachable!("union of node identities"),
                };
                Some(NodeChange {
                    id: id.to_owned(),
                    kind,
                })
            })
            .collect()
    }

    /// Returns the captured digest of one exact canonical native source path.
    /// Bundle and memory sources have no native file fingerprint.
    pub fn source_fingerprint(&self, path: &Path) -> Option<&[u8; 32]> {
        self.source_fingerprints.get(path)
    }
}
struct Position<'a> {
    parent: Option<&'a str>,
    index: usize,
    node: &'a TreeNode,
}
fn positions(nodes: &[TreeNode]) -> BTreeMap<&str, Position<'_>> {
    fn visit<'a>(
        nodes: &'a [TreeNode],
        parent: Option<&'a str>,
        result: &mut BTreeMap<&'a str, Position<'a>>,
    ) {
        for (index, node) in nodes.iter().enumerate() {
            result.insert(
                node.id(),
                Position {
                    parent,
                    index,
                    node,
                },
            );
            if let TreeNode::Group(group) = node {
                visit(&group.children, Some(&group.id), result);
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(nodes, None, &mut result);
    result
}
fn changed_aspects(before: &Position<'_>, after: &Position<'_>) -> Vec<NodeChangeAspect> {
    use NodeChangeAspect as A;
    let mut aspects = BTreeSet::new();
    if before.parent != after.parent {
        aspects.insert(A::Parent);
    }
    if before.index != after.index {
        aspects.insert(A::Order);
    }
    match (before.node, after.node) {
        (TreeNode::Group(before), TreeNode::Group(after)) => {
            if before.enabled != after.enabled {
                aspects.insert(A::Enabled);
            }
            if before.isolation != after.isolation {
                aspects.insert(A::Isolation);
            }
        }
        (TreeNode::Plugin(before), TreeNode::Plugin(after)) => {
            if before.plugin != after.plugin {
                aspects.insert(A::Plugin);
            }
            if before.enabled != after.enabled {
                aspects.insert(A::Enabled);
            }
            if before.config != after.config {
                aspects.insert(A::Configuration);
            }
        }
        _ => {
            aspects.insert(A::Kind);
        }
    }
    aspects.into_iter().collect()
}
