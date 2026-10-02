//! Fiduccia–Mattheyses refinement, applied at every uncoarsening level.
//!
//! Refinement sweeps single-node moves to lower the partition's cut while two
//! vetoes guard the inviolable constraints:
//!
//! - **acyclicity** — the quotient over clusters must stay a DAG. A topological
//!   order of the quotient is maintained incrementally; a move is legal only if
//!   the cluster-pair edges it introduces can be absorbed into that order. The
//!   check inspects only the edges incident to the moved node's endpoints (a
//!   bounded Kahn-style local repair), never a full recomputation over the node
//!   graph;
//! - **capacity** — the target cluster must stay within its level cap.
//!
//! A move applies only when both vetoes pass *and* its gain is strictly
//! positive. Gain is the cut-weight reduction — over the node's outgoing *and*
//! incoming edges — plus mode-dependent cohesion bonuses (`α · naming-token
//! cohesion + β · path cohesion`), supplied by a [`GainFn`]. Passes repeat until
//! one completes with no applied move; determinism comes from a fixed
//! descending-gain order with ties broken by node index.

mod candidates;
mod gain;

pub use self::gain::GainFn;

use crate::cluster::coarsen::CoarseGraph;
use crate::cluster::quotient::{QuotientEdges, ReverseEdges};
use crate::cluster::{LevelCaps, Partition};

use self::candidates::{move_gain, ranked_moves};

