//! LCA-based visibility derivation: a symbol's export scope is the lowest
//! container ancestor covering all its consumers, widened by transitive type
//! exposure.
//!
//! Visibility is derived, never declared. The lowest common ancestor of every
//! consumer's container is the narrowest scope at which a symbol must be visible;
//! a symbol nobody consumes stays private to its own file. Binary-lifting LCA
//! tables give `O(log V)` queries after an `O(V log V)` build, so the whole pass
//! costs `O(E log V)` — one query per consumer edge (FR-8).
//!
//! Type exposure then widens the picture monotonically: the parameter and return
//! types named in an exported function's signature are reachable by every
//! consumer of that function, even when no internal edge says so. Widening each
//! such type up to at least its function's scope is monotone, so the fixpoint
//! terminates. Finally, any declared visibility wider than the derived scope
//! becomes a [`Finding`] — the signal that drives barrel contents, since a
//! folder or domain barrel exports exactly the symbols whose derived scope
//! reaches that level.

use strata_ir::{ContainerId, ContainerTree, Edge, EdgeKind, Node, NodeId, NodeKind, ScopeLevel};

/// The derived export scope of a single symbol, expressed both as the container
/// that owns the scope and the level that container sits at.
///
/// `container` is the lowest common ancestor of every consumer's container, or
/// the symbol's own file when nobody consumes it. `level` is that container's
/// [`ScopeLevel`], the value compared against the declared visibility when
/// emitting findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedScope {
    /// The node whose scope this is.
    pub node: NodeId,
    /// The container at which the symbol must be visible.
    pub container: ContainerId,
    /// The level of `container`.
    pub level: ScopeLevel,
}

/// A symbol whose declared visibility is wider than its derived scope.
///
/// The pair of levels lets the report narrate the narrowing ("exported at
/// package level, needed only at folder level"); the node names the offending
/// symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finding {
    /// The over-exported node.
    pub node: NodeId,
    /// The visibility level declared in source.
    pub declared: ScopeLevel,
    /// The level the symbol is actually needed at.
    pub derived: ScopeLevel,
}

/// The outcome of visibility derivation: every node's derived scope plus the
/// over-export findings.
///
/// `scopes` is ordered by ascending [`NodeId`]; `findings` is ordered the same
/// way, so both are deterministic across runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibilityResult {
    /// The derived scope of every node, by ascending id.
    pub scopes: Vec<DerivedScope>,
    /// Symbols whose declared visibility is wider than derived, by ascending id.
    pub findings: Vec<Finding>,
}

/// Derives each symbol's minimal export scope and flags declared visibility that
/// is wider than derived.
///
/// The derivation has three steps:
///
/// 1. binary-lifting LCA tables are built over `tree` (`O(V log V)` build,
///    `O(log V)` per query);
/// 2. each node's scope is the LCA of its consumers' containers — a symbol with
///    no consumers stays private to its own file;
/// 3. the transitive type-exposure fixpoint widens each signature type up to at
///    least its exported function's scope, iterating to a monotone fixpoint.
///
/// A declared visibility wider than the derived scope yields a [`Finding`].
///
/// `nodes` carries the node→container map and declared visibility that `edges`
/// alone cannot supply; nodes are addressed by their [`NodeId`].
#[must_use]
pub fn derive_visibility(tree: &ContainerTree, nodes: &[Node], edges: &[Edge]) -> VisibilityResult {
    let lca = LcaTable::build(tree);
    let by_id = NodeIndex::build(nodes);

    let mut scope = initial_scopes(&by_id, edges, &lca);
    widen_by_type_exposure(&by_id, edges, &lca, &mut scope);

    let scopes = collect_scopes(&by_id, &scope, &lca);
    let findings = collect_findings(&by_id, &scopes);

    VisibilityResult { scopes, findings }
}

/// A node lookup keyed by raw [`NodeId`] value, plus the nodes in id order.
///
/// `slot` maps a node id to its index in `ordered`; ids absent from the input
/// map to `None`. `ordered` keeps the nodes sorted by ascending id so derived
/// output is deterministic.
struct NodeIndex<'nodes> {
    /// Nodes sorted by ascending id.
    ordered: Vec<&'nodes Node>,
    /// Index into `ordered` for each raw node id, `None` when absent.
    slot: Vec<Option<usize>>,
}

