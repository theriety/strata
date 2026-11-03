//! File-grain inputs to the solver: the priced file graph.

use std::collections::BTreeMap;

use unlace_core::graph::csr::Csr;
use unlace_core::score::KindWeights;
use unlace_ir::{Edge, Node};

/// Builds the weighted file-dependency graph: every symbol edge — at its
/// configured price, not the hard edges alone — is mapped onto its endpoints'
/// owning files, intra-file edges vanish (layout cannot cut them), parallel
/// crossings are summed, and each crossing is priced by the config's kind-weight
/// table — so heavy-edge matching and FM gains see the same prices the objective
/// charges.
///
/// lean: admitting every edge is kept (not gated back to `Hardness::Hard`)
/// because it aligns the search's cut with the cut the score reports (AD-6) and
/// gives soft-only files a non-empty move-set. It does densify the folder
/// quotient, which under the upper levels' current `cut_only` gain nudges toward
/// one low-cut grab-bag domain; the fix is the directory-cohesion term Stage 2
/// adds to those levels (which counterbalances the extra crossings), not a
/// narrower graph here — re-gating would misalign search from score and hide the
/// collapse rather than resolve it.
pub(in crate::analyze) fn build_file_graph(
    edges: &[Edge],
    nodes: &[Node],
    index_of: &BTreeMap<u32, u32>,
    file_count: usize,
    weights: &KindWeights,
    test_zone: &[bool],
) -> Csr {
    let container_of: BTreeMap<u32, u32> = nodes
        .iter()
        .map(|node| (node.id.0, node.container.0))
        .collect();
    let mut crossings: Vec<(u32, u32, f32)> = Vec::new();
    for edge in edges {
        // admit every edge at its configured price — not Hard edges alone — so
        // the search optimizes the same cut the score reports and soft-only
        // files (e.g. type-reference-only TS) get a non-empty move-set. A zero-
        // priced edge stays in the graph but binds nothing: polish never
        // nominates it as a move target and matching never contracts across it
        // (the FIX04 doctrine).
        let (Some(source), Some(target)) = (
            container_of.get(&edge.source.0),
            container_of.get(&edge.target.0),
        ) else {
            continue;
        };
        let (Some(&from), Some(&to)) = (index_of.get(source), index_of.get(target)) else {
            continue;
        };
        if from == to {
            continue;
        }
        // reason: csr weights are f32 by contract (ad-6); narrowing the f64 price is the one lossy step
        #[allow(clippy::cast_possible_truncation)]
        let weight = weights.edge_weight(edge.kind, edge.confidence) as f32;
        // The test tie-cut prices an edge touching a test-zone file at zero —
        // both directions, test↔test included — so test coupling can neither
        // weld a spec to its subject nor bond test files into a place of their
        // own. This is the single pricing choke point every downstream stage
        // (relief piles, polish moves, heavy-edge matching) reads.
        let weight = if test_zone.get(from as usize).copied().unwrap_or(false)
            || test_zone.get(to as usize).copied().unwrap_or(false)
        {
            0.0
        } else {
            weight
        };
        crossings.push((from, to, weight));
    }
    Csr::from_weighted_edges(file_count, &crossings)
}
