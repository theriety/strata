//! Acyclicity-preserving heavy-edge matching, the coarsening phase.
//!
//! Coarsening repeatedly contracts node pairs to shrink the DAG until it is
//! small enough to seed directly, recording at each step how a coarse node maps
//! back to its finer members. The matching is *heavy-edge*: among a node's
//! eligible partners, the one across the heaviest edge wins, so dense
//! dependencies collapse first. The crucial constraint is acyclicity — a pair is
//! eligible to merge only when contracting it cannot create a quotient cycle.
//!
//! Edges in this crate point from dependent to dependency, so the layering
//! invariant is `layer(source) > layer(target)` (see [`crate::layer`]). A pair
//! `(u, v)` joined by `u -> v` is safe to contract exactly when no *alternative*
//! directed path `u ~> v` of length `>= 2` exists: such a path would, after the
//! merge, become a self-reaching detour through other clusters and could close a
//! cycle. The layering gives a cheap necessary screen — a two-hop detour can
//! only exist when `layer(u) - layer(v) >= 2` — so the explicit two-hop search
//! runs only for the remaining `layer(u) - layer(v) <= 1` candidates, which the
//! screen already proves detour-free at length two and beyond.

use crate::cluster::{ClusterId, Partition};
use crate::graph::csr::Csr;

/// One coarsening level: the contracted graph, the map back to the finer level
/// it was built from, and this level's own layering.
#[derive(Debug, Clone, PartialEq)]
pub struct CoarseGraph {
    /// The (possibly contracted) DAG at this level.
    pub graph: Csr,
    /// For each vertex of the *finer* level, the coarse vertex it folded into.
    /// Length equals the finer level's vertex count; values index `graph`.
    pub fine_to_coarse: Vec<u32>,
    /// Longest-path layer of each vertex of `graph` (a contracted vertex
    /// inherits the max layer of its members). Seeding and per-level refinement
    /// both consume this level's own layering, never the base level's.
    pub layers: Vec<u32>,
    /// Capacity weight of each vertex of `graph` — how many cap-counted members
    /// (files at the folder level, containers above it) the vertex stands for.
    /// A contracted vertex carries the sum of its members' weights, so seeding
    /// and refinement can hold the level cap at every scale of the chain.
    pub vertex_weights: Vec<u32>,
}

impl CoarseGraph {
    /// Wraps `graph` as a level that maps each finer vertex to itself (the
    /// identity coarsening used for the chain's base level).
    #[must_use]
    fn identity(graph: Csr, layers: Vec<u32>, vertex_weights: Vec<u32>) -> Self {
        let fine_to_coarse = (0..graph.vertex_count())
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
            .collect();
        Self {
            graph,
            fine_to_coarse,
            layers,
            vertex_weights,
        }
    }

    /// Projects a partition of this level's vertices down to the finer level
    /// this graph was contracted from: finer vertex `v` joins the cluster of
    /// its coarse image `fine_to_coarse[v]`.
    ///
    /// The uncoarsening step of the multilevel scheme — after refining at a
    /// coarse level, the assignment is projected one level finer and refined
    /// again. The result covers every finer vertex.
    #[must_use]
    pub fn project(&self, coarse: &Partition) -> Partition {
        debug_assert!(
            self.fine_to_coarse
                .iter()
                .all(|&image| (image as usize) < coarse.node_count()),
            "fine_to_coarse names a vertex outside the coarse partition"
        );
        let assignment = self
            .fine_to_coarse
            .iter()
            .map(|&image| coarse.cluster_of(image).unwrap_or(ClusterId(0)))
            .collect();
        Partition::from_assignment(assignment, coarse.cluster_count())
    }
}

