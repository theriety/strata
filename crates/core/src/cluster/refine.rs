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

use std::collections::HashMap;

use crate::cluster::coarsen::CoarseGraph;
use crate::cluster::{ClusterId, LevelCaps, Partition};
use crate::graph::csr::Csr;

/// The cohesion-aware gain model: per-node token sets plus the α / β mixing
/// coefficients that select anchored vs greenfield behaviour.
///
/// `naming_tokens[v]` are the case/underscore-split tokens of node `v`'s symbol
/// name; `path_tokens[v]` are the tokens of its current container path. Naming
/// cohesion (weighted by `alpha`) is always on; path cohesion (weighted by
/// `beta`) is greenfield-disabled by passing `beta = 0.0`. Cohesion between a
/// node and a cluster is the mean Jaccard similarity of the node's tokens
/// against the cluster's members' tokens.
#[derive(Debug, Clone)]
pub struct GainFn {
    /// Naming-token sets per node, each sorted and deduplicated.
    naming_tokens: Vec<Vec<u32>>,
    /// Path-token sets per node, each sorted and deduplicated.
    path_tokens: Vec<Vec<u32>>,
    /// Weight on naming-token cohesion (α).
    alpha: f32,
    /// Weight on path cohesion (β); zero in greenfield mode.
    beta: f32,
}

impl GainFn {
    /// Builds a gain model from interned per-node token sets and the mixing
    /// coefficients. Token vectors are sorted and deduplicated defensively so
    /// the Jaccard computation can assume set semantics.
    #[must_use]
    pub fn new(
        naming_tokens: Vec<Vec<u32>>,
        path_tokens: Vec<Vec<u32>>,
        alpha: f32,
        beta: f32,
    ) -> Self {
        let normalise = |mut sets: Vec<Vec<u32>>| {
            for set in &mut sets {
                set.sort_unstable();
                set.dedup();
            }
            sets
        };
        Self {
            naming_tokens: normalise(naming_tokens),
            path_tokens: normalise(path_tokens),
            alpha,
            beta,
        }
    }

    /// A cohesion-free gain model: gain reduces to the cut-weight delta alone.
    /// Useful for tests and for runs where cohesion is disabled.
    #[must_use]
    pub fn cut_only(node_count: usize) -> Self {
        Self {
            naming_tokens: vec![Vec::new(); node_count],
            path_tokens: vec![Vec::new(); node_count],
            alpha: 0.0,
            beta: 0.0,
        }
    }

    /// The cohesion bonus a node gains by being a member of `cluster`:
    /// `α · naming + β · path`.
    fn bonus(&self, node: u32, cluster: ClusterId, parts: &Partition) -> f32 {
        let naming = cohesion(&self.naming_tokens, node, cluster, parts);
        let path = cohesion(&self.path_tokens, node, cluster, parts);
        self.alpha * naming + self.beta * path
    }
}

/// Mean Jaccard cohesion of `node`'s `tokens` against the members of `cluster`
/// (excluding `node` itself). Returns `0.0` for an empty cluster.
fn cohesion(tokens: &[Vec<u32>], node: u32, cluster: ClusterId, parts: &Partition) -> f32 {
    let Some(node_set) = tokens.get(node as usize) else {
        return 0.0;
    };
    let mut total = 0.0_f32;
    let mut peers = 0_u32;
    for (other, owner) in parts.assignment().iter().enumerate() {
        if *owner != cluster {
            continue;
        }
        let other_u32 = u32::try_from(other).unwrap_or(u32::MAX);
        if other_u32 == node {
            continue;
        }
        if let Some(other_set) = tokens.get(other) {
            total += jaccard(node_set, other_set);
            peers += 1;
        }
    }
    if peers == 0 {
        return 0.0;
    }
    // `peers` counts cluster members (bounded by the level cap, ≤ 15), well
    // within f32's exact-integer range; the precision lint is allowed for this
    // single averaging conversion.
    #[allow(clippy::cast_precision_loss)]
    let mean = total / peers as f32;
    mean
}

/// Jaccard similarity of two sorted, deduplicated token sets: `|A ∩ B| / |A ∪ B|`.
fn jaccard(a: &[u32], b: &[u32]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let mut left = a.iter().copied().peekable();
    let mut right = b.iter().copied().peekable();
    let mut intersection = 0_usize;
    while let (Some(&x), Some(&y)) = (left.peek(), right.peek()) {
        match x.cmp(&y) {
            std::cmp::Ordering::Less => {
                left.next();
            }
            std::cmp::Ordering::Greater => {
                right.next();
            }
            std::cmp::Ordering::Equal => {
                intersection += 1;
                left.next();
                right.next();
            }
        }
    }
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    // Token-set cardinalities are tiny (per-symbol name fragments), so the
    // ratio of two small counts is exactly representable; the precision lint is
    // allowed here for the single unavoidable count-to-float conversion.
    #[allow(clippy::cast_precision_loss)]
    let ratio = intersection as f32 / union as f32;
    ratio
}

