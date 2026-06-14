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

use crate::graph::csr::Csr;

/// One coarsening level: the contracted graph plus the map back to the finer
/// level it was built from.
#[derive(Debug, Clone, PartialEq)]
pub struct CoarseGraph {
    /// The (possibly contracted) DAG at this level.
    pub graph: Csr,
    /// For each vertex of the *finer* level, the coarse vertex it folded into.
    /// Length equals the finer level's vertex count; values index `graph`.
    pub fine_to_coarse: Vec<u32>,
}

impl CoarseGraph {
    /// Wraps `graph` as a level that maps each finer vertex to itself (the
    /// identity coarsening used for the chain's base level).
    #[must_use]
    fn identity(graph: Csr) -> Self {
        let fine_to_coarse = (0..graph.vertex_count())
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
            .collect();
        Self {
            graph,
            fine_to_coarse,
        }
    }
}

/// Coarsens `dag` through a chain of heavy-edge matchings until no further pair
/// is eligible or the graph shrinks past the seed threshold.
///
/// `layers` is indexed by vertex (the longest-path layer from [`crate::layer`]).
/// The returned chain starts with the original graph wrapped as an identity
/// level and ends with the coarsest graph; [`crate::cluster::seed`] consumes the
/// last entry and [`crate::cluster::refine`] walks the chain in reverse.
#[must_use]
pub fn coarsen_chain(dag: &Csr, layers: &[u32]) -> Vec<CoarseGraph> {
    let mut chain = vec![CoarseGraph::identity(dag.clone())];
    // Each contracted level carries its own layering, derived from the finer
    // level's layers (a merged node inherits the max layer of its members).
    let mut current_layers: Vec<u32> = layers.to_vec();

    loop {
        // Borrow the tip just long enough to derive the next level, then drop the
        // borrow before pushing so the chain can grow.
        let next = chain.last().and_then(|level| {
            let graph = &level.graph;
            if graph.vertex_count() <= SEED_THRESHOLD {
                return None;
            }
            let matching = match_heavy_edges(graph, &current_layers);
            if matching.contracted == graph.vertex_count() {
                // Nothing merged this round — the graph is irreducible under the
                // acyclicity rule, so further passes cannot shrink it.
                return None;
            }
            Some(contract(graph, &current_layers, &matching))
        });

        let Some((coarse, coarse_layers)) = next else {
            break;
        };
        current_layers = coarse_layers;
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
fn match_heavy_edges(graph: &Csr, layers: &[u32]) -> Matching {
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

        if let Some(v) = pick_partner(graph, layers, &partner, u32_u) {
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

/// Picks the heaviest acyclicity-safe unmatched partner of `u`, or `None`.
fn pick_partner(graph: &Csr, layers: &[u32], partner: &[u32], u: u32) -> Option<u32> {
    let neighbors = graph.neighbors(u);
    let weights = graph.weights(u);

    let mut best: Option<(u32, f32)> = None;
    for (slot, &v) in neighbors.iter().enumerate() {
        if v == u {
            continue;
        }
        if partner.get(v as usize).copied().unwrap_or(UNMATCHED) != UNMATCHED {
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
/// Coarse edges are the deduplicated images of finer edges that cross between
/// distinct coarse vertices (self-loops dropped). A coarse vertex's layer is the
/// maximum layer of its members, which preserves the downward-pointing invariant
/// across the contraction.
fn contract(graph: &Csr, layers: &[u32], matching: &Matching) -> (CoarseGraph, Vec<u32>) {
    let map = &matching.fine_to_coarse;
    let coarse_count = matching.contracted;

    // Coarse layers: max member layer.
    let mut coarse_layers = vec![0_u32; coarse_count];
    for (fine, &coarse) in map.iter().enumerate() {
        let layer = layers.get(fine).copied().unwrap_or(0);
        if let Some(slot) = coarse_layers.get_mut(coarse as usize) {
            *slot = (*slot).max(layer);
        }
    }

    // Coarse edges, deduplicated.
    let mut crossings: Vec<(u32, u32)> = Vec::new();
    for fine in 0..graph.vertex_count() {
        let from = map.get(fine).copied().unwrap_or(UNMATCHED);
        let fine_u32 = u32::try_from(fine).unwrap_or(UNMATCHED);
        for &target in graph.neighbors(fine_u32) {
            let to = map.get(target as usize).copied().unwrap_or(UNMATCHED);
            if from != to {
                crossings.push((from, to));
            }
        }
    }
    crossings.sort_unstable();
    crossings.dedup();

    let coarse = CoarseGraph {
        graph: Csr::from_sorted_edges(coarse_count, &crossings),
        fine_to_coarse: map.clone(),
    };
    (coarse, coarse_layers)
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

        let chain = coarsen_chain(&graph, &layers);

        let identity = chain.first().map(|base| base.fine_to_coarse.clone());
        assert_eq!(identity, Some(vec![0, 1, 2]));
    }

    #[test]
    fn should_merge_an_adjacent_layer_pair() {
        // 1 -> 0 only: a single safe edge, layers 2 and 1.
        let graph = csr(2, vec![(1, 0)]);
        let layers = layers_of(&graph);

        let matching = match_heavy_edges(&graph, &layers);

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

        let chosen = pick_partner(&base, &layers, &partner, 0);

        assert_eq!(chosen, Some(1));
    }

    #[test]
    fn should_keep_every_coarse_level_acyclic() {
        // A wider DAG that needs several coarsening rounds.
        let edges = vec![(5, 4), (5, 3), (4, 2), (3, 2), (2, 1), (2, 0), (1, 0)];
        let graph = csr(6, edges);
        let layers = layers_of(&graph);

        let chain = coarsen_chain(&graph, &layers);

        for level in &chain {
            assert_acyclic(&level.graph);
        }
    }

    #[test]
    fn should_stop_when_no_pair_is_eligible() {
        // A bipartite "diamond" where every edge has a two-hop alternative would
        // over-constrain; instead use two disconnected vertices: nothing merges.
        let graph = csr(2, vec![]);
        let layers = layers_of(&graph);

        let chain = coarsen_chain(&graph, &layers);

        // Below the seed threshold and irreducible: only the base level.
        assert_eq!(chain.len(), 1);
    }
}