/// Coarsens `dag` through a chain of heavy-edge matchings until no further pair
/// is eligible or the graph shrinks past the seed threshold.
///
/// `layers` is indexed by vertex (the longest-path layer from [`crate::layer`]).
/// `vertex_weights` gives each base vertex's capacity weight and `max_weight`
/// the level cap: a pair whose combined weight would exceed the cap is never
/// contracted, so no coarse vertex outgrows a legal cluster (a single base
/// vertex heavier than the cap stays an unsplittable atom). The returned chain
/// starts with the original graph wrapped as an identity level and ends with
/// the coarsest graph; [`crate::cluster::seed`] consumes the last entry and
/// [`crate::cluster::refine`] walks the chain in reverse.
#[must_use]
pub fn coarsen_chain(
    dag: &Csr,
    layers: &[u32],
    vertex_weights: &[u32],
    max_weight: u32,
) -> Vec<CoarseGraph> {
    let mut chain = vec![CoarseGraph::identity(
        dag.clone(),
        layers.to_vec(),
        vertex_weights.to_vec(),
    )];

    loop {
        // Borrow the tip just long enough to derive the next level, then drop the
        // borrow before pushing so the chain can grow.
        let next = chain.last().and_then(|level| {
            let graph = &level.graph;
            if graph.vertex_count() <= SEED_THRESHOLD {
                return None;
            }
            let matching =
                match_heavy_edges(graph, &level.layers, &level.vertex_weights, max_weight);
            if matching.contracted == graph.vertex_count() {
                // Nothing merged this round — the graph is irreducible under the
                // acyclicity rule, so further passes cannot shrink it.
                return None;
            }
            Some(contract(graph, &matching, &level.vertex_weights))
        });

        let Some(coarse) = next else {
            break;
        };
        chain.push(coarse);
    }

    chain
}

/// Stop coarsening once a level has at most this many vertices: small graphs
/// seed and refine directly without the multilevel detour.
const SEED_THRESHOLD: usize = 16;

/// Sentinel for an as-yet-unmatched vertex in a matching pass.
const UNMATCHED: u32 = u32::MAX;

/// The result of one heavy-edge matching pass over a level.
struct Matching {
    /// For each finer vertex, the coarse vertex it joins (dense `0..contracted`).
    fine_to_coarse: Vec<u32>,
    /// The number of coarse vertices produced.
    contracted: usize,
}

/// Selects an acyclicity-preserving heavy-edge matching of `graph`.
///
/// Vertices are visited in topological order — descending layer, then ascending
/// index — so each dependent gets first pick of its dependencies (its outgoing
/// neighbours). Each unmatched vertex `u` picks the heaviest eligible outgoing
/// partner `v` (an edge `u -> v` whose contraction stays acyclic and that is
/// itself unmatched); ties break toward the smaller vertex index. Unmatched
/// vertices map to fresh singleton coarse vertices.
fn match_heavy_edges(
    graph: &Csr,
    layers: &[u32],
    vertex_weights: &[u32],
    max_weight: u32,
) -> Matching {
    let vertex_count = graph.vertex_count();
    let mut partner = vec![UNMATCHED; vertex_count];
    let mut fine_to_coarse = vec![UNMATCHED; vertex_count];
    let mut next_coarse = 0_u32;

    let mut order: Vec<usize> = (0..vertex_count).collect();
    order.sort_by(|&a, &b| {
        let layer_a = layers.get(a).copied().unwrap_or(0);
        let layer_b = layers.get(b).copied().unwrap_or(0);
        layer_b.cmp(&layer_a).then(a.cmp(&b))
    });

    for u in order {
        let u32_u = u32::try_from(u).unwrap_or(UNMATCHED);
        if partner.get(u).copied().unwrap_or(UNMATCHED) != UNMATCHED {
            continue;
        }

        if let Some(v) = pick_partner(graph, layers, &partner, u32_u, vertex_weights, max_weight) {
            // Pair u with v under a shared coarse id.
            let coarse = next_coarse;
            next_coarse += 1;
            assign(&mut partner, &mut fine_to_coarse, u32_u, v, coarse);
        } else {
            // u stands alone as its own coarse vertex.
            if let Some(slot) = partner.get_mut(u) {
                *slot = u32_u;
            }
            if let Some(slot) = fine_to_coarse.get_mut(u) {
                *slot = next_coarse;
            }
            next_coarse += 1;
        }
    }

    Matching {
        fine_to_coarse,
        contracted: next_coarse as usize,
    }
}

