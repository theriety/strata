//! The pure analysis pass: snapshot plus config in, [`AnalyzeResult`] out.
//!
//! [`analyze`] performs no I/O and holds no global state, so an identical
//! snapshot, config, and seed always produce an identical result (AD-5). It runs
//! the full restructuring pipeline — SCC condensation, longest-path layering,
//! multilevel acyclic clustering, scoring, and multi-start diversification — to
//! return up to `k` genuinely different candidate layouts per requested mode, each
//! narrated against the current tree. Alongside the candidates it derives the
//! current tree's structural violations (cycles, polarity breaches, over-exports)
//! and scores the current layout.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use smol_str::SmolStr;
use strata_core::cluster::coarsen::{CoarseGraph, coarsen_chain, tight_layers};
use strata_core::cluster::refine::{GainFn, refine};
use strata_core::cluster::seed::{SeedLevel, seed};
use strata_core::cluster::{ClusterId, LevelCaps, Partition};
use strata_core::condense::{Condensation, condense};
use strata_core::diversify::{ModeConfig, ModeResult as CoreModeResult, SolvedCandidate, Solver};
use strata_core::diversify::{diversify, vi_distance};
use strata_core::graph::csr::{Csr, HardnessFilter, build_csr};
use strata_core::layer::layer;
use strata_core::score::{
    Candidate as ScoreCandidate, Coefficients, CohesionGroup, ContainerSizes, KindWeights,
    ScoreBreakdown as CoreBreakdown, ScoredEdge, score,
};
use strata_core::shatter::{BreakSet, EdgeRef, EdgeWeights, SccView, shatter};
use strata_core::visibility::derive_visibility;
use strata_ir::{
    Container, ContainerId, ContainerTree, Edge, Hardness, Node, NodeId, Polarity, ScopeLevel,
    Snapshot,
};

use crate::config::AnalyzeConfig;
use crate::error::StrataError;
use crate::narrate::{FileFacts, narrate, tokenize};
use crate::result::{
    AnalyzeResult, Candidate, CapacityRemainder, ConditionalSplit, ContainerNode, CurrentStanding,
    CurrentTree, EdgeBreak, Level, ModeResult, Modes, RESULT_SCHEMA_VERSION, ScoreBreakdown,
    Severity, Summary, SymbolPlacement, Violation, ViolationKind,
};
use crate::snapshot::Language;

/// The capacity borderline band: a finding within ±10% of a cap is borderline
/// and never gates CI (reference `BORDERLINE_CAPACITY_MARGIN`).
pub const BORDERLINE_CAPACITY_MARGIN: f64 = 0.1;

/// Analyzes `snapshot` under `config`, returning the owned [`AnalyzeResult`].
///
/// The pass is pure: it reads only the snapshot and config and returns owned
/// data. It derives the current tree's violations (cycles, polarity breaches,
/// over-exports), scores the current layout, then runs the restructuring pipeline
/// to produce one [`ModeResult`] of up to `k` diverse candidates per requested
/// mode.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if the snapshot's container tree
/// cannot be rendered (it is otherwise pre-validated at assembly).
pub fn analyze(snapshot: &Snapshot, config: &AnalyzeConfig) -> Result<AnalyzeResult, StrataError> {
    // `[analysis].jobs` caps the search's parallelism through a scoped pool so
    // the library behaves exactly like the CLI (AD-5); results are
    // thread-count invariant (NFR-1), so `0` (use the global pool) and any
    // positive count all produce byte-identical output.
    let jobs = usize::try_from(config.analysis.jobs).unwrap_or(usize::MAX);
    if jobs == 0 {
        return analyze_inner(snapshot, config);
    }
    match rayon::ThreadPoolBuilder::new().num_threads(jobs).build() {
        Ok(pool) => pool.install(|| analyze_inner(snapshot, config)),
        // pool creation fails only on resource exhaustion; the global pool
        // yields the same bytes, so degrading is safe.
        Err(_) => analyze_inner(snapshot, config),
    }
}

/// The body of [`analyze`], run inside whatever rayon pool the caller scoped.
fn analyze_inner(
    snapshot: &Snapshot,
    config: &AnalyzeConfig,
) -> Result<AnalyzeResult, StrataError> {
    let ir = snapshot.ir();
    let tree = &ir.containers;

    let current_node = render_tree(tree, &ir.nodes, &|node| Some(node.container))?;
    let weights = config.weights.kind_weights();
    let cycles = solve_cycles(snapshot, config, &weights);
    let violations = collect_violations(snapshot, config, &current_node, &cycles);
    let current_breakdown = score_current(snapshot, &config.objective.anchored(), &weights);

    // identity seeding is anchored-only (AD-2) and requires a cap-clean current
    // tree: a layout that already breaches a capacity cap is not a legal
    // candidate, so it may only serve as the delta baseline.
    let capacity_clean = !violations.iter().any(|violation| {
        violation.kind == ViolationKind::Capacity && violation.severity == Severity::Violation
    });

    // conditional splits are layout-invariant (an SCC co-clusters everywhere),
    // so they are computed once and shared verbatim by every candidate.
    let splits = conditional_splits(&cycles, snapshot, config.capacity.file);

    let mode = config.analysis.mode;
    let anchored = mode
        .includes_anchored()
        .then(|| {
            build_mode_result(
                snapshot,
                config,
                &config.objective.anchored(),
                capacity_clean,
                capacity_clean,
                &splits,
            )
        })
        .transpose()?;
    let greenfield = mode
        .includes_greenfield()
        .then(|| {
            build_mode_result(
                snapshot,
                config,
                &config.objective.greenfield(),
                false,
                capacity_clean,
                &splits,
            )
        })
        .transpose()?;

    Ok(AnalyzeResult {
        schema_version: RESULT_SCHEMA_VERSION,
        snapshot_hash: snapshot.hash().to_hex().to_string(),
        summary: summarize(snapshot),
        current: CurrentTree {
            tree: current_node,
            score: current_breakdown.total,
            score_breakdown: current_breakdown.into(),
            violations,
        },
        modes: Modes {
            anchored,
            greenfield,
        },
    })
}

/// Builds the coarse census of the snapshot.
///
/// Files are classified by the same extension rule the snapshotter routes them
/// with, so `files_by_language` mirrors the adapter dispatch; a file no adapter
/// claims (possible only in a hand-assembled snapshot) counts toward `files` but
/// no language.
fn summarize(snapshot: &Snapshot) -> Summary {
    let ir = snapshot.ir();
    let mut files = 0u32;
    let mut files_by_language: BTreeMap<String, u32> = BTreeMap::new();
    for container in ir.containers.containers() {
        if container.level != ScopeLevel::File {
            continue;
        }
        files = files.saturating_add(1);
        if let Some(language) = Language::ALL
            .iter()
            .find(|language| language.matches_extension(&container.name))
        {
            *files_by_language
                .entry(language.name().to_owned())
                .or_default() += 1;
        }
    }

    Summary {
        symbols: u32::try_from(ir.nodes.len()).unwrap_or(u32::MAX),
        edges: u32::try_from(ir.edges.len()).unwrap_or(u32::MAX),
        files,
        files_by_language,
    }
}

/// Collects the violations of the current tree: dependency cycles, polarity
/// breaches, visibility over-exports, and capacity findings against the
/// configured caps.
fn collect_violations(
    snapshot: &Snapshot,
    config: &AnalyzeConfig,
    tree: &ContainerNode,
    cycles: &[SccSolution],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    violations.extend(cycle_violations(snapshot, cycles));
    violations.extend(polarity_violations(snapshot));
    violations.extend(visibility_violations(snapshot));
    violations.extend(capacity_violations(tree, config));
    sort_violations(&mut violations);
    violations
}

/// Sorts violations into the engine-defined total order — hard violations
/// before borderline, then kind (cycle, polarity, capacity, visibility), then
/// location, then detail — so every face, including JSON, shares one order.
fn sort_violations(violations: &mut [Violation]) {
    violations.sort_by(|left, right| {
        severity_rank(left.severity)
            .cmp(&severity_rank(right.severity))
            .then_with(|| kind_rank(left.kind).cmp(&kind_rank(right.kind)))
            .then_with(|| left.location.cmp(&right.location))
            .then_with(|| left.detail.cmp(&right.detail))
    });
}

/// Ranks a severity for the violation ordering: hard violations first.
fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Violation => 0,
        Severity::Borderline => 1,
    }
}

/// Ranks a kind for the violation ordering, mirroring the emission order.
fn kind_rank(kind: ViolationKind) -> u8 {
    match kind {
        ViolationKind::Cycle => 0,
        ViolationKind::Polarity => 1,
        ViolationKind::Capacity => 2,
        ViolationKind::Visibility => 3,
    }
}

/// Reports every multi-node strongly connected component of the hard-edge graph
/// as a cycle violation, carrying the MFAS break set as suggestions.
fn cycle_violations(snapshot: &Snapshot, cycles: &[SccSolution]) -> Vec<Violation> {
    let names = node_names(snapshot);

    cycles
        .iter()
        .map(|solution| {
            let location = solution
                .members
                .iter()
                .filter_map(|node| names.get(&node.0).cloned())
                .collect::<Vec<_>>();
            let breaks = edge_breaks(solution, &names);
            Violation {
                kind: ViolationKind::Cycle,
                severity: Severity::Violation,
                detail: cycle_detail(solution.members.len(), &breaks),
                location,
                break_suggestions: Some(breaks),
            }
        })
        .collect()
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
    let Some(first) = breaks.first() else {
        return format!("{size}-symbol cycle");
    };
    let method = if first.exact { "exact" } else { "heuristic" };
    let mut detail = format!(
        "{size}-symbol cycle; break {} -> {} (w={:.1}, {method})",
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
fn conditional_splits(
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

/// Reports polarity-matrix breaches: production code depending on test code,
/// and test support depending on a test case.
fn polarity_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let polarity_by_id: BTreeMap<u32, Polarity> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.polarity))
        .collect();
    let names = node_names(snapshot);

    ir.edges
        .iter()
        .filter_map(|edge| {
            let source_polarity = polarity_by_id.get(&edge.source.0)?;
            let target_polarity = polarity_by_id.get(&edge.target.0)?;
            let (source_word, target_word) = match (source_polarity, target_polarity) {
                (Polarity::Production, Polarity::TestCase | Polarity::TestSupport) => {
                    ("production symbol", "test code")
                }
                (Polarity::TestSupport, Polarity::TestCase) => ("test support", "test case"),
                _ => return None,
            };
            let source = names.get(&edge.source.0).cloned().unwrap_or_default();
            let target = names.get(&edge.target.0).cloned().unwrap_or_default();
            Some(Violation {
                kind: ViolationKind::Polarity,
                severity: Severity::Violation,
                detail: format!("{source_word} `{source}` depends on {target_word} `{target}`"),
                location: vec![source, target],
                break_suggestions: None,
            })
        })
        .collect()
}

/// Reports symbols whose declared visibility is wider than their derived scope.
fn visibility_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let result = derive_visibility(&ir.containers, &ir.nodes, &ir.edges);
    let names = node_names(snapshot);

    result
        .findings
        .iter()
        .map(|finding| {
            let name = names.get(&finding.node.0).cloned().unwrap_or_default();
            Violation {
                kind: ViolationKind::Visibility,
                severity: Severity::Violation,
                detail: format!(
                    "`{name}` is exported at {:?} but needed only at {:?}",
                    finding.declared, finding.derived
                ),
                location: vec![name],
                break_suggestions: None,
            }
        })
        .collect()
}

/// Derives capacity violations from the current tree against the configured caps.
///
/// A file over its production-SLOC cap and an interior container over its
/// member-count cap each yield a finding; a finding within ±10% of its cap is
/// `borderline` and never gates. Each container is checked against the cap of its
/// own level.
fn capacity_violations(tree: &ContainerNode, config: &AnalyzeConfig) -> Vec<Violation> {
    walk_all_capacity(tree, config)
        .into_iter()
        .map(|(_, violation)| violation)
        .collect()
}

/// Walks the DTO tree, returning each capacity finding with the level it hit.
fn walk_all_capacity(tree: &ContainerNode, config: &AnalyzeConfig) -> Vec<(Level, Violation)> {
    let mut findings = Vec::new();
    let mut path = Vec::new();
    append_display_segments(&mut path, tree);
    walk_capacity(tree, &path, config, &mut findings);
    findings
}

