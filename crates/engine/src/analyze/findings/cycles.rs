//! Cycle findings and the conditional splits derived from solved strongly connected components.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use strata_ir::{Node, ScopeLevel, Snapshot};

use crate::analyze::search::{SccSolution, node_names};
use crate::result::{ConditionalSplit, EdgeBreak, Severity, Violation, ViolationKind};

/// Reports each multi-node strongly connected component of the hard-edge graph
/// as a cycle violation unless every member resolves to the same file container.
/// Missing or non-file members remain reportable because only proven intra-file
/// recursion is suppressed. Reported cycles carry the MFAS break set as
/// suggestions.
pub(super) fn cycle_violations(snapshot: &Snapshot, cycles: &[SccSolution]) -> Vec<Violation> {
    let names = node_names(snapshot);
    let nodes: BTreeMap<u32, &Node> = snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id.0, node))
        .collect();
    let files: BTreeMap<u32, String> = snapshot
        .ir()
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container.name.to_string()))
        .collect();

    cycles
        .iter()
        .filter(|solution| !is_same_file_cycle(solution, &nodes, &files))
        .map(|solution| {
            let location: Vec<String> = solution
                .members
                .iter()
                .filter_map(|node| nodes.get(&node.0))
                .filter_map(|node| files.get(&node.container.0).cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let breaks = edge_breaks(solution, &names);
            Violation {
                kind: ViolationKind::Cycle,
                severity: Severity::Violation,
                detail: cycle_detail(solution.members.len(), &breaks),
                location,
                break_suggestions: Some(breaks),
                capacity: None,
            }
        })
        .collect()
}

fn is_same_file_cycle(
    solution: &SccSolution,
    nodes: &BTreeMap<u32, &Node>,
    files: &BTreeMap<u32, String>,
) -> bool {
    let mut member_files = solution.members.iter().map(|member| {
        nodes.get(&member.0).and_then(|node| {
            files
                .contains_key(&node.container.0)
                .then_some(node.container)
        })
    });
    let Some(Some(first_file)) = member_files.next() else {
        return false;
    };
    member_files.all(|file| file == Some(first_file))
}

/// Maps one SCC's MFAS break set onto named [`EdgeBreak`]s, in the break set's
/// ascending `(source, target)` order.
fn edge_breaks(solution: &SccSolution, names: &BTreeMap<u32, String>) -> Vec<EdgeBreak> {
    let name_of = |local: u32| {
        solution
            .members
            .get(local as usize)
            .and_then(|node| names.get(&node.0))
            .cloned()
            .unwrap_or_default()
    };
    solution
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
        })
        .collect()
}

/// Renders a cycle violation's detail line, leading with the cheapest break.
fn cycle_detail(size: usize, breaks: &[EdgeBreak]) -> String {
    const CONSEQUENCE: &str = "symbols form one placement unit and must remain in one file unless a suggested dependency edge is broken";
    let Some(first) = breaks.first() else {
        return format!("{size}-symbol cycle; {CONSEQUENCE}");
    };
    let method = if first.exact { "exact" } else { "heuristic" };
    let mut detail = format!(
        "{size}-symbol cycle; {CONSEQUENCE}; break {} -> {} (w={:.1}, {method})",
        first.source, first.target, first.weight
    );
    if breaks.len() > 1 {
        let _ = write!(detail, ", +{} more", breaks.len() - 1);
    }
    detail
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
            preconditions: edge_breaks(solution, &names),
            resulting_files: u32::try_from(solution.production_sloc.div_ceil(cap))
                .unwrap_or(u32::MAX),
        })
        .collect()
}