/// Records that `u` and `v` form one coarse vertex `coarse`.
fn assign(partner: &mut [u32], fine_to_coarse: &mut [u32], u: u32, v: u32, coarse: u32) {
    if let Some(slot) = partner.get_mut(u as usize) {
        *slot = v;
    }
    if let Some(slot) = partner.get_mut(v as usize) {
        *slot = u;
    }
    if let Some(slot) = fine_to_coarse.get_mut(u as usize) {
        *slot = coarse;
    }
    if let Some(slot) = fine_to_coarse.get_mut(v as usize) {
        *slot = coarse;
    }
}

/// Picks the heaviest acyclicity-safe unmatched partner of `u` whose merged
/// capacity weight stays within `max_weight`, or `None`.
fn pick_partner(
    graph: &Csr,
    layers: &[u32],
    partner: &[u32],
    u: u32,
    vertex_weights: &[u32],
    max_weight: u32,
) -> Option<u32> {
    let neighbors = graph.neighbors(u);
    let weights = graph.weights(u);
    let weight_u = vertex_weights.get(u as usize).copied().unwrap_or(1);

    let mut best: Option<(u32, f32)> = None;
    for (slot, &v) in neighbors.iter().enumerate() {
        if v == u {
            continue;
        }
        if partner.get(v as usize).copied().unwrap_or(UNMATCHED) != UNMATCHED {
            continue;
        }
        let weight_v = vertex_weights.get(v as usize).copied().unwrap_or(1);
        if weight_u.saturating_add(weight_v) > max_weight {
            continue;
        }
        if !is_safe_to_merge(graph, layers, u, v) {
            continue;
        }
        let weight = weights.get(slot).copied().unwrap_or(0.0);
        let take = match best {
            None => true,
            // Strictly heavier wins; on a tie the smaller index already held the
            // slot (neighbours are sorted ascending), so no swap is needed.
            Some((_, best_weight)) => weight > best_weight,
        };
        if take {
            best = Some((v, weight));
        }
    }

    best.map(|(v, _)| v)
}

/// Returns `true` when contracting the edge `u -> v` cannot create a quotient
/// cycle.
///
/// `u -> v` means `u` depends on `v`, so `layer(u) > layer(v)`. The merge is
/// unsafe only if some *other* directed path `u ~> v` of length `>= 2` exists;
/// after contraction that path would route through the merged node and close a
/// cycle. The layering screen rejects the only configurations where such a
/// detour can exist (`layer(u) - layer(v) >= 2`), and for the surviving
/// `layer(u) - layer(v) <= 1` case an explicit two-hop probe confirms no length-
/// two detour `u -> w -> v` slips through equal-layer ties.
fn is_safe_to_merge(graph: &Csr, layers: &[u32], u: u32, v: u32) -> bool {
    let layer_u = layers.get(u as usize).copied().unwrap_or(0);
    let layer_v = layers.get(v as usize).copied().unwrap_or(0);

    // A two-hop (or longer) detour `u ~> v` forces at least two strictly
    // descending layer steps, i.e. `layer(u) - layer(v) >= 2`. Reject those.
    if layer_u > layer_v.saturating_add(1) {
        return false;
    }

    // Surviving candidates differ by at most one layer; a longer detour is
    // layer-impossible, but an alternative length-two path `u -> w -> v` through
    // an equal-layer `w` can still exist. Probe for it directly.
    for &w in graph.neighbors(u) {
        if w == v || w == u {
            continue;
        }
        if graph.neighbors(w).binary_search(&v).is_ok() {
            return false;
        }
    }

    true
}

