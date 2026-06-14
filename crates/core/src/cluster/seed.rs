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
//! `layer(source) > layer(target)` (see [`crate::layer`]). Ordering vertices by
//! *descending* layer therefore yields a topological order in which every edge
//! points forward (earlier → later), which is the order the greedy growth walks.

use crate::cluster::coarsen::CoarseGraph;
use crate::cluster::{ClusterId, LevelCaps, Partition};

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
/// `layers` is indexed by `top`'s vertices. Vertices are visited in descending
/// layer (ties broken by ascending index) — a topological order for this crate's
/// dependent → dependency edges — and packed greedily into clusters of at most
/// `caps`'s `level` cap members. The resulting quotient is acyclic because each
/// cluster owns a contiguous prefix of that order.
///
/// A cap of zero is treated as one so that growth always makes progress; every
/// vertex still lands in exactly one cluster.
#[must_use]
pub fn seed(top: &CoarseGraph, layers: &[u32], caps: &LevelCaps, level: SeedLevel) -> Partition {
    let vertex_count = top.graph.vertex_count();
    let cap = level.cap(caps).max(1);

    let order = topological_order(vertex_count, layers);

    let mut assignment = vec![ClusterId(0); vertex_count];
    let mut cluster = 0_u32;
    let mut filled = 0_u32;

    for vertex in order {
        if filled == cap {
            cluster += 1;
            filled = 0;
        }
        if let Some(slot) = assignment.get_mut(vertex as usize) {
            *slot = ClusterId(cluster);
        }
        filled += 1;
    }

    let cluster_count = if vertex_count == 0 {
        0
    } else {
        cluster as usize + 1
    };
    Partition::from_assignment(assignment, cluster_count)
}

/// Returns vertices in topological order: descending layer, then ascending
/// index. For dependent → dependency edges this places every source before its
/// targets.
fn topological_order(vertex_count: usize, layers: &[u32]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..vertex_count)
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
        .collect();
    order.sort_by(|&a, &b| {
        let layer_a = layers.get(a as usize).copied().unwrap_or(0);
        let layer_b = layers.get(b as usize).copied().unwrap_or(0);
        layer_b.cmp(&layer_a).then(a.cmp(&b))
    });
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
        CoarseGraph {
            graph,
            fine_to_coarse,
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

        let parts = seed(&graph, &layers, &LevelCaps::defaults(), SeedLevel::Folder);

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

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder);

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

        let parts = seed(&graph, &layers, &caps, SeedLevel::Domain);

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

        let parts = seed(&graph, &layers, &caps, SeedLevel::Folder);

        assert_quotient_acyclic(&graph.graph, &parts);
    }

    #[test]
    fn should_return_an_empty_partition_for_no_vertices() {
        let graph = coarse(0, vec![]);
        let layers: Vec<u32> = Vec::new();

        let parts = seed(&graph, &layers, &LevelCaps::defaults(), SeedLevel::Folder);

        assert_eq!(parts.node_count(), 0);
        assert_eq!(parts.cluster_count(), 0);
    }
}
