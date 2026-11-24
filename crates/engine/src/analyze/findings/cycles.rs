//! The conditional splits derived from the hard symbol graph's solved
//! strongly connected components. Cycle findings live on the file-import
//! graph ([`super::import_cycles`], ADR-35); these splits stay at symbol
//! grain because splitting an over-cap component into files cuts symbol
//! edges, not imports.

use std::collections::BTreeMap;

use unlace_ir::{Node, Snapshot};

use crate::analyze::search::{SccSolution, node_names};
use crate::result::{ConditionalSplit, EdgeBreak};

/// Maps one SCC's MFAS break set onto named [`EdgeBreak`]s. File-crossing
/// edges come first and same-file edges last, each group keeping the break
/// set's ascending `(source, target)` order.
pub(super) fn edge_breaks(
    solution: &SccSolution,
    names: &BTreeMap<u32, String>,
    nodes: &BTreeMap<u32, &Node>,
) -> Vec<EdgeBreak> {
    let name_of = |local: u32| {
        solution
            .members
            .get(local as usize)
            .and_then(|node| names.get(&node.0))
            .cloned()
            .unwrap_or_default()
    };
    let container_of = |local: u32| {
        solution
            .members
            .get(local as usize)
            .and_then(|node| nodes.get(&node.0))
            .map(|node| node.container)
    };
    let mut breaks: Vec<EdgeBreak> = solution
        .break_set
        .edges
        .iter()
        .map(|edge| EdgeBreak {
            source: name_of(edge.source),
            target: name_of(edge.target),
            weight: solution
                .pair_weights
                .get(&(edge.source, edge.target))
                .copied()
                .unwrap_or(0.0),
            exact: solution.break_set.exact,
            same_file: container_of(edge.source).is_some()
                && container_of(edge.source) == container_of(edge.target),
        })
        .collect();
    breaks.sort_by_key(|edge| edge.same_file);
    breaks
}

/// Derives the shared conditional splits: one per solved SCC whose production
/// SLOC exceeds the file cap, with the MFAS break set as preconditions and a
/// ceil-packed file-count estimate.
pub(in crate::analyze) fn conditional_splits(
    cycles: &[SccSolution],
    snapshot: &Snapshot,
    file_cap: u32,
) -> Vec<ConditionalSplit> {
    let names = node_names(snapshot);
    let nodes: BTreeMap<u32, &Node> = snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id.0, node))
        .collect();
    let cap = u64::from(file_cap.max(1));
    cycles
        .iter()
        .filter(|solution| solution.production_sloc > cap)
        .map(|solution| ConditionalSplit {
            scc: solution
                .members
                .iter()
                .filter_map(|node| names.get(&node.0).cloned())
                .collect(),
            preconditions: edge_breaks(solution, &names, &nodes),
            resulting_files: u32::try_from(solution.production_sloc.div_ceil(cap))
                .unwrap_or(u32::MAX),
        })
        .collect()
}