impl<'nodes> NodeIndex<'nodes> {
    /// Builds the index from `nodes`, sorting by ascending id.
    fn build(nodes: &'nodes [Node]) -> Self {
        let mut ordered: Vec<&Node> = nodes.iter().collect();
        ordered.sort_by_key(|node| node.id.0);

        let max_id = ordered.last().map_or(0, |node| node.id.0 as usize);
        let mut slot = vec![None; max_id.saturating_add(1)];
        for (index, node) in ordered.iter().enumerate() {
            if let Some(cell) = slot.get_mut(node.id.0 as usize) {
                *cell = Some(index);
            }
        }

        Self { ordered, slot }
    }

    /// Returns the node with raw id `id`, or `None` when absent.
    fn get(&self, id: u32) -> Option<&'nodes Node> {
        self.slot
            .get(id as usize)
            .copied()
            .flatten()
            .and_then(|index| self.ordered.get(index).copied())
    }
}

/// Computes each consumed node's initial scope: the LCA of its own container and
/// every consumer's container.
///
/// A consumer of `s` is the source of any edge whose target is `s`. The symbol's
/// own container seeds the LCA, so a symbol consumed only within its own file
/// stays file-private while one consumed from elsewhere widens to the common
/// ancestor of its declaration and that consumer. Nodes with no consumers never
/// appear here and default to their own file later.
fn initial_scopes(
    by_id: &NodeIndex,
    edges: &[Edge],
    lca: &LcaTable,
) -> std::collections::BTreeMap<u32, ContainerId> {
    let mut scope: std::collections::BTreeMap<u32, ContainerId> = std::collections::BTreeMap::new();

    for edge in edges {
        let (Some(consumer), Some(target)) = (by_id.get(edge.source.0), by_id.get(edge.target.0))
        else {
            continue;
        };
        let consumer_container = consumer.container;
        scope
            .entry(edge.target.0)
            // seed an unseen symbol with its own container, then fold in the
            // consumer so the scope is the LCA of declaration and consumption.
            .and_modify(|current| *current = lca.lca(*current, consumer_container))
            .or_insert_with(|| lca.lca(target.container, consumer_container));
    }

    scope
}

