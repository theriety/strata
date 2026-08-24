//! FIX07 diagnostic probe: attributes the anchored best candidate's folder
//! displacement to seed granularity versus polish, per committed eval fixture.
//!
//! The probe reads only public APIs (`snapshot_from_root`, `analyze`,
//! `condense`, the result DTO) and ships as an example, so production behavior
//! is byte-identical whether or not this binary ever runs: no engine or core
//! code path changes. The hypothesis under test and the metric definitions
//! live in `docs/architecture/notes/fix07-scc-seed-granularity.md`.
//!
//! Run: `cargo run -p strata-eval --example fix07_scc_split`

#![allow(clippy::print_stdout)] // diagnostic binary; the workspace deny targets library code

use std::collections::{BTreeMap, BTreeSet};

use strata_engine::config::{AnalyzeConfig, Mode};
use strata_engine::core::condense::condense;
use strata_engine::core::graph::csr::Csr;
use strata_engine::ir::Snapshot;
use strata_engine::ir::{Polarity, ScopeLevel};
use strata_engine::result::{ContainerNode, Level, MoveReason, ViolationKind};
use strata_engine::{analyze, snapshot_from_root};
use strata_eval::target::{ConfigSource, RunMode};
use strata_eval::{eval_fixture_root, load_target};

/// Every committed fixture, in corpus order.
const FIXTURES: [&str; 8] = [
    "collapse",
    "large-app",
    "welding",
    "tearing",
    "relief",
    "inversion",
    "naming-drift",
    "cycle-span",
];

/// One file of the analyzed snapshot, indexed in the engine's dense file order.
struct FileRow {
    /// Owning file container id (the engine sorts files by this).
    container: u32,
    /// Full rendered path (non-synthetic ancestor names plus the file name).
    path: String,
    /// Real directory key: the non-synthetic `Folder`-level ancestors, `/`-joined.
    dir: String,
    /// Source-declared file name; the dominant-file tie-break key.
    name: String,
    /// Summed production SLOC of the production symbols the file owns.
    production_sloc: u64,
}

/// One displaced file of the anchored best candidate, with its attribution.
struct Displacement {
    /// Full path of the moved file.
    path: String,
    /// The folded source folder the move narrates.
    from: String,
    /// Which pipeline stage sealed the file's placement.
    origin: Origin,
}

/// Attribution class of a displaced file, in evaluation precedence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// The move narrates `RelievesOverCap`: a FIX03 capacity half split.
    Relief,
    /// The file belongs to a mixed-home SCC: its individual placement was
    /// sealed at seed time; polish translates the SCC wholesale but can never
    /// place the file back home on its own.
    SeedGranularity,
    /// The file's SCC is single-home: seed placed it correctly; polish chose
    /// to move it.
    Polish,
}

fn main() {
    if let Err(error) = run() {
        println!("probe failed: {error}");
    }
}

fn run() -> Result<(), String> {
    println!(
        "fixture\tfiles\tmulti_sccs\tmixed_sccs\tfiles_in_mixed\tseed_torn_struct\tdisplaced\tdisp_relief\tdisp_mixed\tdisp_polish\tmoves_total"
    );
    for name in FIXTURES {
        probe_fixture(name)?;
    }
    Ok(())
}