/// Reverse adjacency over the node graph: for each node, the predecessors `p`
/// with an edge `p -> node` and the weight that edge carries.
///
/// The CSR stores only forward edges, so refinement materialises this view once
/// per level. Both the acyclicity veto (which needs a moved node's incoming
/// cluster edges) and the gain function (which credits the cut-weight reduction
/// on incoming edges, not just outgoing ones) read from it.
struct ReverseEdges {
    /// `predecessors[v]` holds every `(p, weight)` with an edge `p -> v`.
    predecessors: Vec<Vec<(u32, f32)>>,
}

impl ReverseEdges {
    /// Builds the reverse adjacency by scanning every forward edge once.
    fn from_graph(graph: &Csr) -> Self {
        let mut predecessors: Vec<Vec<(u32, f32)>> = vec![Vec::new(); graph.vertex_count()];
        for v in 0..graph.vertex_count() {
            let v32 = u32::try_from(v).unwrap_or(u32::MAX);
            let neighbours = graph.neighbors(v32);
            let weights = graph.weights(v32);
            for (slot, &neighbour) in neighbours.iter().enumerate() {
                if neighbour == v32 {
                    continue;
                }
                let weight = weights.get(slot).copied().unwrap_or(0.0);
                if let Some(list) = predecessors.get_mut(neighbour as usize) {
                    list.push((v32, weight));
                }
            }
        }
        Self { predecessors }
    }

    /// Returns the `(predecessor, weight)` edges entering `node`.
    fn of(&self, node: u32) -> &[(u32, f32)] {
        self.predecessors
            .get(node as usize)
            .map_or(&[][..], Vec::as_slice)
    }
}

/// Refines `parts` in place with FM single-node moves over `g`, under the
/// `level` cap and the cohesion-aware `gain`.
///
/// Each pass scores every node's best legal move (one that keeps the quotient
/// acyclic and the target within cap), then applies the strictly-positive-gain
/// moves in descending gain order, ties broken by node index. Passes repeat
/// until one applies no move. The partition is never left cyclic or over-cap.
pub fn refine(
    g: &CoarseGraph,
    parts: &mut Partition,
    gain: &GainFn,
    caps: &LevelCaps,
    level: crate::cluster::seed::SeedLevel,
) {
    let cap = level_cap(caps, level);
    let graph = &g.graph;
    let reverse = ReverseEdges::from_graph(graph);
    let mut quotient = QuotientEdges::from_partition(graph, parts);

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
            if parts.size_of(candidate.target) >= cap {
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
            moved = true;
        }

        if !moved {
            break;
        }
    }
}

/// Returns the member cap for `level`.
fn level_cap(caps: &LevelCaps, level: crate::cluster::seed::SeedLevel) -> u32 {
    use crate::cluster::seed::SeedLevel;
    match level {
        SeedLevel::Folder => caps.folder,
        SeedLevel::Domain => caps.domain,
        SeedLevel::Package => caps.package,
        SeedLevel::PackageGroup => caps.package_group,
    }
    .max(1)
}

/// A scored candidate move of one node to a neighbouring cluster.
struct Candidate {
    /// The node to move.
    node: u32,
    /// The cluster to move it into.
    target: ClusterId,
    /// The move's gain: cut-weight reduction plus cohesion delta.
    gain: f32,
}

/// Scores, for every node, its best target cluster among the clusters its
/// neighbours occupy, returning one candidate per node.
fn ranked_moves(
    graph: &Csr,
    reverse: &ReverseEdges,
    parts: &Partition,
    gain: &GainFn,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for v in 0..graph.vertex_count() {
        let v32 = u32::try_from(v).unwrap_or(u32::MAX);
        let Some(source) = parts.cluster_of(v32) else {
            continue;
        };
        if let Some(candidate) = best_move(graph, reverse, parts, gain, v32, source) {
            out.push(candidate);
        }
    }
    out
}

/// Finds `node`'s highest-gain move to a distinct cluster occupied by one of its
/// graph neighbours (in either direction). Ties break toward the smaller cluster
/// id. Returns `None` when no neighbouring cluster differs from `source`.
fn best_move(
    graph: &Csr,
    reverse: &ReverseEdges,
    parts: &Partition,
    gain: &GainFn,
    node: u32,
    source: ClusterId,
) -> Option<Candidate> {
    let mut targets: Vec<ClusterId> = neighbouring_clusters(graph, reverse, parts, node)
        .into_iter()
        .filter(|&c| c != source)
        .collect();
    targets.sort_unstable();
    targets.dedup();

    let mut best: Option<Candidate> = None;
    for target in targets {
        let g = move_gain(graph, reverse, parts, gain, node, source, target);
        let take = match &best {
            None => true,
            Some(current) => g > current.gain,
        };
        if take {
            best = Some(Candidate {
                node,
                target,
                gain: g,
            });
        }
    }
    best
}