/// Contracts `graph` under `matching`, building the coarse DAG and its layering.
///
/// Coarse edges are the summed-weight images of finer edges that cross between
/// distinct coarse vertices (self-loops dropped); coarse vertex weights are the
/// summed capacity weights of the members that folded together. The coarse
/// layering is recomputed tight from the contracted graph (see
/// [`tight_layers`]).
fn contract(graph: &Csr, matching: &Matching, fine_weights: &[u32]) -> CoarseGraph {
    let map = &matching.fine_to_coarse;
    let coarse_count = matching.contracted;

    let mut vertex_weights = vec![0_u32; coarse_count];
    for (fine, &coarse) in map.iter().enumerate() {
        if let Some(slot) = vertex_weights.get_mut(coarse as usize) {
            *slot = slot.saturating_add(fine_weights.get(fine).copied().unwrap_or(1));
        }
    }

    // Coarse edges, with parallel crossings' weights summed so heavy-edge
    // matching at the next level sees the aggregate pull between coarse nodes.
    let mut crossings: Vec<(u32, u32, f32)> = Vec::new();
    for fine in 0..graph.vertex_count() {
        let from = map.get(fine).copied().unwrap_or(UNMATCHED);
        let fine_u32 = u32::try_from(fine).unwrap_or(UNMATCHED);
        let weights = graph.weights(fine_u32);
        for (slot, &target) in graph.neighbors(fine_u32).iter().enumerate() {
            let to = map.get(target as usize).copied().unwrap_or(UNMATCHED);
            if from != to {
                let weight = weights.get(slot).copied().unwrap_or(0.0);
                crossings.push((from, to, weight));
            }
        }
    }

    let coarse = Csr::from_weighted_edges(coarse_count, &crossings);
    let coarse_layers = tight_layers(&coarse);
    CoarseGraph {
        graph: coarse,
        fine_to_coarse: map.clone(),
        layers: coarse_layers,
        vertex_weights,
    }
}