/// Recursively checks `node` and its descendants against their level caps.
fn walk_capacity(
    node: &ContainerNode,
    path: &[String],
    config: &AnalyzeConfig,
    findings: &mut Vec<(Level, Violation)>,
) {
    let (measure, cap) = match node.level {
        Level::File => (node.production_sloc.unwrap_or(0), config.capacity.file),
        Level::Folder => (child_count(node), config.capacity.folder),
        Level::Domain => (child_count(node), config.capacity.domain),
        Level::Package => (child_count(node), config.capacity.package),
        Level::PackageGroup => (child_count(node), config.capacity.package_group),
    };

    if let Some(finding) = capacity_finding(node, path, measure, cap) {
        findings.push((node.level, finding));
    }

    if let Some(children) = &node.children {
        for child in children {
            let mut child_path = path.to_vec();
            append_display_segments(&mut child_path, child);
            walk_capacity(child, &child_path, config, findings);
        }
    }
}

/// Appends `node`'s display segments to `path`.
///
/// Interior DTO names are already incremental, so they split directly into
/// segments; a file contributes only its basename because the finding's
/// `detail` line carries the full path. Adjacent levels sharing one name (the
/// synthetic `workspace` chain over a root-level file) contribute it once.
fn append_display_segments(path: &mut Vec<String>, node: &ContainerNode) {
    let name = if node.level == Level::File {
        node.name.rsplit('/').next().unwrap_or(node.name.as_str())
    } else {
        node.name.as_str()
    };
    for segment in name.split('/').filter(|segment| !segment.is_empty()) {
        if path.last().map(String::as_str) != Some(segment) {
            path.push(segment.to_owned());
        }
    }
}

/// Returns the child count of an interior container.
fn child_count(node: &ContainerNode) -> u32 {
    node.children.as_ref().map_or(0, |children| {
        u32::try_from(children.len()).unwrap_or(u32::MAX)
    })
}

/// Builds a capacity finding if `measure` is at or over the borderline band of
/// `cap`, classifying borderline (within ±10%) versus a hard breach.
fn capacity_finding(
    node: &ContainerNode,
    path: &[String],
    measure: u32,
    cap: u32,
) -> Option<Violation> {
    let cap_f = f64::from(cap);
    let measure_f = f64::from(measure);
    let lower = cap_f * (1.0 - BORDERLINE_CAPACITY_MARGIN);

    // below the borderline band entirely: not a finding.
    if measure_f < lower {
        return None;
    }
    // a hard breach is strictly over the cap; the band around the cap is borderline.
    let upper = cap_f * (1.0 + BORDERLINE_CAPACITY_MARGIN);
    let severity = if measure_f > upper {
        Severity::Violation
    } else {
        Severity::Borderline
    };

    Some(Violation {
        kind: ViolationKind::Capacity,
        severity,
        location: path.to_vec(),
        detail: format!(
            "{} `{}` holds {measure} against a cap of {cap}",
            level_word(node.level),
            node.name
        ),
        break_suggestions: None,
    })
}

/// Returns the noun for a container level used in a capacity message.
fn level_word(level: Level) -> &'static str {
    match level {
        Level::File => "file",
        Level::Folder => "folder",
        Level::Domain => "domain",
        Level::Package => "package",
        Level::PackageGroup => "package group",
    }
}

/// Scores the snapshot's current layout under `coefficients`.
///
/// The current candidate carries the snapshot's own edges (each crossing the LCA
/// level of its endpoints in the current tree), the file-level container sizes,
/// and a zero move distance, so its objective is the genuine `J(T0)` baseline the
/// candidates are measured against.
fn score_current(
    snapshot: &Snapshot,
    coefficients: &Coefficients,
    weights: &KindWeights,
) -> CoreBreakdown {
    let ir = snapshot.ir();
    let container_of: BTreeMap<u32, ContainerId> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.container))
        .collect();
    let candidate = score_candidate(
        snapshot,
        &|id| container_of.get(&id).copied(),
        &ir.containers,
        0.0,
    );
    score(&candidate, coefficients, weights)
}

/// Builds one mode's result by running the diversifying restructuring search.
///
/// The mode's [`Coefficients`] select anchored vs greenfield behaviour. The
/// search runs the full multilevel scheme per seed (multi-start), scores each
/// assembled five-level layout under the mode's objective, and diversifies to up
/// to `k` genuinely different candidates by max-min variation of information.
/// When `seed_identity` is set (anchored mode on a cap-clean tree) the pool also
/// carries the identity layout — "change nothing", scored at the true current
/// tree — so a suggested restructuring can never silently lose to the current
/// layout. Each surviving partition is reconstructed into a candidate
/// [`ContainerNode`] tree, priced against the current layout (`improvement`),
/// and narrated against it. The mode's `current_standing` reports where today's
/// tree stands: `infeasible` when it breaches a capacity cap, `optimal` when the
/// identity layout won the pool, `outscored` otherwise.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if a candidate tree cannot be rendered.
fn build_mode_result(
    snapshot: &Snapshot,
    config: &AnalyzeConfig,
    coefficients: &Coefficients,
    seed_identity: bool,
    capacity_clean: bool,
    splits: &[ConditionalSplit],
) -> Result<ModeResult, StrataError> {
    let solver = PipelineSolver::new(snapshot, config, *coefficients, seed_identity);
    let mode_config = mode_config(config);
    let CoreModeResult {
        candidates,
        solution_space_converged,
    } = diversify(&solver, &mode_config);

    let current_breakdown = score_current(snapshot, coefficients, &config.weights.kind_weights());
    let current_tree = &snapshot.ir().containers;
    let mut built = Vec::with_capacity(candidates.len());
    for (index, solved) in candidates.iter().enumerate() {
        let mut candidate = solver.build_candidate(
            current_tree,
            solved,
            u32::try_from(index + 1).unwrap_or(u32::MAX),
            splits,
        )?;
        candidate.improvement = current_breakdown.total - candidate.score;
        built.push(candidate);
    }

    let current_standing = if capacity_clean {
        let identity_won = candidates
            .first()
            .is_some_and(|best| solver.identity.as_ref() == Some(&best.partition));
        if identity_won {
            CurrentStanding::Optimal
        } else {
            CurrentStanding::Outscored
        }
    } else {
        CurrentStanding::Infeasible
    };

    // an infeasible standing must say what the best candidate actually fixes,
    // so its tree is re-checked against the same caps as the current one.
    let best_candidate_capacity = if capacity_clean {
        None
    } else {
        built.first().map(|best| {
            let hard: Vec<Level> = walk_all_capacity(&best.tree, config)
                .into_iter()
                .filter(|(_, finding)| finding.severity == Severity::Violation)
                .map(|(level, _)| level)
                .collect();
            let file_level = hard.iter().filter(|&&level| level == Level::File).count();
            CapacityRemainder {
                remaining: u32::try_from(hard.len()).unwrap_or(u32::MAX),
                file_level: u32::try_from(file_level).unwrap_or(u32::MAX),
            }
        })
    };

    let pairwise_distance = pairwise_distances(&candidates);
    Ok(ModeResult {
        candidates: built,
        pairwise_distance,
        solution_space_converged,
        current_score: current_breakdown.total,
        current_score_breakdown: current_breakdown.into(),
        current_standing,
        best_candidate_capacity,
    })
}

/// Returns the variation-of-information matrix over the diversified candidates.
fn pairwise_distances(candidates: &[SolvedCandidate]) -> Vec<Vec<f64>> {
    candidates
        .iter()
        .map(|left| {
            candidates
                .iter()
                .map(|right| vi_distance(&left.partition, &right.partition))
                .collect()
        })
        .collect()
}

/// Translates the engine config into the diversifier's [`ModeConfig`].
fn mode_config(config: &AnalyzeConfig) -> ModeConfig {
    ModeConfig {
        k: config.analysis.candidates as usize,
        base_seed: config.analysis.seed,
        score_tolerance: config.diversity.score_tolerance,
        min_distance: config.diversity.min_distance,
        pool_per_candidate: config.diversity.seeds_per_candidate as usize,
    }
}

/// One file container of the current tree: the movable atom of the search.
///
/// Clustering, capacity, narration, and move distance all treat the file as
/// indivisible in this slice (symbol-level packing is the one unglued phase), so
/// the search graph's vertices are files rather than symbols — which also makes
/// the folder cap (files per folder) exact instead of approximated in SCC counts.
struct FileInfo {
    /// The file's container id in the current tree.
    container: u32,
    /// The file's full repo-relative path (its container name).
    name: SmolStr,
    /// Summed production SLOC of the symbols currently in the file.
    production_sloc: u32,
}

/// Bound on polish sweeps: two passes catch the follow-up moves the first pass
/// unlocks without ballooning the wall clock.
const POLISH_SWEEPS: usize = 2;

/// Candidate destination folders examined per move unit during polish.
const POLISH_TARGETS: usize = 4;

/// The restartable solver that runs the cluster pipeline once per seed.
///
/// All of the seed-independent work — the weighted file-dependency graph, its
/// SCC condensation, the coarsening chain, and the per-level cohesion gain
/// models — is computed once at construction; each [`Solver::solve`] call seeds
/// the coarsest level deterministically from its seed, refines the partition
/// down the whole chain, assembles the five-level layout, and polishes it under
/// the full objective, so every seed yields a pure, reproducible candidate. In
/// anchored mode the pool additionally carries the identity layout ("change
/// nothing"), so a suggested restructuring can never silently score worse than
/// the current tree.
struct PipelineSolver<'a> {
    /// The analyzed snapshot.
    snapshot: &'a Snapshot,
    /// The current tree's file containers, ascending container id; vertex `i` of
    /// the file graph is `files[i]`.
    files: Vec<FileInfo>,
    /// File-container id to file-graph vertex.
    index_of: BTreeMap<u32, u32>,
    /// The SCC condensation of the weighted hard-edge file graph.
    condensation: Condensation,
    /// The coarsening chain over the condensation DAG (base level first).
    chain: Vec<CoarseGraph>,
    /// One cohesion gain model per chain level, token sets folded upward.
    gains: Vec<GainFn>,
    /// The condensation DAG with every edge reversed, for pull ranking.
    reverse_dag: Csr,
    /// The per-level member caps.
    caps: LevelCaps,
    /// The objective coefficients for this mode.
    coefficients: Coefficients,
    /// The configured edge-kind weights pricing the cut term.
    weights: KindWeights,
    /// The identity partition (anchored mode on a cap-clean tree), else `None`.
    identity: Option<Partition>,
    /// The configured base seed; seed offsets 0 and 1 select identity entries.
    base_seed: u64,
    /// The current root's name, reused for candidate package groups.
    root_name: SmolStr,
    /// The per-file facts narration consults when explaining moves.
    facts: FileFacts,
}

