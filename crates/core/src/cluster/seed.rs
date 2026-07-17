//! Topological greedy seeding, the initial-partition phase.
//!
//! Seeding builds the first partition at the top of the coarsening chain. It
//! walks vertices in topological order and grows the current cluster until its
//! member cap is hit, then opens the next one. Because clusters take *contiguous*
//! ranges of a topological order, the seed quotient is acyclic by construction:
//! every edge runs from an earlier vertex to a later one, so a cross-cluster edge
//! can only point from a lower-numbered cluster to a higher-numbered one.
//!
//! Edges in this crate point from dependent to dependency, with
//! `layer(source) > layer(target)` (see [`crate::layer`]). The greedy growth walks
//! a group-aware topological order: a Kahn sweep that, among the vertices free to
//! emit, keeps a home-directory group contiguous *across* layers before falling
//! back to descending layer. Every edge still points forward (earlier → later), so
//! contiguous clusters over the order stay acyclic.

use crate::cluster::coarsen::CoarseGraph;
use crate::cluster::{ClusterId, LevelCaps, Partition};
use crate::graph::csr::Csr;

/// Which level's cap a seeding pass enforces.
///
/// Clustering runs once per container level (folder → domain → …); the seed cap
/// is the member cap for that level, selected from [`LevelCaps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedLevel {
    /// Group condensation nodes into folders.
    Folder,
    /// Group folders into domains.
    Domain,
    /// Group domains into packages.
    Package,
    /// Group packages into package groups.
    PackageGroup,
}

impl SeedLevel {
    /// Returns the member cap this level enforces from `caps`.
    #[must_use]
    fn cap(self, caps: &LevelCaps) -> u32 {
        match self {
            Self::Folder => caps.folder,
            Self::Domain => caps.domain,
            Self::Package => caps.package,
            Self::PackageGroup => caps.package_group,
        }
    }
}

/// Seeds an initial partition of `top` by growing clusters along topological
/// order under the `level` cap.
///
/// `layers` is indexed by `top`'s vertices. Vertices are visited in a group-aware
/// topological order (see `topological_order`) and packed greedily into clusters
/// whose summed capacity weight (`top.vertex_weights`; files at the folder level)
/// stays within `caps`'s `level` cap. The resulting quotient is acyclic because
/// each cluster owns a contiguous prefix of that topological order.
///
/// `affinity` is an optional per-vertex grouping key (e.g. a home-directory
/// ordinal): the order keeps a group's vertices contiguous *across* layers — as
/// far as the DAG permits — so same-origin files are not scattered across folders.
/// It only ever biases the choice among vertices already free to emit, so it can
/// never break the topological order. An empty slice restores pure
/// descending-layer, ascending-index order.
///
/// A cap of zero is treated as one so that growth always makes progress; every
/// vertex still lands in exactly one cluster, and a single vertex heavier than
/// the cap (an unsplittable atom) fills a cluster of its own.
#[must_use]
pub fn seed(
    top: &CoarseGraph,
    layers: &[u32],
    caps: &LevelCaps,
    level: SeedLevel,
    affinity: &[u32],
) -> Partition {
    let vertex_count = top.graph.vertex_count();
    let cap = level.cap(caps).max(1);

    let order = topological_order(&top.graph, layers, affinity);

    let mut assignment = vec![ClusterId(0); vertex_count];
    let mut cluster = 0_u32;
    let mut filled = 0_u32;

    // upper levels (domain/package/group) open a fresh cluster at every
    // home-directory boundary, not only when the cap fills — so distinct homes
    // become distinct domains instead of packing to the cap-minimum count. The
    // folder level keeps pure cap packing (its affinity only reorders). The
    // topological order already groups same-home vertices contiguously, so a
    // boundary split yields ~one cluster per home, bounded by cap.
    let split_at_home = level != SeedLevel::Folder && !affinity.is_empty();
    let mut prev_affinity: Option<u32> = None;

    for vertex in order {
        let weight = top
            .vertex_weights
            .get(vertex as usize)
            .copied()
            .unwrap_or(1);
        let vertex_affinity = affinity.get(vertex as usize).copied();
        let home_boundary =
            split_at_home && prev_affinity.is_some() && vertex_affinity != prev_affinity;
        if filled > 0 && (home_boundary || filled.saturating_add(weight) > cap) {
            cluster += 1;
            filled = 0;
        }
        if let Some(slot) = assignment.get_mut(vertex as usize) {
            *slot = ClusterId(cluster);
        }
        filled = filled.saturating_add(weight);
        prev_affinity = vertex_affinity;
    }

    let cluster_count = if vertex_count == 0 {
        0
    } else {
        cluster as usize + 1
    };
    Partition::from_assignment(assignment, cluster_count)
}