/// Computes tight longest-path layers of `graph` (edges point from dependent to
/// dependency): sinks sit at layer `1` and `layer(u) = 1 + max(layer(dep))`.
///
/// Inheriting a coarse vertex's layer from its members leaves gaps (a merged
/// pair keeps the max member layer, so adjacent coarse vertices can sit two
/// layers apart), and a gapped layering blinds the [`is_safe_to_merge`] screen —
/// a direct one-edge hop then looks like a two-hop detour and every further
/// merge is rejected, stalling the chain. Each level therefore recomputes its
/// own layering from scratch. The result is order-independent (each layer is a
/// max over all dependencies), hence deterministic. Public so the engine can
/// layer the upper-level quotient graphs it clusters recursively.
#[must_use]
pub fn tight_layers(graph: &Csr) -> Vec<u32> {
    let count = graph.vertex_count();
    let mut layers = vec![1_u32; count];

    // Kahn over the dependency direction: a vertex is ready once all of its
    // dependencies (outgoing neighbours) are finalized.
    let mut dependents: Vec<Vec<u32>> = vec![Vec::new(); count];
    let mut pending: Vec<u32> = vec![0; count];
    let mut ready: Vec<u32> = Vec::new();
    for u in 0..count {
        let u32_u = u32::try_from(u).unwrap_or(UNMATCHED);
        let dependencies = graph.neighbors(u32_u);
        if let Some(slot) = pending.get_mut(u) {
            *slot = u32::try_from(dependencies.len()).unwrap_or(u32::MAX);
        }
        if dependencies.is_empty() {
            ready.push(u32_u);
        }
        for &v in dependencies {
            if let Some(list) = dependents.get_mut(v as usize) {
                list.push(u32_u);
            }
        }
    }

    while let Some(v) = ready.pop() {
        let layer_v = layers.get(v as usize).copied().unwrap_or(1);
        for &u in dependents
            .get(v as usize)
            .map_or(&[] as &[u32], Vec::as_slice)
        {
            if let Some(slot) = layers.get_mut(u as usize) {
                *slot = (*slot).max(layer_v + 1);
            }
            if let Some(slot) = pending.get_mut(u as usize) {
                *slot = slot.saturating_sub(1);
                if *slot == 0 {
                    ready.push(u);
                }
            }
        }
    }

    layers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a CSR over `vertex_count` vertices from an unsorted edge list.
    fn csr(vertex_count: usize, mut edges: Vec<(u32, u32)>) -> Csr {
        edges.sort_unstable();
        edges.dedup();
        Csr::from_sorted_edges(vertex_count, &edges)
    }

    /// Unit capacity weights: one member per vertex, no weight cap in play.
    fn unit(vertex_count: usize) -> Vec<u32> {
        vec![1; vertex_count]
    }

    /// Computes longest-path layers directly from a DAG whose edges point from
    /// dependent to dependency, mirroring [`crate::layer`] over a condensation.
    fn layers_of(graph: &Csr) -> Vec<u32> {
        let count = graph.vertex_count();
        let mut layers = vec![1_u32; count];
        // Repeated relaxation; the graphs in tests are tiny.
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

    /// Asserts that `graph` is acyclic by checking a layering exists where every
    /// edge points strictly downward.
    fn assert_acyclic(graph: &Csr) {
        let layers = layers_of(graph);
        for v in 0..graph.vertex_count() {
            let v32 = u32::try_from(v).unwrap_or(u32::MAX);
            let lv = layers.get(v).copied().unwrap_or(0);
            for &dep in graph.neighbors(v32) {
                let ld = layers.get(dep as usize).copied().unwrap_or(0);
                assert!(lv > ld, "edge {v} -> {dep} does not descend a layer");
            }
        }
    }

    #[test]
    fn should_wrap_the_base_level_as_an_identity_map() {
        let graph = csr(3, vec![(2, 1), (1, 0)]);
        let layers = layers_of(&graph);

        let chain = coarsen_chain(&graph, &layers, &unit(graph.vertex_count()), u32::MAX);

        let identity = chain.first().map(|base| base.fine_to_coarse.clone());
        assert_eq!(identity, Some(vec![0, 1, 2]));
    }

    #[test]
    fn should_merge_an_adjacent_layer_pair() {
        // 1 -> 0 only: a single safe edge, layers 2 and 1.
        let graph = csr(2, vec![(1, 0)]);
        let layers = layers_of(&graph);

        let matching = match_heavy_edges(&graph, &layers, &unit(graph.vertex_count()), u32::MAX);

        // Both vertices fold into one coarse vertex.
        assert_eq!(matching.contracted, 1);
        assert_eq!(matching.fine_to_coarse, vec![0, 0]);
    }

    #[test]
    fn should_refuse_to_merge_across_a_two_hop_detour() {
        // 2 -> 1 -> 0 and 2 -> 0: merging 2 and 0 would close a cycle through 1.
        let graph = csr(3, vec![(2, 1), (1, 0), (2, 0)]);
        let layers = layers_of(&graph);

        // Directly probe the diagonal edge (2 -> 0).
        assert!(!is_safe_to_merge(&graph, &layers, 2, 0));
    }

    #[test]
    fn should_allow_merge_when_only_a_direct_edge_connects_the_pair() {
        // 2 -> 1 -> 0 chain: 2 -> 1 has no alternative path, so it is safe.
        let graph = csr(3, vec![(2, 1), (1, 0)]);
        let layers = layers_of(&graph);

        assert!(is_safe_to_merge(&graph, &layers, 2, 1));
    }

    #[test]
    fn should_pick_the_smaller_index_on_a_weight_tie() {
        // 0 -> 1 and 0 -> 2 with equal (zero) weights: the smaller neighbour wins.
        let base = csr(3, vec![(0, 1), (0, 2)]);
        let layers = [2, 1, 1];
        let partner = [UNMATCHED; 3];

        let chosen = pick_partner(&base, &layers, &partner, 0, &unit(3), u32::MAX);

        assert_eq!(chosen, Some(1));
    }

    #[test]
    fn should_keep_every_coarse_level_acyclic() {
        // A wider DAG that needs several coarsening rounds.
        let edges = vec![(5, 4), (5, 3), (4, 2), (3, 2), (2, 1), (2, 0), (1, 0)];
        let graph = csr(6, edges);
        let layers = layers_of(&graph);

        let chain = coarsen_chain(&graph, &layers, &unit(graph.vertex_count()), u32::MAX);

        for level in &chain {
            assert_acyclic(&level.graph);
        }
    }

    #[test]
    fn should_carry_each_levels_own_layers() {
        // a 40-vertex chain coarsens at least twice; every level must carry a
        // layering sized to its own vertex count, not the base level's.
        let edges: Vec<(u32, u32)> = (1..40).map(|i| (i, i - 1)).collect();
        let graph = csr(40, edges);
        let layers = layers_of(&graph);

        let chain = coarsen_chain(&graph, &layers, &unit(graph.vertex_count()), u32::MAX);

        assert!(chain.len() > 2, "40-vertex chain must coarsen twice");
        for level in &chain {
            assert_eq!(level.layers.len(), level.graph.vertex_count());
        }
    }

    #[test]
    fn should_compose_projections_down_the_whole_chain() {
        // the defect-1 regression tripwire: a partition of the coarsest level,
        // projected level by level, must land on the base level with every
        // vertex owned by the cluster of its composed coarse image.
        let edges: Vec<(u32, u32)> = (1..40).map(|i| (i, i - 1)).collect();
        let graph = csr(40, edges);
        let layers = layers_of(&graph);
        let chain = coarsen_chain(&graph, &layers, &unit(graph.vertex_count()), u32::MAX);
        assert!(chain.len() > 2, "40-vertex chain must coarsen twice");

        let coarsest = chain.last().map_or(0, |level| level.graph.vertex_count());
        assert!(coarsest <= SEED_THRESHOLD, "chain must reach the threshold");

        // one singleton cluster per coarsest vertex, so cluster id == vertex id.
        let assignment = (0..coarsest)
            .map(|v| ClusterId(u32::try_from(v).unwrap_or(u32::MAX)))
            .collect();
        let mut parts = Partition::from_assignment(assignment, coarsest);

        for level in chain.iter().rev() {
            parts = level.project(&parts);
        }

        assert_eq!(parts.node_count(), 40);
        for base in 0..40_u32 {
            let mut image = base;
            for level in chain.iter().skip(1) {
                image = level
                    .fine_to_coarse
                    .get(image as usize)
                    .copied()
                    .unwrap_or(u32::MAX);
            }
            assert_eq!(parts.cluster_of(base), Some(ClusterId(image)));
        }
    }

    #[test]
    fn should_stop_when_no_pair_is_eligible() {
        // A bipartite "diamond" where every edge has a two-hop alternative would
        // over-constrain; instead use two disconnected vertices: nothing merges.
        let graph = csr(2, vec![]);
        let layers = layers_of(&graph);

        let chain = coarsen_chain(&graph, &layers, &unit(graph.vertex_count()), u32::MAX);

        // Below the seed threshold and irreducible: only the base level.
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn should_refuse_to_contract_a_pair_past_the_weight_cap() {
        // 1 -> 0 is safe to merge, but the combined capacity weight 11 + 5
        // exceeds the cap of 15, so the pair must stay apart.
        let graph = csr(2, vec![(1, 0)]);
        let layers = layers_of(&graph);

        let matching = match_heavy_edges(&graph, &layers, &[11, 5], 15);

        assert_eq!(matching.contracted, 2);
    }

    #[test]
    fn should_sum_member_weights_into_the_coarse_vertex() {
        // 20 chained vertices of weight 3, cap 15: the chain coarsens, and every
        // level carries per-vertex weight sums that never breach the cap.
        let edges: Vec<(u32, u32)> = (1..20).map(|i| (i, i - 1)).collect();
        let graph = csr(20, edges);
        let layers = layers_of(&graph);

        let chain = coarsen_chain(&graph, &layers, &[3; 20], 15);

        assert!(chain.len() > 1, "the chain must coarsen at least once");
        for level in &chain {
            assert_eq!(level.vertex_weights.len(), level.graph.vertex_count());
            assert!(level.vertex_weights.iter().all(|&weight| weight <= 15));
            assert_eq!(level.vertex_weights.iter().sum::<u32>(), 60);
        }
    }
}