impl<'a> PipelineSolver<'a> {
    /// Builds the solver, computing every seed-independent pipeline input once.
    fn new(
        snapshot: &'a Snapshot,
        config: &AnalyzeConfig,
        coefficients: Coefficients,
        seed_identity: bool,
    ) -> Self {
        let ir = snapshot.ir();
        let weights = config.weights.kind_weights();

        let mut files: Vec<FileInfo> = ir
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| FileInfo {
                container: container.id.0,
                name: container.name.clone(),
                production_sloc: 0,
            })
            .collect();
        files.sort_by_key(|file| file.container);
        let index_of: BTreeMap<u32, u32> = files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.container, u32::try_from(index).unwrap_or(u32::MAX)))
            .collect();
        for node in &ir.nodes {
            if node.polarity != Polarity::Production {
                continue;
            }
            let Some(&index) = index_of.get(&node.container.0) else {
                continue;
            };
            if let Some(file) = files.get_mut(index as usize) {
                file.production_sloc = file.production_sloc.saturating_add(node.effective_size);
            }
        }

        let file_graph = build_file_graph(&ir.edges, &ir.nodes, &index_of, files.len(), &weights);
        let condensation = condense(&file_graph);
        let layers = layer(&condensation);
        // an SCC's capacity weight is its file count, so clusters honour the
        // folder cap in files at every level of the coarsening chain.
        let scc_weights: Vec<u32> = condensation
            .members
            .iter()
            .map(|members| u32::try_from(members.len()).unwrap_or(u32::MAX))
            .collect();
        let caps = level_caps(config);
        let chain = coarsen_chain(&condensation.dag, &layers, &scc_weights, caps.folder.max(1));
        let gains = level_gains(&files, &condensation, &chain, &coefficients);
        let reverse_dag = reverse_csr(&condensation.dag);

        let parent_of: BTreeMap<u32, Option<u32>> = ir
            .containers
            .containers()
            .iter()
            .map(|container| (container.id.0, container.parent.map(|parent| parent.0)))
            .collect();
        let identity = seed_identity.then(|| identity_partition(&files, &condensation, &parent_of));
        let root_name = ir
            .containers
            .containers()
            .iter()
            .find(|container| container.level == ScopeLevel::PackageGroup)
            .map_or_else(|| SmolStr::new("workspace"), |group| group.name.clone());
        let facts = file_facts(snapshot, &weights, config.capacity.folder);

        Self {
            snapshot,
            files,
            index_of,
            condensation,
            chain,
            gains,
            reverse_dag,
            caps,
            coefficients,
            weights,
            identity,
            base_seed: config.analysis.seed,
            root_name,
            facts,
        }
    }

    /// Runs the full multilevel scheme for one seed: seed the coarsest level
    /// under a seed-perturbed layer order, refine it there, then project the
    /// partition one level finer and re-refine at every level of the chain (the
    /// uncoarsening loop the spec's cluster pseudocode mandates).
    fn multilevel(&self, seed_value: u64) -> Partition {
        let Some(top) = self.chain.last() else {
            return Partition::from_assignment(Vec::new(), 0);
        };
        let perturbed = perturb_layers(&top.layers, seed_value);
        let mut parts = seed(top, &perturbed, &self.caps, SeedLevel::Folder);
        if let Some(gain) = self.gains.last() {
            refine(top, &mut parts, gain, &self.caps, SeedLevel::Folder);
        }
        // windows pair [fine, coarse]; walking them in reverse projects the
        // coarse partition onto the finer graph and re-refines it there, with
        // gains[i] being the fine graph's cohesion model.
        for (window, gain) in self.chain.windows(2).zip(&self.gains).rev() {
            let [fine, coarse] = window else {
                continue;
            };
            parts = coarse.project(&parts);
            refine(fine, &mut parts, gain, &self.caps, SeedLevel::Folder);
        }
        debug_assert_eq!(
            parts.node_count(),
            self.condensation.members.len(),
            "the projected partition must cover every file scc"
        );
        parts
    }

    /// Scores the five-level layout `parts` induces under this mode's
    /// coefficients.
    fn evaluate(&self, parts: &Partition) -> f64 {
        let assembled = self.assemble(parts);
        let placement = |id: u32| assembled.placement.get(&id).copied();
        let distance = move_distance(self.snapshot, &assembled.tree);
        let candidate = score_candidate(self.snapshot, &placement, &assembled.tree, distance);
        score(&candidate, &self.coefficients, &self.weights).total
    }

    /// The J(T)-polish pass: sweeps every file SCC in deterministic order and
    /// greedily relocates it to the strongest-pulling folder whenever the move
    /// strictly lowers the full five-level objective. Capacity (files per
    /// folder) and quotient acyclicity stay hard vetoes, never penalties. At
    /// most [`POLISH_SWEEPS`] passes, stopping early once a sweep applies no
    /// move. Returns the final score so `solve` never re-evaluates.
    fn polish(&self, parts: &mut Partition) -> f64 {
        let mut best = self.evaluate(parts);
        // live per-folder FILE counts: clusters size in SCCs, caps in files.
        let mut file_count: Vec<u32> = vec![0; parts.cluster_count()];
        for (scc, members) in self.condensation.members.iter().enumerate() {
            let Some(cluster) = parts.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX)) else {
                continue;
            };
            if let Some(slot) = file_count.get_mut(cluster.0 as usize) {
                *slot = slot.saturating_add(u32::try_from(members.len()).unwrap_or(u32::MAX));
            }
        }
        for _ in 0..POLISH_SWEEPS {
            let mut improved = false;
            for scc in 0..self.condensation.members.len() {
                let scc32 = u32::try_from(scc).unwrap_or(u32::MAX);
                let Some(source) = parts.cluster_of(scc32) else {
                    continue;
                };
                let unit_files = self.condensation.members.get(scc).map_or(0, |members| {
                    u32::try_from(members.len()).unwrap_or(u32::MAX)
                });
                for target in self.pull_targets(parts, scc32, source) {
                    let target_files = file_count
                        .get(target.0 as usize)
                        .copied()
                        .unwrap_or(u32::MAX);
                    if target_files.saturating_add(unit_files) > self.caps.folder {
                        continue;
                    }
                    if !parts.move_node(scc32, target) {
                        continue;
                    }
                    let total = if is_acyclic(&parts.quotient(&self.condensation.dag)) {
                        self.evaluate(parts)
                    } else {
                        f64::INFINITY
                    };
                    if total < best {
                        best = total;
                        if let Some(slot) = file_count.get_mut(source.0 as usize) {
                            *slot = slot.saturating_sub(unit_files);
                        }
                        if let Some(slot) = file_count.get_mut(target.0 as usize) {
                            *slot = slot.saturating_add(unit_files);
                        }
                        improved = true;
                        break;
                    }
                    parts.move_node(scc32, source);
                }
            }
            if !improved {
                break;
            }
        }
        best
    }

    /// Ranks the folders pulling hardest on `scc` — summed edge weight over both
    /// directions — and returns up to [`POLISH_TARGETS`] of them, strongest
    /// first, ties broken by the lower cluster id.
    fn pull_targets(&self, parts: &Partition, scc: u32, source: ClusterId) -> Vec<ClusterId> {
        let mut pull: BTreeMap<ClusterId, f64> = BTreeMap::new();
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                let Some(cluster) = parts.cluster_of(neighbour) else {
                    continue;
                };
                if cluster == source {
                    continue;
                }
                *pull.entry(cluster).or_insert(0.0) +=
                    f64::from(weights.get(slot).copied().unwrap_or(0.0));
            }
        }
        let mut ranked: Vec<(ClusterId, f64)> = pull.into_iter().collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.cmp(&right.0))
        });
        ranked
            .into_iter()
            .take(POLISH_TARGETS)
            .map(|(cluster, _)| cluster)
            .collect()
    }

    /// Wraps a finished partition, re-pricing it at the true current tree when
    /// it converged back to the identity layout, so every identity entry in the
    /// pool carries one consistent score.
    fn finish(&self, parts: Partition, total: f64) -> SolvedCandidate {
        if let Some(identity) = &self.identity
            && identity == &parts
        {
            return self.identity_entry(identity);
        }
        SolvedCandidate {
            partition: parts,
            score: total,
        }
    }

    /// The identity pool entry: the "change nothing" layout scored on the actual
    /// current tree, so the anchored pool always contains the current score and
    /// a suggested candidate can never silently lose to it.
    fn identity_entry(&self, identity: &Partition) -> SolvedCandidate {
        SolvedCandidate {
            partition: identity.clone(),
            score: score_current(self.snapshot, &self.coefficients, &self.weights).total,
        }
    }

    /// Groups the file-graph vertices by the folder cluster their SCC lands in,
    /// members sorted by file path for deterministic emission.
    fn folder_members(&self, parts: &Partition) -> BTreeMap<u32, Vec<u32>> {
        let mut members_of: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for vertex in 0..self.files.len() {
            let Some(scc) = self.condensation.membership.get(vertex) else {
                continue;
            };
            let Some(cluster) = parts.cluster_of(scc.0) else {
                debug_assert!(false, "the folder partition must cover every file scc");
                continue;
            };
            members_of
                .entry(cluster.0)
                .or_default()
                .push(u32::try_from(vertex).unwrap_or(u32::MAX));
        }
        for members in members_of.values_mut() {
            members.sort_by(|&left, &right| {
                let left_name = self
                    .files
                    .get(left as usize)
                    .map_or("", |file| file.name.as_str());
                let right_name = self
                    .files
                    .get(right as usize)
                    .map_or("", |file| file.name.as_str());
                left_name.cmp(right_name).then(left.cmp(&right))
            });
        }
        members_of
    }

    /// Assembles the five-level candidate tree a folder partition induces.
    ///
    /// Folders are the partition's non-empty clusters. The upper levels come
    /// from clustering each level's weighted quotient in turn (folders → domains
    /// → packages → package groups). Containers are named from the files they
    /// transitively hold — folders by the plurality parent directory (weighted
    /// by production SLOC, then file count), domains and packages by two- and
    /// one-segment path prefixes, the group by the current root's name — and
    /// file leaves keep their full current paths so file identity stays stable
    /// across trees.
    fn assemble(&self, parts: &Partition) -> CandidateTree {
        if self.files.is_empty() {
            let root = Container {
                id: ContainerId(0),
                name: self.root_name.clone(),
                level: ScopeLevel::PackageGroup,
                parent: None,
            };
            return CandidateTree {
                tree: ContainerTree::new(vec![root]),
                placement: BTreeMap::new(),
            };
        }

        let members_of = self.folder_members(parts);

        // one clustering pass per upper level, each over the previous level's
        // weighted quotient graph.
        let folder_quotient = parts.quotient(&self.condensation.dag);
        let domain_parts = cluster_level(&folder_quotient, &self.caps, SeedLevel::Domain);
        let domain_quotient = domain_parts.quotient(&folder_quotient);
        let package_parts = cluster_level(&domain_quotient, &self.caps, SeedLevel::Package);
        let package_quotient = package_parts.quotient(&domain_quotient);
        let group_parts = cluster_level(&package_quotient, &self.caps, SeedLevel::PackageGroup);

        // ancestry of every non-empty folder cluster, plus the directory tallies
        // each level's containers are named from.
        let mut chain_of: BTreeMap<u32, (u32, u32, u32)> = BTreeMap::new();
        let mut folder_tally: NameTally = BTreeMap::new();
        let mut domain_tally: NameTally = BTreeMap::new();
        let mut package_tally: NameTally = BTreeMap::new();
        for (&folder, members) in &members_of {
            let domain = domain_parts
                .cluster_of(folder)
                .map_or(0, |cluster| cluster.0);
            let package = package_parts
                .cluster_of(domain)
                .map_or(0, |cluster| cluster.0);
            let group = group_parts
                .cluster_of(package)
                .map_or(0, |cluster| cluster.0);
            chain_of.insert(folder, (domain, package, group));
            for &vertex in members {
                let Some(file) = self.files.get(vertex as usize) else {
                    continue;
                };
                let dir = file.name.rsplit_once('/').map_or("", |(dir, _)| dir);
                let sloc = file.production_sloc;
                vote(&mut folder_tally, folder, dir_key(dir, usize::MAX), sloc);
                vote(&mut domain_tally, domain, dir_key(dir, 2), sloc);
                vote(&mut package_tally, package, dir_key(dir, 1), sloc);
            }
        }

        self.emit(
            &members_of,
            &chain_of,
            &folder_tally,
            &domain_tally,
            &package_tally,
        )
    }

    /// Interns the candidate containers parent-before-child — package groups,
    /// packages, domains, then each folder with its files — and records every
    /// symbol's file placement.
    fn emit(
        &self,
        members_of: &BTreeMap<u32, Vec<u32>>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        folder_tally: &NameTally,
        domain_tally: &NameTally,
        package_tally: &NameTally,
    ) -> CandidateTree {
        let mut containers: Vec<Container> = Vec::new();
        let mut used: BTreeMap<(Option<u32>, ScopeLevel), BTreeSet<SmolStr>> = BTreeMap::new();

        let groups: BTreeSet<u32> = chain_of.values().map(|&(_, _, group)| group).collect();
        let mut group_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for &group in &groups {
            let id = push_container(
                &mut containers,
                &mut used,
                &self.root_name,
                ScopeLevel::PackageGroup,
                None,
            );
            group_ids.insert(group, id);
        }

        let packages: BTreeMap<u32, u32> = chain_of
            .values()
            .map(|&(_, package, group)| (package, group))
            .collect();
        let mut package_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&package, &group) in &packages {
            let name = package_tally
                .get(&package)
                .map_or_else(|| SmolStr::new("workspace"), plurality);
            let id = push_container(
                &mut containers,
                &mut used,
                &name,
                ScopeLevel::Package,
                group_ids.get(&group).copied(),
            );
            package_ids.insert(package, id);
        }

        let domains: BTreeMap<u32, u32> = chain_of
            .values()
            .map(|&(domain, package, _)| (domain, package))
            .collect();
        let mut domain_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&domain, &package) in &domains {
            let key = domain_tally
                .get(&domain)
                .map_or_else(|| SmolStr::new("workspace"), plurality);
            // cumulative names must path-extend the parent's, or display
            // folding would re-emit the parent's segments under it.
            let name = extend_under(
                &final_name(&containers, package_ids.get(&package).copied()),
                &key,
            );
            let id = push_container(
                &mut containers,
                &mut used,
                &name,
                ScopeLevel::Domain,
                package_ids.get(&package).copied(),
            );
            domain_ids.insert(domain, id);
        }

        let mut file_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&folder, members) in members_of {
            let Some(&(domain, _, _)) = chain_of.get(&folder) else {
                continue;
            };
            let key = folder_tally
                .get(&folder)
                .map_or_else(|| SmolStr::new("workspace"), plurality);
            let name = extend_under(
                &final_name(&containers, domain_ids.get(&domain).copied()),
                &key,
            );
            let folder_id = push_container(
                &mut containers,
                &mut used,
                &name,
                ScopeLevel::Folder,
                domain_ids.get(&domain).copied(),
            );
            for &vertex in members {
                let Some(file) = self.files.get(vertex as usize) else {
                    continue;
                };
                let id = push_container(
                    &mut containers,
                    &mut used,
                    &file.name,
                    ScopeLevel::File,
                    Some(folder_id),
                );
                file_ids.insert(vertex, id);
            }
        }

        let mut placement = BTreeMap::new();
        for node in &self.snapshot.ir().nodes {
            let Some(&vertex) = self.index_of.get(&node.container.0) else {
                continue;
            };
            if let Some(&file_id) = file_ids.get(&vertex) {
                placement.insert(node.id.0, file_id);
            }
        }

        CandidateTree {
            tree: ContainerTree::new(containers),
            placement,
        }
    }

    /// Builds one DTO [`Candidate`] from a solved partition.
    ///
    /// The identity survivor is emitted as a verbatim clone of the current tree
    /// — zero moves, byte-equal layout — never re-derived through assembly, so
    /// "already optimal" genuinely means nothing changes. Every other partition
    /// is assembled, rescored under this mode's coefficients, and narrated
    /// against the current layout.
    ///
    /// # Errors
    ///
    /// Returns [`StrataError::SnapshotInvalid`] if the tree cannot be rendered.
    fn build_candidate(
        &self,
        current_tree: &ContainerTree,
        solved: &SolvedCandidate,
        index: u32,
        splits: &[ConditionalSplit],
    ) -> Result<Candidate, StrataError> {
        let nodes = &self.snapshot.ir().nodes;
        if self.identity.as_ref() == Some(&solved.partition) {
            let breakdown = score_current(self.snapshot, &self.coefficients, &self.weights);
            let node = render_tree(current_tree, nodes, &|node: &Node| Some(node.container))?;
            return Ok(Candidate {
                index,
                score: breakdown.total,
                score_breakdown: ScoreBreakdown::from(breakdown),
                improvement: 0.0,
                tree: node,
                conditional_splits: splits.to_vec(),
                delta_narration: Vec::new(),
            });
        }

        let assembled = self.assemble(&solved.partition);
        let placement = |id: u32| assembled.placement.get(&id).copied();
        let distance = move_distance(self.snapshot, &assembled.tree);
        let breakdown = score(
            &score_candidate(self.snapshot, &placement, &assembled.tree, distance),
            &self.coefficients,
            &self.weights,
        );
        let placement_of = |node: &Node| assembled.placement.get(&node.id.0).copied();
        let node = render_tree(&assembled.tree, nodes, &placement_of)?;
        let delta = narrate(current_tree, &assembled.tree, &self.facts);

        Ok(Candidate {
            index,
            score: breakdown.total,
            score_breakdown: ScoreBreakdown::from(breakdown),
            improvement: 0.0,
            tree: node,
            conditional_splits: splits.to_vec(),
            delta_narration: delta,
        })
    }
}