/// Returns vertices in a group-aware topological order.
///
/// A Kahn sweep over the dependent → dependency edges emits a vertex only once
/// every vertex that must precede it has been emitted, so the order is always
/// topological (and contiguous clusters over it stay acyclic). Among the ready
/// frontier the choice is: first a vertex whose `affinity` key continues the last
/// emitted group — so a folder's dependency chain stays contiguous *across*
/// layers, not just within one — then descending layer, then ascending affinity,
/// then ascending index. Grouping never overrides the DAG: only ready vertices are
/// ever chosen, so a cross-group edge that forces interleaving is honoured.
///
/// An empty `affinity` slice makes every key equal, so group continuity always
/// holds and the order collapses to descending layer, then ascending index — the
/// prior behaviour for the upper levels that carry no home key.
///
/// The sweep is *cycle-tolerant*: every vertex is always emitted exactly once,
/// even when the input has cycles. On a DAG the ready frontier never empties
/// early and the result is a true topological order. On a cyclic graph — which
/// the upper levels can see, since a quotient over real containers may cycle —
/// the frontier can empty with vertices left over; the sweep then admits the
/// best-ranked remaining vertex, cutting the cycle there. Order within a cycle is
/// necessarily not topological (no order is), so the seed quotient's acyclicity
/// guarantee holds only for acyclic input.
fn topological_order(graph: &Csr, layers: &[u32], affinity: &[u32]) -> Vec<u32> {
    let vertex_count = graph.vertex_count();
    // in-degree over incoming (dependent → this) edges: a vertex is ready once
    // every vertex that must precede it in the order has been emitted.
    let mut indegree = vec![0_u32; vertex_count];
    for source in 0..vertex_count {
        let source32 = u32::try_from(source).unwrap_or(u32::MAX);
        for &target in graph.neighbors(source32) {
            if let Some(slot) = indegree.get_mut(target as usize) {
                *slot = slot.saturating_add(1);
            }
        }
    }

    let mut emitted = vec![false; vertex_count];
    let mut order = Vec::with_capacity(vertex_count);
    let mut last_affinity: Option<u32> = None;

    for _ in 0..vertex_count {
        // pick the best vertex under the group key: continue the last group
        // first, then descending layer, ascending affinity, ascending index.
        // The first pass considers only *ready* (in-degree zero) vertices, which
        // is what keeps the order topological on a DAG. A cycle, however, leaves
        // every one of its members with an unmet in-edge, so the ready frontier
        // can empty while vertices remain. The second pass then admits the best
        // unemitted vertex regardless of in-degree, cutting the cycle at its
        // best-ranked member. Without it the sweep would truncate and strand the
        // cycle (and everything downstream of it) in cluster 0, breaching the cap.
        let mut chosen: Option<u32> = None;
        for ready_only in [true, false] {
            let mut best: Option<(u8, core::cmp::Reverse<u32>, u32, u32)> = None;
            for vertex in 0..vertex_count {
                if emitted.get(vertex).copied().unwrap_or(true) {
                    continue;
                }
                if ready_only && indegree.get(vertex).copied().unwrap_or(0) != 0 {
                    continue;
                }
                let vertex32 = u32::try_from(vertex).unwrap_or(u32::MAX);
                let layer = layers.get(vertex).copied().unwrap_or(0);
                let key_affinity = affinity.get(vertex).copied().unwrap_or(0);
                let rank = u8::from(last_affinity != Some(key_affinity));
                let key = (rank, core::cmp::Reverse(layer), key_affinity, vertex32);
                if best.is_none_or(|current| key < current) {
                    best = Some(key);
                    chosen = Some(vertex32);
                }
            }
            if chosen.is_some() {
                break;
            }
        }
        let Some(vertex) = chosen else {
            break;
        };
        if let Some(slot) = emitted.get_mut(vertex as usize) {
            *slot = true;
        }
        order.push(vertex);
        last_affinity = Some(affinity.get(vertex as usize).copied().unwrap_or(0));
        for &target in graph.neighbors(vertex) {
            if let Some(slot) = indegree.get_mut(target as usize) {
                *slot = slot.saturating_sub(1);
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::csr::Csr;

    /// Wraps an edge list as a single-level [`CoarseGraph`] over `vertex_count`
    /// vertices (identity fine-to-coarse map).
    fn coarse(vertex_count: usize, mut edges: Vec<(u32, u32)>) -> CoarseGraph {
        edges.sort_unstable();
        edges.dedup();
        let graph = Csr::from_sorted_edges(vertex_count, &edges);
        let fine_to_coarse = (0..vertex_count)
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
            .collect();
        let layers = layers_of(&graph);
        let vertex_weights = vec![1; vertex_count];
        CoarseGraph {
            graph,
            fine_to_coarse,
            layers,
            vertex_weights,
        }
    }

    /// Computes longest-path layers over dependent → dependency edges.
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

    /// Asserts the partition's quotient is acyclic: no cross-cluster edge points
    /// from a higher cluster id to a lower one.
    fn assert_quotient_acyclic(graph: &Csr, parts: &Partition) {
        for v in 0..graph.vertex_count() {
            let v32 = u32::try_from(v).unwrap_or(u32::MAX);
            let Some(cv) = parts.cluster_of(v32) else {
                continue;
            };
            for &dep in graph.neighbors(v32) {
                let Some(cd) = parts.cluster_of(dep) else {
                    continue;
                };
                // Source depends on target; with contiguous topological clusters
                // the source's cluster id must be <= the target's.
                assert!(cv.0 <= cd.0, "quotient edge {} -> {} ascends", cv.0, cd.0);
            }
        }
    }

    #[test]
    fn should_place_every_vertex_in_a_cluster() {
        let graph = coarse(4, vec![(3, 2), (2, 1), (1, 0)]);
        let layers = layers_of(&graph.graph);

        let parts = seed(
            &graph,
            &layers,
            &LevelCaps::defaults(),
            SeedLevel::Folder,
            &[],
        );

        assert_eq!(parts.node_count(), 4);
        for v in 0..4 {
            assert!(parts.cluster_of(v).is_some());
        }
    }

    #[test]
    fn should_open_a_new_cluster_once_the_cap_is_hit() {
        // Five vertices in a chain, cap of 2 → clusters of sizes 2, 2, 1.
        let graph = coarse(5, vec![(4, 3), (3, 2), (2, 1), (1, 0)]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 2,
            ..LevelCaps::defaults()
        };

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &[]);

        assert_eq!(parts.cluster_count(), 3);
    }

    #[test]
    fn should_respect_the_cap_on_every_cluster() {
        let graph = coarse(7, vec![(6, 5), (5, 4), (4, 3), (3, 2), (2, 1), (1, 0)]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            domain: 3,
            ..LevelCaps::defaults()
        };

        let parts = seed(&graph, &layers, &caps, SeedLevel::Domain, &[]);

        for c in 0..parts.cluster_count() {
            let id = ClusterId(u32::try_from(c).unwrap_or(u32::MAX));
            assert!(parts.size_of(id) <= 3);
        }
    }

    #[test]
    fn should_seed_an_acyclic_quotient() {
        // A branching DAG: 4 -> {2,3}, 2 -> 1, 3 -> 1, 1 -> 0.
        let graph = coarse(5, vec![(4, 2), (4, 3), (2, 1), (3, 1), (1, 0)]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 2,
            ..LevelCaps::defaults()
        };

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &[]);

        assert_quotient_acyclic(&graph.graph, &parts);
    }

    #[test]
    fn should_pack_by_capacity_weight_not_vertex_count() {
        // three vertices weighing 10, 10, and 3 under a cap of 15: the second
        // vertex cannot join the first (20 > 15), but the third fits beside it.
        let mut graph = coarse(3, vec![]);
        graph.vertex_weights = vec![10, 10, 3];
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 15,
            ..LevelCaps::defaults()
        };

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &[]);

        assert_eq!(parts.cluster_count(), 2);
        assert_eq!(parts.size_of(ClusterId(0)), 1);
        assert_eq!(parts.size_of(ClusterId(1)), 2);
    }

    #[test]
    fn should_grant_an_over_cap_atom_its_own_cluster() {
        // a single vertex heavier than the cap still lands in exactly one
        // cluster, alone, and its neighbours open a fresh cluster after it.
        let mut graph = coarse(2, vec![]);
        graph.vertex_weights = vec![40, 2];
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 15,
            ..LevelCaps::defaults()
        };

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &[]);

        assert_eq!(parts.cluster_count(), 2);
        assert_eq!(parts.size_of(ClusterId(0)), 1);
    }

    #[test]
    fn should_return_an_empty_partition_for_no_vertices() {
        let graph = coarse(0, vec![]);
        let layers: Vec<u32> = Vec::new();

        let parts = seed(
            &graph,
            &layers,
            &LevelCaps::defaults(),
            SeedLevel::Folder,
            &[],
        );

        assert_eq!(parts.node_count(), 0);
        assert_eq!(parts.cluster_count(), 0);
    }

    #[test]
    fn should_pack_same_affinity_vertices_contiguously() {
        // Four edgeless vertices, all on layer 1, interleaved by home directory:
        // vertices 0 and 2 share key 0, vertices 1 and 3 share key 1. Under a
        // cap of 2, raw index order would split each pair across clusters; the
        // affinity key regroups them so each cluster holds one home directory.
        let graph = coarse(4, vec![]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 2,
            ..LevelCaps::defaults()
        };
        let affinity = [0, 1, 0, 1];

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &affinity);

        assert_eq!(parts.cluster_count(), 2);
        assert_eq!(parts.cluster_of(0), parts.cluster_of(2));
        assert_eq!(parts.cluster_of(1), parts.cluster_of(3));
        assert_ne!(parts.cluster_of(0), parts.cluster_of(1));
    }

    #[test]
    fn should_group_cross_layer_folders_into_contiguous_clusters() {
        // The planted-partition check: three "folders" (A={0,1,2}, B={3,4,5},
        // C={6,7,8}), each an internal dependency chain spanning three layers
        // (2->1->0, 5->4->3, 8->7->6). Every folder therefore has one vertex on
        // each of layers 3, 2, 1, so a layer-major seed packs one file from every
        // folder into each cap-3 cluster — maximally torn. A group-aware order
        // must instead keep each folder's cross-layer chain contiguous, recovering
        // the planted partition: one cluster per folder.
        let graph = coarse(9, vec![(2, 1), (1, 0), (5, 4), (4, 3), (8, 7), (7, 6)]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 3,
            ..LevelCaps::defaults()
        };
        let affinity = [0, 0, 0, 1, 1, 1, 2, 2, 2];

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &affinity);

        assert_eq!(parts.cluster_count(), 3);
        for folder in [[0, 1, 2], [3, 4, 5], [6, 7, 8]] {
            let first = parts.cluster_of(folder[0]);
            assert!(first.is_some());
            assert_eq!(parts.cluster_of(folder[1]), first);
            assert_eq!(parts.cluster_of(folder[2]), first);
        }
        assert_ne!(parts.cluster_of(0), parts.cluster_of(3));
        assert_ne!(parts.cluster_of(3), parts.cluster_of(6));
        assert_quotient_acyclic(&graph.graph, &parts);
    }

    #[test]
    fn should_keep_grouping_topological_under_a_cross_folder_edge() {
        // Grouping must never override the DAG. Two folders A={0,1}, B={2,3} with
        // internal edges 1->0 and 3->2, plus a cross edge 0->3 (A's sink depends
        // on B's source). Affinity asks to group A together, but 0 depends on 3,
        // so 3 must precede 0 in any valid order. The seed keeps the order
        // topological (acyclic quotient) even though it cannot fully honour the
        // grouping.
        let graph = coarse(4, vec![(1, 0), (3, 2), (0, 3)]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 2,
            ..LevelCaps::defaults()
        };
        let affinity = [0, 0, 1, 1];

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &affinity);

        assert_quotient_acyclic(&graph.graph, &parts);
    }

    #[test]
    fn should_keep_affinity_grouping_within_topological_layers() {
        // A two-layer DAG: sources 2,3 (layer 2) each depend on sinks 0,1
        // (layer 1). Affinity pairs 2 with 0 and 3 with 1, but layer dominates,
        // so the seed still emits every source before any sink and the quotient
        // stays acyclic — affinity only reorders within a layer.
        let graph = coarse(4, vec![(2, 0), (2, 1), (3, 0), (3, 1)]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 1,
            ..LevelCaps::defaults()
        };
        let affinity = [0, 1, 0, 1];

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &affinity);

        assert_quotient_acyclic(&graph.graph, &parts);
    }

    #[test]
    fn should_pack_a_pure_cycle_within_the_cap() {
        // A 4-cycle has no ready vertex at all: a truncating Kahn sweep would
        // emit nothing and leave every vertex in cluster 0, breaching the cap.
        // The cycle-tolerant sweep must still distribute all four vertices into
        // cap-respecting clusters.
        let graph = coarse(4, vec![(0, 1), (1, 2), (2, 3), (3, 0)]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 2,
            ..LevelCaps::defaults()
        };

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &[]);

        assert_eq!(parts.cluster_count(), 2);
        for c in 0..parts.cluster_count() {
            let id = ClusterId(u32::try_from(c).unwrap_or(u32::MAX));
            assert!(parts.size_of(id) <= 2, "cluster {c} breaches the cap");
        }
    }

    #[test]
    fn should_emit_the_vertices_downstream_of_a_cycle() {
        // The 0 <-> 1 cycle blocks the whole graph: 2 and 3 sit downstream of
        // it, so a truncating sweep never reaches them either. Cycle tolerance
        // must unlock the tail and pack all four vertices under the cap.
        let graph = coarse(4, vec![(0, 1), (1, 0), (1, 2), (2, 3)]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 2,
            ..LevelCaps::defaults()
        };

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &[]);

        assert_eq!(parts.cluster_count(), 2);
        for c in 0..parts.cluster_count() {
            let id = ClusterId(u32::try_from(c).unwrap_or(u32::MAX));
            assert!(parts.size_of(id) <= 2, "cluster {c} breaches the cap");
        }
    }

    #[test]
    fn should_open_an_upper_level_cluster_at_each_home_boundary() {
        // Six edgeless vertices from three homes (keys 0,1,2, two each). The
        // domain cap of 12 has ample headroom to pack all six into one cluster,
        // which is exactly the cap-minimum grab-bag we want to avoid at the upper
        // levels. Because affinity is non-empty and the level is not Folder, the
        // seed opens a fresh cluster at every home-key change — one domain per
        // home — instead of packing to the cap.
        let graph = coarse(6, vec![]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            domain: 12,
            ..LevelCaps::defaults()
        };
        let affinity = [0, 0, 1, 1, 2, 2];

        let parts = seed(&graph, &layers, &caps, SeedLevel::Domain, &affinity);

        assert_eq!(parts.cluster_count(), 3);
        assert_eq!(parts.cluster_of(0), parts.cluster_of(1));
        assert_eq!(parts.cluster_of(2), parts.cluster_of(3));
        assert_eq!(parts.cluster_of(4), parts.cluster_of(5));
        assert_ne!(parts.cluster_of(0), parts.cluster_of(2));
        assert_ne!(parts.cluster_of(2), parts.cluster_of(4));
    }

    #[test]
    fn should_not_split_the_folder_level_at_home_boundaries() {
        // The same three-home layout, but at the Folder level the home-boundary
        // split is disabled: affinity only reorders visitation, so all six
        // vertices pack into a single cap-12 folder. This guards the gate that
        // keeps folders packing purely by capacity.
        let graph = coarse(6, vec![]);
        let layers = layers_of(&graph.graph);
        let caps = LevelCaps {
            folder: 12,
            ..LevelCaps::defaults()
        };
        let affinity = [0, 0, 1, 1, 2, 2];

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder, &affinity);

        assert_eq!(parts.cluster_count(), 1);
    }
}