/// The clusters occupied by `node`'s neighbours in *both* directions: the
/// clusters of its forward dependencies and of its reverse dependents. Pulling a
/// node toward either side can reduce the cut, so both are candidate targets.
fn neighbouring_clusters(
    graph: &Csr,
    reverse: &ReverseEdges,
    parts: &Partition,
    node: u32,
) -> Vec<ClusterId> {
    let mut clusters = Vec::new();
    for &neighbour in graph.neighbors(node) {
        if let Some(cluster) = parts.cluster_of(neighbour) {
            clusters.push(cluster);
        }
    }
    for &(predecessor, _) in reverse.of(node) {
        if let Some(cluster) = parts.cluster_of(predecessor) {
            clusters.push(cluster);
        }
    }
    clusters
}

/// The gain of moving `node` from `source` to `target`: the reduction in cut
/// weight plus the change in cohesion bonus.
///
/// The cut-weight delta spans the node's incident edges in *both* directions.
/// An edge to `source` (a forward dependency or reverse dependent currently in
/// `source`) becomes a cut edge after the move (a loss); an edge to `target`
/// stops being cut (a gain). Folding the incoming edges in is required: omitting
/// them would halve the cut term and score a move as if it severed no in-edges.
fn move_gain(
    graph: &Csr,
    reverse: &ReverseEdges,
    parts: &Partition,
    gain: &GainFn,
    node: u32,
    source: ClusterId,
    target: ClusterId,
) -> f32 {
    let mut internal_to_source = 0.0_f32;
    let mut internal_to_target = 0.0_f32;
    let mut tally = |neighbour: u32, weight: f32| match parts.cluster_of(neighbour) {
        Some(c) if c == source => internal_to_source += weight,
        Some(c) if c == target => internal_to_target += weight,
        _ => {}
    };

    // Outgoing edges node -> w.
    let neighbours = graph.neighbors(node);
    let weights = graph.weights(node);
    for (slot, &neighbour) in neighbours.iter().enumerate() {
        if neighbour == node {
            continue;
        }
        let weight = weights.get(slot).copied().unwrap_or(0.0);
        tally(neighbour, weight);
    }
    // Incoming edges p -> node.
    for &(predecessor, weight) in reverse.of(node) {
        if predecessor == node {
            continue;
        }
        tally(predecessor, weight);
    }

    // Cut delta: edges that were internal to source become cut (a loss), edges
    // to target stop being cut (a gain). Positive favours the move.
    let cut_delta = internal_to_target - internal_to_source;

    let bonus_delta = gain.bonus(node, target, parts) - gain.bonus(node, source, parts);
    cut_delta + bonus_delta
}

/// The quotient's directed edge multiset over clusters, maintained incrementally
/// alongside a topological order so a move's acyclicity veto touches only the
/// cluster-pair edges incident to its endpoints — never the full node graph.
struct QuotientEdges {
    /// Count of finer edges projecting onto each ordered cluster pair `(a, b)`,
    /// `a != b`.
    counts: HashMap<(ClusterId, ClusterId), u32>,
    /// Live forward adjacency over clusters, derived from `counts`: a pair is
    /// present here exactly while its count is positive.
    adjacency: HashMap<ClusterId, Vec<ClusterId>>,
    /// A topological index per cluster: `order[a] < order[b]` for every live
    /// edge `a -> b`. Maintained incrementally across moves.
    order: HashMap<ClusterId, u32>,
}

impl QuotientEdges {
    /// Projects every graph edge onto cluster pairs to seed the multiset and its
    /// adjacency, then computes an initial topological order over the clusters.
    fn from_partition(graph: &Csr, parts: &Partition) -> Self {
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
    fn move_deltas(
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
    fn stays_acyclic_under(&self, deltas: &[((ClusterId, ClusterId), i64)]) -> bool {
        let added: Vec<(ClusterId, ClusterId)> = deltas
            .iter()
            .filter(|&&(pair, change)| change > 0 && self.is_new_edge(pair, change))
            .map(|&(pair, _)| pair)
            .collect();
        if added.is_empty() {
            return true;
        }
        let extra = extra_adjacency(&added);
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
    fn apply(&mut self, deltas: &[((ClusterId, ClusterId), i64)]) {
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

/// Forward adjacency lists over a slice of freshly-added cluster-pair edges,
/// used to extend the live quotient during a tentative acyclicity check.
fn extra_adjacency(added: &[(ClusterId, ClusterId)]) -> HashMap<ClusterId, Vec<ClusterId>> {
    let mut extra: HashMap<ClusterId, Vec<ClusterId>> = HashMap::new();
    for &(from, to) in added {
        extra.entry(from).or_default().push(to);
    }
    extra
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::seed::SeedLevel;

    /// Builds a CSR over `vertex_count` vertices from `(source, target)` edges,
    /// sorted and deduplicated.
    fn csr(vertex_count: usize, mut edges: Vec<(u32, u32)>) -> Csr {
        edges.sort_unstable();
        edges.dedup();
        Csr::from_sorted_edges(vertex_count, &edges)
    }

    /// Wraps a graph as an identity-mapped [`CoarseGraph`].
    fn coarse(graph: Csr) -> CoarseGraph {
        let fine_to_coarse = (0..graph.vertex_count())
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
            .collect();
        CoarseGraph {
            graph,
            fine_to_coarse,
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
        if let Some(list) = reverse.predecessors.get_mut(1) {
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
        if let Some(list) = reverse.predecessors.get_mut(2) {
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