impl Solver for PipelineSolver<'_> {
    fn solve(&self, seed: u64) -> SolvedCandidate {
        let offset = seed.wrapping_sub(self.base_seed);
        if let Some(identity) = &self.identity {
            if offset == 0 {
                return self.identity_entry(identity);
            }
            if offset == 1 {
                // "current plus local improvements": refine and polish starting
                // from the identity layout instead of a fresh seed.
                let mut parts = identity.clone();
                if let (Some(base), Some(gain)) = (self.chain.first(), self.gains.first()) {
                    refine(base, &mut parts, gain, &self.caps, SeedLevel::Folder);
                }
                let total = self.polish(&mut parts);
                return self.finish(parts, total);
            }
        }
        let mut parts = self.multilevel(seed);
        let total = self.polish(&mut parts);
        self.finish(parts, total)
    }
}

/// Applies a deterministic seed-driven jitter to the layering used for seeding.
///
/// The seeding order is descending layer, ties broken by index; nudging each
/// vertex's layer by a small seed-derived amount reorders the ties differently per
/// seed without changing the longest-path structure materially, so each restart
/// grows a different initial clustering. Seed `0` returns the layers unchanged so
/// the base seed reproduces the canonical clustering.
fn perturb_layers(layers: &[u32], seed_value: u64) -> Vec<u32> {
    if seed_value == 0 {
        return layers.to_vec();
    }
    layers
        .iter()
        .enumerate()
        .map(|(index, &base)| {
            let mut state = seed_value
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(u64::try_from(index).unwrap_or(u64::MAX));
            // a small splitmix step gives a reproducible per-(seed, index) jitter.
            state ^= state >> 30;
            state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            let jitter = u32::try_from(state % 3).unwrap_or(0);
            base.saturating_mul(3).saturating_add(jitter)
        })
        .collect()
}

/// A reconstructed candidate tree plus the placement of every symbol node.
struct CandidateTree {
    /// The candidate container tree: package groups over packages, domains, and
    /// folders derived per level, each folder holding whole current files.
    tree: ContainerTree,
    /// The file container each symbol node lands in, keyed by node id.
    placement: BTreeMap<u32, ContainerId>,
}

/// Builds the weighted file-dependency graph: every hard symbol edge is mapped
/// onto its endpoints' owning files, intra-file edges vanish (layout cannot cut
/// them), parallel crossings are summed, and each crossing is priced by the
/// config's kind-weight table — so heavy-edge matching and FM gains see the same
/// prices the objective charges.
fn build_file_graph(
    edges: &[Edge],
    nodes: &[Node],
    index_of: &BTreeMap<u32, u32>,
    file_count: usize,
    weights: &KindWeights,
) -> Csr {
    let container_of: BTreeMap<u32, u32> = nodes
        .iter()
        .map(|node| (node.id.0, node.container.0))
        .collect();
    let mut crossings: Vec<(u32, u32, f32)> = Vec::new();
    for edge in edges {
        if edge.hardness != Hardness::Hard {
            continue;
        }
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
        crossings.push((from, to, weight));
    }
    Csr::from_weighted_edges(file_count, &crossings)
}

/// Builds the per-file facts narration consults: config-priced edge weights
/// summed per directed file pair (every edge, the objective's currency),
/// spec files (symbols exclusively test cases), and the folder cap.
fn file_facts(snapshot: &Snapshot, weights: &KindWeights, folder_cap: u32) -> FileFacts {
    let ir = snapshot.ir();
    let file_of: BTreeMap<u32, &SmolStr> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, &container.name))
        .collect();
    let container_of: BTreeMap<u32, u32> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.container.0))
        .collect();

    let mut edge_weights: BTreeMap<(String, String), f64> = BTreeMap::new();
    for edge in &ir.edges {
        let (Some(source), Some(target)) = (
            container_of
                .get(&edge.source.0)
                .and_then(|container| file_of.get(container)),
            container_of
                .get(&edge.target.0)
                .and_then(|container| file_of.get(container)),
        ) else {
            continue;
        };
        if source == target {
            continue;
        }
        *edge_weights
            .entry((source.to_string(), target.to_string()))
            .or_insert(0.0) += weights.edge_weight(edge.kind, edge.confidence);
    }

    // a spec file holds at least one symbol and nothing but test cases.
    let mut case_only: BTreeMap<u32, bool> = BTreeMap::new();
    for node in &ir.nodes {
        let entry = case_only.entry(node.container.0).or_insert(true);
        *entry &= node.polarity == Polarity::TestCase;
    }
    let test_case_files = case_only
        .iter()
        .filter(|&(_, &only_cases)| only_cases)
        .filter_map(|(container, _)| file_of.get(container).map(std::string::ToString::to_string))
        .collect();

    FileFacts {
        edge_weights,
        test_case_files,
        folder_cap,
    }
}

/// Returns `graph` with every edge reversed, weights preserved.
fn reverse_csr(graph: &Csr) -> Csr {
    let mut edges: Vec<(u32, u32, f32)> = Vec::with_capacity(graph.edge_count());
    for vertex in 0..graph.vertex_count() {
        let from = u32::try_from(vertex).unwrap_or(u32::MAX);
        let weights = graph.weights(from);
        for (slot, &to) in graph.neighbors(from).iter().enumerate() {
            edges.push((to, from, weights.get(slot).copied().unwrap_or(0.0)));
        }
    }
    Csr::from_weighted_edges(graph.vertex_count(), &edges)
}

/// Interns `token` into the dense id space, returning its id.
fn intern_token(interner: &mut BTreeMap<String, u32>, token: &str) -> u32 {
    if let Some(&id) = interner.get(token) {
        return id;
    }
    let id = u32::try_from(interner.len()).unwrap_or(u32::MAX);
    interner.insert(token.to_owned(), id);
    id
}

/// Builds one cohesion gain model per coarsening level.
///
/// Base-level vertices are file SCCs: naming tokens come from the member files'
/// basenames (shared tokenizer with the α scoring term) and path tokens from
/// their parent-directory segments, interned into dense ids. Every coarser
/// level unions the token multisets of the finer vertices it contracted, so the
/// α·naming + β·path bonus stays meaningful up the whole chain.
fn level_gains(
    files: &[FileInfo],
    condensation: &Condensation,
    chain: &[CoarseGraph],
    coefficients: &Coefficients,
) -> Vec<GainFn> {
    let mut interner: BTreeMap<String, u32> = BTreeMap::new();
    let mut naming: Vec<Vec<u32>> = Vec::with_capacity(condensation.members.len());
    let mut path: Vec<Vec<u32>> = Vec::with_capacity(condensation.members.len());
    for members in &condensation.members {
        let mut name_tokens = Vec::new();
        let mut path_tokens = Vec::new();
        for member in members {
            let Some(file) = files.get(member.0 as usize) else {
                continue;
            };
            for token in tokenize(&file.name) {
                name_tokens.push(intern_token(&mut interner, &token));
            }
            let dir = file.name.rsplit_once('/').map_or("", |(dir, _)| dir);
            for segment in dir.split('/').filter(|segment| !segment.is_empty()) {
                path_tokens.push(intern_token(&mut interner, segment));
            }
        }
        naming.push(name_tokens);
        path.push(path_tokens);
    }

    // reason: α/β live in f64 config space but the FM gain arithmetic is f32 by design
    #[allow(clippy::cast_possible_truncation)]
    let (alpha, beta) = (coefficients.alpha as f32, coefficients.beta as f32);

    let mut gains = Vec::with_capacity(chain.len());
    gains.push(GainFn::new(naming.clone(), path.clone(), alpha, beta));
    for level in chain.iter().skip(1) {
        let count = level.graph.vertex_count();
        let mut coarse_naming: Vec<Vec<u32>> = vec![Vec::new(); count];
        let mut coarse_path: Vec<Vec<u32>> = vec![Vec::new(); count];
        for (fine, &coarse) in level.fine_to_coarse.iter().enumerate() {
            if let (Some(slot), Some(tokens)) =
                (coarse_naming.get_mut(coarse as usize), naming.get(fine))
            {
                slot.extend_from_slice(tokens);
            }
            if let (Some(slot), Some(tokens)) =
                (coarse_path.get_mut(coarse as usize), path.get(fine))
            {
                slot.extend_from_slice(tokens);
            }
        }
        gains.push(GainFn::new(
            coarse_naming.clone(),
            coarse_path.clone(),
            alpha,
            beta,
        ));
        naming = coarse_naming;
        path = coarse_path;
    }
    gains
}

