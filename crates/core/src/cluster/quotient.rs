//! Quotient edge multiset over clusters, kept alongside a topological order so
//! refinement's acyclicity veto touches only the cluster pairs a move affects.

use std::collections::HashMap;

use crate::cluster::{ClusterId, Partition};
use crate::graph::csr::Csr;

/// Reverse adjacency over the node graph: for each node, the predecessors `p`
/// with an edge `p -> node` and the weight that edge carries.
///
/// The CSR stores only forward edges, so refinement materialises this view once
/// per level. Both the acyclicity veto (which needs a moved node's incoming
/// cluster edges) and the gain function (which credits the cut-weight reduction
/// on incoming edges, not just outgoing ones) read from it.
pub(super) struct ReverseEdges {
    /// `predecessors[v]` holds every `(p, weight)` with an edge `p -> v`.
    predecessors: Vec<Vec<(u32, f32)>>,
}

impl ReverseEdges {
    /// Builds the reverse adjacency from `graph.reversed()`, dropping self-loops.
    pub(super) fn from_graph(graph: &Csr) -> Self {
        let reversed = graph.reversed();
        let predecessors = (0..reversed.vertex_count())
            .map(|v| {
                let v32 = u32::try_from(v).unwrap_or(u32::MAX);
                reversed
                    .neighbors(v32)
                    .iter()
                    .zip(reversed.weights(v32))
                    .filter(|&(&p, _)| p != v32)
                    .map(|(&p, &w)| (p, w))
                    .collect()
            })
            .collect();
        Self { predecessors }
    }

    /// Returns the `(predecessor, weight)` edges entering `node`.
    pub(super) fn of(&self, node: u32) -> &[(u32, f32)] {
        self.predecessors
            .get(node as usize)
            .map_or(&[][..], Vec::as_slice)
    }

    /// Test-only mutable access to the predecessor lists, for building fixtures.
    #[cfg(test)]
    pub(super) fn predecessors_mut(&mut self) -> &mut Vec<Vec<(u32, f32)>> {
        &mut self.predecessors
    }
}

/// The quotient's directed edge multiset over clusters, maintained incrementally
/// alongside a topological order so a move's acyclicity veto touches only the
/// cluster-pair edges incident to its endpoints — never the full node graph.
pub(super) struct QuotientEdges {
    /// Count of finer edges projecting onto each ordered cluster pair `(a, b)`,
    /// `a != b`.
    pub(super) counts: HashMap<(ClusterId, ClusterId), u32>,
    /// Live forward adjacency over clusters, derived from `counts`: a pair is
    /// present here exactly while its count is positive.
    adjacency: HashMap<ClusterId, Vec<ClusterId>>,
    /// A topological index per cluster: `order[a] < order[b]` for every live
    /// edge `a -> b`. Maintained incrementally across moves.
    pub(super) order: HashMap<ClusterId, u32>,
}

impl QuotientEdges {
    /// Projects every graph edge onto cluster pairs to seed the multiset and its
    /// adjacency, then computes an initial topological order over the clusters.
    pub(super) fn from_partition(graph: &Csr, parts: &Partition) -> Self {
        let mut counts: HashMap<(ClusterId, ClusterId), u32> = HashMap::new();
        for v in 0..graph.vertex_count() {
            let v32 = u32::try_from(v).unwrap_or(u32::MAX);
            for &neighbour in graph.neighbors(v32) {
                let (Some(from), Some(to)) = (parts.cluster_of(v32), parts.cluster_of(neighbour))
                else {
                    continue;
                };
                if from != to {
                    *counts.entry((from, to)).or_insert(0) += 1;
                }
            }
        }
        let adjacency = adjacency_of(counts.keys().copied());
        let order = topological_order(parts.cluster_count(), &adjacency);
        Self {
            counts,
            adjacency,
            order,
        }
    }

