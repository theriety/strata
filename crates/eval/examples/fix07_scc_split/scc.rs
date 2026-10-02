//! The replica's SCC layer: condensation, dominant homes, and seed-stage tear
//! accounting.

use std::collections::BTreeMap;

use strata_engine::config::AnalyzeConfig;
use strata_engine::core::condense::condense;
use strata_engine::core::graph::csr::Csr;
use strata_engine::ir::Snapshot;

use super::rows::FileRow;

/// Seed-stage structural accounting for one fixture.
pub(super) struct SeedAccounting {
    /// SCCs holding more than one file (the only atoms seed can fold).
    pub(super) multi_sccs: usize,
    /// Multi-file SCCs whose members span more than one real directory.
    pub(super) mixed_sccs: usize,
    /// Files living in a mixed-home SCC.
    pub(super) files_in_mixed: usize,
    /// Files outside their SCC's dominant directory — displacement the seed
    /// creates the instant it folds, before J scores anything.
    pub(super) seed_torn_struct: usize,
    /// Whether each SCC is mixed-home, keyed by SCC index.
    pub(super) mixed_of_scc: Vec<bool>,
}

/// Classifies every multi-file SCC of the replica condensation and tallies the
/// structural tear the dominant-home fold produces at seed time.
pub(super) fn seed_stage_accounting(
    rows: &[FileRow],
    scc_members: &[Vec<usize>],
) -> SeedAccounting {
    let mut accounting = SeedAccounting {
        multi_sccs: 0,
        mixed_sccs: 0,
        files_in_mixed: 0,
        seed_torn_struct: 0,
        mixed_of_scc: vec![false; scc_members.len()],
    };
    for (scc_index, members) in scc_members.iter().enumerate() {
        if members.len() < 2 {
            continue;
        }
        accounting.multi_sccs += 1;
        let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
        for &vertex in members {
            if let Some(row) = rows.get(vertex) {
                *tally.entry(row.dir.as_str()).or_insert(0) += 1;
            }
        }
        if tally.len() < 2 {
            continue;
        }
        accounting.mixed_sccs += 1;
        accounting.files_in_mixed += members.len();
        if let Some(flag) = accounting.mixed_of_scc.get_mut(scc_index) {
            *flag = true;
        }
        let dominant_dir = dominant_member(rows, members)
            .and_then(|vertex| rows.get(vertex))
            .map_or_else(String::new, |row| row.dir.clone());
        for (dir, count) in &tally {
            if *dir != dominant_dir.as_str() {
                accounting.seed_torn_struct += count;
            }
        }
        print_mixed_scc(rows, members, &dominant_dir, &tally);
    }
    accounting
}

/// Prints one mixed-home SCC: its directory split and the seed's fold target.
fn print_mixed_scc(
    rows: &[FileRow],
    members: &[usize],
    dominant_dir: &str,
    tally: &BTreeMap<&str, usize>,
) {
    let split = tally
        .iter()
        .map(|(dir, count)| format!("{dir}:{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("  mixed-scc dirs[{split}] -> seed folds onto '{dominant_dir}'");
    for &vertex in members {
        if let Some(row) = rows.get(vertex) {
            let marker = if row.dir == dominant_dir { " " } else { ">" };
            println!("    {marker} {}", row.path);
        }
    }
}

/// Rebuilds the engine's priced file graph and condenses it into SCC member
/// lists over the dense file indices, mirroring `build_file_graph` (every edge
/// admitted at configured price) inside `PipelineSolver::new`.
pub(super) fn condensation_members(
    snapshot: &Snapshot,
    rows: &[FileRow],
    config: &AnalyzeConfig,
) -> Vec<Vec<usize>> {
    let ir = snapshot.ir();
    let index_of_container: BTreeMap<u32, usize> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.container, index))
        .collect();
    let container_of_node: BTreeMap<u32, u32> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.container.0))
        .collect();
    let weights = config.profiles.anchored.weights.kind_weights();
    let mut crossings: Vec<(u32, u32, f32)> = Vec::new();
    for edge in &ir.edges {
        let resolved = (
            container_of_node
                .get(&edge.source.0)
                .and_then(|container| index_of_container.get(container)),
            container_of_node
                .get(&edge.target.0)
                .and_then(|container| index_of_container.get(container)),
        );
        let (Some(from), Some(to)) = resolved else {
            continue;
        };
        if from == to {
            continue;
        }
        #[allow(clippy::cast_possible_truncation)] // csr weights are f32 by contract (ad-6)
        let weight = weights.edge_weight(edge.kind, edge.confidence) as f32;
        crossings.push((
            u32::try_from(*from).unwrap_or(u32::MAX),
            u32::try_from(*to).unwrap_or(u32::MAX),
            weight,
        ));
    }
    let graph = Csr::from_weighted_edges(rows.len(), &crossings);
    let condensation = condense(&graph);
    condensation
        .members
        .iter()
        .map(|members| {
            members
                .iter()
                .map(|vertex| usize::try_from(vertex.0).unwrap_or(usize::MAX))
                .collect()
        })
        .collect()
}

/// Picks the seed-dominant member of an SCC: highest production SLOC, then
/// lexicographically smallest file name — the rule `real_dir_partition`
/// applies before folding the SCC into one home directory.
fn dominant_member(rows: &[FileRow], members: &[usize]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for &member in members {
        let Some(row) = rows.get(member) else {
            continue;
        };
        let better = best.is_none_or(|current| match rows.get(current) {
            Some(top) => {
                row.production_sloc > top.production_sloc
                    || (row.production_sloc == top.production_sloc && row.name < top.name)
            }
            None => true,
        });
        if better {
            best = Some(member);
        }
    }
    best
}
