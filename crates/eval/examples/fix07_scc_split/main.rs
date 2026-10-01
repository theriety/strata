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

use std::collections::BTreeMap;

use strata_engine::config::{AnalyzeConfig, ProfileName};
use strata_engine::result::ViolationKind;
use strata_engine::{analyze, snapshot_from_root};
use strata_eval::target::{ConfigSource, RunMode, TargetSpec};
use strata_eval::{eval_fixture_root, load_target};

mod displacement;
mod rows;
mod scc;

use displacement::{Origin, check_scc_atomicity, collect_displacements, print_displacements};
use rows::{collect_files, validate_paths};
use scc::{condensation_members, seed_stage_accounting};

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

fn config_for(spec: &TargetSpec) -> AnalyzeConfig {
    let mut config = AnalyzeConfig::default();
    config.analysis.profiles = match spec.run.mode {
        RunMode::Both => vec![ProfileName::Anchored, ProfileName::Greenfield],
        RunMode::Anchored => vec![ProfileName::Anchored],
        RunMode::Greenfield => vec![ProfileName::Greenfield],
    };
    for profile in [
        &mut config.profiles.anchored,
        &mut config.profiles.greenfield,
    ] {
        profile.candidates = spec.run.candidates;
        profile.seed = spec.run.seed;
    }
    config
}

/// Mirrors the harness invocation (`both` modes, k candidates, the target seed,
/// built-in defaults) and prints the fixture's attribution row plus details.
fn probe_fixture(name: &str) -> Result<(), String> {
    let spec = load_target(name).map_err(|error| format!("{name}: {error}"))?;
    if !matches!(spec.run.config, ConfigSource::Defaults) {
        return Err(format!("{name}: fixture-local configs are not probed"));
    }
    let config = config_for(&spec);

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
        .shared_findings
        .iter()
        .chain(
            result
                .profiles
                .anchored
                .iter()
                .flat_map(|profile| &profile.current.unique_findings),
        )
        .filter(|violation| matches!(violation.kind, ViolationKind::Cycle))
        .count();

    let Some(anchored) = result.profiles.anchored.as_ref() else {
        return Err(format!("{name}: anchored mode absent"));
    };
    let Some(best) = anchored.candidates.first() else {
        return Err(format!("{name}: no anchored candidates"));
    };

    println!(
        "  crosscheck: engine_cycle_findings={cycles} best_conditional_splits={} replica_multi_file_sccs={} standing={:?} pool_converged={}",
        best.conditional_splits.len(),
        accounting.multi_sccs,
        anchored.current.standing,
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