/// Refines `parts` in place with FM single-node moves over `g`, under the
/// `level` cap and the cohesion-aware `gain`.
///
/// Each pass scores every node's best legal move (one that keeps the quotient
/// acyclic and the target within cap — measured in summed capacity weight from
/// `g.vertex_weights`, files at the folder level), then applies the
/// strictly-positive-gain moves in descending gain order, ties broken by node
/// index. Passes repeat until one applies no move. The partition is never left
/// cyclic, and no cluster ever grows past the cap.
pub fn refine(
    g: &CoarseGraph,
    parts: &mut Partition,
    gain: &GainFn,
    caps: &LevelCaps,
    level: crate::cluster::seed::SeedLevel,
) {
    let cap = level.cap(caps).max(1);
    let graph = &g.graph;
    let reverse = ReverseEdges::from_graph(graph);
    let mut quotient = QuotientEdges::from_partition(graph, parts);
    let vertex_weight =
        |node: u32| -> u32 { g.vertex_weights.get(node as usize).copied().unwrap_or(1) };
    let mut cluster_weights = vec![0_u32; parts.cluster_count()];
    for node in 0..graph.vertex_count() {
        let node = u32::try_from(node).unwrap_or(u32::MAX);
        if let Some(cluster) = parts.cluster_of(node)
            && let Some(slot) = cluster_weights.get_mut(cluster.0 as usize)
        {
            *slot = slot.saturating_add(vertex_weight(node));
        }
    }

    loop {
        let mut moved = false;
        let mut candidates = ranked_moves(graph, &reverse, parts, gain);
        // Apply in descending gain, ties by node index (already the sort key).
        candidates.sort_by(|a, b| {
            b.gain
                .partial_cmp(&a.gain)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.node.cmp(&b.node))
        });

        for candidate in candidates {
            if candidate.gain <= 0.0 {
                break;
            }
            let Some(source) = parts.cluster_of(candidate.node) else {
                continue;
            };
            if source == candidate.target {
                continue;
            }
            let weight = vertex_weight(candidate.node);
            let target_weight = cluster_weights
                .get(candidate.target.0 as usize)
                .copied()
                .unwrap_or(u32::MAX);
            if target_weight.saturating_add(weight) > cap {
                continue;
            }
            // The ranking is computed once per pass, but earlier moves shift the
            // partition under later candidates. Re-evaluate the gain against the
            // live partition so a move that has since turned non-improving is
            // dropped — without this, two mutually-attracted nodes oscillate by
            // chasing each other's stale positive gain.
            let live_gain = move_gain(
                graph,
                &reverse,
                parts,
                gain,
                candidate.node,
                source,
                candidate.target,
            );
            if live_gain <= 0.0 {
                continue;
            }
            let deltas = QuotientEdges::move_deltas(
                graph,
                &reverse,
                parts,
                candidate.node,
                source,
                candidate.target,
            );
            if !quotient.stays_acyclic_under(&deltas) {
                continue;
            }
            quotient.apply(&deltas);
            parts.move_node(candidate.node, candidate.target);
            if let Some(slot) = cluster_weights.get_mut(source.0 as usize) {
                *slot = slot.saturating_sub(weight);
            }
            if let Some(slot) = cluster_weights.get_mut(candidate.target.0 as usize) {
                *slot = slot.saturating_add(weight);
            }
            moved = true;
        }

        if !moved {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::candidates::move_gain;
    use super::gain::jaccard;
    use super::*;
    use crate::cluster::ClusterId;
    use crate::cluster::seed::SeedLevel;
    use crate::graph::csr::Csr;

    /// Builds a CSR over `vertex_count` vertices from `(source, target)` edges,
    /// sorted and deduplicated.
    fn csr(vertex_count: usize, mut edges: Vec<(u32, u32)>) -> Csr {
        edges.sort_unstable();
        edges.dedup();
        Csr::from_sorted_edges(vertex_count, &edges)
    }

    /// Wraps a graph as an identity-mapped [`CoarseGraph`] with flat layers.
    fn coarse(graph: Csr) -> CoarseGraph {
        let fine_to_coarse = (0..graph.vertex_count())
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
            .collect();
        let layers = vec![1_u32; graph.vertex_count()];
        let vertex_weights = vec![1; graph.vertex_count()];
        CoarseGraph {
            graph,
            fine_to_coarse,
            layers,
            vertex_weights,
        }
    }

    /// Builds a partition from a raw cluster-id assignment.
    fn partition(assignment: &[u32], cluster_count: usize) -> Partition {
        let ids = assignment.iter().copied().map(ClusterId).collect();
        Partition::from_assignment(ids, cluster_count)
    }

    /// Asserts the quotient induced by `parts` over `graph` is acyclic.
    fn assert_quotient_acyclic(graph: &Csr, parts: &Partition) {
        let quotient = QuotientEdges::from_partition(graph, parts);
        let edges: Vec<(ClusterId, ClusterId)> = quotient.counts.keys().copied().collect();
        assert!(is_acyclic(&edges), "quotient is cyclic: {edges:?}");
    }

    /// Independent acyclicity oracle over a cluster-pair edge list (Kahn's
    /// algorithm), used only by tests to cross-check the incremental veto.
    fn is_acyclic(edges: &[(ClusterId, ClusterId)]) -> bool {
        let mut indegree: HashMap<ClusterId, u32> = HashMap::new();
        let mut adjacency: HashMap<ClusterId, Vec<ClusterId>> = HashMap::new();
        let mut nodes: Vec<ClusterId> = Vec::new();
        for &(from, to) in edges {
            adjacency.entry(from).or_default().push(to);
            *indegree.entry(to).or_insert(0) += 1;
            indegree.entry(from).or_insert(0);
            nodes.push(from);
            nodes.push(to);
        }
        nodes.sort_unstable();
        nodes.dedup();

        let mut queue: Vec<ClusterId> = nodes
            .iter()
            .copied()
            .filter(|n| indegree.get(n).copied().unwrap_or(0) == 0)
            .collect();
        let mut visited = 0_usize;
        while let Some(node) = queue.pop() {
            visited += 1;
            for &target in adjacency.get(&node).into_iter().flatten() {
                if let Some(slot) = indegree.get_mut(&target) {
                    *slot -= 1;
                    if *slot == 0 {
                        queue.push(target);
                    }
                }
            }
        }
        visited == nodes.len()
    }

    #[test]
    fn should_report_full_overlap_as_jaccard_one() {
        assert!((jaccard(&[1, 2, 3], &[1, 2, 3]) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn should_report_disjoint_sets_as_jaccard_zero() {
        assert!(jaccard(&[1, 2], &[3, 4]).abs() < f32::EPSILON);
    }

    #[test]
    fn should_detect_a_cyclic_quotient() {
        let cycle = vec![(ClusterId(0), ClusterId(1)), (ClusterId(1), ClusterId(0))];
        assert!(!is_acyclic(&cycle));
    }

    #[test]
    fn should_accept_an_acyclic_quotient() {
        let chain = vec![(ClusterId(0), ClusterId(1)), (ClusterId(1), ClusterId(2))];
        assert!(is_acyclic(&chain));
    }

    #[test]
    fn should_pull_a_node_toward_a_cohesive_cluster() {
        // 0 -> 1, separate clusters; structural CSR weights are zero, so naming
        // cohesion drives the move: nodes 0 and 1 share the token set {7}.
        let graph = coarse(csr(2, vec![(0, 1)]));
        let mut parts = partition(&[0, 1], 2);
        let gain = GainFn::new(
            vec![vec![7], vec![7]],
            vec![Vec::new(), Vec::new()],
            1.0,
            0.0,
        );

        refine(
            &graph,
            &mut parts,
            &gain,
            &LevelCaps::defaults(),
            SeedLevel::Folder,
        );

        assert_eq!(parts.cluster_of(0), parts.cluster_of(1));
    }

    #[test]
    fn should_hold_a_same_dir_group_against_a_weak_cut_lure() {
        // Planted partition: dir A = {4,5} (path token 100), dir B = {0,1,2,3}
        // (path token 200). Node 5 (in A) has a weak two-edge pull into B plus
        // one intra-A edge — a net cut gain of +1 from tearing into B. Strong
        // path cohesion (β = 2) must out-vote that weak lure and keep 5 with 4.
        let graph = coarse(Csr::from_weighted_edges(
            6,
            &[(5, 0, 1.0), (5, 1, 1.0), (5, 4, 1.0)],
        ));
        let mut parts = partition(&[0, 0, 0, 0, 1, 1], 2);
        let path = vec![
            vec![200],
            vec![200],
            vec![200],
            vec![200],
            vec![100],
            vec![100],
        ];
        let naming = vec![Vec::new(); 6];
        let gain = GainFn::new(naming, path, 0.5, 2.0);

        refine(
            &graph,
            &mut parts,
            &gain,
            &LevelCaps::defaults(),
            SeedLevel::Folder,
        );

        // node 5 stays in dir A alongside node 4 — the grab-bag tear is refused.
        assert_eq!(parts.cluster_of(5), parts.cluster_of(4));
    }

    #[test]
    fn should_yield_a_same_dir_node_to_a_strong_dependency() {
        // Same planted partition, but node 5 now depends on four B members: the
        // cut gain of tearing into B is +4, which out-votes the same β = 2 path
        // cohesion. The penalty is soft, not a hard veto, so a genuinely strong
        // dependency still pulls the node across.
        let graph = coarse(Csr::from_weighted_edges(
            6,
            &[(5, 0, 1.0), (5, 1, 1.0), (5, 2, 1.0), (5, 3, 1.0)],
        ));
        let mut parts = partition(&[0, 0, 0, 0, 1, 1], 2);
        let path = vec![
            vec![200],
            vec![200],
            vec![200],
            vec![200],
            vec![100],
            vec![100],
        ];
        let naming = vec![Vec::new(); 6];
        let gain = GainFn::new(naming, path, 0.5, 2.0);

        refine(
            &graph,
            &mut parts,
            &gain,
            &LevelCaps::defaults(),
            SeedLevel::Folder,
        );

        // node 5 follows its strong dependency into dir B.
        assert_eq!(parts.cluster_of(5), parts.cluster_of(0));
    }

    #[test]
    fn should_keep_the_quotient_acyclic_after_refinement() {
        // A small DAG: 0 -> 1 -> 2, every node in its own cluster.
        let graph = coarse(csr(3, vec![(0, 1), (1, 2)]));
        let mut parts = partition(&[0, 1, 2], 3);
        let gain = GainFn::cut_only(3);

        refine(
            &graph,
            &mut parts,
            &gain,
            &LevelCaps::defaults(),
            SeedLevel::Folder,
        );

        assert_quotient_acyclic(&graph.graph, &parts);
    }

    #[test]
    fn should_veto_a_move_that_would_overflow_the_cap() {
        // 0 -> 1, both wanting to merge, but the folder cap is 1: no move fits.
        let graph = coarse(csr(2, vec![(0, 1)]));
        let mut parts = partition(&[0, 1], 2);
        let gain = GainFn::cut_only(2);
        let caps = LevelCaps {
            folder: 1,
            ..LevelCaps::defaults()
        };

        refine(&graph, &mut parts, &gain, &caps, SeedLevel::Folder);

        // The cap veto kept them apart.
        assert_ne!(parts.cluster_of(0), parts.cluster_of(1));
    }

    #[test]
    fn should_veto_a_move_by_capacity_weight_not_vertex_count() {
        // 0 -> 1 pull each other, and each cluster holds a single vertex, but
        // vertex 0 weighs 8 and vertex 1 weighs 10: merged they breach the cap
        // of 15 even though the target holds just one member.
        let mut graph = coarse(csr(2, vec![(0, 1)]));
        graph.vertex_weights = vec![8, 10];
        let mut parts = partition(&[0, 1], 2);
        let gain = GainFn::cut_only(2);
        let caps = LevelCaps {
            folder: 15,
            ..LevelCaps::defaults()
        };

        refine(&graph, &mut parts, &gain, &caps, SeedLevel::Folder);

        // The weighted cap veto kept them apart.
        assert_ne!(parts.cluster_of(0), parts.cluster_of(1));
    }

    #[test]
    fn should_veto_a_move_that_would_make_the_quotient_cyclic() {
        // Chain 0 -> 1 -> 2 with clusters A={0}, B={1}, C={2}: quotient A->B->C is
        // acyclic. Moving node 2 into A turns edge 1->2 into B->A, closing the
        // cycle A->B->A. The incremental veto must reject it.
        let graph = csr(3, vec![(0, 1), (1, 2)]);
        let parts = partition(&[0, 1, 2], 3);
        let reverse = ReverseEdges::from_graph(&graph);
        let quotient = QuotientEdges::from_partition(&graph, &parts);

        let cycle_move =
            QuotientEdges::move_deltas(&graph, &reverse, &parts, 2, ClusterId(2), ClusterId(0));
        assert!(!quotient.stays_acyclic_under(&cycle_move));

        // Moving node 0 into B (its dependency's cluster) merely shortens the
        // chain to B->C, which stays acyclic.
        let safe_move =
            QuotientEdges::move_deltas(&graph, &reverse, &parts, 0, ClusterId(0), ClusterId(1));
        assert!(quotient.stays_acyclic_under(&safe_move));
    }

    #[test]
    fn should_count_incoming_edges_in_the_cut_delta() {
        // Edge 0 -> 1 with a unit weight; node 0 in cluster A, node 1 in B.
        // Moving node 1 (the edge *target*) into A removes the only cut edge, so
        // the cut delta is +w. The contribution comes solely from node 1's
        // incoming edge: a gain that ignored predecessors would score this as 0.
        let mut reverse = ReverseEdges::from_graph(&csr(2, vec![(0, 1)]));
        if let Some(list) = reverse.predecessors_mut().get_mut(1) {
            list.clear();
            list.push((0, 1.0));
        }
        let graph = csr(2, vec![(0, 1)]);
        let parts = partition(&[0, 1], 2);
        let gain = GainFn::cut_only(2);

        let delta = move_gain(
            &graph,
            &reverse,
            &parts,
            &gain,
            1,
            ClusterId(1),
            ClusterId(0),
        );

        assert!((delta - 1.0).abs() < f32::EPSILON, "delta was {delta}");
    }

    #[test]
    fn should_sum_every_incoming_edge_toward_the_move() {
        // Two predecessors of node 2 sit in the target cluster A; the cut delta
        // over node 2's incoming edges is their summed weight.
        let mut reverse = ReverseEdges::from_graph(&csr(3, vec![(0, 2), (1, 2)]));
        if let Some(list) = reverse.predecessors_mut().get_mut(2) {
            *list = vec![(0, 1.0), (1, 2.0)];
        }
        let graph = csr(3, vec![(0, 2), (1, 2)]);
        let parts = partition(&[0, 0, 1], 2);
        let gain = GainFn::cut_only(3);

        let delta = move_gain(
            &graph,
            &reverse,
            &parts,
            &gain,
            2,
            ClusterId(1),
            ClusterId(0),
        );

        // Both heavy in-edges (weights 1 and 2) stop being cut: delta = 3.
        assert!((delta - 3.0).abs() < f32::EPSILON, "delta was {delta}");
    }

    #[test]
    fn should_maintain_a_topological_order_incrementally() {
        // Chain A->B->C; after a legal move that rewires edges the maintained
        // order must still rank every live edge source before its target.
        let graph = csr(4, vec![(0, 1), (1, 2), (2, 3)]);
        let parts = partition(&[0, 1, 2, 3], 4);
        let reverse = ReverseEdges::from_graph(&graph);
        let mut quotient = QuotientEdges::from_partition(&graph, &parts);

        // Move node 3 from D into C: edge 2->3 (C->D) becomes internal, dropping
        // the D node from the quotient. The order over the survivors stays valid.
        let deltas =
            QuotientEdges::move_deltas(&graph, &reverse, &parts, 3, ClusterId(3), ClusterId(2));
        assert!(quotient.stays_acyclic_under(&deltas));
        quotient.apply(&deltas);

        for (&(from, to), &count) in &quotient.counts {
            assert!(count > 0);
            let rank_from = quotient.order.get(&from).copied().unwrap_or(u32::MAX);
            let rank_to = quotient.order.get(&to).copied().unwrap_or(0);
            assert!(
                rank_from < rank_to,
                "edge {from:?} -> {to:?} violates the maintained order"
            );
        }
    }

    /// Longest-path layers over dependent → dependency edges, by repeated
    /// relaxation (the test graphs are tiny).
    fn layers_of(graph: &Csr) -> Vec<u32> {
        let count = graph.vertex_count();
        let mut layers = vec![1_u32; count];
        for _ in 0..count {
            for v in 0..count {
                let v32 = u32::try_from(v).unwrap_or(u32::MAX);
                let mut best = 0_u32;
                for &dep in graph.neighbors(v32) {
                    best = best.max(layers.get(dep as usize).copied().unwrap_or(0));
                }
                if let Some(slot) = layers.get_mut(v) {
                    *slot = best + 1;
                }
            }
        }
        layers
    }

    proptest::proptest! {
        /// Seeding then refining an arbitrary DAG must never leave the quotient
        /// cyclic nor any cluster over its cap. Edges run high → low index so the
        /// generated graph is acyclic by construction (dependent → dependency).
        #[test]
        fn should_preserve_acyclicity_and_caps_through_refinement(
            node_count in 2_u32..10,
            raw_edges in proptest::collection::vec((0_u32..10, 0_u32..10), 0..30),
            folder_cap in 1_u32..6,
        ) {
            let edges: Vec<(u32, u32)> = raw_edges
                .into_iter()
                .filter(|&(a, b)| a < node_count && b < node_count && a > b)
                .collect();
            let graph = coarse(csr(node_count as usize, edges));
            let layers = layers_of(&graph.graph);
            let caps = LevelCaps { folder: folder_cap, ..LevelCaps::defaults() };

            let mut parts = crate::cluster::seed::seed(
                &graph,
                &layers,
                &caps,
                crate::cluster::seed::SeedLevel::Folder,
                &[],
            );
            // Cohesion-free gain keeps the move set structural; invariants must
            // hold regardless of which moves apply.
            let gain = GainFn::cut_only(node_count as usize);
            refine(&graph, &mut parts, &gain, &caps, SeedLevel::Folder);

            assert_quotient_acyclic(&graph.graph, &parts);
            for c in 0..parts.cluster_count() {
                let id = ClusterId(u32::try_from(c).unwrap_or(u32::MAX));
                proptest::prop_assert!(parts.size_of(id) <= folder_cap);
            }
        }

        /// The incremental veto must agree with an independent full-Kahn oracle
        /// on every candidate single-node move from a random acyclic partition.
        #[test]
        fn should_match_the_oracle_on_every_candidate_move(
            node_count in 2_u32..8,
            raw_edges in proptest::collection::vec((0_u32..8, 0_u32..8), 0..20),
        ) {
            let edges: Vec<(u32, u32)> = raw_edges
                .into_iter()
                .filter(|&(a, b)| a < node_count && b < node_count && a > b)
                .collect();
            let graph = csr(node_count as usize, edges);
            // Seed an acyclic partition: cluster id = node's own index keeps the
            // quotient acyclic (edges run high index -> low index).
            let raw: Vec<u32> = (0..node_count).collect();
            let parts = partition(&raw, node_count as usize);
            let reverse = ReverseEdges::from_graph(&graph);
            let quotient = QuotientEdges::from_partition(&graph, &parts);

            for node in 0..node_count {
                let Some(source) = parts.cluster_of(node) else { continue };
                for target_raw in 0..node_count {
                    let target = ClusterId(target_raw);
                    if target == source { continue; }
                    let deltas = QuotientEdges::move_deltas(
                        &graph, &reverse, &parts, node, source, target,
                    );
                    let incremental = quotient.stays_acyclic_under(&deltas);

                    // Oracle: apply deltas to a fresh edge set, run full Kahn.
                    let mut edge_counts: HashMap<(ClusterId, ClusterId), i64> = quotient
                        .counts
                        .iter()
                        .map(|(&p, &c)| (p, i64::from(c)))
                        .collect();
                    for &(pair, change) in &deltas {
                        *edge_counts.entry(pair).or_insert(0) += change;
                    }
                    let live: Vec<(ClusterId, ClusterId)> = edge_counts
                        .into_iter()
                        .filter(|&(_, c)| c > 0)
                        .map(|(p, _)| p)
                        .collect();
                    let oracle = is_acyclic(&live);

                    proptest::prop_assert_eq!(
                        incremental, oracle,
                        "veto disagreed with oracle moving {} {:?}->{:?}",
                        node, source, target
                    );
                }
            }
        }
    }
}
