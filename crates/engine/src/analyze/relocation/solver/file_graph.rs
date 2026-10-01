//! File-grain inputs to the solver: test-zone marks and the priced file graph.

use std::collections::BTreeMap;

use strata_core::graph::csr::Csr;
use strata_core::score::KindWeights;
use strata_ir::{Edge, Node, Polarity};

use crate::analyze::relocation::{FileInfo, TestPolicy};

/// One symbol relocation the FIX08 symbol polish accepted, in candidate-tree
/// file ids. `delta` is the strict J improvement it earned at acceptance time.
pub(in crate::analyze) fn test_zone_marks(
    tests: &TestPolicy,
    files: &[FileInfo],
    nodes: &[Node],
) -> Vec<bool> {
    let mut marks: Vec<bool> = files
        .iter()
        .map(|file| {
            tests.matches(&file.name)
                || (tests.builtins && TestPolicy::matches_builtin_path(&file.name))
        })
        .collect();
    if tests.builtins {
        let mut case_only: BTreeMap<u32, bool> = BTreeMap::new();
        for node in nodes {
            let entry = case_only.entry(node.container.0).or_insert(true);
            *entry &= node.polarity != Polarity::Production;
        }
        for (index, file) in files.iter().enumerate() {
            if !case_only.get(&file.container).copied().unwrap_or(false) {
                continue;
            }
            if let Some(mark) = marks.get_mut(index) {
                *mark = true;
            }
        }
    }
    marks
}

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