/// Widens signature types up to their exported functions' scopes until no scope
/// changes.
///
/// "Exported" means a derived scope above file level (something outside the
/// file consumes it). A signature type of such a function is any `Type` node
/// reached by a [`EdgeKind::TypeReference`] or [`EdgeKind::Inheritance`] edge
/// from the function. Each widening can only raise a type's scope toward the
/// root, so the process is monotone and terminates.
fn widen_by_type_exposure(
    by_id: &NodeIndex,
    edges: &[Edge],
    lca: &LcaTable,
    scope: &mut std::collections::BTreeMap<u32, ContainerId>,
) {
    loop {
        let mut changed = false;

        for edge in edges {
            if !is_signature_edge(edge.kind) {
                continue;
            }
            let (Some(function), Some(referenced)) =
                (by_id.get(edge.source.0), by_id.get(edge.target.0))
            else {
                continue;
            };
            if referenced.kind != NodeKind::Type {
                continue;
            }

            let function_scope = scope
                .get(&function.id.0)
                .copied()
                .unwrap_or(function.container);
            // a function only exposes its signature when itself exported above
            // its own file; a file-private function leaks nothing.
            if lca.level(function_scope) <= ScopeLevel::File {
                continue;
            }

            let current = scope
                .get(&referenced.id.0)
                .copied()
                .unwrap_or(referenced.container);
            let widened = lca.lca(current, function_scope);
            if widened != current {
                scope.insert(referenced.id.0, widened);
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}

/// Returns whether `kind` places its target in a function signature position.
fn is_signature_edge(kind: EdgeKind) -> bool {
    matches!(kind, EdgeKind::TypeReference | EdgeKind::Inheritance)
}

/// Materializes the per-node [`DerivedScope`] list in ascending id order.
///
/// A node absent from `scope` had no consumers and stays private to its own
/// file.
fn collect_scopes(
    by_id: &NodeIndex,
    scope: &std::collections::BTreeMap<u32, ContainerId>,
    lca: &LcaTable,
) -> Vec<DerivedScope> {
    by_id
        .ordered
        .iter()
        .map(|node| {
            let container = scope.get(&node.id.0).copied().unwrap_or(node.container);
            DerivedScope {
                node: node.id,
                container,
                level: lca.level(container),
            }
        })
        .collect()
}

/// Flags every node whose declared visibility sits strictly above its derived
/// scope level.
fn collect_findings(by_id: &NodeIndex, scopes: &[DerivedScope]) -> Vec<Finding> {
    scopes
        .iter()
        .filter_map(|scope| {
            let node = by_id.get(scope.node.0)?;
            (node.visibility > scope.level).then_some(Finding {
                node: scope.node,
                declared: node.visibility,
                derived: scope.level,
            })
        })
        .collect()
}

/// Binary-lifting ancestor tables over a [`ContainerTree`] forest.
///
/// `up[k][v]` is the `2^k`-th ancestor of container `v`, saturating at the root
/// (a root is its own ancestor). `depth[v]` is the root-relative depth used to
/// align two nodes before the lift. A forest is handled by giving every root a
/// self-loop, so two containers in different trees resolve their LCA to one of
/// the roots — the widest scope, which is the correct conservative answer.
struct LcaTable {
    /// `up[k][v]`: the `2^k`-th ancestor of `v`.
    up: Vec<Vec<u32>>,
    /// Root-relative depth of each container.
    depth: Vec<u32>,
    /// Level of each container, indexed by raw id.
    level: Vec<ScopeLevel>,
    /// Number of binary-lifting tiers (`ceil(log2(max_depth)) + 1`).
    tiers: usize,
}

impl LcaTable {
    /// Builds the ancestor tables over `tree`.
    fn build(tree: &ContainerTree) -> Self {
        let containers = tree.containers();
        let count = containers.len();

        let mut parent = vec![0_u32; count];
        let mut level = vec![ScopeLevel::File; count];
        for container in containers {
            let id = container.id.0 as usize;
            if let Some(slot) = parent.get_mut(id) {
                *slot = container.parent.map_or(container.id.0, |link| link.0);
            }
            if let Some(slot) = level.get_mut(id) {
                *slot = container.level;
            }
        }

        let depth = Self::depths(&parent);
        let max_depth = depth.iter().copied().max().unwrap_or(0);
        let tiers = usize::try_from(u32::BITS - max_depth.leading_zeros())
            .unwrap_or(1)
            .max(1);

        let mut up = vec![parent.clone()];
        for tier in 1..tiers {
            let previous = up.get(tier - 1).cloned().unwrap_or_default();
            let lifted = previous
                .iter()
                .map(|&ancestor| previous.get(ancestor as usize).copied().unwrap_or(ancestor))
                .collect();
            up.push(lifted);
        }

        Self {
            up,
            depth,
            level,
            tiers,
        }
    }

    /// Computes each container's depth from its tree root by walking parents,
    /// memoizing as it goes.
    fn depths(parent: &[u32]) -> Vec<u32> {
        let count = parent.len();
        let mut depth = vec![u32::MAX; count];

        for start in 0..count {
            // walk to a resolved ancestor or the root, recording the chain.
            let mut chain = Vec::new();
            let mut cursor = start;
            loop {
                let resolved = depth.get(cursor).copied().unwrap_or(u32::MAX);
                if resolved != u32::MAX {
                    break;
                }
                chain.push(cursor);
                let next = parent
                    .get(cursor)
                    .copied()
                    .unwrap_or_else(|| u32::try_from(cursor).unwrap_or(0))
                    as usize;
                if next == cursor {
                    // a root: depth zero, then unwind the chain above it.
                    if let Some(slot) = depth.get_mut(cursor) {
                        *slot = 0;
                    }
                    chain.pop();
                    break;
                }
                cursor = next;
            }

            let mut base = depth.get(cursor).copied().unwrap_or(0);
            while let Some(node) = chain.pop() {
                base = base.saturating_add(1);
                if let Some(slot) = depth.get_mut(node) {
                    *slot = base;
                }
            }
        }

        depth
    }

    /// Returns the level of container `id`, defaulting to [`ScopeLevel::File`]
    /// when out of range.
    fn level(&self, id: ContainerId) -> ScopeLevel {
        self.level
            .get(id.0 as usize)
            .copied()
            .unwrap_or(ScopeLevel::File)
    }

    /// Returns the lowest common ancestor of two containers.
    ///
    /// The deeper container is lifted to the shallower's depth, then both rise in
    /// lockstep until they meet. Containers in different trees of the forest
    /// resolve to a root, the widest (most conservative) scope.
    fn lca(&self, left: ContainerId, right: ContainerId) -> ContainerId {
        let mut a = left.0;
        let mut b = right.0;
        let depth_a = self.depth.get(a as usize).copied().unwrap_or(0);
        let depth_b = self.depth.get(b as usize).copied().unwrap_or(0);

        if depth_a < depth_b {
            b = self.lift(b, depth_b.saturating_sub(depth_a));
        } else {
            a = self.lift(a, depth_a.saturating_sub(depth_b));
        }

        if a == b {
            return ContainerId(a);
        }

        for tier in (0..self.tiers).rev() {
            let up_a = self.ancestor(a, tier);
            let up_b = self.ancestor(b, tier);
            if up_a != up_b {
                a = up_a;
                b = up_b;
            }
        }

        ContainerId(self.ancestor(a, 0))
    }

    /// Lifts container `node` up by `steps` levels via the binary-lifting tables.
    fn lift(&self, node: u32, steps: u32) -> u32 {
        let mut current = node;
        let mut remaining = steps;
        let mut tier = 0;
        while remaining > 0 {
            if remaining & 1 == 1 {
                current = self.ancestor(current, tier);
            }
            remaining >>= 1;
            tier += 1;
        }
        current
    }

    /// Returns the `2^tier`-th ancestor of `node`, saturating at `node` when the
    /// tier or id is out of range.
    fn ancestor(&self, node: u32, tier: usize) -> u32 {
        self.up
            .get(tier)
            .and_then(|table| table.get(node as usize))
            .copied()
            .unwrap_or(node)
    }
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use strata_ir::{Container, Hardness, NodeKind, Polarity};

    use super::*;

    /// Builds a container at `id` and `level` with optional `parent`.
    fn container(id: u32, level: ScopeLevel, parent: Option<u32>) -> Container {
        Container {
            id: ContainerId(id),
            name: SmolStr::new(format!("c{id}")),
            level,
            parent: parent.map(ContainerId),
        }
    }

    /// Builds a node owned by `container` with the given `kind` and declared
    /// `visibility`.
    fn node(id: u32, container: u32, kind: NodeKind, visibility: ScopeLevel) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(format!("n{id}")),
            kind,
            polarity: Polarity::Production,
            container: ContainerId(container),
            visibility,
            effective_size: 1,
        }
    }

    /// Builds a `source -> target` edge of `kind`.
    fn edge(source: u32, target: u32, kind: EdgeKind) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind,
            hardness: Hardness::Soft,
            confidence: 1.0,
        }
    }

    /// A small tree: package 0 over folders 1 and 2, each over one file (3, 4).
    fn sample_tree() -> ContainerTree {
        ContainerTree::new(vec![
            container(0, ScopeLevel::Package, None),
            container(1, ScopeLevel::Folder, Some(0)),
            container(2, ScopeLevel::Folder, Some(0)),
            container(3, ScopeLevel::File, Some(1)),
            container(4, ScopeLevel::File, Some(2)),
        ])
    }

    #[test]
    fn should_keep_an_unconsumed_symbol_file_private() {
        let tree = sample_tree();
        let nodes = vec![node(0, 3, NodeKind::Symbol, ScopeLevel::File)];

        let result = derive_visibility(&tree, &nodes, &[]);

        assert_eq!(
            result.scopes,
            vec![DerivedScope {
                node: NodeId(0),
                container: ContainerId(3),
                level: ScopeLevel::File,
            }]
        );
        assert!(result.findings.is_empty());
    }

    #[test]
    fn should_derive_file_scope_for_a_same_file_consumer() {
        let tree = sample_tree();
        // node 1 in file 3 consumes node 0 in file 3.
        let nodes = vec![
            node(0, 3, NodeKind::Symbol, ScopeLevel::File),
            node(1, 3, NodeKind::Symbol, ScopeLevel::File),
        ];
        let edges = vec![edge(1, 0, EdgeKind::Call)];

        let result = derive_visibility(&tree, &nodes, &edges);

        let scope_zero = result.scopes.iter().find(|scope| scope.node == NodeId(0));
        assert_eq!(scope_zero.map(|scope| scope.level), Some(ScopeLevel::File));
    }

    #[test]
    fn should_derive_folder_scope_for_cross_file_consumers_in_one_folder() {
        // folder 1 holds two files, 3 and 5; consumers in both reach the symbol.
        let mut containers = sample_tree().containers().to_vec();
        containers.push(container(5, ScopeLevel::File, Some(1)));
        let tree = ContainerTree::new(containers);

        // node 0 in file 3; consumed by node 1 (file 3) and node 2 (file 5).
        let nodes = vec![
            node(0, 3, NodeKind::Symbol, ScopeLevel::File),
            node(1, 3, NodeKind::Symbol, ScopeLevel::File),
            node(2, 5, NodeKind::Symbol, ScopeLevel::File),
        ];
        let edges = vec![edge(1, 0, EdgeKind::Call), edge(2, 0, EdgeKind::Call)];

        let result = derive_visibility(&tree, &nodes, &edges);

        let scope_zero = result.scopes.iter().find(|scope| scope.node == NodeId(0));
        assert_eq!(
            scope_zero.map(|scope| scope.level),
            Some(ScopeLevel::Folder)
        );
    }

    #[test]
    fn should_derive_package_scope_for_cross_folder_consumers() {
        let tree = sample_tree();
        // node 0 in file 3 (folder 1); consumed from file 4 (folder 2).
        let nodes = vec![
            node(0, 3, NodeKind::Symbol, ScopeLevel::File),
            node(1, 4, NodeKind::Symbol, ScopeLevel::File),
        ];
        let edges = vec![edge(1, 0, EdgeKind::Call)];

        let result = derive_visibility(&tree, &nodes, &edges);

        let scope_zero = result.scopes.iter().find(|scope| scope.node == NodeId(0));
        assert_eq!(
            scope_zero.map(|scope| scope.level),
            Some(ScopeLevel::Package)
        );
    }

    #[test]
    fn should_flag_a_declared_visibility_wider_than_derived() {
        let tree = sample_tree();
        // node 0 declared package-public but consumed only within its own file.
        let nodes = vec![
            node(0, 3, NodeKind::Symbol, ScopeLevel::Package),
            node(1, 3, NodeKind::Symbol, ScopeLevel::File),
        ];
        let edges = vec![edge(1, 0, EdgeKind::Call)];

        let result = derive_visibility(&tree, &nodes, &edges);

        assert_eq!(
            result.findings,
            vec![Finding {
                node: NodeId(0),
                declared: ScopeLevel::Package,
                derived: ScopeLevel::File,
            }]
        );
    }

    #[test]
    fn should_not_flag_a_declared_visibility_matching_derived() {
        let tree = sample_tree();
        // declared folder, consumed across files of one folder -> derived folder.
        let mut containers = sample_tree().containers().to_vec();
        containers.push(container(5, ScopeLevel::File, Some(1)));
        let tree2 = ContainerTree::new(containers);
        let _ = tree;

        let nodes = vec![
            node(0, 3, NodeKind::Symbol, ScopeLevel::Folder),
            node(1, 5, NodeKind::Symbol, ScopeLevel::File),
        ];
        let edges = vec![edge(1, 0, EdgeKind::Call)];

        let result = derive_visibility(&tree2, &nodes, &edges);

        assert!(result.findings.is_empty());
    }

    #[test]
    fn should_widen_a_signature_type_to_its_exported_function_scope() {
        let tree = sample_tree();
        // node 0 (function, file 3) is consumed across folders -> package scope.
        // node 2 (type, file 3) appears in node 0's signature; it should inherit
        // package scope even though it has no cross-folder consumer of its own.
        let nodes = vec![
            node(0, 3, NodeKind::Symbol, ScopeLevel::File),
            node(1, 4, NodeKind::Symbol, ScopeLevel::File),
            node(2, 3, NodeKind::Type, ScopeLevel::File),
        ];
        let edges = vec![
            edge(1, 0, EdgeKind::Call),
            edge(0, 2, EdgeKind::TypeReference),
        ];

        let result = derive_visibility(&tree, &nodes, &edges);

        let scope_type = result.scopes.iter().find(|scope| scope.node == NodeId(2));
        assert_eq!(
            scope_type.map(|scope| scope.level),
            Some(ScopeLevel::Package)
        );
    }

    #[test]
    fn should_not_widen_a_signature_type_for_a_file_private_function() {
        let tree = sample_tree();
        // function 0 is consumed only within its own file (file scope); its
        // signature type 2 must stay file-private.
        let nodes = vec![
            node(0, 3, NodeKind::Symbol, ScopeLevel::File),
            node(1, 3, NodeKind::Symbol, ScopeLevel::File),
            node(2, 3, NodeKind::Type, ScopeLevel::File),
        ];
        let edges = vec![
            edge(1, 0, EdgeKind::Call),
            edge(0, 2, EdgeKind::TypeReference),
        ];

        let result = derive_visibility(&tree, &nodes, &edges);

        let scope_type = result.scopes.iter().find(|scope| scope.node == NodeId(2));
        assert_eq!(scope_type.map(|scope| scope.level), Some(ScopeLevel::File));
    }

    #[test]
    fn should_propagate_type_exposure_transitively_to_a_fixpoint() {
        // function 0 (package scope) exposes type 2; type 2 exposes type 3 via
        // inheritance. Both should reach package scope through the fixpoint.
        let tree = sample_tree();
        let nodes = vec![
            node(0, 3, NodeKind::Symbol, ScopeLevel::File),
            node(1, 4, NodeKind::Symbol, ScopeLevel::File),
            node(2, 3, NodeKind::Type, ScopeLevel::File),
            node(3, 3, NodeKind::Type, ScopeLevel::File),
        ];
        let edges = vec![
            edge(1, 0, EdgeKind::Call),
            edge(0, 2, EdgeKind::TypeReference),
            edge(2, 3, EdgeKind::Inheritance),
        ];

        let result = derive_visibility(&tree, &nodes, &edges);

        let scope_three = result.scopes.iter().find(|scope| scope.node == NodeId(3));
        assert_eq!(
            scope_three.map(|scope| scope.level),
            Some(ScopeLevel::Package)
        );
    }

    #[test]
    fn should_emit_scopes_and_findings_in_ascending_node_order() {
        let tree = sample_tree();
        let nodes = vec![
            node(2, 3, NodeKind::Symbol, ScopeLevel::Package),
            node(0, 3, NodeKind::Symbol, ScopeLevel::Package),
            node(1, 3, NodeKind::Symbol, ScopeLevel::File),
        ];
        let edges = vec![edge(1, 0, EdgeKind::Call), edge(1, 2, EdgeKind::Call)];

        let result = derive_visibility(&tree, &nodes, &edges);

        let scope_ids: Vec<u32> = result.scopes.iter().map(|scope| scope.node.0).collect();
        assert_eq!(scope_ids, vec![0, 1, 2]);
        let finding_ids: Vec<u32> = result
            .findings
            .iter()
            .map(|finding| finding.node.0)
            .collect();
        assert_eq!(finding_ids, vec![0, 2]);
    }

    #[test]
    fn should_resolve_lca_across_forest_roots_to_a_root() {
        // two disjoint trees: package 0 -> file 1, and package 2 -> file 3.
        let tree = ContainerTree::new(vec![
            container(0, ScopeLevel::Package, None),
            container(1, ScopeLevel::File, Some(0)),
            container(2, ScopeLevel::Package, None),
            container(3, ScopeLevel::File, Some(2)),
        ]);
        // node 0 in file 1 consumed from file 3 in the other tree.
        let nodes = vec![
            node(0, 1, NodeKind::Symbol, ScopeLevel::File),
            node(1, 3, NodeKind::Symbol, ScopeLevel::File),
        ];
        let edges = vec![edge(1, 0, EdgeKind::Call)];

        let result = derive_visibility(&tree, &nodes, &edges);

        // the LCA is a package root: the widest, conservative scope.
        let scope_zero = result.scopes.iter().find(|scope| scope.node == NodeId(0));
        assert_eq!(
            scope_zero.map(|scope| scope.level),
            Some(ScopeLevel::Package)
        );
    }
}