    /// Returns the cluster-pair edge deltas a move of `node` from `source` to
    /// `target` induces, computed against the *pre-move* partition `parts`.
    ///
    /// Both directions matter: every outgoing edge `node -> w` shifts its source
    /// endpoint from `source` to `target`, and every incoming edge `p -> node`
    /// shifts its target endpoint likewise. Reflexive edges are internal and
    /// contribute nothing. The result is the minimal signed change to apply to
    /// the live multiset.
    pub(super) fn move_deltas(
        graph: &Csr,
        reverse: &ReverseEdges,
        parts: &Partition,
        node: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> Vec<((ClusterId, ClusterId), i64)> {
        let mut deltas: HashMap<(ClusterId, ClusterId), i64> = HashMap::new();
        let mut bump = |from: ClusterId, to: ClusterId, change: i64| {
            if from != to {
                *deltas.entry((from, to)).or_insert(0) += change;
            }
        };
        // Outgoing edges node -> w: source -> to becomes target -> to.
        for &neighbour in graph.neighbors(node) {
            if neighbour == node {
                continue;
            }
            if let Some(to) = parts.cluster_of(neighbour) {
                bump(source, to, -1);
                bump(target, to, 1);
            }
        }
        // Incoming edges p -> node: from -> source becomes from -> target.
        for &(predecessor, _) in reverse.of(node) {
            if predecessor == node {
                continue;
            }
            if let Some(from) = parts.cluster_of(predecessor) {
                bump(from, source, -1);
                bump(from, target, 1);
            }
        }
        deltas.into_iter().collect()
    }

    /// Returns `true` when applying `deltas` leaves the quotient acyclic.
    ///
    /// Only the cluster pairs whose count rises from zero introduce a new edge,
    /// and only a new edge can close a cycle. For each such edge `a -> b` the
    /// test asks whether `b` already reaches `a` in the live quotient (extended
    /// with the other newly-added edges from this same move). The reachability
    /// search is bounded by the topological order — it explores only clusters
    /// ranked at or before `a` — so the cost scales with the affected region,
    /// not the whole quotient.
    pub(super) fn stays_acyclic_under(&self, deltas: &[((ClusterId, ClusterId), i64)]) -> bool {
        let added: Vec<(ClusterId, ClusterId)> = deltas
            .iter()
            .filter(|&&(pair, change)| change > 0 && self.is_new_edge(pair, change))
            .map(|&(pair, _)| pair)
            .collect();
        if added.is_empty() {
            return true;
        }
        let extra = adjacency_of(added.iter().copied());
        for &(from, to) in &added {
            if self.reaches(to, from, &extra) {
                return false;
            }
        }
        true
    }

    /// Returns `true` when `pair` is absent from the live multiset, so adding
    /// `change` introduces a brand-new quotient edge.
    fn is_new_edge(&self, pair: (ClusterId, ClusterId), change: i64) -> bool {
        let current = self.counts.get(&pair).copied().unwrap_or(0);
        i64::from(current) == 0 && change > 0
    }

    /// Returns `true` when `target` is reachable from `start` over the live
    /// adjacency augmented with `extra`, exploring only clusters whose
    /// topological rank does not exceed `target`'s (a forward edge can never
    /// reach a higher-ranked cluster, so the search stays local).
    fn reaches(
        &self,
        start: ClusterId,
        target: ClusterId,
        extra: &HashMap<ClusterId, Vec<ClusterId>>,
    ) -> bool {
        let bound = self.order.get(&target).copied().unwrap_or(u32::MAX);
        let mut stack = vec![start];
        let mut seen: Vec<ClusterId> = Vec::new();
        while let Some(node) = stack.pop() {
            if node == target {
                return true;
            }
            if self.order.get(&node).copied().unwrap_or(0) > bound {
                continue;
            }
            if seen.contains(&node) {
                continue;
            }
            seen.push(node);
            for next in self
                .adjacency
                .get(&node)
                .into_iter()
                .flatten()
                .chain(extra.get(&node).into_iter().flatten())
            {
                stack.push(*next);
            }
        }
        false
    }

    /// Commits `deltas` into the live multiset and adjacency, dropping any pair
    /// that reaches a non-positive count, then repairs the topological order so
    /// it stays consistent with the new edge set.
    pub(super) fn apply(&mut self, deltas: &[((ClusterId, ClusterId), i64)]) {
        for &(pair, change) in deltas {
            let entry = self.counts.entry(pair).or_insert(0);
            let updated = i64::from(*entry) + change;
            if updated <= 0 {
                self.counts.remove(&pair);
            } else {
                *entry = u32::try_from(updated).unwrap_or(u32::MAX);
            }
        }
        self.adjacency = adjacency_of(self.counts.keys().copied());
        self.order = repair_order(&self.order, &self.adjacency);
    }
}

/// Builds forward adjacency lists from a set of live cluster-pair edges.
fn adjacency_of(
    pairs: impl Iterator<Item = (ClusterId, ClusterId)>,
) -> HashMap<ClusterId, Vec<ClusterId>> {
    let mut adjacency: HashMap<ClusterId, Vec<ClusterId>> = HashMap::new();
    for (from, to) in pairs {
        adjacency.entry(from).or_default().push(to);
    }
    adjacency
}

/// Computes a topological order over `0..cluster_count` clusters via Kahn's
/// algorithm, assigning each a monotonically increasing rank. Clusters in a
/// cyclic component (which the vetoes forbid at steady state) fall back to a
/// rank past the acyclic prefix so the order stays total.
fn topological_order(
    cluster_count: usize,
    adjacency: &HashMap<ClusterId, Vec<ClusterId>>,
) -> HashMap<ClusterId, u32> {
    let nodes: Vec<ClusterId> = (0..cluster_count)
        .map(|c| ClusterId(u32::try_from(c).unwrap_or(u32::MAX)))
        .collect();
    kahn_ranks(&nodes, adjacency)
}

/// Recomputes a topological order over the clusters currently present in `prior`
/// using Kahn's algorithm against the repaired `adjacency`. Reusing the prior
/// node set keeps the order defined over exactly the live clusters.
fn repair_order(
    prior: &HashMap<ClusterId, u32>,
    adjacency: &HashMap<ClusterId, Vec<ClusterId>>,
) -> HashMap<ClusterId, u32> {
    let mut nodes: Vec<ClusterId> = prior.keys().copied().collect();
    nodes.sort_unstable();
    kahn_ranks(&nodes, adjacency)
}

/// Assigns a topological rank to each cluster in `nodes` via Kahn's algorithm
/// over `adjacency`. Lower-numbered clusters drain first on ties, so the order
/// is deterministic; any cluster left after the acyclic prefix (only possible
/// under a cycle, which the vetoes prevent) is appended in id order.
fn kahn_ranks(
    nodes: &[ClusterId],
    adjacency: &HashMap<ClusterId, Vec<ClusterId>>,
) -> HashMap<ClusterId, u32> {
    let present: Vec<ClusterId> = {
        let mut sorted = nodes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        sorted
    };
    let mut indegree: HashMap<ClusterId, u32> = present.iter().map(|&c| (c, 0)).collect();
    for &from in &present {
        for &to in adjacency.get(&from).into_iter().flatten() {
            if let Some(slot) = indegree.get_mut(&to) {
                *slot += 1;
            }
        }
    }

    let mut ready: Vec<ClusterId> = present
        .iter()
        .copied()
        .filter(|c| indegree.get(c).copied().unwrap_or(0) == 0)
        .collect();
    ready.sort_unstable_by(|a, b| b.cmp(a)); // pop() yields the smaller id first.

    let mut order: HashMap<ClusterId, u32> = HashMap::new();
    let mut rank = 0_u32;
    while let Some(node) = ready.pop() {
        order.insert(node, rank);
        rank += 1;
        for &to in adjacency.get(&node).into_iter().flatten() {
            if let Some(slot) = indegree.get_mut(&to) {
                *slot = slot.saturating_sub(1);
                if *slot == 0 {
                    insert_sorted_descending(&mut ready, to);
                }
            }
        }
    }
    // Any cluster not yet ranked sits in a cycle; append in id order so the map
    // stays total. The acyclicity veto keeps the steady state cycle-free.
    let mut leftover: Vec<ClusterId> = present
        .iter()
        .copied()
        .filter(|c| !order.contains_key(c))
        .collect();
    leftover.sort_unstable();
    for cluster in leftover {
        order.insert(cluster, rank);
        rank += 1;
    }
    order
}

/// Inserts `value` into a descending-sorted vector, keeping it sorted so a
/// trailing `pop()` always removes the smallest element.
fn insert_sorted_descending(queue: &mut Vec<ClusterId>, value: ClusterId) {
    let position = queue
        .binary_search_by(|probe| value.cmp(probe))
        .unwrap_or_else(|index| index);
    queue.insert(position, value);
}
