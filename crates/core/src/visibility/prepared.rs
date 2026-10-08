//! Visibility finding counts over a fixed tree and edge set whose declarations
//! move between containers.
//!
//! [`derive_visibility`](super::derive_visibility) rebuilds everything from its
//! three inputs on every call. A search that asks "how many findings does this
//! trial placement leave?" thousands of times over one tree and one edge set
//! pays that rebuild each time, although only the nodes' containers changed.
//! [`PreparedVisibility`] resolves the invariant half once — the LCA tables, the
//! edges as node positions, and the nodes that can be flagged at all — and
//! answers each query from the containers alone.

use strata_ir::{ContainerId, ContainerTree, Edge, Node, NodeKind, ScopeLevel};

use super::{LcaTable, exempt_from_advice, is_signature_edge};

/// The placement-independent half of visibility derivation, built once for a
/// tree, a node table and an edge set.
///
/// Nodes are addressed by their position in the slice handed to
/// [`PreparedVisibility::new`]; node ids must be unique, as a validated
/// snapshot guarantees. For any assignment of containers it reports exactly the
/// finding count [`derive_visibility`](super::derive_visibility) would.
pub struct PreparedVisibility {
    /// Ancestor tables over the container tree.
    lca: LcaTable,
    /// `(target, consumer)` node positions per edge whose endpoints are both
    /// nodes, in edge order.
    consumers: Vec<(usize, usize)>,
    /// `(function, referenced type)` node positions per signature edge onto a
    /// type node, in edge order.
    signatures: Vec<(usize, usize)>,
    /// `(position, declared)` of every node that advice may flag: not exempt,
    /// and declared above file level (no derived scope sits below a file, so a
    /// file-level declaration is never wider than its scope).
    reportable: Vec<(usize, ScopeLevel)>,
    /// The number of nodes the containers slice must cover.
    len: usize,
}

impl PreparedVisibility {
    /// Resolves the invariant half of visibility derivation over `tree`,
    /// `nodes` and `edges`.
    #[must_use]
    pub fn new(tree: &ContainerTree, nodes: &[Node], edges: &[Edge]) -> Self {
        let max_id = nodes.iter().map(|node| node.id.0 as usize).max();
        let mut position: Vec<Option<usize>> = vec![None; max_id.map_or(0, |id| id + 1)];
        for (index, node) in nodes.iter().enumerate() {
            if let Some(cell) = position.get_mut(node.id.0 as usize) {
                *cell = Some(index);
            }
        }
        let find = |id: u32| position.get(id as usize).copied().flatten();

        let mut consumers = Vec::new();
        let mut signatures = Vec::new();
        for edge in edges {
            let (Some(source), Some(target)) = (find(edge.source.0), find(edge.target.0)) else {
                continue;
            };
            consumers.push((target, source));
            let referenced_is_type = nodes
                .get(target)
                .is_some_and(|node| node.kind == NodeKind::Type);
            if is_signature_edge(edge.kind) && referenced_is_type {
                signatures.push((source, target));
            }
        }

        let reportable = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| !exempt_from_advice(node) && node.visibility > ScopeLevel::File)
            .map(|(index, node)| (index, node.visibility))
            .collect();

        Self {
            lca: LcaTable::build(tree),
            consumers,
            signatures,
            reportable,
            len: nodes.len(),
        }
    }

    /// Counts the over-exported declarations when node `i` of the prepared node
    /// table lives in `containers[i]`.
    ///
    /// `containers` must cover every prepared node (checked in debug builds).
    /// Node ids are unique, as a validated snapshot guarantees, so addressing
    /// nodes by position agrees with the derivation's addressing by id.
    #[must_use]
    pub fn finding_count(&self, containers: &[ContainerId]) -> usize {
        debug_assert_eq!(containers.len(), self.len);
        let mut scope: Vec<Option<ContainerId>> = vec![None; self.len];

        for &(target, consumer) in &self.consumers {
            let (Some(&consumer_container), Some(&target_container)) =
                (containers.get(consumer), containers.get(target))
            else {
                continue;
            };
            if let Some(cell) = scope.get_mut(target) {
                let seed = cell.unwrap_or(target_container);
                *cell = Some(self.lca.lca(seed, consumer_container));
            }
        }

        self.widen_by_type_exposure(containers, &mut scope);

        self.reportable
            .iter()
            .filter(|&&(position, declared)| {
                scope
                    .get(position)
                    .copied()
                    .flatten()
                    .or_else(|| containers.get(position).copied())
                    .is_some_and(|container| declared > self.lca.level(container))
            })
            .count()
    }

    /// Widens signature types up to their exported functions' scopes until no
    /// scope changes; the prepared mirror of the derivation's exposure pass.
    fn widen_by_type_exposure(
        &self,
        containers: &[ContainerId],
        scope: &mut [Option<ContainerId>],
    ) {
        loop {
            let mut changed = false;

            for &(function, referenced) in &self.signatures {
                let Some(function_scope) = scope
                    .get(function)
                    .copied()
                    .flatten()
                    .or_else(|| containers.get(function).copied())
                else {
                    continue;
                };
                // a function only exposes its signature when itself exported
                // above its own file; a file-private function leaks nothing.
                if self.lca.level(function_scope) <= ScopeLevel::File {
                    continue;
                }

                let Some(current) = scope
                    .get(referenced)
                    .copied()
                    .flatten()
                    .or_else(|| containers.get(referenced).copied())
                else {
                    continue;
                };
                let widened = self.lca.lca(current, function_scope);
                if widened != current
                    && let Some(cell) = scope.get_mut(referenced)
                {
                    *cell = Some(widened);
                    changed = true;
                }
            }

            if !changed {
                break;
            }
        }
    }
}
