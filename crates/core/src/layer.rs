//! Longest-path layering over the SCC condensation.
//!
//! Each condensation node is assigned a layer such that every hard dependency
//! points strictly downward: `layer(v) = 1 + max(layer(dep))` over `v`'s hard
//! dependencies, with dependency-free nodes (sources) at layer `1`. The pass is
//! linear — a single sweep in topological order — and exposes the natural
//! strata of the codebase.

use crate::condense::Condensation;

/// Assigns every SCC in `cond` its longest-path layer.
///
/// The returned vector is indexed by SCC id. Because [`crate::condense::condense`]
/// emits SCCs in reverse topological order, every dependency of an SCC has a
/// strictly smaller id and is therefore already finalized when that SCC is
/// processed, so a single ascending sweep suffices.
///
/// The invariant — no hard edge points from a lower to a higher layer — is
/// property-tested.
#[must_use]
pub fn layer(cond: &Condensation) -> Vec<u32> {
    let dag = &cond.dag;
    let scc_count = dag.vertex_count();
    let mut layers = vec![1_u32; scc_count];

    // Ascending SCC id is a topological order: an SCC's dependencies (its
    // forward neighbours in the quotient DAG) all carry smaller ids.
    for scc in 0..scc_count {
        let scc_u32 = u32::try_from(scc).unwrap_or(u32::MAX);
        let mut best_dependency = 0_u32;
        for &dependency in dag.neighbors(scc_u32) {
            let depth = layers.get(dependency as usize).copied().unwrap_or(0);
            best_dependency = best_dependency.max(depth);
        }
        if let Some(slot) = layers.get_mut(scc) {
            *slot = best_dependency + 1;
        }
    }

    layers
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use smol_str::SmolStr;
    use strata_ir::{
        Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness,
        IntermediateRepresentation, Node, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot,
    };

    use super::*;
    use crate::condense::condense;
    use crate::graph::csr::{Csr, HardnessFilter, build_csr};

    fn node(id: u32) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(format!("n{id}")),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
        }
    }

    fn edge(source: u32, target: u32) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind: EdgeKind::Call,
            hardness: Hardness::Hard,
            confidence: 1.0,
        }
    }

    fn condensation(node_count: u32, edges: Vec<Edge>) -> Condensation {
        let nodes = (0..node_count).map(node).collect();
        let tree = ContainerTree::new(vec![Container {
            id: ContainerId(0),
            name: SmolStr::new("root"),
            level: ScopeLevel::File,
            parent: None,
            synthetic: false,
        }]);
        let ir = IntermediateRepresentation::new(nodes, edges, tree);

        let assembled = Snapshot::assemble(ir);
        assert!(
            assembled.is_ok(),
            "hand-built test ir failed to assemble: {assembled:?}"
        );

        assembled.ok().map_or_else(
            || Condensation {
                dag: Csr::from_sorted_edges(0, &[]),
                membership: Vec::new(),
                members: Vec::new(),
            },
            |snap| {
                let csr = build_csr(&snap, HardnessFilter::HardOnly).forward;
                condense(&csr)
            },
        )
    }

    /// Returns the layer of the SCC owning node `n`, or `0` when absent.
    fn layer_of(cond: &Condensation, layers: &[u32], n: u32) -> u32 {
        cond.membership
            .get(n as usize)
            .and_then(|scc| layers.get(scc.0 as usize))
            .copied()
            .unwrap_or(0)
    }

    #[test]
    fn should_place_a_source_at_layer_one() {
        let cond = condensation(2, vec![edge(0, 1)]);

        let layers = layer(&cond);

        assert_eq!(layer_of(&cond, &layers, 1), 1);
    }

    #[test]
    fn should_stack_a_chain_by_longest_path() {
        let cond = condensation(3, vec![edge(0, 1), edge(1, 2)]);

        let layers = layer(&cond);

        let stacked = [
            layer_of(&cond, &layers, 2),
            layer_of(&cond, &layers, 1),
            layer_of(&cond, &layers, 0),
        ];
        assert_eq!(stacked, [1, 2, 3]);
    }

    #[test]
    fn should_take_the_longest_path_at_a_join() {
        // 0 -> 1 -> 3 and 0 -> 2, with 3 -> ... ; node 0 depends on a length-2
        // path through 1->3 and a length-1 path through 2.
        let cond = condensation(4, vec![edge(0, 1), edge(1, 3), edge(0, 2), edge(2, 3)]);

        let layers = layer(&cond);

        // 3 is the sink (layer 1); 1 and 2 sit at layer 2; 0 at layer 3.
        assert_eq!(layer_of(&cond, &layers, 0), 3);
    }

    #[test]
    fn should_give_every_member_of_a_cycle_one_layer() {
        let cond = condensation(3, vec![edge(0, 1), edge(1, 0), edge(1, 2)]);

        let layers = layer(&cond);

        assert_eq!(layer_of(&cond, &layers, 0), layer_of(&cond, &layers, 1));
    }

    #[test]
    fn should_never_point_a_hard_edge_to_a_higher_layer() {
        let cond = condensation(5, vec![edge(0, 1), edge(1, 2), edge(0, 3), edge(3, 4)]);

        let layers = layer(&cond);

        // For every quotient edge source -> dep, layer(source) > layer(dep).
        for scc in 0..cond.dag.vertex_count() {
            let scc_u32 = u32::try_from(scc).unwrap_or(u32::MAX);
            let source_layer = layers.get(scc).copied().unwrap_or(0);
            for &dep in cond.dag.neighbors(scc_u32) {
                let dep_layer = layers.get(dep as usize).copied().unwrap_or(0);
                assert!(source_layer > dep_layer);
            }
        }
    }

    proptest! {
        /// Over arbitrary graphs, every quotient-DAG edge must descend a layer:
        /// `layer(source) > layer(dep)`. This is the layering invariant.
        #[test]
        fn should_keep_every_quotient_edge_pointing_downward(
            node_count in 1_u32..12,
            raw_edges in proptest::collection::vec((0_u32..12, 0_u32..12), 0..40),
        ) {
            let edges = raw_edges
                .into_iter()
                .filter(|&(source, target)| source < node_count && target < node_count)
                .map(|(source, target)| edge(source, target))
                .collect();
            let cond = condensation(node_count, edges);

            let layers = layer(&cond);

            for scc in 0..cond.dag.vertex_count() {
                let scc_u32 = u32::try_from(scc).unwrap_or(u32::MAX);
                let source_layer = layers.get(scc).copied().unwrap_or(0);
                for &dep in cond.dag.neighbors(scc_u32) {
                    let dep_layer = layers.get(dep as usize).copied().unwrap_or(0);
                    prop_assert!(source_layer > dep_layer);
                }
            }
        }
    }
}