/// Builds the identity partition: each file SCC lands in a cluster keyed by the
/// current parent folder of its dominant member — the file with the largest
/// production SLOC, ties to the lexicographically smaller path (it only differs
/// from the literal current layout on cross-folder cycles, which must
/// co-cluster anyway). Cluster ids are dense over the distinct parent keys in
/// ascending container-id order; a rootless file keys to a shared sentinel.
fn identity_partition(
    files: &[FileInfo],
    condensation: &Condensation,
    parent_of: &BTreeMap<u32, Option<u32>>,
) -> Partition {
    let keys: Vec<u32> = condensation
        .members
        .iter()
        .map(|members| {
            let mut dominant: Option<&FileInfo> = None;
            for member in members {
                let Some(file) = files.get(member.0 as usize) else {
                    continue;
                };
                let better = dominant.is_none_or(|top| {
                    file.production_sloc > top.production_sloc
                        || (file.production_sloc == top.production_sloc && file.name < top.name)
                });
                if better {
                    dominant = Some(file);
                }
            }
            dominant
                .and_then(|file| parent_of.get(&file.container).copied().flatten())
                .unwrap_or(u32::MAX)
        })
        .collect();
    let distinct: BTreeSet<u32> = keys.iter().copied().collect();
    let cluster_of_key: BTreeMap<u32, u32> = distinct
        .iter()
        .enumerate()
        .map(|(index, &key)| (key, u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    let assignment = keys
        .iter()
        .map(|key| ClusterId(cluster_of_key.get(key).copied().unwrap_or(0)))
        .collect();
    Partition::from_assignment(assignment, distinct.len())
}

/// Clusters a weighted quotient graph one level up (folders → domains, domains
/// → packages, …) with the same multilevel scheme the base level uses, minus
/// cohesion (upper levels carry no token sets) and seed perturbation (the level
/// is fully determined by the partition below it, keeping assembly a pure
/// function of the folder partition).
fn cluster_level(graph: &Csr, caps: &LevelCaps, level: SeedLevel) -> Partition {
    let layers = tight_layers(graph);
    // each quotient vertex is one container of the level below, so capacity
    // weights are all one: the cap counts members directly.
    let unit_weights = vec![1_u32; graph.vertex_count()];
    let cap = match level {
        SeedLevel::Folder => caps.folder,
        SeedLevel::Domain => caps.domain,
        SeedLevel::Package => caps.package,
        SeedLevel::PackageGroup => caps.package_group,
    };
    let chain = coarsen_chain(graph, &layers, &unit_weights, cap.max(1));
    let Some(top) = chain.last() else {
        return Partition::from_assignment(Vec::new(), 0);
    };
    let mut parts = seed(top, &top.layers, caps, level);
    refine(
        top,
        &mut parts,
        &GainFn::cut_only(top.graph.vertex_count()),
        caps,
        level,
    );
    for window in chain.windows(2).rev() {
        let [fine, coarse] = window else {
            continue;
        };
        parts = coarse.project(&parts);
        refine(
            fine,
            &mut parts,
            &GainFn::cut_only(fine.graph.vertex_count()),
            caps,
            level,
        );
    }
    parts
}

/// Returns the first `take` segments of `dir` as a container name key, or
/// `workspace` when the directory is the repository root.
fn dir_key(dir: &str, take: usize) -> SmolStr {
    let segments: Vec<&str> = dir
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return SmolStr::new("workspace");
    }
    let bounded = take.min(segments.len());
    SmolStr::new(segments.get(..bounded).unwrap_or(&segments).join("/"))
}

/// Returns the final (dedup-suffixed) name of the container `id` interned in
/// `containers`, or the empty string when the id is absent.
fn final_name(containers: &[Container], id: Option<ContainerId>) -> SmolStr {
    id.and_then(|id| containers.get(id.0 as usize))
        .map_or_else(|| SmolStr::new(""), |container| container.name.clone())
}

/// Rewrites the elected directory `key` to nest under the parent's cumulative
/// name: a key that already equals or path-extends the parent passes through,
/// anything else keeps only its last segment appended to the parent. Mixed
/// clusters elect directories from foreign subtrees (a folder dominated by
/// `src/render` landing in a `src/core` domain), and without this rewrite the
/// display fold would re-emit the foreign prefix (`src/core/src/render`).
fn extend_under(parent: &str, key: &SmolStr) -> SmolStr {
    if parent.is_empty()
        || key.as_str() == parent
        || key
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
    {
        return key.clone();
    }
    let last = key.rsplit('/').next().unwrap_or(key);
    SmolStr::new(format!("{parent}/{last}"))
}

/// Per-cluster directory election: each key holds its accumulated
/// (production SLOC, file count) vote.
type NameTally = BTreeMap<u32, BTreeMap<SmolStr, (u64, u32)>>;

/// Adds one file's vote for `key` — production SLOC weighs first, file count
/// second, so test-only files cannot outvote the production home directory.
fn vote(tally: &mut NameTally, cluster: u32, key: SmolStr, production_sloc: u32) {
    let (sloc, count) = tally.entry(cluster).or_default().entry(key).or_default();
    *sloc = sloc.saturating_add(u64::from(production_sloc));
    *count = count.saturating_add(1);
}

/// Returns the heaviest-weighted key in `tally`, ties broken by the
/// lexicographically smallest key so container naming is deterministic across
/// runs.
fn plurality<V: Ord>(tally: &BTreeMap<SmolStr, V>) -> SmolStr {
    tally
        .iter()
        .max_by(|left, right| left.1.cmp(right.1).then(right.0.cmp(left.0)))
        .map_or_else(|| SmolStr::new("workspace"), |(key, _)| key.clone())
}

/// Pushes a container with the next dense id, deduplicating sibling names with
/// a numeric suffix so two clusters that elect the same directory stay distinct.
fn push_container(
    containers: &mut Vec<Container>,
    used: &mut BTreeMap<(Option<u32>, ScopeLevel), BTreeSet<SmolStr>>,
    name: &SmolStr,
    level: ScopeLevel,
    parent: Option<ContainerId>,
) -> ContainerId {
    let siblings = used
        .entry((parent.map(|parent| parent.0), level))
        .or_default();
    let mut unique = name.clone();
    let mut suffix = 2_u32;
    while siblings.contains(&unique) {
        unique = SmolStr::new(format!("{name}-{suffix}"));
        suffix = suffix.saturating_add(1);
    }
    siblings.insert(unique.clone());
    let id = ContainerId(u32::try_from(containers.len()).unwrap_or(u32::MAX));
    containers.push(Container {
        id,
        name: unique,
        level,
        parent,
    });
    id
}

/// Returns whether `graph` is a DAG (Kahn's algorithm visits every vertex).
fn is_acyclic(graph: &Csr) -> bool {
    let count = graph.vertex_count();
    let mut indegree = vec![0_u32; count];
    for vertex in 0..count {
        let from = u32::try_from(vertex).unwrap_or(u32::MAX);
        for &to in graph.neighbors(from) {
            if let Some(slot) = indegree.get_mut(to as usize) {
                *slot = slot.saturating_add(1);
            }
        }
    }
    let mut ready: Vec<u32> = indegree
        .iter()
        .enumerate()
        .filter(|&(_, &degree)| degree == 0)
        .map(|(vertex, _)| u32::try_from(vertex).unwrap_or(u32::MAX))
        .collect();
    let mut visited = 0_usize;
    while let Some(vertex) = ready.pop() {
        visited = visited.saturating_add(1);
        for &to in graph.neighbors(vertex) {
            if let Some(slot) = indegree.get_mut(to as usize) {
                *slot = slot.saturating_sub(1);
                if *slot == 0 {
                    ready.push(to);
                }
            }
        }
    }
    visited == count
}

/// Builds the scorer's [`ScoreCandidate`] view from a node-placement function over
/// the candidate `tree`.
///
/// `placement` maps each node id to the file container it occupies in the
/// candidate tree; the edge LCA levels, per-container child sizes, and naming
/// groups are derived from that placement and the tree. `move_distance` is the
/// already-computed fraction of relocated symbols.
fn score_candidate(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
    move_distance: f64,
) -> ScoreCandidate {
    let ir = snapshot.ir();
    let parent_of: BTreeMap<u32, Option<ContainerId>> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container.parent))
        .collect();
    let level_of: BTreeMap<u32, ScopeLevel> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container.level))
        .collect();

    let edges = ir
        .edges
        .iter()
        .filter_map(|edge| {
            let source = placement(edge.source.0)?;
            let target = placement(edge.target.0)?;
            let lca = lca_level(&parent_of, &level_of, source, target);
            Some(ScoredEdge {
                kind: edge.kind,
                confidence: edge.confidence,
                lca_level: lca,
            })
        })
        .collect();

    let containers = container_sizes(snapshot, placement, tree);
    let (cohesion_groups, path_cohesion) = cohesion_inputs(snapshot, placement, tree);

    ScoreCandidate {
        edges,
        containers,
        cohesion_groups,
        path_cohesion,
        move_distance,
    }
}

/// Derives the naming-cohesion groups and the path-cohesion fraction of a
/// placement (the α and β scoring inputs, previously stubbed).
///
/// Every parent container that directly holds files forms one group carrying
/// its production SLOC and the basename token set of each member file. Path
/// cohesion is the production-SLOC-weighted fraction of files whose parent
/// container is named exactly by the file's current directory path — an
/// unchanged layout scores ~1.0 and every relocation dilutes it.
fn cohesion_inputs(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
) -> (Vec<CohesionGroup>, f64) {
    let ir = snapshot.ir();
    // production SLOC landing in each file container under this placement.
    let mut file_sloc: BTreeMap<u32, u32> = BTreeMap::new();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(container) = placement(node.id.0) {
            let slot = file_sloc.entry(container.0).or_default();
            *slot = slot.saturating_add(node.effective_size);
        }
    }

    let name_of: BTreeMap<u32, &SmolStr> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();
    let mut groups: BTreeMap<u32, CohesionGroup> = BTreeMap::new();
    let mut matched = 0_u64;
    let mut total = 0_u64;
    for container in tree.containers() {
        if container.level != ScopeLevel::File {
            continue;
        }
        let Some(parent) = container.parent else {
            continue;
        };
        let sloc = file_sloc.get(&container.id.0).copied().unwrap_or(0);
        let group = groups.entry(parent.0).or_insert_with(|| CohesionGroup {
            production_sloc: 0,
            members: Vec::new(),
        });
        group.production_sloc = group.production_sloc.saturating_add(sloc);
        group.members.push(tokenize(&container.name));

        let dir = container.name.rsplit_once('/').map_or("", |(dir, _)| dir);
        let parent_name = name_of.get(&parent.0).map_or("", |name| name.as_str());
        total = total.saturating_add(u64::from(sloc));
        if parent_name == dir {
            matched = matched.saturating_add(u64::from(sloc));
        }
    }

    let path_cohesion = if total == 0 {
        0.0
    } else {
        // reason: sloc totals fit u32 sums; the f64 mantissa loses nothing material
        #[allow(clippy::cast_precision_loss)]
        let ratio = matched as f64 / total as f64;
        ratio
    };
    (groups.into_values().collect(), path_cohesion)
}

/// Returns the level of the lowest common ancestor of two containers.
///
/// Walks the ancestor chain of `left` into a set, then ascends `right` until a
/// shared ancestor is found; the highest endpoints share is the package-group root
/// of an empty intersection, so disjoint subtrees cross at the coarsest level.
fn lca_level(
    parent_of: &BTreeMap<u32, Option<ContainerId>>,
    level_of: &BTreeMap<u32, ScopeLevel>,
    left: ContainerId,
    right: ContainerId,
) -> ScopeLevel {
    let mut ancestors = std::collections::BTreeSet::new();
    let mut up_left = Some(left);
    while let Some(id) = up_left {
        if !ancestors.insert(id.0) {
            break;
        }
        up_left = parent_of.get(&id.0).copied().flatten();
    }

    let mut up_right = Some(right);
    while let Some(id) = up_right {
        if ancestors.contains(&id.0) {
            return level_of
                .get(&id.0)
                .copied()
                .unwrap_or(ScopeLevel::PackageGroup);
        }
        up_right = parent_of.get(&id.0).copied().flatten();
    }
    ScopeLevel::PackageGroup
}