/// Mirrors the harness invocation (`both` modes, k candidates, the target seed,
/// built-in defaults) and prints the fixture's attribution row plus details.
fn probe_fixture(name: &str) -> Result<(), String> {
    let spec = load_target(name).map_err(|error| format!("{name}: {error}"))?;
    if !matches!(spec.run.config, ConfigSource::Defaults) {
        return Err(format!("{name}: fixture-local configs are not probed"));
    }
    let mut config = AnalyzeConfig::default();
    config.analysis.mode = match spec.run.mode {
        RunMode::Both => Mode::Both,
        RunMode::Anchored => Mode::Anchored,
        RunMode::Greenfield => Mode::Greenfield,
    };
    config.analysis.candidates = spec.run.candidates;
    config.analysis.seed = spec.run.seed;

    let root = eval_fixture_root(name);
    let snapshot =
        snapshot_from_root(&root, &config).map_err(|error| format!("{name}: {error}"))?;
    let result = analyze(&snapshot, &config).map_err(|error| format!("{name}: {error}"))?;

    let rows = collect_files(&snapshot);
    validate_paths(name, &rows, &result.current.tree);

    // Replica of the engine's file graph (every edge at configured price) and
    // its SCC condensation — the search grain every stage moves in.
    let index_of_path: BTreeMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.path.clone(), index))
        .collect();
    let scc_members = condensation_members(&snapshot, &rows, &config);
    let mut scc_of_vertex: Vec<Option<usize>> = vec![None; rows.len()];
    for (scc_index, members) in scc_members.iter().enumerate() {
        for &vertex in members {
            if let Some(slot) = scc_of_vertex.get_mut(vertex) {
                *slot = Some(scc_index);
            }
        }
    }

    // Seed-stage structural accounting: a multi-file SCC whose members span
    // several real directories is torn the moment the seed folds it onto its
    // dominant member's home directory — before J scores anything.
    let accounting = seed_stage_accounting(&rows, &scc_members);

    // Engine-side cross-check of the replica: the engine's own cycle findings
    // and split preconditions are the observable trace of its SCC layer.
    let cycles = result
        .current
        .violations
        .iter()
        .filter(|violation| matches!(violation.kind, ViolationKind::Cycle))
        .count();

    let Some(anchored) = result.modes.anchored.as_ref() else {
        return Err(format!("{name}: anchored mode absent"));
    };
    let Some(best) = anchored.candidates.first() else {
        return Err(format!("{name}: no anchored candidates"));
    };

    println!(
        "  crosscheck: engine_cycle_findings={cycles} best_conditional_splits={} replica_multi_file_sccs={} standing={:?} pool_converged={}",
        best.conditional_splits.len(),
        accounting.multi_sccs,
        anchored.current_standing,
        anchored.solution_space_converged,
    );
    for candidate in &anchored.candidates {
        print_candidate(candidate);
    }

    let (displacements, unmapped) = collect_displacements(
        &best.delta_narration,
        &index_of_path,
        &scc_of_vertex,
        &accounting.mixed_of_scc,
    );

    let disp_relief = displacements
        .iter()
        .filter(|displacement| displacement.origin == Origin::Relief)
        .count();
    let disp_mixed = displacements
        .iter()
        .filter(|displacement| displacement.origin == Origin::SeedGranularity)
        .count();
    let disp_polish = displacements
        .iter()
        .filter(|displacement| displacement.origin == Origin::Polish)
        .count();

    println!(
        "{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{disp_relief}\t{disp_mixed}\t{disp_polish}\t{}",
        rows.len(),
        accounting.multi_sccs,
        accounting.mixed_sccs,
        accounting.files_in_mixed,
        accounting.seed_torn_struct,
        displacements.len(),
        best.delta_narration.len(),
    );
    print_displacements(&displacements);
    if unmapped > 0 {
        println!("  UNMAPPED paths (identity mismatch): {unmapped}");
    }
    check_scc_atomicity(
        &rows,
        &index_of_path,
        &scc_members,
        &scc_of_vertex,
        &displacements,
    );
    Ok(())
}

/// Seed-stage structural accounting for one fixture.
struct SeedAccounting {
    /// SCCs holding more than one file (the only atoms seed can fold).
    multi_sccs: usize,
    /// Multi-file SCCs whose members span more than one real directory.
    mixed_sccs: usize,
    /// Files living in a mixed-home SCC.
    files_in_mixed: usize,
    /// Files outside their SCC's dominant directory — displacement the seed
    /// creates the instant it folds, before J scores anything.
    seed_torn_struct: usize,
    /// Whether each SCC is mixed-home, keyed by SCC index.
    mixed_of_scc: Vec<bool>,
}

