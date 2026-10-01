//! Candidate move scoring for refinement.
//!
//! Ranks, for every node, its best target among the clusters its neighbours
//! occupy, with gain spanning cut-weight change in both edge directions plus the
//! cohesion delta.

use crate::cluster::quotient::ReverseEdges;
use crate::cluster::{ClusterId, Partition};
use crate::graph::csr::Csr;

use super::GainFn;

/// A scored candidate move of one node to a neighbouring cluster.
pub(super) struct Candidate {
    /// The node to move.
    pub(super) node: u32,
    /// The cluster to move it into.
    pub(super) target: ClusterId,
    /// The move's gain: cut-weight reduction plus cohesion delta.
    pub(super) gain: f32,
}

/// Scores, for every node, its best target cluster among the clusters its
/// neighbours occupy, returning one candidate per node.
pub(super) fn ranked_moves(
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
pub(super) fn move_gain(
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