/// Computes the per-container child subtree sizes (in production SLOC) for the
/// imbalance term.
fn container_sizes(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
) -> Vec<ContainerSizes> {
    let ir = snapshot.ir();
    // each file's production SLOC is the sum of effective_size over the production
    // nodes placed in it (test-zoned nodes already carry zero size).
    let mut file_sloc: BTreeMap<u32, u32> = BTreeMap::new();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(container) = placement(node.id.0) {
            let entry = file_sloc.entry(container.0).or_default();
            *entry = entry.saturating_add(node.effective_size);
        }
    }

    // bottom-up subtree size of each container.
    let mut subtree: BTreeMap<u32, u32> = file_sloc.clone();
    let mut children_by_parent: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for container in tree.containers() {
        if let Some(parent) = container.parent {
            children_by_parent
                .entry(parent.0)
                .or_default()
                .push(container.id.0);
        }
    }

    // accumulate sizes bottom-up by repeatedly summing known children; the
    // laminar tree is at most five levels deep, so five passes reach fixpoint.
    for _ in 0..5 {
        for container in tree.containers() {
            let total: u32 = children_by_parent
                .get(&container.id.0)
                .into_iter()
                .flatten()
                .filter_map(|child| subtree.get(child).copied())
                .sum();
            if total > 0 {
                subtree.insert(container.id.0, total);
            }
        }
    }

    children_by_parent
        .into_values()
        .map(|children| ContainerSizes {
            child_sizes: children
                .iter()
                .map(|child| subtree.get(child).copied().unwrap_or(0))
                .collect(),
        })
        .collect()
}

/// Computes the move distance of a candidate tree: the fraction of symbols whose
/// owning file path differs from the current layout.
fn move_distance(snapshot: &Snapshot, candidate: &ContainerTree) -> f64 {
    let ir = snapshot.ir();
    let current_paths = container_path_strings(&ir.containers);
    let candidate_paths = container_path_strings(candidate);
    let candidate_file_name: BTreeMap<smol_str::SmolStr, Vec<String>> = candidate
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .filter_map(|container| {
            candidate_paths
                .get(&container.id.0)
                .map(|path| (container.name.clone(), path.clone()))
        })
        .collect();

    let total = ir.nodes.len();
    if total == 0 {
        return 0.0;
    }
    // match candidate files by the original file's name, which the assembly
    // preserves; the id → name map avoids a per-node linear scan.
    let current_name: BTreeMap<u32, &SmolStr> = ir
        .containers
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();
    let moved = ir
        .nodes
        .iter()
        .filter(|node| {
            let Some(current) = current_paths.get(&node.container.0) else {
                return false;
            };
            let Some(name) = current_name.get(&node.container.0) else {
                return false;
            };
            candidate_file_name
                .get(name.as_str())
                .is_none_or(|candidate_path| candidate_path != current)
        })
        .count();

    f64::from(u32::try_from(moved).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total).unwrap_or(u32::MAX))
}

/// Returns each container's root-to-node name path, keyed by container id.
fn container_path_strings(tree: &ContainerTree) -> BTreeMap<u32, Vec<String>> {
    let by_id: BTreeMap<u32, &Container> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let mut paths = BTreeMap::new();
    for container in tree.containers() {
        let mut path = Vec::new();
        let mut cursor = Some(container.id);
        while let Some(id) = cursor {
            let Some(node) = by_id.get(&id.0) else {
                break;
            };
            path.push(node.name.to_string());
            cursor = node.parent;
        }
        path.reverse();
        paths.insert(container.id.0, path);
    }
    paths
}

/// Extracts the per-level member caps from the engine config.
fn level_caps(config: &AnalyzeConfig) -> LevelCaps {
    LevelCaps {
        folder: config.capacity.folder,
        domain: config.capacity.domain,
        package: config.capacity.package,
        package_group: config.capacity.package_group,
    }
}

/// Renders a [`ContainerTree`] into the nested [`ContainerNode`] DTO, attaching
/// each file's symbols and production SLOC.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if the tree has no root container.
fn render_tree(
    tree: &ContainerTree,
    nodes: &[Node],
    placement: &dyn Fn(&Node) -> Option<ContainerId>,
) -> Result<ContainerNode, StrataError> {
    let containers = tree.containers();
    let mut children_by_parent: BTreeMap<u32, Vec<&Container>> = BTreeMap::new();
    let mut roots = Vec::new();
    for container in containers {
        match container.parent {
            Some(parent) => children_by_parent
                .entry(parent.0)
                .or_default()
                .push(container),
            None => roots.push(container),
        }
    }

    let contents = file_contents_by_container(nodes, placement);

    // a forest with several roots is wrapped under a synthetic package group so
    // the DTO is always a single tree; a lone root is rendered directly.
    match roots.as_slice() {
        [] => Err(StrataError::SnapshotInvalid {
            source: strata_ir::SnapshotError::Serialization {
                reason: "container tree has no root".to_owned(),
            },
        }),
        [root] => Ok(render_node(root, &children_by_parent, &contents, "")),
        many => Ok(ContainerNode {
            name: "workspace".to_owned(),
            level: Level::PackageGroup,
            children: Some(
                many.iter()
                    .map(|root| render_node(root, &children_by_parent, &contents, ""))
                    .collect(),
            ),
            symbols: None,
            production_sloc: None,
        }),
    }
}

/// Recursively renders one container and its descendants.
///
/// Interior container names are *cumulative* path prefixes internally; the DTO
/// carries only each node's increment over its parent so a rendered tree never
/// repeats segments. Files keep their full path (their stable identity) and a
/// root keeps its own name.
fn render_node(
    container: &Container,
    children_by_parent: &BTreeMap<u32, Vec<&Container>>,
    contents: &BTreeMap<u32, FileContents>,
    parent_name: &str,
) -> ContainerNode {
    if container.level == ScopeLevel::File {
        let file = contents.get(&container.id.0);
        let symbols = file.map(|file| file.symbols.clone()).unwrap_or_default();
        let production_sloc = file.map_or(0, |file| file.production_sloc);
        return ContainerNode {
            name: container.name.to_string(),
            level: Level::from(container.level),
            children: None,
            symbols: Some(symbols),
            production_sloc: Some(production_sloc),
        };
    }

    let children = children_by_parent
        .get(&container.id.0)
        .map(|children| {
            children
                .iter()
                .map(|child| render_node(child, children_by_parent, contents, &container.name))
                .collect()
        })
        .unwrap_or_default();

    ContainerNode {
        name: increment_name(&container.name, parent_name),
        level: Level::from(container.level),
        children: Some(children),
        symbols: None,
        production_sloc: None,
    }
}

/// Returns `name`'s increment over its parent's cumulative name: the suffix it
/// adds when it extends the parent, its last segment when it repeats the parent
/// outright, and the whole name when the two are unrelated (or at the root).
fn increment_name(name: &str, parent_name: &str) -> String {
    if parent_name.is_empty() {
        return name.to_owned();
    }
    if name == parent_name {
        return name.rsplit('/').next().unwrap_or(name).to_owned();
    }
    name.strip_prefix(parent_name)
        .and_then(|rest| rest.strip_prefix('/'))
        .map_or_else(|| name.to_owned(), str::to_owned)
}

/// A file container's rendered contents: the symbols placed in it and the sum of
/// production SLOC over its production-polarity symbols.
struct FileContents {
    /// The symbol placements rendered for the file, in node order.
    symbols: Vec<SymbolPlacement>,
    /// True production SLOC: the sum of `effective_size` over the production
    /// nodes placed in the file (test-zoned nodes already carry zero size).
    production_sloc: u32,
}

/// Groups symbol placements and sums production SLOC by the container each node
/// is placed in, using `placement` to map a node to its (current or candidate)
/// file container.
fn file_contents_by_container(
    nodes: &[Node],
    placement: &dyn Fn(&Node) -> Option<ContainerId>,
) -> BTreeMap<u32, FileContents> {
    let mut by_container: BTreeMap<u32, FileContents> = BTreeMap::new();
    for node in nodes {
        let Some(container) = placement(node) else {
            continue;
        };
        let entry = by_container
            .entry(container.0)
            .or_insert_with(|| FileContents {
                symbols: Vec::new(),
                production_sloc: 0,
            });
        entry.symbols.push(SymbolPlacement {
            name: node.name.to_string(),
            visibility: Level::from(node.visibility),
        });
        if node.polarity == Polarity::Production {
            entry.production_sloc = entry.production_sloc.saturating_add(node.effective_size);
        }
    }
    by_container
}

/// Builds a node-id to name lookup keyed by the raw id.
fn node_names(snapshot: &Snapshot) -> BTreeMap<u32, String> {
    snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id.0, node.name.to_string()))
        .collect()
}

/// One solved multi-member SCC: its members, config-priced internal edges, a
/// minimum-weight break set, and the members' total production SLOC.
///
/// `pair_weights` keys are *local* indices into `members` — the same numbering
/// the break set's [`EdgeRef`]s use.
struct SccSolution {
    /// The SCC's member nodes, ascending id.
    members: Vec<NodeId>,
    /// Summed edge weight per directed local pair.
    pair_weights: BTreeMap<(u32, u32), f64>,
    /// The MFAS solution over the SCC.
    break_set: BreakSet,
    /// Total production SLOC across members (drives conditional splits).
    production_sloc: u64,
}