/// Classifies every multi-file SCC of the replica condensation and tallies the
/// structural tear the dominant-home fold produces at seed time.
fn seed_stage_accounting(rows: &[FileRow], scc_members: &[Vec<usize>]) -> SeedAccounting {
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

/// Prints one candidate's rank, score, and move volume.
fn print_candidate(candidate: &strata_engine::result::Candidate) {
    let moved: usize = candidate
        .delta_narration
        .iter()
        .map(|mv| mv.files.len())
        .sum();
    println!(
        "  candidate {} score={:.3} improvement={:.3} moves={} files_moved={}",
        candidate.index,
        candidate.score,
        candidate.improvement,
        candidate.delta_narration.len(),
        moved,
    );
}

/// Flattens the best candidate's narration into displaced files and applies the
/// attribution precedence: relief by reason tag, then mixed-home SCC, then
/// polish. Returns the displacements plus how many paths failed to map back to
/// the replica's file identity.
fn collect_displacements(
    narration: &[strata_engine::result::Move],
    index_of_path: &BTreeMap<String, usize>,
    scc_of_vertex: &[Option<usize>],
    mixed_of_scc: &[bool],
) -> (Vec<Displacement>, usize) {
    let mut displacements: Vec<Displacement> = Vec::new();
    for mv in narration {
        let origin = if matches!(mv.reason, MoveReason::RelievesOverCap { .. }) {
            Origin::Relief
        } else {
            Origin::Polish
        };
        for file_move in &mv.files {
            displacements.push(Displacement {
                path: file_move.path.clone(),
                from: file_move.from.clone(),
                origin,
            });
        }
    }
    // Re-classify: any non-relief displacement inside a mixed-home SCC is a
    // seed-granularity tear, whatever pull motivated the whole-SCC translation.
    let mut unmapped = 0_usize;
    for displacement in &mut displacements {
        if displacement.origin == Origin::Relief {
            continue;
        }
        let owner = index_of_path
            .get(displacement.path.as_str())
            .copied()
            .map(|index| (index, scc_of_vertex.get(index).copied().flatten()));
        match owner {
            Some((_, Some(scc))) => {
                if mixed_of_scc.get(scc).copied().unwrap_or(false) {
                    displacement.origin = Origin::SeedGranularity;
                }
            }
            _ => unmapped += 1,
        }
    }
    (displacements, unmapped)
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

/// Prints one line per displaced file with its attribution class.
fn print_displacements(displacements: &[Displacement]) {
    for displacement in displacements {
        let tag = match displacement.origin {
            Origin::Relief => "relief",
            Origin::SeedGranularity => "seed-granularity",
            Origin::Polish => "polish",
        };
        println!(
            "  moved {} [{} from '{}']",
            displacement.path, tag, displacement.from
        );
    }
}

/// Reports any displaced file whose SCC mates did not move with it: polish
/// moves whole SCCs, so a partial group means the replica's grain diverged
/// from the engine's.
fn check_scc_atomicity(
    rows: &[FileRow],
    index_of_path: &BTreeMap<String, usize>,
    scc_members: &[Vec<usize>],
    scc_of_vertex: &[Option<usize>],
    displacements: &[Displacement],
) {
    let moved: BTreeSet<&str> = displacements
        .iter()
        .map(|displacement| displacement.path.as_str())
        .collect();
    let mut partial = 0_usize;
    let mut shown = 0_usize;
    for displacement in displacements {
        let owner = index_of_path
            .get(displacement.path.as_str())
            .and_then(|index| scc_of_vertex.get(*index).copied().flatten());
        let Some(scc) = owner else {
            continue;
        };
        for &vertex in scc_members.get(scc).into_iter().flatten() {
            let Some(row) = rows.get(vertex) else {
                continue;
            };
            if !moved.contains(row.path.as_str()) {
                partial += 1;
                if shown < 3 {
                    println!(
                        "  atomicity: {} moved without SCC mate {}",
                        displacement.path, row.path
                    );
                    shown += 1;
                }
            }
        }
    }
    if partial > shown {
        println!("  atomicity: {partial} unreported mates in total");
    }
}

/// Collects the snapshot's files in engine order (ascending container id).
///
/// The snapshot names every file container by its repo-relative path, which is
/// also the identity narration and both rendered trees use; the real directory
/// key is that path minus its last segment.
fn collect_files(snapshot: &Snapshot) -> Vec<FileRow> {
    let ir = snapshot.ir();
    let mut sloc_of: BTreeMap<u32, u64> = BTreeMap::new();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        *sloc_of.entry(node.container.0).or_insert(0) += u64::from(node.effective_size);
    }
    let mut rows: Vec<FileRow> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|file| {
            let path = file.name.to_string();
            let dir = match path.rsplit_once('/') {
                Some((prefix, _)) => prefix.to_owned(),
                None => String::from("."),
            };
            FileRow {
                container: file.id.0,
                dir,
                name: file.name.to_string(),
                production_sloc: sloc_of.get(&file.id.0).copied().unwrap_or(0),
                path,
            }
        })
        .collect();
    rows.sort_by_key(|row| row.container);
    rows
}

/// Rebuilds the engine's priced file graph and condenses it into SCC member
/// lists over the dense file indices, mirroring `build_file_graph` (every edge
/// admitted at configured price) inside `PipelineSolver::new`.
fn condensation_members(
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
    let weights = config.weights.kind_weights();
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

/// Walks the rendered current tree and warns when the IR-derived file paths do
/// not reproduce the DTO's file identity exactly.
fn validate_paths(name: &str, rows: &[FileRow], tree: &ContainerNode) {
    let mut rendered: BTreeSet<String> = BTreeSet::new();
    gather_files(tree, &mut rendered);
    let derived: BTreeSet<&str> = rows.iter().map(|row| row.path.as_str()).collect();
    let ir_only: Vec<&str> = derived
        .iter()
        .filter(|path| !rendered.contains(**path))
        .copied()
        .collect();
    let dto_only: Vec<&str> = rendered
        .iter()
        .filter(|path| !derived.contains(path.as_str()))
        .map(String::as_str)
        .collect();
    if ir_only.is_empty() && dto_only.is_empty() {
        println!("== {name}: path identity verified ({} files)", rows.len());
        return;
    }
    println!(
        "== {name}: PATH MISMATCH ir_only={:?} dto_only={:?}",
        ir_only.first(),
        dto_only.first()
    );
}

/// Depth-first collection of every file path in a rendered tree; a file's own
/// container name is its repo-relative path, matching the snapshot's naming.
fn gather_files(node: &ContainerNode, out: &mut BTreeSet<String>) {
    if node.level == Level::File {
        out.insert(node.name.clone());
        return;
    }
    for child in node.children.iter().flatten() {
        gather_files(child, out);
    }
}