/// Runs MFAS over every multi-member SCC of the hard-edge graph.
///
/// SCCs solve sequentially in condensation order: `shatter_all` is unusable
/// here because each SCC gets its own dense local numbering, and one shared
/// weight table would collide the [`EdgeRef`]s. Edge prices come from the
/// config's kind-weight table so break suggestions rank by the same currency
/// the objective charges.
fn solve_cycles(
    snapshot: &Snapshot,
    config: &AnalyzeConfig,
    weights: &KindWeights,
) -> Vec<SccSolution> {
    let views = build_csr(snapshot, HardnessFilter::HardOnly);
    let condensation = condense(&views.forward);
    let ir = snapshot.ir();
    let limits = config.solver.limits();
    let node_by_id: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();

    condensation
        .members
        .iter()
        .filter(|members| members.len() > 1)
        .map(|members| {
            let local: BTreeMap<u32, u32> = members
                .iter()
                .enumerate()
                .map(|(index, node)| (node.0, u32::try_from(index).unwrap_or(u32::MAX)))
                .collect();

            let mut pair_weights: BTreeMap<(u32, u32), f64> = BTreeMap::new();
            for edge in &ir.edges {
                if edge.hardness != Hardness::Hard {
                    continue;
                }
                let (Some(&source), Some(&target)) =
                    (local.get(&edge.source.0), local.get(&edge.target.0))
                else {
                    continue;
                };
                if source == target {
                    continue;
                }
                *pair_weights.entry((source, target)).or_insert(0.0) +=
                    weights.edge_weight(edge.kind, edge.confidence);
            }

            let view = SccView::new(
                u32::try_from(members.len()).unwrap_or(u32::MAX),
                pair_weights
                    .keys()
                    .map(|&(source, target)| EdgeRef { source, target }),
            );
            let table = EdgeWeights::from_pairs(
                pair_weights
                    .iter()
                    .map(|(&(source, target), &weight)| (EdgeRef { source, target }, weight)),
            );
            let break_set = shatter(&view, &table, &limits);

            let production_sloc = members
                .iter()
                .filter_map(|node| node_by_id.get(&node.0))
                .filter(|node| node.polarity == Polarity::Production)
                .map(|node| u64::from(node.effective_size))
                .sum();

            SccSolution {
                members: members.clone(),
                pair_weights,
                break_set,
                production_sloc,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use strata_ir::{
        ContainerId, Edge, EdgeKind, Hardness, IntermediateRepresentation, NodeId, NodeKind,
    };

    use super::*;

    /// Builds a symbol node owning a container.
    fn node(id: u32, name: &str, container: u32, polarity: Polarity) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(name),
            kind: NodeKind::Symbol,
            polarity,
            container: ContainerId(container),
            visibility: ScopeLevel::File,
            effective_size: 1,
        }
    }

    /// Builds a hard call edge.
    fn edge(source: u32, target: u32) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind: EdgeKind::Call,
            hardness: Hardness::Hard,
            confidence: 1.0,
        }
    }

    /// Builds a container at a level under an optional parent.
    fn container(id: u32, name: &str, level: ScopeLevel, parent: Option<u32>) -> Container {
        Container {
            id: ContainerId(id),
            name: SmolStr::new(name),
            level,
            parent: parent.map(ContainerId),
        }
    }

    /// Assembles a snapshot from parts, falling back to a minimal valid one on
    /// failure so a test never panics on assembly.
    fn snapshot(nodes: Vec<Node>, edges: Vec<Edge>, containers: Vec<Container>) -> Snapshot {
        let ir = IntermediateRepresentation::new(nodes, edges, ContainerTree::new(containers));
        Snapshot::assemble(ir).unwrap_or_else(|_| minimal_snapshot())
    }

    /// Returns a minimal valid snapshot: a single empty root file container.
    fn minimal_snapshot() -> Snapshot {
        let ir = IntermediateRepresentation::new(
            vec![],
            vec![],
            ContainerTree::new(vec![container(0, "root", ScopeLevel::File, None)]),
        );
        Snapshot::assemble(ir).unwrap_or_else(|_| minimal_snapshot())
    }

    /// Collects every container's name, level, and any rendered production SLOC
    /// over a tree, pre-order.
    fn collect_tree(
        node: &ContainerNode,
        names: &mut Vec<String>,
        sloc: &mut Vec<u32>,
        levels: &mut Vec<Level>,
    ) {
        names.push(node.name.clone());
        levels.push(node.level);
        if let Some(value) = node.production_sloc {
            sloc.push(value);
        }
        for child in node.children.iter().flatten() {
            collect_tree(child, names, sloc, levels);
        }
    }

    /// Builds a config requesting `k` candidates in both modes.
    fn config_with_k(k: u32) -> AnalyzeConfig {
        let mut config = AnalyzeConfig::default();
        config.analysis.candidates = k;
        config
    }

    /// Builds a file node with `sloc` production SLOC.
    fn file(name: &str, sloc: u32) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: Some(Vec::new()),
            production_sloc: Some(sloc),
        }
    }

    /// Builds a folder node holding `children`.
    fn folder(name: &str, children: Vec<ContainerNode>) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::Folder,
            children: Some(children),
            symbols: None,
            production_sloc: None,
        }
    }

    /// Builds a config with the file cap set to `cap`.
    fn config_with_file_cap(cap: u32) -> AnalyzeConfig {
        let mut config = AnalyzeConfig::default();
        config.capacity.file = cap;
        config
    }

    #[test]
    fn should_report_a_cycle_as_a_violation() {
        let snapshot = snapshot(
            vec![
                node(0, "a", 0, Polarity::Production),
                node(1, "b", 0, Polarity::Production),
            ],
            vec![edge(0, 1), edge(1, 0)],
            vec![container(0, "file", ScopeLevel::File, None)],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let violations = result
            .map(|result| result.current.violations)
            .unwrap_or_default();

        assert!(violations.iter().any(|v| v.kind == ViolationKind::Cycle));
    }

    #[test]
    fn should_report_a_polarity_breach() {
        let snapshot = snapshot(
            vec![
                node(0, "prod", 0, Polarity::Production),
                node(1, "helper", 0, Polarity::TestSupport),
            ],
            vec![edge(0, 1)],
            vec![container(0, "file", ScopeLevel::File, None)],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let violations = result
            .map(|result| result.current.violations)
            .unwrap_or_default();

        assert!(violations.iter().any(|v| v.kind == ViolationKind::Polarity));
    }

    #[test]
    fn should_report_a_test_support_dependency_on_a_test_case() {
        let snapshot = snapshot(
            vec![
                node(0, "helper", 0, Polarity::TestSupport),
                node(1, "spec", 0, Polarity::TestCase),
            ],
            vec![edge(0, 1)],
            vec![container(0, "file", ScopeLevel::File, None)],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let violations = result
            .map(|result| result.current.violations)
            .unwrap_or_default();

        assert!(violations.iter().any(|v| v.kind == ViolationKind::Polarity
            && v.detail == "test support `helper` depends on test case `spec`"));
    }

    #[test]
    fn should_locate_a_nested_over_cap_file_by_ancestors_and_basename() {
        // interior DTO names are incremental; the file keeps its full path but
        // its location contributes only the basename (detail has the path).
        let tree = folder(
            "app",
            vec![folder(
                "spec",
                vec![folder(
                    "agent",
                    vec![folder(
                        "mocks",
                        vec![file("spec/agent/mocks/google/gen.ts", 300)],
                    )],
                )],
            )],
        );

        let findings = capacity_violations(&tree, &config_with_file_cap(250));

        let location = findings
            .iter()
            .find(|f| f.severity == Severity::Violation)
            .map(|f| f.location.clone())
            .unwrap_or_default();
        assert_eq!(location, vec!["app", "spec", "agent", "mocks", "gen.ts"]);
    }

    #[test]
    fn should_collapse_repeated_synthetic_levels_in_capacity_locations() {
        // a root-level file hangs under the synthetic workspace chain, whose
        // levels all render the same segment; the location keeps it once.
        let tree = folder(
            "over-capacity",
            vec![folder(
                "workspace",
                vec![folder(
                    "workspace",
                    vec![folder("workspace", vec![file("huge.py", 300)])],
                )],
            )],
        );

        let findings = capacity_violations(&tree, &config_with_file_cap(250));

        let location = findings
            .iter()
            .find(|f| f.severity == Severity::Violation)
            .map(|f| f.location.clone())
            .unwrap_or_default();
        assert_eq!(location, vec!["over-capacity", "workspace", "huge.py"]);
    }

    #[test]
    fn should_report_a_file_over_its_cap_as_a_hard_violation() {
        let tree = file("big", 100);

        let findings = capacity_violations(&tree, &config_with_file_cap(10));

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings.first().map(|f| f.severity),
            Some(Severity::Violation)
        );
    }

    #[test]
    fn should_report_a_file_within_the_band_as_borderline() {
        // cap 100, file at 105 sits inside the +10% band -> borderline.
        let tree = file("near", 105);

        let findings = capacity_violations(&tree, &config_with_file_cap(100));

        assert_eq!(
            findings.first().map(|f| f.severity),
            Some(Severity::Borderline)
        );
    }

    #[test]
    fn should_not_report_a_file_well_under_its_cap() {
        let tree = file("small", 10);

        let findings = capacity_violations(&tree, &config_with_file_cap(100));

        assert!(findings.is_empty());
    }

    #[test]
    fn should_count_folder_members_against_the_folder_cap() {
        let children = (0..20).map(|i| file(&format!("f{i}"), 1)).collect();
        let tree = folder("dir", children);
        let mut config = AnalyzeConfig::default();
        config.capacity.folder = 5;

        let findings = capacity_violations(&tree, &config);

        assert!(findings.iter().any(|f| f.kind == ViolationKind::Capacity
            && f.severity == Severity::Violation
            && f.location == vec!["dir".to_owned()]));
    }

    #[test]
    fn should_be_deterministic_in_its_hash() {
        let make = || {
            snapshot(
                vec![node(0, "a", 0, Polarity::Production)],
                vec![],
                vec![container(0, "file", ScopeLevel::File, None)],
            )
        };

        let first = analyze(&make(), &AnalyzeConfig::default())
            .map(|result| result.snapshot_hash)
            .unwrap_or_default();
        let second = analyze(&make(), &AnalyzeConfig::default())
            .map(|result| result.snapshot_hash)
            .unwrap_or_default();

        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[test]
    fn should_produce_both_modes_when_requested() {
        let snapshot = snapshot(
            vec![node(0, "a", 0, Polarity::Production)],
            vec![],
            vec![container(0, "file", ScopeLevel::File, None)],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let modes = result.map(|result| result.modes).unwrap_or_default();

        assert!(modes.anchored.is_some());
        assert!(modes.greenfield.is_some());
    }

    #[test]
    fn should_return_up_to_k_candidates_per_mode() {
        // a graph with several independent files gives the clusterer room to find
        // more than one distinct grouping, so diversification returns multiple.
        let nodes = (0..8)
            .map(|i| node(i, &format!("sym{i}"), i, Polarity::Production))
            .collect::<Vec<_>>();
        let containers = (0..8)
            .map(|i| container(i, &format!("file{i}"), ScopeLevel::File, None))
            .collect::<Vec<_>>();
        let snapshot = snapshot(nodes, vec![edge(0, 1), edge(2, 3), edge(4, 5)], containers);

        let anchored = analyze(&snapshot, &config_with_k(3))
            .ok()
            .and_then(|result| result.modes.anchored)
            .unwrap_or(ModeResult {
                candidates: Vec::new(),
                pairwise_distance: Vec::new(),
                solution_space_converged: false,
                current_score: 0.0,
                current_score_breakdown: ScoreBreakdown {
                    cut: 0.0,
                    imbalance: 0.0,
                    naming: 0.0,
                    path: 0.0,
                    anchor: 0.0,
                },
                current_standing: CurrentStanding::Outscored,
                best_candidate_capacity: None,
            });

        // never more than k, always at least one candidate is produced.
        assert!(!anchored.candidates.is_empty());
        assert!(anchored.candidates.len() <= 3);
        // candidates are ranked, best (lowest) score first.
        for window in anchored.candidates.windows(2) {
            let (Some(first), Some(second)) = (window.first(), window.get(1)) else {
                continue;
            };
            assert!(first.score <= second.score);
        }
        // the pairwise distance matrix is square over the returned candidates.
        assert_eq!(anchored.pairwise_distance.len(), anchored.candidates.len());
    }

    #[test]
    fn should_index_candidates_from_one() {
        let snapshot = snapshot(
            vec![
                node(0, "a", 0, Polarity::Production),
                node(1, "b", 1, Polarity::Production),
            ],
            vec![],
            vec![
                container(0, "file_a", ScopeLevel::File, None),
                container(1, "file_b", ScopeLevel::File, None),
            ],
        );

        let first_index = analyze(&snapshot, &config_with_k(2))
            .ok()
            .and_then(|result| result.modes.anchored)
            .and_then(|anchored| anchored.candidates.first().map(|candidate| candidate.index));

        assert_eq!(first_index, Some(1));
    }

    #[test]
    fn should_render_a_nested_tree_with_file_symbols() {
        let snapshot = snapshot(
            vec![node(0, "sym", 1, Polarity::Production)],
            vec![],
            vec![
                container(0, "pkg", ScopeLevel::Package, None),
                container(1, "file", ScopeLevel::File, Some(0)),
            ],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let tree = result.map(|result| result.current.tree);

        let root = tree.unwrap_or_else(|_| ContainerNode {
            name: String::new(),
            level: Level::File,
            children: None,
            symbols: None,
            production_sloc: None,
        });
        assert_eq!(root.level, Level::Package);
        let file = root
            .children
            .and_then(|children| children.into_iter().next());
        assert_eq!(file.and_then(|file| file.production_sloc), Some(1));
    }

    #[test]
    fn should_render_a_multi_root_forest_under_a_synthetic_group() {
        let snapshot = snapshot(
            vec![],
            vec![],
            vec![
                container(0, "a", ScopeLevel::Package, None),
                container(1, "b", ScopeLevel::Package, None),
            ],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let level = result.map_or(Level::File, |result| result.current.tree.level);

        assert_eq!(level, Level::PackageGroup);
    }

    #[test]
    fn should_sort_violations_by_severity_then_kind_then_location() {
        let finding = |kind, severity, location: &str| Violation {
            kind,
            severity,
            location: vec![location.to_owned()],
            detail: String::new(),
            break_suggestions: None,
        };
        let mut violations = vec![
            finding(ViolationKind::Capacity, Severity::Borderline, "a"),
            finding(ViolationKind::Visibility, Severity::Violation, "b"),
            finding(ViolationKind::Capacity, Severity::Violation, "z"),
            finding(ViolationKind::Capacity, Severity::Violation, "a"),
            finding(ViolationKind::Cycle, Severity::Violation, "y"),
        ];

        sort_violations(&mut violations);

        let order: Vec<(ViolationKind, Severity, &str)> = violations
            .iter()
            .filter_map(|violation| {
                violation
                    .location
                    .first()
                    .map(|location| (violation.kind, violation.severity, location.as_str()))
            })
            .collect();
        assert_eq!(
            order,
            vec![
                (ViolationKind::Cycle, Severity::Violation, "y"),
                (ViolationKind::Capacity, Severity::Violation, "a"),
                (ViolationKind::Capacity, Severity::Violation, "z"),
                (ViolationKind::Visibility, Severity::Violation, "b"),
                (ViolationKind::Capacity, Severity::Borderline, "a"),
            ]
        );
    }

    #[test]
    fn should_elect_names_by_production_sloc_before_file_count() {
        // two production files under src outweigh five zero-SLOC spec files.
        let mut tally: NameTally = BTreeMap::new();
        for _ in 0..5 {
            vote(&mut tally, 0, SmolStr::new("spec/adapters"), 0);
        }
        vote(&mut tally, 0, SmolStr::new("src/adapters"), 60);
        vote(&mut tally, 0, SmolStr::new("src/adapters"), 60);
        let elected = tally.get(&0).map(plurality).unwrap_or_default();
        assert_eq!(elected, "src/adapters");

        // an all-test cluster has zero SLOC everywhere and degrades to the
        // old file-count plurality.
        let mut tests_only: NameTally = BTreeMap::new();
        for _ in 0..3 {
            vote(&mut tests_only, 0, SmolStr::new("spec/agent"), 0);
        }
        vote(&mut tests_only, 0, SmolStr::new("spec/batch"), 0);
        let fallback = tests_only.get(&0).map(plurality).unwrap_or_default();
        assert_eq!(fallback, "spec/agent");
    }

    #[test]
    fn should_build_candidates_with_real_names_and_five_levels() {
        // two files under a real directory path, each holding a multi-line
        // production symbol, plus a hard edge so clustering has a pair to group.
        let sized = |id: u32, name: &str, container: u32, size: u32| Node {
            id: NodeId(id),
            name: SmolStr::new(name),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(container),
            visibility: ScopeLevel::File,
            effective_size: size,
        };
        let snapshot = snapshot(
            vec![sized(0, "alpha", 4, 10), sized(1, "beta", 5, 7)],
            vec![edge(0, 1)],
            vec![
                container(0, "strata", ScopeLevel::PackageGroup, None),
                container(1, "crates", ScopeLevel::Package, Some(0)),
                container(2, "crates/engine", ScopeLevel::Domain, Some(1)),
                container(3, "crates/engine/src", ScopeLevel::Folder, Some(2)),
                container(4, "crates/engine/src/alpha.rs", ScopeLevel::File, Some(3)),
                container(5, "crates/engine/src/beta.rs", ScopeLevel::File, Some(3)),
            ],
        );

        let candidate = analyze(&snapshot, &config_with_k(1))
            .ok()
            .and_then(|result| result.modes.anchored)
            .and_then(|mode| mode.candidates.into_iter().next());

        let mut names = Vec::new();
        let mut sloc = Vec::new();
        let mut levels = Vec::new();
        if let Some(candidate) = &candidate {
            collect_tree(&candidate.tree, &mut names, &mut sloc, &mut levels);
        }
        assert!(!names.is_empty(), "expected an anchored candidate");

        // names are real directory-derived, never synthetic cluster labels.
        assert!(
            names.iter().all(|name| !name.contains("cluster-")),
            "expected real names, got {names:?}"
        );
        // interior names render incrementally: the domain "crates/engine" adds
        // "engine" over the package "crates", the folder adds "src"; files keep
        // their full path as their stable identity.
        assert!(names.iter().any(|name| name == "crates"));
        assert!(names.iter().any(|name| name == "engine"));
        assert!(names.iter().any(|name| name == "src"));
        assert!(
            names
                .iter()
                .any(|name| name == "crates/engine/src/alpha.rs")
        );

        // the full five-level laminar hierarchy is present.
        for expected in [
            Level::PackageGroup,
            Level::Package,
            Level::Domain,
            Level::Folder,
            Level::File,
        ] {
            assert!(
                levels.contains(&expected),
                "missing {expected:?} in {levels:?}"
            );
        }

        // production SLOC sums effective_size (10, 7), not the symbol count (1).
        assert!(
            sloc.contains(&10) && sloc.contains(&7),
            "expected summed effective_size, got {sloc:?}"
        );
    }

    #[test]
    fn should_be_deterministic_across_runs() {
        let make = || {
            let nodes = (0..6)
                .map(|i| node(i, &format!("sym{i}"), i, Polarity::Production))
                .collect::<Vec<_>>();
            let containers = (0..6)
                .map(|i| container(i, &format!("file{i}"), ScopeLevel::File, None))
                .collect::<Vec<_>>();
            snapshot(nodes, vec![edge(0, 1), edge(2, 3)], containers)
        };

        let scores = |snapshot: &Snapshot| {
            analyze(snapshot, &config_with_k(3))
                .ok()
                .and_then(|result| result.modes.anchored)
                .map(|mode| mode.candidates.iter().map(|c| c.score).collect::<Vec<_>>())
                .unwrap_or_default()
        };

        assert_eq!(scores(&make()), scores(&make()));
    }

    /// Builds a production symbol node with an explicit effective size.
    fn sized_node(id: u32, name: &str, container: u32, size: u32) -> Node {
        Node {
            effective_size: size,
            ..node(id, name, container, Polarity::Production)
        }
    }

    #[test]
    fn should_solve_a_cycle_with_priced_weights_and_a_minimal_break_set() {
        // a -> b -> c -> a; the c -> a edge is inheritance (1.5), the rest
        // calls (1.0), so the optimal break is a 1.0 call edge.
        let snapshot = snapshot(
            vec![
                node(0, "a", 0, Polarity::Production),
                node(1, "b", 1, Polarity::Production),
                node(2, "c", 2, Polarity::Production),
            ],
            vec![
                edge(0, 1),
                edge(1, 2),
                Edge {
                    kind: EdgeKind::Inheritance,
                    ..edge(2, 0)
                },
            ],
            vec![
                container(0, "a.ts", ScopeLevel::File, None),
                container(1, "b.ts", ScopeLevel::File, None),
                container(2, "c.ts", ScopeLevel::File, None),
            ],
        );

        let solutions = solve_cycles(
            &snapshot,
            &AnalyzeConfig::default(),
            &KindWeights::default(),
        );

        assert_eq!(solutions.len(), 1);
        let solution = solutions.first();
        assert_eq!(
            solution.map(|s| s.members.clone()),
            Some(vec![NodeId(0), NodeId(1), NodeId(2)])
        );
        let weight_of = |pair: (u32, u32)| {
            solution
                .and_then(|s| s.pair_weights.get(&pair))
                .copied()
                .unwrap_or(0.0)
        };
        assert!((weight_of((0, 1)) - 1.0).abs() < f64::EPSILON);
        assert!((weight_of((1, 2)) - 1.0).abs() < f64::EPSILON);
        assert!((weight_of((2, 0)) - 1.5).abs() < f64::EPSILON);
        assert_eq!(solution.map(|s| s.break_set.exact), Some(true));
        assert_eq!(solution.map(|s| s.break_set.edges.len()), Some(1));
        // the pricier inheritance edge must survive.
        assert!(solution.is_some_and(|s| {
            s.break_set
                .edges
                .iter()
                .all(|e| !(e.source == 2 && e.target == 0))
        }));
        assert_eq!(solution.map(|s| s.production_sloc), Some(3));
    }

    #[test]
    fn should_solve_no_cycles_on_an_acyclic_graph() {
        let snapshot = snapshot(
            vec![
                node(0, "a", 0, Polarity::Production),
                node(1, "b", 1, Polarity::Production),
            ],
            vec![edge(0, 1)],
            vec![
                container(0, "a.ts", ScopeLevel::File, None),
                container(1, "b.ts", ScopeLevel::File, None),
            ],
        );

        let solutions = solve_cycles(
            &snapshot,
            &AnalyzeConfig::default(),
            &KindWeights::default(),
        );

        assert!(solutions.is_empty());
    }

    /// Folder clusters of the identity partition, keyed by file container id.
    fn identity_clusters(snapshot: &Snapshot, file_containers: &[u32]) -> Vec<Option<ClusterId>> {
        let solver = PipelineSolver::new(
            snapshot,
            &AnalyzeConfig::default(),
            Coefficients::anchored(),
            true,
        );
        file_containers
            .iter()
            .map(|container| {
                let vertex = solver.index_of.get(container).copied()?;
                let scc = solver.condensation.membership.get(vertex as usize)?;
                solver
                    .identity
                    .as_ref()
                    .and_then(|identity| identity.cluster_of(scc.0))
            })
            .collect()
    }

    #[test]
    fn should_map_singleton_file_sccs_onto_their_current_folders() {
        // two folders, two files each; identity must reproduce the folders.
        let snapshot = snapshot(
            vec![
                node(0, "a", 2, Polarity::Production),
                node(1, "b", 3, Polarity::Production),
                node(2, "c", 4, Polarity::Production),
                node(3, "d", 5, Polarity::Production),
            ],
            vec![],
            vec![
                container(0, "left", ScopeLevel::Folder, None),
                container(1, "right", ScopeLevel::Folder, None),
                container(2, "left/a.ts", ScopeLevel::File, Some(0)),
                container(3, "left/b.ts", ScopeLevel::File, Some(0)),
                container(4, "right/c.ts", ScopeLevel::File, Some(1)),
                container(5, "right/d.ts", ScopeLevel::File, Some(1)),
            ],
        );

        let clusters = identity_clusters(&snapshot, &[2, 3, 4, 5]);

        assert!(clusters.iter().all(Option::is_some));
        assert_eq!(clusters.first(), clusters.get(1));
        assert_eq!(clusters.get(2), clusters.get(3));
        assert_ne!(clusters.first(), clusters.get(2));
    }

    #[test]
    fn should_place_a_cross_folder_cycle_by_its_dominant_file() {
        // the symbol cycle 0 <-> 1 fuses files 2 (left, sloc 5) and 4 (right,
        // sloc 1) into one file SCC; it must land in left's cluster, beside
        // left resident file 3.
        let snapshot = snapshot(
            vec![
                sized_node(0, "a", 2, 5),
                sized_node(1, "b", 4, 1),
                sized_node(2, "c", 3, 1),
                sized_node(3, "d", 5, 1),
            ],
            vec![edge(0, 1), edge(1, 0)],
            vec![
                container(0, "left", ScopeLevel::Folder, None),
                container(1, "right", ScopeLevel::Folder, None),
                container(2, "left/a.ts", ScopeLevel::File, Some(0)),
                container(3, "left/c.ts", ScopeLevel::File, Some(0)),
                container(4, "right/b.ts", ScopeLevel::File, Some(1)),
                container(5, "right/d.ts", ScopeLevel::File, Some(1)),
            ],
        );

        let clusters = identity_clusters(&snapshot, &[2, 4, 3, 5]);

        assert!(clusters.iter().all(Option::is_some));
        assert_eq!(
            clusters.first(),
            clusters.get(1),
            "the fused SCC is one cluster"
        );
        assert_eq!(
            clusters.first(),
            clusters.get(2),
            "cycle follows dominant left file"
        );
        assert_ne!(clusters.first(), clusters.get(3));
    }

    #[test]
    fn should_break_a_dominance_tie_by_the_smaller_file_name() {
        // equal SLOC on both sides of the cycle; "left/a.ts" < "right/b.ts",
        // so the fused file SCC lands in left, beside left resident file 3.
        let snapshot = snapshot(
            vec![
                sized_node(0, "a", 2, 1),
                sized_node(1, "b", 4, 1),
                sized_node(2, "c", 3, 1),
            ],
            vec![edge(0, 1), edge(1, 0)],
            vec![
                container(0, "left", ScopeLevel::Folder, None),
                container(1, "right", ScopeLevel::Folder, None),
                container(2, "left/a.ts", ScopeLevel::File, Some(0)),
                container(3, "left/c.ts", ScopeLevel::File, Some(0)),
                container(4, "right/b.ts", ScopeLevel::File, Some(1)),
            ],
        );

        let clusters = identity_clusters(&snapshot, &[2, 3]);

        assert!(clusters.iter().all(Option::is_some));
        assert_eq!(clusters.first(), clusters.get(1));
    }
}
