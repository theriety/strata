//! The pure analysis pass: snapshot plus config in, [`AnalyzeResult`] out.
//!
//! [`analyze`] performs no I/O and holds no global state, so an identical
//! snapshot, config, and seed always produce an identical result (AD-5). It runs
//! the full restructuring pipeline — SCC condensation, the real-directory folder
//! partition, objective-driven polish, upper-level acyclic clustering, and
//! scoring — to return up to `k` candidate layouts per requested mode, each
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
    AnalyzeResult, Candidate, CapacityBreach, CapacityRemainder, ConditionalSplit, ContainerNode,
    CurrentStanding, CurrentTree, EdgeBreak, Level, ModeResult, Modes, RESULT_SCHEMA_VERSION,
    ScoreBreakdown, Severity, Summary, SymbolPlacement, Violation, ViolationKind,
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

    let current_node = render_tree(
        tree,
        &ir.nodes,
        &|node| Some(node.container),
        &BTreeMap::new(),
    )?;
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
                capacity: None,
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
                capacity: None,
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
                capacity: None,
            }
        })
        .collect()
}

/// Derives capacity violations from the current tree against the configured caps.
///
/// A file over its production-SLOC cap and an interior container over its
/// member-count cap each yield a finding; a finding within ±10% of its cap is
/// `borderline` and never gates. Each container is checked against the cap of
/// its own level. A folder is measured by the files it holds directly (its real
/// directory membership), so the interior nodes of a rendered directory chain —
/// which hold only subdirectories — never spawn findings; domains and above are
/// measured by direct child count, which counts each nested directory chain
/// once at its top-level root.
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
        Level::Folder => (file_child_count(node), config.capacity.folder),
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

/// Returns the number of file children a folder holds directly.
///
/// A rendered directory chain interleaves interior folder nodes that own only
/// subdirectories; measuring the files at each directory keeps capacity
/// findings anchored to real membership instead of chain length.
fn file_child_count(node: &ContainerNode) -> u32 {
    let files = node.children.as_ref().map_or(0, |children| {
        children
            .iter()
            .filter(|child| child.level == Level::File)
            .count()
    });
    u32::try_from(files).unwrap_or(u32::MAX)
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
        // for files the container name IS the full repo-relative path; folder
        // and higher paths are already carried by `location`.
        capacity: Some(CapacityBreach {
            measured: measure,
            cap,
            path: (node.level == Level::File).then(|| node.name.clone()),
        }),
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
        // an infeasible standing must say what each candidate actually fixes,
        // so its tree is re-checked against the same caps as the current one.
        candidate.capacity_remainder =
            (!capacity_clean).then(|| capacity_remainder(&candidate.tree, config));
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

    let pairwise_distance = pairwise_distances(&candidates);
    Ok(ModeResult {
        candidates: built,
        pairwise_distance,
        solution_space_converged,
        current_score: current_breakdown.total,
        current_score_breakdown: current_breakdown.into(),
        current_standing,
    })
}

/// Counts the hard capacity findings left in a candidate's tree: the total and
/// how many are file-level breaches, which no move can fix — only conditional
/// splits can.
fn capacity_remainder(tree: &ContainerNode, config: &AnalyzeConfig) -> CapacityRemainder {
    let hard: Vec<Level> = walk_all_capacity(tree, config)
        .into_iter()
        .filter(|(_, finding)| finding.severity == Severity::Violation)
        .map(|(level, _)| level)
        .collect();
    let file_level = hard.iter().filter(|&&level| level == Level::File).count();
    CapacityRemainder {
        remaining: u32::try_from(hard.len()).unwrap_or(u32::MAX),
        file_level: u32::try_from(file_level).unwrap_or(u32::MAX),
    }
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
    /// The file's folder/domain/package name keys, read from the laminar
    /// container tree so naming honors the manifest package roots and
    /// transparent source roots (`src`/`spec`) that tree already resolved,
    /// rather than re-electing from raw leading path segments.
    home: LaminarHome,
}

/// The laminar container tree's already-resolved folder, domain, and package
/// name keys for one file — full-prefix keys (`ai/adapters`, `ai`) with any
/// transparent source-root segment stripped and the package resolved to its
/// nearest manifest root.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LaminarHome {
    /// The file's folder-level container name key.
    folder: SmolStr,
    /// The file's domain-level container name key.
    domain: SmolStr,
    /// The file's package-level container name key.
    package: SmolStr,
    /// True when the folder-level container is the synthetic `workspace` bucket
    /// (a root-level file with no real directory of its own). Functionally
    /// determined by `folder`, so it never splits two otherwise-equal homes into
    /// distinct clusters; it carries the current tree's collapse marker across to
    /// the candidate folder so a greenfield render drops the bucket too.
    synthetic: bool,
}

/// Reads the laminar folder/domain/package name keys for the file container
/// `file_container` by walking its ancestor chain in `by_id`. The laminar tree
/// (`build_laminar_tree`) already stripped a transparent leading source root and
/// resolved the nearest package root, so reusing its names keeps candidate
/// naming consistent with the current tree instead of re-deriving from raw paths
/// (which would surface `src` as a package/folder). Folders and package roots
/// nest, so the NEAREST ancestor at each level wins — deeper keys are the real
/// place. A level missing from the chain inherits the nearest broader key (a
/// file directly in its domain directory has that directory as its real
/// folder), and only a chain with no package at all falls back to the laminar
/// synthetic `workspace` bucket.
fn laminar_home(by_id: &BTreeMap<u32, &Container>, file_container: u32) -> LaminarHome {
    let (mut folder, mut domain, mut package) = (None, None, None);
    let mut synthetic = false;
    let mut current = by_id.get(&file_container).copied();
    while let Some(container) = current {
        match container.level {
            ScopeLevel::Folder if folder.is_none() => {
                folder = Some(container.name.clone());
                synthetic = container.synthetic;
            }
            ScopeLevel::Domain if domain.is_none() => domain = Some(container.name.clone()),
            ScopeLevel::Package if package.is_none() => package = Some(container.name.clone()),
            _ => {}
        }
        current = container
            .parent
            .and_then(|parent| by_id.get(&parent.0).copied());
    }
    let package = package.unwrap_or_else(|| SmolStr::new("workspace"));
    let domain = domain.unwrap_or_else(|| package.clone());
    let folder = folder.unwrap_or_else(|| domain.clone());
    LaminarHome {
        folder,
        domain,
        package,
        synthetic,
    }
}

/// Bound on polish sweeps: two passes catch the follow-up moves the first pass
/// unlocks without ballooning the wall clock.
const POLISH_SWEEPS: usize = 2;

/// Candidate destination folders examined per move unit during polish.
const POLISH_TARGETS: usize = 4;

/// The restartable solver that runs the candidate pipeline once per seed.
///
/// All of the seed-independent work — the weighted file-dependency graph, its
/// SCC condensation, and the real-directory folder partition — is computed once
/// at construction; each [`Solver::solve`] call starts from that real partition
/// (folders are reality, not a clustering product), polishes it under the full
/// objective, and assembles the five-level layout, so every seed yields a pure,
/// reproducible candidate. In anchored mode the pool additionally carries the
/// identity layout ("change nothing"), so a suggested restructuring can never
/// silently score worse than the current tree.
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
    /// The real-directory folder partition: every file SCC in the cluster of
    /// its current parent folder. Folders are reality, so this is the one
    /// folder-grain start every seed shares.
    real_partition: Partition,
    /// Each real folder cluster's directory name, indexed by cluster id.
    real_folder_names: Vec<SmolStr>,
    /// Whether each real folder cluster is the synthetic `workspace` bucket,
    /// indexed by cluster id in lockstep with `real_folder_names`. Carries the
    /// current tree's collapse marker onto the candidate folder it induces.
    real_folder_synthetic: Vec<bool>,
    /// The configured base seed; seed offset 0 selects the identity entry.
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

        // index the laminar containers by id so each file can read its already-
        // resolved folder/domain/package name keys off its ancestor chain.
        let by_id: BTreeMap<u32, &Container> = ir
            .containers
            .containers()
            .iter()
            .map(|container| (container.id.0, container))
            .collect();
        let mut files: Vec<FileInfo> = ir
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| FileInfo {
                container: container.id.0,
                name: container.name.clone(),
                production_sloc: 0,
                home: laminar_home(&by_id, container.id.0),
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
        let caps = level_caps(config);
        let reverse_dag = reverse_csr(&condensation.dag);

        // folders are reality: the identity layout and the search's folder
        // partition are the same object — each file SCC in its real directory
        // — so anchored seeding just clones it.
        let (real_partition, real_folder_names, real_folder_synthetic) =
            real_dir_partition(&files, &condensation);
        let identity = seed_identity.then(|| real_partition.clone());
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
            reverse_dag,
            caps,
            coefficients,
            weights,
            identity,
            real_partition,
            real_folder_names,
            real_folder_synthetic,
            base_seed: config.analysis.seed,
            root_name,
            facts,
        }
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
    /// folder) and quotient cyclicity stay hard vetoes, never penalties — but
    /// the cyclicity veto is relative, not absolute: a move is barred when it
    /// *grows* the number of folders caught in quotient cycles, never for
    /// cyclicity the current layout already has. Misplaced files routinely
    /// entangle real folder graphs in cycles no single move can dissolve; an
    /// absolute veto would price every move at infinity on such a base and
    /// freeze the pass wholesale. On an acyclic base the two vetoes agree.
    /// At most [`POLISH_SWEEPS`] passes, stopping early once a sweep applies
    /// no move. Returns the final score so `solve` never re-evaluates.
    fn polish(&self, parts: &mut Partition) -> f64 {
        let mut best = self.evaluate(parts);
        let mut cyclic_base = cyclic_vertex_count(&parts.quotient(&self.condensation.dag));
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
                    let cyclic_now = cyclic_vertex_count(&parts.quotient(&self.condensation.dag));
                    let total = if cyclic_now <= cyclic_base {
                        self.evaluate(parts)
                    } else {
                        f64::INFINITY
                    };
                    if total < best {
                        best = total;
                        cyclic_base = cyclic_now;
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

    /// Dominant laminar home key (via `key`, weighted by production SLOC) per
    /// base vertex of an upper clustering level.
    ///
    /// `cluster_of` maps a folder cluster to the base vertex it contributes to at
    /// this level — the identity for the domain level (each folder is a vertex),
    /// the domain partition for the package level (each domain is a vertex). An
    /// absent (empty) vertex falls back to `workspace`. The result seeds and gains
    /// [`cluster_level`] so containers group by home directory.
    fn home_keys(
        &self,
        members_of: &BTreeMap<u32, Vec<u32>>,
        vertex_count: usize,
        cluster_of: impl Fn(u32) -> u32,
        key: impl Fn(&FileInfo) -> &SmolStr,
    ) -> Vec<SmolStr> {
        let mut tally: NameTally = BTreeMap::new();
        for (&folder, members) in members_of {
            let cluster = cluster_of(folder);
            for &vertex in members {
                if let Some(file) = self.files.get(vertex as usize) {
                    vote(&mut tally, cluster, key(file).clone(), file.production_sloc);
                }
            }
        }
        (0..vertex_count)
            .map(|vertex| {
                tally
                    .get(&u32::try_from(vertex).unwrap_or(u32::MAX))
                    .map_or_else(|| SmolStr::new("workspace"), plurality)
            })
            .collect()
    }

    /// Assembles the five-level candidate tree a folder partition induces.
    ///
    /// Folders are the partition's non-empty clusters and keep their real
    /// directory names ([`real_dir_partition`]) — folders are reality, so no
    /// election happens at that level. The upper levels come from clustering
    /// each level's weighted quotient in turn (folders → domains → packages →
    /// package groups) and are named from the files they transitively hold —
    /// each file votes its laminar domain and package name keys
    /// (source-root-transparent, package-root-resolved), weighted by production
    /// SLOC then file count, and [`elect`] names the cluster through its
    /// never-mixed, never-numeric ladder — strict-majority home, shared home
    /// prefix, top-two join, dominant token, then an anchored non-numeric last
    /// resort; the group takes the current root's name — and file leaves keep
    /// their full current paths so file identity stays stable across trees.
    fn assemble(&self, parts: &Partition) -> CandidateTree {
        if self.files.is_empty() {
            let root = Container {
                id: ContainerId(0),
                name: self.root_name.clone(),
                level: ScopeLevel::PackageGroup,
                parent: None,
                synthetic: false,
            };
            return CandidateTree {
                tree: ContainerTree::new(vec![root]),
                placement: BTreeMap::new(),
                key_by_id: BTreeMap::new(),
            };
        }

        let members_of = self.folder_members(parts);

        // one clustering pass per upper level, each over the previous level's
        // weighted quotient graph. Each pass carries a home-directory seed
        // affinity keyed by the dominant laminar home of the containers below it,
        // so folders group by home directory into named domains instead of pooling
        // by index order into a cut-minimal grab-bag no single home could honestly
        // name. The package-group level has no home key, so it keeps the neutral
        // descending-layer order.
        let folder_quotient = parts.quotient(&self.condensation.dag);
        let domain_homes = self.home_keys(
            &members_of,
            folder_quotient.vertex_count(),
            |folder| folder,
            |file| &file.home.domain,
        );
        let domain_parts = cluster_level(
            &folder_quotient,
            &self.caps,
            SeedLevel::Domain,
            &home_affinity(&domain_homes),
        );
        let domain_quotient = domain_parts.quotient(&folder_quotient);
        let package_homes = self.home_keys(
            &members_of,
            domain_quotient.vertex_count(),
            |folder| {
                domain_parts
                    .cluster_of(folder)
                    .map_or(0, |cluster| cluster.0)
            },
            |file| &file.home.package,
        );
        let package_parts = cluster_level(
            &domain_quotient,
            &self.caps,
            SeedLevel::Package,
            &home_affinity(&package_homes),
        );
        let package_quotient = package_parts.quotient(&domain_quotient);
        let group_parts =
            cluster_level(&package_quotient, &self.caps, SeedLevel::PackageGroup, &[]);

        // ancestry of every non-empty folder cluster, plus the directory tallies
        // each level's containers are named from.
        let mut chain_of: BTreeMap<u32, (u32, u32, u32)> = BTreeMap::new();
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
                let sloc = file.production_sloc;
                // vote with the laminar tree's resolved name keys, not raw path
                // prefixes, so source roots stay transparent and the package
                // resolves to its manifest root (never a bare `src`).
                vote(&mut domain_tally, domain, file.home.domain.clone(), sloc);
                vote(&mut package_tally, package, file.home.package.clone(), sloc);
            }
        }

        self.emit(&members_of, &chain_of, &domain_tally, &package_tally)
    }

    /// Interns the candidate containers parent-before-child — package groups,
    /// packages, domains, then each folder with its files — and records every
    /// symbol's file placement.
    fn emit(
        &self,
        members_of: &BTreeMap<u32, Vec<u32>>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        package_tally: &NameTally,
    ) -> CandidateTree {
        let mut arena = ContainerArena::default();
        let mut key_by_id: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let domain_ids = self.intern_upper_levels(
            &mut arena,
            &mut key_by_id,
            chain_of,
            domain_tally,
            package_tally,
        );

        let mut file_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&folder, members) in members_of {
            let Some(&(domain, _, _)) = chain_of.get(&folder) else {
                continue;
            };
            // folders are reality: the cluster keeps its full real key — one
            // container per distinct real location with an injective name
            // (`qualify_folder_names`), so sibling folders never collide and no
            // synthetic `-N` twin can arise. A key that doesn't path-extend its
            // elected domain renders whole, which is the honest display of a
            // foreign real directory folded into the suggested domain.
            let key = self
                .real_folder_names
                .get(folder as usize)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let parent = domain_ids.get(&domain).copied();
            let folder_id = arena.push(ContainerSpec {
                name: &key,
                level: ScopeLevel::Folder,
                parent,
                synthetic: self
                    .real_folder_synthetic
                    .get(folder as usize)
                    .copied()
                    .unwrap_or(false),
            });
            for &vertex in members {
                let Some(file) = self.files.get(vertex as usize) else {
                    continue;
                };
                let id = arena.push(ContainerSpec {
                    name: &file.name,
                    level: ScopeLevel::File,
                    parent: Some(folder_id),
                    synthetic: false,
                });
                file_ids.insert(vertex, id);
            }
        }

        CandidateTree {
            tree: ContainerTree::new(arena.containers),
            placement: self.placements(&file_ids),
            key_by_id,
        }
    }

    /// Interns the upper naming ladder — package groups over packages over
    /// domains — into `arena`, parent before child, and returns each domain
    /// cluster's [`ContainerId`] so [`emit`](Self::emit) can hang folders and
    /// files beneath it.
    ///
    /// Every level's sibling names are elected through the never-mixed,
    /// never-numeric [`elect`] ladder and then made injective by
    /// [`qualify_elected`], disambiguating a shared elected name with the
    /// cluster's lexicographically smallest real folder — its *anchor* — the
    /// same real-location qualifier folder twins use, so no reachable elected
    /// path ever falls back to the arena's numeric backstop.
    fn intern_upper_levels(
        &self,
        arena: &mut ContainerArena,
        key_by_id: &mut BTreeMap<u32, SmolStr>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        package_tally: &NameTally,
    ) -> BTreeMap<u32, ContainerId> {
        // folders are reality: each upper cluster anchors on the smallest real
        // directory name it holds, so two siblings that elect one name split
        // apart by their true locations rather than a synthetic `-N` twin.
        let mut group_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let mut package_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let mut domain_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        for (&folder, &(domain, package, group)) in chain_of {
            let name = self
                .real_folder_names
                .get(folder as usize)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            anchor_min(&mut group_anchor, group, &name);
            anchor_min(&mut package_anchor, package, &name);
            anchor_min(&mut domain_anchor, domain, &name);
        }

        let groups: BTreeSet<u32> = chain_of.values().map(|&(_, _, group)| group).collect();
        let group_raw: BTreeMap<u32, (u32, SmolStr)> = groups
            .iter()
            .map(|&group| (group, (0, self.root_name.clone())))
            .collect();
        let group_names = qualify_elected(&group_raw, &group_anchor);
        let mut group_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&group, name) in &group_names {
            let id = arena.push(ContainerSpec {
                name,
                level: ScopeLevel::PackageGroup,
                parent: None,
                synthetic: false,
            });
            if let Some((_, raw)) = group_raw.get(&group) {
                record_undecorated_key(key_by_id, id, raw, name);
            }
            group_ids.insert(group, id);
        }

        let packages: BTreeMap<u32, u32> = chain_of
            .values()
            .map(|&(_, package, group)| (package, group))
            .collect();
        let package_raw: BTreeMap<u32, (u32, SmolStr)> = packages
            .iter()
            .map(|(&package, &group)| {
                // the fallback is unreachable: `packages` and `package_anchor`
                // are both built from `chain_of`, so every key holds an anchor.
                let anchor = package_anchor
                    .get(&package)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                let name = package_tally
                    .get(&package)
                    .map_or_else(|| SmolStr::new("workspace"), |tally| elect(tally, &anchor));
                (package, (group, name))
            })
            .collect();
        let package_names = qualify_elected(&package_raw, &package_anchor);
        let mut package_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&package, &group) in &packages {
            let name = package_names
                .get(&package)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let id = arena.push(ContainerSpec {
                name: &name,
                level: ScopeLevel::Package,
                parent: group_ids.get(&group).copied(),
                synthetic: false,
            });
            if let Some((_, raw)) = package_raw.get(&package) {
                record_undecorated_key(key_by_id, id, raw, &name);
            }
            package_ids.insert(package, id);
        }

        Self::intern_domains(
            arena,
            key_by_id,
            chain_of,
            domain_tally,
            &domain_anchor,
            &package_ids,
        )
    }

    /// Interns the domain level beneath already-interned packages.
    ///
    /// Each domain elects its name through the [`elect`] ladder, disambiguates
    /// with its cluster anchor via [`qualify_elected`], and hangs off its parent
    /// package. The elected key lands verbatim — a name foreign to its parent
    /// renders whole, the honest display of a suggested grouping that spans real
    /// locations (folders set the precedent). Returns each cluster's domain id.
    fn intern_domains(
        arena: &mut ContainerArena,
        key_by_id: &mut BTreeMap<u32, SmolStr>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        domain_anchor: &BTreeMap<u32, SmolStr>,
        package_ids: &BTreeMap<u32, ContainerId>,
    ) -> BTreeMap<u32, ContainerId> {
        let domains: BTreeMap<u32, u32> = chain_of
            .values()
            .map(|&(domain, package, _)| (domain, package))
            .collect();
        let domain_raw: BTreeMap<u32, (u32, SmolStr)> = domains
            .iter()
            .map(|(&domain, &package)| {
                // the fallback is unreachable: `domains` and `domain_anchor`
                // are both built from `chain_of`, so every key holds an anchor.
                let anchor = domain_anchor
                    .get(&domain)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                let name = domain_tally
                    .get(&domain)
                    .map_or_else(|| SmolStr::new("workspace"), |tally| elect(tally, &anchor));
                (domain, (package, name))
            })
            .collect();
        let domain_names = qualify_elected(&domain_raw, domain_anchor);
        let mut domain_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&domain, &package) in &domains {
            let name = domain_names
                .get(&domain)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let id = arena.push(ContainerSpec {
                name: &name,
                level: ScopeLevel::Domain,
                parent: package_ids.get(&package).copied(),
                synthetic: false,
            });
            if let Some((_, raw)) = domain_raw.get(&domain) {
                record_undecorated_key(key_by_id, id, raw, &name);
            }
            domain_ids.insert(domain, id);
        }

        domain_ids
    }

    /// Maps every symbol node to the candidate file container holding it, by the
    /// file vertex the node's current container indexes to.
    fn placements(&self, file_ids: &BTreeMap<u32, ContainerId>) -> BTreeMap<u32, ContainerId> {
        let mut placement = BTreeMap::new();
        for node in &self.snapshot.ir().nodes {
            let Some(&vertex) = self.index_of.get(&node.container.0) else {
                continue;
            };
            if let Some(&file_id) = file_ids.get(&vertex) {
                placement.insert(node.id.0, file_id);
            }
        }
        placement
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
            let node = render_tree(
                current_tree,
                nodes,
                &|node: &Node| Some(node.container),
                &BTreeMap::new(),
            )?;
            return Ok(Candidate {
                index,
                score: breakdown.total,
                score_breakdown: ScoreBreakdown::from(breakdown),
                improvement: 0.0,
                tree: node,
                conditional_splits: splits.to_vec(),
                delta_narration: Vec::new(),
                capacity_remainder: None,
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
        let node = render_tree(&assembled.tree, nodes, &placement_of, &assembled.key_by_id)?;
        let delta = narrate(current_tree, &assembled.tree, &self.facts);

        Ok(Candidate {
            index,
            score: breakdown.total,
            score_breakdown: ScoreBreakdown::from(breakdown),
            improvement: 0.0,
            tree: node,
            conditional_splits: splits.to_vec(),
            delta_narration: delta,
            capacity_remainder: None,
        })
    }
}

impl Solver for PipelineSolver<'_> {
    fn solve(&self, seed: u64) -> SolvedCandidate {
        let offset = seed.wrapping_sub(self.base_seed);
        if offset == 0
            && let Some(identity) = &self.identity
        {
            return self.identity_entry(identity);
        }
        // lean: every non-identity seed converges on the same polished layout —
        // folders are reality, so the folder-level seed perturbation that used
        // to differentiate restarts is gone and the pool collapses toward
        // identity plus one improvement candidate. Re-sourcing diversity at the
        // domain grain (the level that is still a clustered suggestion) is the
        // upgrade path; this slice deliberately does not paper over the
        // collapse.
        let mut parts = self.real_partition.clone();
        let total = self.polish(&mut parts);
        self.finish(parts, total)
    }
}

/// A reconstructed candidate tree plus the placement of every symbol node.
struct CandidateTree {
    /// The candidate container tree: package groups over packages, domains, and
    /// folders derived per level, each folder holding whole current files.
    tree: ContainerTree,
    /// The file container each symbol node lands in, keyed by node id.
    placement: BTreeMap<u32, ContainerId>,
    /// The undecorated elected key of each upper container whose display name
    /// `qualify_elected` had to disambiguate, keyed by container id — empty when
    /// no sibling name collided. The render boundary strips a folder's increment
    /// against its domain's key from this map, never the decorated display
    /// label, so a disambiguated domain never re-embeds a folder's key as a
    /// fabricated directory chain.
    key_by_id: BTreeMap<u32, SmolStr>,
}

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
        // admit every edge at its configured price — not Hard edges alone — so
        // the search optimizes the same cut the score reports and soft-only
        // files (e.g. type-reference-only TS) get a non-empty move-set. A zero-
        // priced kind still seeds mobility without shifting the cut.
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

/// Lifts per-SCC folder keys onto the coarsest chain level, so the seed can pack
/// same-folder files together. Each base SCC is followed up the chain through
/// every level's `fine_to_coarse` map to the top vertex it folds into, and each
/// top vertex takes the folder key held by the most SCCs beneath it (ties to the
/// smaller key). Merged super-nodes are connected by construction, so their
/// internal folder mixing matters little; the payoff is the edgeless singletons,
/// which arrive at the top one-to-one with their true home folder.
fn fold_affinity_to_top(chain: &[CoarseGraph], scc_keys: &[u32]) -> Vec<u32> {
    let Some(top) = chain.last() else {
        return Vec::new();
    };
    let mut tallies: Vec<BTreeMap<u32, u32>> = vec![BTreeMap::new(); top.graph.vertex_count()];
    for (scc, &key) in scc_keys.iter().enumerate() {
        let mut vertex = u32::try_from(scc).unwrap_or(u32::MAX);
        for level in chain.iter().skip(1) {
            vertex = level
                .fine_to_coarse
                .get(vertex as usize)
                .copied()
                .unwrap_or(vertex);
        }
        if let Some(tally) = tallies.get_mut(vertex as usize) {
            *tally.entry(key).or_insert(0) += 1;
        }
    }
    tallies
        .iter()
        .map(|tally| {
            tally
                .iter()
                .max_by(|left, right| left.1.cmp(right.1).then(right.0.cmp(left.0)))
                .map_or(u32::MAX, |(key, _)| *key)
        })
        .collect()
}

/// Builds the real-directory folder partition: each file SCC lands in the
/// cluster of its dominant member's laminar home — the file with the largest
/// production SLOC, ties to the lexicographically smaller path (an SCC
/// spanning folders must co-cluster anyway, so it stays with its heaviest
/// member). Folders come from reality, not from clustering, so this partition
/// doubles as the identity layout. Clusters key on the full laminar location
/// (folder, domain, package), never the folder name alone: real directories
/// are already unique by their full-depth keys, and the full location keeps
/// same-named fallback buckets (`workspace`) of different packages apart.
/// Cluster ids are dense over the distinct locations in ascending order, and
/// the returned names carry each cluster's real directory key so emission
/// never re-elects folder names.
fn real_dir_partition(
    files: &[FileInfo],
    condensation: &Condensation,
) -> (Partition, Vec<SmolStr>, Vec<bool>) {
    let fallback = || LaminarHome {
        folder: SmolStr::new("workspace"),
        domain: SmolStr::new("workspace"),
        package: SmolStr::new("workspace"),
        synthetic: false,
    };
    let keys: Vec<LaminarHome> = condensation
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
            dominant.map_or_else(fallback, |file| file.home.clone())
        })
        .collect();
    let distinct: BTreeSet<LaminarHome> = keys.iter().cloned().collect();
    let cluster_of_key: BTreeMap<LaminarHome, u32> = distinct
        .iter()
        .enumerate()
        .map(|(index, key)| (key.clone(), u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    let assignment = keys
        .iter()
        .map(|key| ClusterId(cluster_of_key.get(key).copied().unwrap_or(0)))
        .collect();
    let names = qualify_folder_names(&distinct);
    // the synthetic marker rides in lockstep with `names`: both map the distinct
    // homes in the same iteration order, so cluster `i` names and marks the same
    // real location. It is folder-determined, so it never perturbs the clustering.
    let synthetic: Vec<bool> = distinct.iter().map(|home| home.synthetic).collect();
    (
        Partition::from_assignment(assignment, names.len()),
        names,
        synthetic,
    )
}

/// Names each distinct real location by its folder key, qualifying key ties by
/// real location so distinct places never share a name: a folder key unique
/// among the distinct locations stays bare; a key shared across packages
/// qualifies as `{folder} ({package})`; a key shared within one package
/// qualifies as `{folder} ({package} {domain})`. Qualifiers dot their path
/// separators so display folding never splits a qualifier into path segments.
/// Distinct locations always differ in some coordinate, so the tiered names
/// are injective for every laminar-derived snapshot; only a hand-built folder
/// name that textually embeds another location's qualifier can still collide,
/// which the arena's numeric backstop absorbs.
fn qualify_folder_names(distinct: &BTreeSet<LaminarHome>) -> Vec<SmolStr> {
    let mut folder_count: BTreeMap<&SmolStr, u32> = BTreeMap::new();
    let mut pair_count: BTreeMap<(&SmolStr, &SmolStr), u32> = BTreeMap::new();
    for home in distinct {
        *folder_count.entry(&home.folder).or_default() += 1;
        *pair_count.entry((&home.folder, &home.package)).or_default() += 1;
    }
    let dotted = |key: &SmolStr| key.replace('/', ".");
    distinct
        .iter()
        .map(|home| {
            let folder_ties = folder_count.get(&home.folder).copied().unwrap_or(0);
            let pair_ties = pair_count
                .get(&(&home.folder, &home.package))
                .copied()
                .unwrap_or(0);
            if folder_ties == 1 {
                home.folder.clone()
            } else if pair_ties == 1 {
                SmolStr::new(format!("{} ({})", home.folder, dotted(&home.package)))
            } else {
                SmolStr::new(format!(
                    "{} ({} {})",
                    home.folder,
                    dotted(&home.package),
                    dotted(&home.domain)
                ))
            }
        })
        .collect()
}

/// Clusters a weighted quotient graph one level up (folders → domains, domains
/// → packages, …) with the same multilevel scheme the base level uses, minus
/// seed perturbation (the level is fully determined by the partition below it,
/// keeping assembly a pure function of the folder partition).
///
/// `affinity` keys each base vertex (a below-level container) by its dominant
/// home directory: the seed keeps same-home containers contiguous, so cap
/// boundaries fall *between* home directories and each domain/package comes out
/// home-coherent (a named `adapters`, `agent`, …) instead of an index-order
/// grab-bag that [`elect`] could name only through its lower rungs — a shared
/// prefix or a top-two join rather than one honest home. An empty slice — as
/// for the package-group level, which has no home key — restores the prior
/// neutral descending-layer order.
///
/// lean: refinement stays [`GainFn::cut_only`]. A cohesion gain here would not be
/// score-aligned — the objective's naming term scores only file/folder symbol
/// groups (`score`'s `cohesion_groups`), never domains — so it would bias the
/// search off the objective while, being far smaller than any integer cut gain,
/// never actually holding a folder that pure cut wants to move. The seed affinity
/// is the whole fix; a genuine cross-home coupling (a strictly-positive cut gain)
/// is still honoured, and the cross-home composite [`elect`] then joins for it
/// stays honest — named after its real origins, never a synthetic label.
fn cluster_level(graph: &Csr, caps: &LevelCaps, level: SeedLevel, affinity: &[u32]) -> Partition {
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
    // lift the per-base-vertex home affinity onto the coarsest level so the seed
    // keeps same-home containers contiguous (empty affinity folds to the neutral
    // descending-layer order).
    let top_affinity = fold_affinity_to_top(&chain, affinity);
    let mut parts = seed(top, &top.layers, caps, level, &top_affinity);
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

/// Interns candidate containers with dense ids, keeping the sibling names taken
/// under each `(parent, level)` scope as a last-resort collision guard.
#[derive(Default)]
struct ContainerArena {
    /// The containers interned so far, indexed by their dense id.
    containers: Vec<Container>,
    /// The sibling names already taken under each `(parent, level)` scope.
    used: BTreeMap<(Option<u32>, ScopeLevel), BTreeSet<SmolStr>>,
}

impl ContainerArena {
    /// Interns `spec` under the next dense id and returns it.
    ///
    /// Every reachable naming path is injective by construction — folder names
    /// through `qualify_folder_names`, elected group/package/domain names
    /// through `qualify_elected` — so the numeric suffix below is a backstop
    /// for names that textually embed another sibling's qualifier: a real
    /// directory literally named like `http (ai)` colliding with a qualified
    /// twin (non-adversarial), or a hand-built snapshot reusing a qualified
    /// shape outright (adversarial). Neither arises from real elected paths.
    fn push(&mut self, spec: ContainerSpec<'_>) -> ContainerId {
        let ContainerSpec {
            name,
            level,
            parent,
            synthetic,
        } = spec;
        let siblings = self
            .used
            .entry((parent.map(|parent| parent.0), level))
            .or_default();
        let mut unique = name.clone();
        let mut suffix = 2_u32;
        while siblings.contains(&unique) {
            unique = SmolStr::new(format!("{name}-{suffix}"));
            suffix = suffix.saturating_add(1);
        }
        siblings.insert(unique.clone());
        let id = ContainerId(u32::try_from(self.containers.len()).unwrap_or(u32::MAX));
        self.containers.push(Container {
            id,
            name: unique,
            level,
            parent,
            synthetic,
        });
        id
    }
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

/// True when a name is fit to serve as an elected identity: non-empty, not
/// all-digit at every `/`-segment (`2024`, `2024/2025`), and not tailed by a
/// `-<digits>` marker (`report-2`) — the shapes reserved for real directory
/// names and the arena's collision backstop, which a *suggested* container
/// name must never imitate.
fn fit_for_election(name: &str) -> bool {
    let numeric = name
        .split('/')
        .all(|segment| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()));
    let suffixed = name
        .rsplit_once('-')
        .is_some_and(|(_, tail)| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()));
    !name.is_empty() && !numeric && !suffixed
}

/// Returns the plurality home when it holds a *strict* majority of the
/// cluster's weight and is fit to elect. Weight is production SLOC (as
/// `plurality` ranks), degrading to file count only for an all-test,
/// zero-SLOC cluster — a production home keeps its name despite companion
/// specs. Cross-multiplication keeps the test integer-exact, and demanding
/// `2 × win > total` sends a tied pair to the ladder's lower rungs instead
/// of crowning one side.
fn strict_majority(tally: &BTreeMap<SmolStr, (u64, u32)>) -> Option<SmolStr> {
    let winner = plurality(tally);
    let (win_sloc, win_count) = tally.get(&winner).copied()?;
    let total_sloc = tally
        .values()
        .fold(0_u64, |total, &(sloc, _)| total.saturating_add(sloc));
    let majority = if total_sloc > 0 {
        win_sloc.saturating_mul(2) > total_sloc
    } else {
        let total_count = tally.values().fold(0_u64, |total, &(_, count)| {
            total.saturating_add(u64::from(count))
        });
        u64::from(win_count).saturating_mul(2) > total_count
    };
    (majority && fit_for_election(&winner)).then_some(winner)
}

/// Returns the longest `/`-segment prefix shared by every key in `tally` —
/// empty when the keys already diverge at their first segment.
fn shared_prefix(tally: &BTreeMap<SmolStr, (u64, u32)>) -> SmolStr {
    let mut keys = tally.keys();
    let Some(first) = keys.next() else {
        return SmolStr::new("");
    };
    let mut prefix: Vec<&str> = first.split('/').collect();
    for key in keys {
        let shared = prefix
            .iter()
            .zip(key.split('/'))
            .take_while(|(held, segment)| **held == *segment)
            .count();
        prefix.truncate(shared);
    }
    SmolStr::new(prefix.join("/"))
}

/// Joins the cluster's two heaviest homes with `/` — ranked by production
/// SLOC then file count, the same vote order every other rung uses, ties by
/// key order — when at least two homes exist and the composite is fit to
/// elect. A balanced grab-bag with no shared prefix is honestly named after
/// both of its real origins, and a test-only spec dump outnumbering the
/// production homes in files can never lead the joined name.
///
/// Package-qualified homes share their leading segments; the second home is
/// relativized against the first before joining, so `ai/adapters` +
/// `ai/model` composes `ai/adapters/model`. A joined name is synthetic by
/// design — a proposed container not existing yet is the point — but it must
/// stay coherent: `ai/adapters/ai/model` re-embeds the shared root mid-path,
/// a nesting no human would ever write, and bakes in the very segment
/// repetition the display fold exists to prevent. This rung fires only when a
/// divergent third home has already emptied the all-member [`shared_prefix`]
/// consensus, so the pair's common ancestor alone would misclaim the cluster;
/// naming both heavy homes stays the more honest cover. When one home is the
/// other's ancestor, though, that ancestor covers both and elects alone.
fn join_top_two(tally: &BTreeMap<SmolStr, (u64, u32)>) -> Option<SmolStr> {
    let mut ranked: Vec<(&SmolStr, (u64, u32))> =
        tally.iter().map(|(key, &weight)| (key, weight)).collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    let [(first, _), (second, _), ..] = ranked.as_slice() else {
        return None;
    };
    let shared = first
        .split('/')
        .zip(second.split('/'))
        .take_while(|(left, right)| left == right)
        .count();
    let remainder = second.split('/').skip(shared).collect::<Vec<_>>().join("/");
    let joined = if remainder.is_empty() {
        // `second` is an ancestor of `first`: the ancestor covers both homes.
        (*second).clone()
    } else if shared == first.split('/').count() {
        // `first` is an ancestor of `second`: same cover, other direction.
        (*first).clone()
    } else {
        SmolStr::new(format!("{first}/{remainder}"))
    };
    fit_for_election(&joined).then_some(joined)
}

/// Returns the heaviest non-numeric path token across the cluster's home
/// keys — weight accumulated as (production SLOC, file count), ties by the
/// lexicographically smaller token — skipping tokens unfit to elect. Rescues
/// a name when whole keys are numeric (`2024/2025`) but a real word survives
/// inside them.
fn dominant_token(tally: &BTreeMap<SmolStr, (u64, u32)>) -> Option<SmolStr> {
    let mut tokens: BTreeMap<&str, (u64, u32)> = BTreeMap::new();
    for (key, &(sloc, count)) in tally {
        for segment in key.split('/') {
            if segment.is_empty() || segment.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let (token_sloc, token_count) = tokens.entry(segment).or_default();
            *token_sloc = token_sloc.saturating_add(sloc);
            *token_count = token_count.saturating_add(count);
        }
    }
    let mut ranked: Vec<(&str, (u64, u32))> = tokens.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    ranked
        .into_iter()
        .map(|(token, _)| token)
        .find(|token| fit_for_election(token))
        .map(SmolStr::new)
}

/// Elects a cluster's directory name through a deterministic ladder that is
/// structurally incapable of yielding a synthetic label or a bare number —
/// the first fit rung wins:
///
/// 1. the strict-majority home ([`strict_majority`]);
/// 2. the longest home prefix every member shares ([`shared_prefix`]);
/// 3. the two heaviest homes joined ([`join_top_two`]);
/// 4. the dominant non-numeric path token ([`dominant_token`]);
/// 5. the first home key — wrapped with the cluster's dot-encoded `anchor`
///    folder when the key alone is unfit (`2024 (2024.x)`), so even an
///    all-numeric grab-bag renders as a real, non-numeric identity.
fn elect(tally: &BTreeMap<SmolStr, (u64, u32)>, anchor: &SmolStr) -> SmolStr {
    if let Some(winner) = strict_majority(tally) {
        return winner;
    }
    let prefix = shared_prefix(tally);
    if !prefix.is_empty() && fit_for_election(&prefix) {
        return prefix;
    }
    if let Some(joined) = join_top_two(tally) {
        return joined;
    }
    if let Some(token) = dominant_token(tally) {
        return token;
    }
    let first = tally
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| SmolStr::new("workspace"));
    if fit_for_election(&first) {
        return first;
    }
    SmolStr::new(format!("{first} ({})", anchor.replace('/', ".")))
}

/// Keeps the lexicographically smallest anchor folder name seen per cluster.
fn anchor_min(anchors: &mut BTreeMap<u32, SmolStr>, cluster: u32, name: &SmolStr) {
    anchors
        .entry(cluster)
        .and_modify(|held| {
            if *name < *held {
                *held = name.clone();
            }
        })
        .or_insert_with(|| name.clone());
}

/// Qualifies elected sibling names into injective ones: a raw name shared by
/// two clusters under one parent gains each cluster's dot-encoded anchor
/// folder — `app (pa.app.x)` — the same real-location style folder twins use,
/// so no reachable elected path ever needs the arena's numeric backstop.
/// Records `id`'s undecorated elected key when `qualify_elected` decorated its
/// display `name`, so the render boundary can strip a folder's increment against
/// the real key rather than the anchor-decorated label. An undecorated name (the
/// common, no-collision case) already matches the folder-key prefix, so it is
/// left out — keeping the map empty and the render byte-identical to before.
fn record_undecorated_key(
    key_by_id: &mut BTreeMap<u32, SmolStr>,
    id: ContainerId,
    raw: &SmolStr,
    display: &SmolStr,
) {
    if raw != display {
        key_by_id.insert(id.0, raw.clone());
    }
}

fn qualify_elected(
    raw: &BTreeMap<u32, (u32, SmolStr)>,
    anchors: &BTreeMap<u32, SmolStr>,
) -> BTreeMap<u32, SmolStr> {
    let mut sibling_count: BTreeMap<(u32, &SmolStr), u32> = BTreeMap::new();
    for (parent, name) in raw.values() {
        *sibling_count.entry((*parent, name)).or_default() += 1;
    }
    raw.iter()
        .map(|(&cluster, (parent, name))| {
            let colliding = sibling_count.get(&(*parent, name)).copied().unwrap_or(0) > 1;
            let name = if colliding {
                let anchor = anchors
                    .get(&cluster)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                SmolStr::new(format!("{name} ({})", anchor.replace('/', ".")))
            } else {
                name.clone()
            };
            (cluster, name)
        })
        .collect()
}

/// Interns each base vertex's dominant home directory `homes[v]` into a seed
/// affinity ordinal, so `seed` keeps same-home containers contiguous and cap
/// boundaries fall between home directories. Equal keys share an ordinal; the
/// order the distinct keys are numbered is irrelevant (affinity only groups).
fn home_affinity(homes: &[SmolStr]) -> Vec<u32> {
    let mut key_ids: BTreeMap<SmolStr, u32> = BTreeMap::new();
    homes
        .iter()
        .map(|home| {
            let next = u32::try_from(key_ids.len()).unwrap_or(u32::MAX);
            *key_ids.entry(home.clone()).or_insert(next)
        })
        .collect()
}

/// The description of one container to intern: the name key it carries, the
/// level it sits at, and its parent.
#[derive(Clone, Copy)]
struct ContainerSpec<'name> {
    /// The name key the container's cluster carries.
    name: &'name SmolStr,
    /// The level the container sits at.
    level: ScopeLevel,
    /// The container's parent, or `None` at the root.
    parent: Option<ContainerId>,
    /// True for the synthetic `workspace` folder bucket the render collapses.
    /// Only a root-file folder is ever synthetic; every upper level is false.
    synthetic: bool,
}

/// Counts the vertices of `graph` sitting inside a cyclic strongly connected
/// component — the quotient's cyclicity mass the polish veto compares before
/// and after a move. Zero exactly when the graph is a DAG (the quotient
/// carries no self-loops, so every singleton component is acyclic).
fn cyclic_vertex_count(graph: &Csr) -> usize {
    condense(graph)
        .members
        .iter()
        .filter(|members| members.len() > 1)
        .map(Vec::len)
        .sum()
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
/// cohesion is the production-SLOC-weighted fraction of files placed under the
/// same folder key their own directory already resolves to in the snapshot's
/// laminar tree — an unchanged layout scores 1.0 and every relocation dilutes
/// it.
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

    // the real folder key each file currently lives under, read from the
    // snapshot's own laminar tree; both trees key a file container by its raw
    // repo path, so the path joins a placed file back to its real directory.
    let real_folder_of = folder_key_of_files(&ir.containers);

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

        let placed = name_of.get(&parent.0).map_or("", |name| name.as_str());
        let real = real_folder_of
            .get(container.name.as_str())
            .copied()
            .unwrap_or("");
        total = total.saturating_add(u64::from(sloc));
        if !real.is_empty() && placed == real {
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
/// owning file changes real folder.
///
/// A file's location is exactly its folder key — the path it would be moved to —
/// so the comparison reads folder keys on both sides and never composes a
/// root-to-leaf path. Labels above the folder are display, not location:
/// renaming a domain relocates nothing and must not register here.
fn move_distance(snapshot: &Snapshot, candidate: &ContainerTree) -> f64 {
    let ir = snapshot.ir();
    let total = ir.nodes.len();
    if total == 0 {
        return 0.0;
    }
    let current_folder = folder_key_of_files(&ir.containers);
    let candidate_folder = folder_key_of_files(candidate);

    // match candidate files by the original file's path, which the assembly
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
            let Some(name) = current_name.get(&node.container.0) else {
                return false;
            };
            let Some(current) = current_folder.get(name.as_str()) else {
                return false;
            };
            // a file the candidate drops entirely has left its folder.
            candidate_folder
                .get(name.as_str())
                .is_none_or(|placed| placed != current)
        })
        .count();

    f64::from(u32::try_from(moved).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total).unwrap_or(u32::MAX))
}

/// Maps each file container's path key to the folder key holding it directly.
///
/// This is the one primitive both location-sensitive terms read: a file's real
/// place is the folder key it sits under, so β (does the file still sit where it
/// already lives?) and μ (did the file leave?) ask the same question of the same
/// key space. Comparing keys rather than a raw-path prefix is what keeps
/// source-root transparency intact — one folder legitimately merges `src/x` with
/// `spec/x`, so it has no single raw directory to compare against.
fn folder_key_of_files(tree: &ContainerTree) -> BTreeMap<&str, &str> {
    let name_of: BTreeMap<u32, &SmolStr> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();
    tree.containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .filter_map(|container| {
            let folder = name_of.get(&container.parent?.0)?;
            Some((container.name.as_str(), folder.as_str()))
        })
        .collect()
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
    key_by_id: &BTreeMap<u32, SmolStr>,
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
        [root] => Ok(render_node(
            root,
            &children_by_parent,
            &contents,
            "",
            "",
            key_by_id,
        )),
        many => Ok(ContainerNode {
            name: "workspace".to_owned(),
            level: Level::PackageGroup,
            children: Some(
                many.iter()
                    .map(|root| {
                        render_node(root, &children_by_parent, &contents, "", "", key_by_id)
                    })
                    .collect(),
            ),
            symbols: None,
            production_sloc: None,
        }),
    }
}

/// Renders the container nodes `container` contributes to its parent's child
/// list, collapsing the synthetic `workspace` bucket at the render boundary.
///
/// A synthetic bucket names no real directory — it exists only so the internal
/// tree stays strictly level-ascending over a root-level file — so it
/// contributes no node of its own: its children rise to sit directly under the
/// nearest real ancestor (a package's root files become siblings of its real
/// folders). The internal tree keeps the bucket; only the DTO drops it. Every
/// other container contributes itself.
fn render_contributions(
    container: &Container,
    children_by_parent: &BTreeMap<u32, Vec<&Container>>,
    contents: &BTreeMap<u32, FileContents>,
    parent_name: &str,
    parent_key: &str,
    key_by_id: &BTreeMap<u32, SmolStr>,
) -> Vec<ContainerNode> {
    if container.synthetic {
        return children_by_parent
            .get(&container.id.0)
            .map(|children| {
                children
                    .iter()
                    .flat_map(|child| {
                        render_contributions(
                            child,
                            children_by_parent,
                            contents,
                            parent_name,
                            parent_key,
                            key_by_id,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    vec![render_node(
        container,
        children_by_parent,
        contents,
        parent_name,
        parent_key,
        key_by_id,
    )]
}

/// Recursively renders one container and its descendants.
///
/// Interior container names are *cumulative* path prefixes internally; the DTO
/// carries only each node's increment over its parent so a rendered tree never
/// repeats segments. Files keep their full path (their stable identity) and a
/// root keeps its own name. A folder's multi-segment increment is a real
/// relative directory path, so it expands into one nested folder node per
/// segment ([`nest_folder_segments`]); slash-named domains, packages, and
/// groups are elected labels and render whole.
///
/// A folder's increment strips its parent's *key* (`parent_key`), not the
/// parent's rendered display name: a domain whose display label
/// `qualify_elected` decorated (`core (constellation-ts.core)`) is no longer a
/// prefix of the folder key, so stripping the label would leave the whole key to
/// re-embed as a fabricated directory chain. `key_by_id` supplies the
/// undecorated key of any decorated ancestor; every other container keys on its
/// own name, so the two coincide and the render is unchanged.
fn render_node(
    container: &Container,
    children_by_parent: &BTreeMap<u32, Vec<&Container>>,
    contents: &BTreeMap<u32, FileContents>,
    parent_name: &str,
    parent_key: &str,
    key_by_id: &BTreeMap<u32, SmolStr>,
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

    let own_key = key_by_id
        .get(&container.id.0)
        .map_or(container.name.as_str(), SmolStr::as_str);
    let children = children_by_parent
        .get(&container.id.0)
        .map(|children| {
            let rendered = children
                .iter()
                .flat_map(|child| {
                    render_contributions(
                        child,
                        children_by_parent,
                        contents,
                        &container.name,
                        own_key,
                        key_by_id,
                    )
                })
                .collect();
            merge_sibling_folders(rendered)
        })
        .unwrap_or_default();

    let increment = increment_name(
        &container.name,
        if container.level == ScopeLevel::Folder {
            parent_key
        } else {
            parent_name
        },
    );
    if container.level == ScopeLevel::Folder {
        return nest_folder_segments(&increment, children);
    }

    ContainerNode {
        name: increment,
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

/// Expands a folder's parent-relative directory path into a nested chain of
/// folder nodes, one per path segment, with `children` under the deepest.
///
/// Folders are reality: a multi-segment folder increment such as `a/b/c`
/// denotes real nested directories, so the DTO renders the chain
/// `a` → `b` → `c` rather than one slash-named node. Only the rendered
/// boundary nests folders under folders — the internal tree keeps one
/// slash-keyed folder per real directory as its stable identity.
fn nest_folder_segments(increment: &str, children: Vec<ContainerNode>) -> ContainerNode {
    let mut segments = increment
        .split('/')
        .filter(|segment| !segment.is_empty())
        .rev();
    let mut node = ContainerNode {
        name: segments.next().unwrap_or(increment).to_owned(),
        level: Level::Folder,
        children: Some(children),
        symbols: None,
        production_sloc: None,
    };
    for segment in segments {
        node = ContainerNode {
            name: segment.to_owned(),
            level: Level::Folder,
            children: Some(vec![node]),
            symbols: None,
            production_sloc: None,
        };
    }
    node
}

/// Merges sibling folder nodes sharing a name into one directory trie.
///
/// Trie expansion can surface the same real parent directory from several
/// internal folder keys (`deep/x` and `deep/y` both render a `deep` node);
/// duplicate siblings would misstate the directory tree, so equal-named folder
/// siblings merge recursively, keeping first-seen order. Non-folder siblings
/// pass through untouched.
fn merge_sibling_folders(children: Vec<ContainerNode>) -> Vec<ContainerNode> {
    let mut merged: Vec<ContainerNode> = Vec::new();
    for child in children {
        if child.level == Level::Folder
            && let Some(existing) = merged
                .iter_mut()
                .find(|node| node.level == Level::Folder && node.name == child.name)
        {
            let mut combined: Vec<ContainerNode> =
                existing.children.take().into_iter().flatten().collect();
            combined.extend(child.children.into_iter().flatten());
            existing.children = Some(merge_sibling_folders(combined));
            continue;
        }
        merged.push(child);
    }
    merged
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
        ContainerId, Edge, EdgeKind, Hardness, IntermediateRepresentation, Layout, NodeId,
        NodeKind, build_laminar_tree,
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
            synthetic: false,
        }
    }

    /// Builds a synthetic `workspace`-bucket container (domain or folder) — the
    /// empty-scope node the render collapses.
    fn synthetic_container(
        id: u32,
        name: &str,
        level: ScopeLevel,
        parent: Option<u32>,
    ) -> Container {
        Container {
            synthetic: true,
            ..container(id, name, level, parent)
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
        interior(name, Level::Folder, children)
    }

    /// Builds an interior node at `level` holding `children`.
    fn interior(name: &str, level: Level, children: Vec<ContainerNode>) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level,
            children: Some(children),
            symbols: None,
            production_sloc: None,
        }
    }

    /// Returns the node's only child, or `None` when it has zero or several.
    fn only_child(node: ContainerNode) -> Option<ContainerNode> {
        node.children.and_then(|children| {
            if children.len() == 1 {
                children.into_iter().next()
            } else {
                None
            }
        })
    }

    #[test]
    fn should_collapse_the_synthetic_workspace_bucket_under_the_package() {
        // a root-level file hangs off a synthetic `workspace` domain+folder
        // bucket so the internal tree stays strictly level-ascending. The bucket
        // names no real directory, so the DTO collapses it: the file renders
        // directly under its package, with no `workspace` domain or folder node.
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "crates/app", ScopeLevel::Package, Some(0)),
            synthetic_container(2, "crates/app/workspace", ScopeLevel::Domain, Some(1)),
            synthetic_container(3, "crates/app/workspace", ScopeLevel::Folder, Some(2)),
            container(4, "crates/app/src/lib.rs", ScopeLevel::File, Some(3)),
        ]);

        let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

        let package = rendered.and_then(only_child);
        let children = package.and_then(|node| node.children).unwrap_or_default();
        let shape: Vec<(&str, Level)> = children
            .iter()
            .map(|node| (node.name.as_str(), node.level))
            .collect();
        assert_eq!(shape, vec![("crates/app/src/lib.rs", Level::File)]);
    }

    #[test]
    fn should_render_root_files_beside_real_folders_when_a_package_has_both() {
        // a package holding both root-level files and a real sub-folder renders
        // the root files as siblings of the real folder directly under the
        // package -- the collapsed synthetic bucket never wraps them in a
        // phantom `workspace` level.
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "crates/app", ScopeLevel::Package, Some(0)),
            synthetic_container(2, "crates/app/workspace", ScopeLevel::Domain, Some(1)),
            synthetic_container(3, "crates/app/workspace", ScopeLevel::Folder, Some(2)),
            container(4, "crates/app/src/lib.rs", ScopeLevel::File, Some(3)),
            container(5, "crates/app/io", ScopeLevel::Domain, Some(1)),
            container(6, "crates/app/io", ScopeLevel::Folder, Some(5)),
            container(7, "crates/app/src/io/read.rs", ScopeLevel::File, Some(6)),
        ]);

        let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

        let package = rendered.and_then(only_child);
        let children = package.and_then(|node| node.children).unwrap_or_default();
        let names: Vec<&str> = children.iter().map(|node| node.name.as_str()).collect();
        assert!(
            names.contains(&"crates/app/src/lib.rs"),
            "the root file must sit directly under the package, got {names:?}"
        );
        assert!(
            children.iter().any(|node| node.level == Level::Domain),
            "the real folder's domain must remain, got {names:?}"
        );
        assert!(
            !names.contains(&"workspace"),
            "no synthetic workspace node may survive, got {names:?}"
        );
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
    fn should_nest_a_multi_segment_folder_into_a_directory_chain() {
        // folders are reality: a real directory foreign to its domain renders
        // as one nested folder node per path segment, deepest holding the
        // files, never as a single slash-named node.
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ws", ScopeLevel::Package, Some(0)),
            container(2, "shared", ScopeLevel::Domain, Some(1)),
            container(3, "google/capabilities", ScopeLevel::Folder, Some(2)),
            container(4, "src/google/capabilities/a.ts", ScopeLevel::File, Some(3)),
        ]);

        let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

        let google = rendered
            .and_then(only_child)
            .and_then(only_child)
            .and_then(only_child);
        assert_eq!(
            google.as_ref().map(|node| (node.name.as_str(), node.level)),
            Some(("google", Level::Folder))
        );
        let capabilities = google.and_then(only_child);
        assert_eq!(
            capabilities
                .as_ref()
                .map(|node| (node.name.as_str(), node.level)),
            Some(("capabilities", Level::Folder))
        );
        let files: Vec<String> = capabilities
            .and_then(|node| node.children)
            .unwrap_or_default()
            .into_iter()
            .map(|child| child.name)
            .collect();
        assert_eq!(files, vec!["src/google/capabilities/a.ts".to_owned()]);
    }

    #[test]
    fn should_render_a_slash_named_domain_as_a_single_node() {
        // domains are the suggestion: an elected join like `d1/d2` is a label,
        // not a directory, so it renders whole and never trie-splits.
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ws", ScopeLevel::Package, Some(0)),
            container(2, "d1/d2", ScopeLevel::Domain, Some(1)),
            container(3, "d1/d2/x", ScopeLevel::Folder, Some(2)),
            container(4, "src/d1/x/a.ts", ScopeLevel::File, Some(3)),
        ]);

        let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

        let domain = rendered.and_then(only_child).and_then(only_child);
        assert_eq!(
            domain.as_ref().map(|node| (node.name.as_str(), node.level)),
            Some(("d1/d2", Level::Domain))
        );
        let folder_names: Vec<String> = domain
            .and_then(|node| node.children)
            .unwrap_or_default()
            .into_iter()
            .map(|child| child.name)
            .collect();
        assert_eq!(folder_names, vec!["x".to_owned()]);
    }

    #[test]
    fn should_not_fabricate_folders_under_a_disambiguated_domain() {
        // two merged domains both elect the bare package prefix `cts`, so
        // `qualify_elected` disambiguates their display as `cts (cts.core)` /
        // `cts (cts.io)`. The decorated label is not a prefix of the folder key
        // `cts/core`, so stripping it would leave the whole key to re-embed as a
        // fabricated `cts` → `core` chain. Stripping the domain's real key `cts`
        // (from `key_by_id`) renders the one real directory `core`.
        let tree = ContainerTree::new(vec![
            container(0, "cts", ScopeLevel::PackageGroup, None),
            container(1, "cts", ScopeLevel::Package, Some(0)),
            container(2, "cts (cts.core)", ScopeLevel::Domain, Some(1)),
            container(3, "cts/core", ScopeLevel::Folder, Some(2)),
            container(4, "src/core/a.ts", ScopeLevel::File, Some(3)),
        ]);
        let key_by_id: BTreeMap<u32, SmolStr> = [(2, SmolStr::new("cts"))].into_iter().collect();

        let rendered = render_tree(&tree, &[], &|_| None, &key_by_id).ok();

        let domain = rendered.and_then(only_child).and_then(only_child);
        assert_eq!(
            domain.as_ref().map(|node| (node.name.as_str(), node.level)),
            Some(("cts (cts.core)", Level::Domain))
        );
        let folder_names: Vec<String> = domain
            .and_then(|node| node.children)
            .unwrap_or_default()
            .into_iter()
            .map(|child| child.name)
            .collect();
        assert_eq!(folder_names, vec!["core".to_owned()]);
    }

    #[test]
    fn should_merge_sibling_folders_sharing_a_parent_directory() {
        // two real directories under one parent (`deep/x`, `deep/y`) render
        // as a single `deep` trie holding two subdirectories, never as
        // duplicate `deep` siblings.
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ws", ScopeLevel::Package, Some(0)),
            container(2, "shared", ScopeLevel::Domain, Some(1)),
            container(3, "deep/x", ScopeLevel::Folder, Some(2)),
            container(4, "src/deep/x/a.ts", ScopeLevel::File, Some(3)),
            container(5, "deep/y", ScopeLevel::Folder, Some(2)),
            container(6, "src/deep/y/b.ts", ScopeLevel::File, Some(5)),
        ]);

        let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

        let deep = rendered
            .and_then(only_child)
            .and_then(only_child)
            .and_then(only_child);
        assert_eq!(deep.as_ref().map(|node| node.name.as_str()), Some("deep"));
        let subdirs: Vec<(String, Vec<String>)> = deep
            .and_then(|node| node.children)
            .unwrap_or_default()
            .into_iter()
            .map(|child| {
                let files = child
                    .children
                    .iter()
                    .flatten()
                    .map(|file| file.name.clone())
                    .collect();
                (child.name, files)
            })
            .collect();
        assert_eq!(
            subdirs,
            vec![
                ("x".to_owned(), vec!["src/deep/x/a.ts".to_owned()]),
                ("y".to_owned(), vec!["src/deep/y/b.ts".to_owned()]),
            ]
        );
    }

    /// Builds a production symbol of `sloc` owning `container`.
    fn sloc_node(id: u32, name: &str, container: ContainerId, sloc: u32) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(name),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container,
            visibility: ScopeLevel::File,
            effective_size: sloc,
        }
    }

    #[test]
    fn should_measure_full_path_cohesion_for_the_unchanged_laminar_layout() {
        // `cohesion_inputs` documents the unchanged layout as scoring ~1.0: every
        // file still sits in the folder its own directory names, so nothing is
        // relocated and nothing dilutes the fraction. Build the tree the way the
        // real analysis does -- via `build_laminar_tree`, over a nested directory
        // -- and score the identity placement, where each symbol stays in the file
        // container the tree already put it in.
        let paths: Vec<SmolStr> = vec![
            SmolStr::new("src/core/engine/a.ts"),
            SmolStr::new("src/core/engine/b.ts"),
        ];
        let layout = Layout {
            package_roots: vec![],
            source_roots: vec![SmolStr::new("src")],
        };
        let built = build_laminar_tree(&paths, "workspace", &layout);

        let nodes: Vec<Node> = paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| {
                let container = *built.files.get(path)?;
                // reason: two fixture files index well inside u32.
                #[allow(clippy::cast_possible_truncation)]
                let id = index as u32;
                Some(sloc_node(id, path.as_str(), container, 10))
            })
            .collect();
        let container_of: BTreeMap<u32, ContainerId> = nodes
            .iter()
            .map(|node| (node.id.0, node.container))
            .collect();
        let snapshot = snapshot(nodes, vec![], built.tree.containers().to_vec());
        let tree = snapshot.ir().containers.clone();

        let (_, path_cohesion) =
            cohesion_inputs(&snapshot, &|id| container_of.get(&id).copied(), &tree);

        assert!(
            (path_cohesion - 1.0).abs() < f64::EPSILON,
            "the unchanged layout must be fully path-cohesive, measured {path_cohesion}"
        );
    }

    #[test]
    fn should_not_count_a_file_as_moved_when_only_a_display_name_above_its_folder_changes() {
        // a file's real location is its folder key: `src/core/engine/a.ts` lives
        // in `workspace/core/engine` no matter what label the domain above it
        // wears. Naming a domain is a display act, never a path component, so a
        // relabelled ancestor must not register as a relocation.
        let paths: Vec<SmolStr> = vec![SmolStr::new("src/core/engine/a.ts")];
        let layout = Layout {
            package_roots: vec![],
            source_roots: vec![SmolStr::new("src")],
        };
        let built = build_laminar_tree(&paths, "workspace", &layout);
        // `build_laminar_tree` interns every input path, so the lookup always
        // hits; the fallback is unreachable and only keeps the strict lint clean.
        let file_id = built
            .files
            .get(&SmolStr::new("src/core/engine/a.ts"))
            .copied()
            .unwrap_or(ContainerId(0));
        let containers = built.tree.containers().to_vec();
        let snapshot = snapshot(
            vec![sloc_node(0, "src/core/engine/a.ts", file_id, 10)],
            vec![],
            containers.clone(),
        );

        // the same tree, with only the domain container's display label decorated.
        let relabelled: Vec<Container> = containers
            .iter()
            .cloned()
            .map(|mut container| {
                if container.level == ScopeLevel::Domain {
                    container.name = SmolStr::new(format!("{} (workspace.core)", container.name));
                }
                container
            })
            .collect();

        let distance = move_distance(&snapshot, &ContainerTree::new(relabelled));

        assert!(
            distance.abs() < f64::EPSILON,
            "relabelling a domain moves no file, measured {distance}"
        );
    }

    #[test]
    fn should_measure_folder_capacity_by_direct_file_membership() {
        // synthetic interior directories hold only subdirectories and stay
        // silent; each directory is measured by the files it holds directly.
        let tree = interior(
            "shared",
            Level::Domain,
            vec![
                folder(
                    "a",
                    vec![folder(
                        "b",
                        vec![folder("c", vec![file("f1", 10), file("f2", 10)])],
                    )],
                ),
                folder("x", vec![folder("y", vec![file("g1", 10)])]),
            ],
        );
        let mut config = AnalyzeConfig::default();
        config.capacity.folder = 1;

        let findings = walk_all_capacity(&tree, &config);

        let folder_findings: Vec<(Vec<String>, Severity)> = findings
            .iter()
            .filter(|(level, _)| *level == Level::Folder)
            .map(|(_, violation)| (violation.location.clone(), violation.severity))
            .collect();
        assert_eq!(
            folder_findings,
            vec![
                (
                    vec![
                        "shared".to_owned(),
                        "a".to_owned(),
                        "b".to_owned(),
                        "c".to_owned(),
                    ],
                    Severity::Violation
                ),
                (
                    vec!["shared".to_owned(), "x".to_owned(), "y".to_owned()],
                    Severity::Borderline
                ),
            ]
        );
    }

    #[test]
    fn should_count_domain_capacity_by_top_level_directory_subtrees() {
        // a domain holding the single real directory `a/b/c` measures one
        // child subtree, not one per nested segment.
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ws", ScopeLevel::Package, Some(0)),
            container(2, "shared", ScopeLevel::Domain, Some(1)),
            container(3, "a/b/c", ScopeLevel::Folder, Some(2)),
            container(4, "src/a/b/c/f.ts", ScopeLevel::File, Some(3)),
        ]);
        let mut config = AnalyzeConfig::default();
        config.capacity.domain = 1;

        let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();
        let findings = rendered
            .map(|dto| walk_all_capacity(&dto, &config))
            .unwrap_or_default();

        let domain_findings: Vec<Severity> = findings
            .iter()
            .filter(|(level, _)| *level == Level::Domain)
            .map(|(_, violation)| violation.severity)
            .collect();
        assert_eq!(domain_findings, vec![Severity::Borderline]);
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
    fn should_populate_capacity_remainder_on_every_candidate_when_infeasible() {
        let make = || {
            snapshot(
                vec![
                    node(0, "a", 0, Polarity::Production),
                    node(1, "b", 1, Polarity::Production),
                ],
                vec![edge(0, 1)],
                vec![
                    container(0, "src/a.ts", ScopeLevel::File, Some(2)),
                    container(1, "src/b.ts", ScopeLevel::File, Some(2)),
                    container(2, "src", ScopeLevel::Folder, None),
                ],
            )
        };

        // a zero file cap makes every file a hard breach: infeasible standing,
        // so every candidate must carry its own remainder.
        let mut infeasible_config = config_with_k(2);
        infeasible_config.capacity.file = 0;
        let infeasible = analyze(&make(), &infeasible_config)
            .ok()
            .and_then(|result| result.modes.anchored);
        let candidates = infeasible
            .as_ref()
            .map_or(&[] as &[_], |mode| &mode.candidates);
        assert!(!candidates.is_empty());
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.capacity_remainder.is_some())
        );

        // a clean tree carries no remainder on any candidate.
        let clean = analyze(&make(), &config_with_k(2))
            .ok()
            .and_then(|result| result.modes.anchored);
        let clean_candidates = clean.as_ref().map_or(&[] as &[_], |mode| &mode.candidates);
        assert!(!clean_candidates.is_empty());
        assert!(
            clean_candidates
                .iter()
                .all(|candidate| candidate.capacity_remainder.is_none())
        );
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
            capacity: None,
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
    fn should_elect_real_home_names_never_a_neutral_label() {
        let anchor = SmolStr::new("src/core");

        // a coherent cluster: one origin holds the SLOC majority, so it names.
        let mut coherent: NameTally = BTreeMap::new();
        vote(&mut coherent, 0, SmolStr::new("src/core"), 200);
        vote(&mut coherent, 0, SmolStr::new("src/io"), 30);
        vote(&mut coherent, 0, SmolStr::new("src/net"), 20);
        assert_eq!(
            coherent
                .get(&0)
                .map(|tally| elect(tally, &anchor))
                .unwrap_or_default(),
            "src/core"
        );

        // a grab-bag: no origin reaches half the weight, so the ladder falls
        // to the homes' shared prefix — a real place, never a neutral label.
        let mut grabbag: NameTally = BTreeMap::new();
        vote(&mut grabbag, 0, SmolStr::new("src/openai"), 40);
        vote(&mut grabbag, 0, SmolStr::new("src/google"), 35);
        vote(&mut grabbag, 0, SmolStr::new("src/anthropic"), 33);
        assert_eq!(
            grabbag
                .get(&0)
                .map(|tally| elect(tally, &anchor))
                .unwrap_or_default(),
            "src"
        );

        // a production home keeps its name despite many companion spec files,
        // because SLOC — not file count — decides the majority.
        let mut with_specs: NameTally = BTreeMap::new();
        vote(&mut with_specs, 0, SmolStr::new("src/adapters"), 120);
        for _ in 0..5 {
            vote(&mut with_specs, 0, SmolStr::new("spec/adapters"), 0);
        }
        assert_eq!(
            with_specs
                .get(&0)
                .map(|tally| elect(tally, &anchor))
                .unwrap_or_default(),
            "src/adapters"
        );
    }

    #[test]
    fn should_join_top_two_homes_by_production_sloc_not_file_count() {
        // three divergent production homes plus a spec dump that wins on file
        // count alone: no home holds a strict SLOC majority and the keys share
        // no prefix, so the join fires — and the two production-SLOC-heaviest
        // homes must lead the composite. A zero-SLOC spec dump outnumbering
        // them in files can never lead the joined name.
        let mut votes: NameTally = BTreeMap::new();
        vote(&mut votes, 0, SmolStr::new("lib/core"), 300);
        vote(&mut votes, 0, SmolStr::new("vendor/util"), 250);
        vote(&mut votes, 0, SmolStr::new("tools/gen"), 200);
        for _ in 0..20 {
            vote(&mut votes, 0, SmolStr::new("spec/everything"), 0);
        }
        let anchor = SmolStr::new("lib/core");

        assert_eq!(
            votes
                .get(&0)
                .map(|tally| elect(tally, &anchor))
                .unwrap_or_default(),
            "lib/core/vendor/util"
        );
    }

    #[test]
    fn should_relativize_the_shared_prefix_when_joining_top_two_homes() {
        // package-qualified homes share their package root; the join must
        // relativize the second home against the first instead of re-embedding
        // the root — `ai/adapters/ai/model` repeats the root mid-path, an
        // incoherent nesting no human would ever propose.
        let anchor = SmolStr::new("ai/adapters");
        let mut votes: NameTally = BTreeMap::new();
        vote(&mut votes, 0, SmolStr::new("ai/adapters"), 300);
        vote(&mut votes, 0, SmolStr::new("ai/model"), 250);
        vote(&mut votes, 0, SmolStr::new("workspace"), 200);

        assert_eq!(
            votes
                .get(&0)
                .map(|tally| elect(tally, &anchor))
                .unwrap_or_default(),
            "ai/adapters/model"
        );

        // when one home is the other's ancestor, the ancestor already covers
        // both and elects alone instead of a self-embedding composite.
        let mut nested: NameTally = BTreeMap::new();
        vote(&mut nested, 0, SmolStr::new("ai/adapters"), 300);
        vote(&mut nested, 0, SmolStr::new("ai"), 250);
        vote(&mut nested, 0, SmolStr::new("workspace"), 200);

        assert_eq!(
            nested
                .get(&0)
                .map(|tally| elect(tally, &anchor))
                .unwrap_or_default(),
            "ai"
        );
    }

    #[test]
    fn should_relocate_a_misplaced_file_despite_a_cyclic_folder_base() {
        // folders `a` and `b` cycle through each other on two disjoint file
        // pairs per direction (a1→b1, a2→b2 against b3→a3, b4→a4), so no
        // single relocation can dissolve the cycle — as tangles between
        // deliberately misplaced files behave on real repositories. A
        // whole-state acyclicity veto then prices every move at infinity and
        // freezes the polish pass wholesale; the veto must only bar moves that
        // grow the cyclicity, keeping the strictly-improving relocation of
        // `m.ts` (every edge pointing into `c`, far from the cycle) available.
        let snapshot = snapshot(
            vec![
                homed(0, "a1", 6, 10),
                homed(1, "a2", 7, 10),
                homed(2, "a3", 8, 10),
                homed(3, "a4", 9, 10),
                homed(4, "m", 10, 10),
                homed(5, "b1", 11, 10),
                homed(6, "b2", 12, 10),
                homed(7, "b3", 13, 10),
                homed(8, "b4", 14, 10),
                homed(9, "c1", 15, 10),
                homed(10, "c2", 16, 10),
            ],
            vec![
                edge(0, 5),
                edge(1, 6),
                edge(7, 2),
                edge(8, 3),
                edge(4, 9),
                edge(4, 10),
                edge(9, 10),
            ],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "app", ScopeLevel::Package, Some(0)),
                container(2, "app/core", ScopeLevel::Domain, Some(1)),
                container(3, "app/core/a", ScopeLevel::Folder, Some(2)),
                container(4, "app/core/b", ScopeLevel::Folder, Some(2)),
                container(5, "app/core/c", ScopeLevel::Folder, Some(2)),
                container(6, "src/core/a/a1.ts", ScopeLevel::File, Some(3)),
                container(7, "src/core/a/a2.ts", ScopeLevel::File, Some(3)),
                container(8, "src/core/a/a3.ts", ScopeLevel::File, Some(3)),
                container(9, "src/core/a/a4.ts", ScopeLevel::File, Some(3)),
                container(10, "src/core/a/m.ts", ScopeLevel::File, Some(3)),
                container(11, "src/core/b/b1.ts", ScopeLevel::File, Some(4)),
                container(12, "src/core/b/b2.ts", ScopeLevel::File, Some(4)),
                container(13, "src/core/b/b3.ts", ScopeLevel::File, Some(4)),
                container(14, "src/core/b/b4.ts", ScopeLevel::File, Some(4)),
                container(15, "src/core/c/c1.ts", ScopeLevel::File, Some(5)),
                container(16, "src/core/c/c2.ts", ScopeLevel::File, Some(5)),
            ],
        );

        let moves = analyze(&snapshot, &config_with_k(1))
            .ok()
            .and_then(|result| result.modes.greenfield)
            .and_then(|mode| mode.candidates.into_iter().next())
            .map(|candidate| candidate.delta_narration)
            .unwrap_or_default();

        let destination = moves
            .iter()
            .find(|entry| {
                entry
                    .files
                    .iter()
                    .any(|file| file.path == "src/core/a/m.ts")
            })
            .map(|entry| entry.to.clone());

        assert!(
            destination
                .as_deref()
                .is_some_and(|to| to.split('/').next_back() == Some("c")),
            "expected m.ts to land in folder c, got {destination:?} among {moves:?}"
        );
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

    /// A production symbol node with an explicit size, for the clustering tests.
    fn homed(id: u32, name: &str, container: u32, size: u32) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(name),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(container),
            visibility: ScopeLevel::File,
            effective_size: size,
        }
    }

    /// Collects the `(level, name)` pairs of a mode's first candidate tree.
    fn candidate_containers(snapshot: &Snapshot, config: &AnalyzeConfig) -> Vec<(Level, String)> {
        let candidate = analyze(snapshot, config)
            .ok()
            .and_then(|result| result.modes.greenfield)
            .and_then(|mode| mode.candidates.into_iter().next());
        let (mut names, mut sloc, mut levels) = (Vec::new(), Vec::new(), Vec::new());
        if let Some(candidate) = &candidate {
            collect_tree(&candidate.tree, &mut names, &mut sloc, &mut levels);
        }
        levels.into_iter().zip(names).collect()
    }

    #[test]
    fn should_separate_domains_by_home_directory() {
        // two home directories (`adapters` with openai+google folders, `agent`
        // with loop+plan) with a weak cross edge: before the seed carried a home
        // affinity the upper level pooled all four folders by index order into one
        // cut-minimal cluster that `elect` could only call `mixed`; now each
        // domain packs its own home and elects a real name.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 5, 10),
                homed(1, "b", 6, 10),
                homed(2, "c", 7, 10),
                homed(3, "d", 8, 10),
                homed(4, "e", 12, 10),
                homed(5, "f", 13, 10),
                homed(6, "g", 14, 10),
                homed(7, "h", 15, 10),
            ],
            vec![edge(0, 1), edge(2, 3), edge(4, 5), edge(6, 7), edge(0, 4)],
            vec![
                container(0, "ai", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
                container(3, "ai/adapters/openai", ScopeLevel::Folder, Some(2)),
                container(4, "ai/adapters/google", ScopeLevel::Folder, Some(2)),
                container(9, "ai/agent", ScopeLevel::Domain, Some(1)),
                container(10, "ai/agent/loop", ScopeLevel::Folder, Some(9)),
                container(11, "ai/agent/plan", ScopeLevel::Folder, Some(9)),
                container(5, "src/adapters/openai/a.ts", ScopeLevel::File, Some(3)),
                container(6, "src/adapters/openai/b.ts", ScopeLevel::File, Some(3)),
                container(7, "src/adapters/google/c.ts", ScopeLevel::File, Some(4)),
                container(8, "src/adapters/google/d.ts", ScopeLevel::File, Some(4)),
                container(12, "src/agent/loop/e.ts", ScopeLevel::File, Some(10)),
                container(13, "src/agent/loop/f.ts", ScopeLevel::File, Some(10)),
                container(14, "src/agent/plan/g.ts", ScopeLevel::File, Some(11)),
                container(15, "src/agent/plan/h.ts", ScopeLevel::File, Some(11)),
            ],
        );
        let mut config = config_with_k(1);
        config.capacity.folder = 2;
        config.capacity.domain = 2;
        config.capacity.package = 4;

        let containers = candidate_containers(&snapshot, &config);
        let domains: Vec<&String> = containers
            .iter()
            .filter(|(level, _)| *level == Level::Domain)
            .map(|(_, name)| name)
            .collect();

        assert!(
            domains.iter().all(|name| name.as_str() != "mixed"),
            "domains collapsed into a mixed grab-bag: {domains:?}"
        );
        assert!(
            domains.iter().any(|name| name.as_str() == "adapters")
                && domains.iter().any(|name| name.as_str() == "agent"),
            "expected the two home directories to elect distinct domains, got {domains:?}"
        );
    }

    #[test]
    fn should_keep_an_over_cap_real_directory_whole_under_its_real_name() {
        // one real directory (`openai`) holds more files than the folder cap and
        // no sibling directory exists to relieve into. Folders are reality: the
        // directory stays one whole folder under its real name — never a
        // synthetic `openai-gpt` or `openai-2` split — and the breach surfaces
        // downstream as an honest capacity violation instead.
        let snapshot = snapshot(
            vec![
                homed(0, "gpt", 4, 10),
                homed(1, "gpt", 5, 10),
                homed(2, "dalle", 6, 10),
                homed(3, "dalle", 7, 10),
            ],
            vec![edge(0, 1), edge(2, 3)],
            vec![
                container(0, "ai", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
                container(3, "ai/adapters/openai", ScopeLevel::Folder, Some(2)),
                container(
                    4,
                    "src/adapters/openai/gpt_one.ts",
                    ScopeLevel::File,
                    Some(3),
                ),
                container(
                    5,
                    "src/adapters/openai/gpt_two.ts",
                    ScopeLevel::File,
                    Some(3),
                ),
                container(
                    6,
                    "src/adapters/openai/dalle_one.ts",
                    ScopeLevel::File,
                    Some(3),
                ),
                container(
                    7,
                    "src/adapters/openai/dalle_two.ts",
                    ScopeLevel::File,
                    Some(3),
                ),
            ],
        );
        let mut config = config_with_k(1);
        config.capacity.folder = 2;

        let containers = candidate_containers(&snapshot, &config);
        let folders: Vec<&String> = containers
            .iter()
            .filter(|(level, _)| *level == Level::Folder)
            .map(|(_, name)| name)
            .collect();

        assert_eq!(
            folders.len(),
            1,
            "the real directory must stay one whole folder, got {folders:?}"
        );
        assert!(
            folders.iter().all(|name| name.as_str() == "openai"),
            "the folder must keep its real directory name, got {folders:?}"
        );
    }

    /// Collects each file-holding directory's folder-chain path and the file
    /// names directly under it over a candidate tree, in pre-order.
    ///
    /// Folder nodes render one path segment each, so a directory's identity is
    /// the slash-joined chain of folder segments below its domain; interior
    /// chain nodes holding no files directly are not directories of interest
    /// and are skipped.
    fn folder_files(node: &ContainerNode, folders: &mut Vec<(String, Vec<String>)>) {
        folder_files_under(node, "", folders);
    }

    /// Walks below [`folder_files`], threading the folder-chain `prefix`.
    fn folder_files_under(
        node: &ContainerNode,
        prefix: &str,
        folders: &mut Vec<(String, Vec<String>)>,
    ) {
        let path = if node.level != Level::Folder {
            String::new()
        } else if prefix.is_empty() {
            node.name.clone()
        } else {
            format!("{prefix}/{}", node.name)
        };
        if node.level == Level::Folder {
            let files: Vec<String> = node
                .children
                .iter()
                .flatten()
                .filter(|child| child.level == Level::File)
                .map(|child| child.name.clone())
                .collect();
            if !files.is_empty() {
                folders.push((path.clone(), files));
            }
        }
        for child in node.children.iter().flatten() {
            folder_files_under(child, &path, folders);
        }
    }

    /// An over-cap real directory beside an under-cap one: `one` holds three
    /// files against a folder cap of two, and its file `c` couples hard
    /// (three priced edges) to `d` in `two`. The old folder clustering split
    /// `one` into a suffixed synthetic sibling holding files from both real
    /// directories; the real-directory partition must not.
    fn over_cap_real_dir_snapshot() -> Snapshot {
        snapshot(
            vec![
                homed(0, "alpha", 5, 10),
                homed(1, "beta", 6, 10),
                homed(2, "c_zero", 7, 10),
                homed(3, "c_one", 7, 10),
                homed(4, "c_two", 7, 10),
                homed(5, "delta", 8, 10),
            ],
            vec![edge(2, 5), edge(3, 5), edge(4, 5)],
            vec![
                container(0, "ai", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/svc", ScopeLevel::Domain, Some(1)),
                container(3, "ai/svc/one", ScopeLevel::Folder, Some(2)),
                container(4, "ai/svc/two", ScopeLevel::Folder, Some(2)),
                container(5, "src/svc/one/a.ts", ScopeLevel::File, Some(3)),
                container(6, "src/svc/one/b.ts", ScopeLevel::File, Some(3)),
                container(7, "src/svc/one/c.ts", ScopeLevel::File, Some(3)),
                container(8, "src/svc/two/d.ts", ScopeLevel::File, Some(4)),
            ],
        )
    }

    /// The parent folder name of `file` in a candidate tree's folder listing.
    fn folder_of<'a>(folders: &'a [(String, Vec<String>)], file: &str) -> Option<&'a str> {
        folders
            .iter()
            .find(|(_, files)| files.iter().any(|name| name == file))
            .map(|(name, _)| name.as_str())
    }

    /// True when a name carries a synthetic numeric dedup suffix like `-2`.
    fn numeric_suffixed(name: &str) -> bool {
        name.rsplit_once('-')
            .is_some_and(|(_, tail)| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()))
    }

    /// Collects every container name rendered in a candidate tree, pre-order.
    fn tree_names(node: &ContainerNode) -> Vec<String> {
        let (mut names, mut sloc, mut levels) = (Vec::new(), Vec::new(), Vec::new());
        collect_tree(node, &mut names, &mut sloc, &mut levels);
        names
    }

    #[test]
    fn should_keep_fallback_folders_of_different_packages_apart() {
        // reality is the location, not the name: `a.ts` and `b.ts` both sit
        // directly in their domain directories (inheriting them as folder
        // keys), but they live in different real packages, so no candidate may
        // pool them into one folder cluster (which would drag them into one
        // shared domain and package).
        let snapshot = snapshot(
            vec![homed(0, "alpha", 3, 10), homed(1, "beta", 6, 10)],
            vec![],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/app", ScopeLevel::Domain, Some(1)),
                container(3, "src/a.ts", ScopeLevel::File, Some(2)),
                container(4, "bi", ScopeLevel::Package, Some(0)),
                container(5, "bi/app", ScopeLevel::Domain, Some(4)),
                container(6, "src/b.ts", ScopeLevel::File, Some(5)),
            ],
        );
        let config = config_with_k(1);

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let mut folders = Vec::new();
                folder_files(&candidate.tree, &mut folders);
                assert!(
                    folders.iter().all(|(_, files)| files.len() == 1),
                    "files of different packages must not pool into one \
                     fallback folder, got {folders:?}"
                );
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
    }

    #[test]
    fn should_never_suffix_same_basename_folders_merged_into_one_domain() {
        // `ai/adapters/http` and `ai/core/http` share a basename but are
        // different real places; hard coupling merges their domains into one
        // suggestion, and each folder must still surface its own real key —
        // never a truncated twin deduped into a synthetic `http-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "alpha", 4, 40),
                homed(1, "beta", 5, 20),
                homed(2, "gamma", 8, 20),
                homed(3, "delta", 9, 10),
            ],
            vec![edge(0, 2), edge(1, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
                container(3, "ai/adapters/http", ScopeLevel::Folder, Some(2)),
                container(4, "src/adapters/http/client.ts", ScopeLevel::File, Some(3)),
                container(5, "src/adapters/http/codec.ts", ScopeLevel::File, Some(3)),
                container(6, "ai/core", ScopeLevel::Domain, Some(1)),
                container(7, "ai/core/http", ScopeLevel::Folder, Some(6)),
                container(8, "src/core/http/util.ts", ScopeLevel::File, Some(7)),
                container(9, "src/core/http/parse.ts", ScopeLevel::File, Some(7)),
            ],
        );
        let mut config = config_with_k(2);
        // both folders sit at the cap, so polish cannot cross-pull members and
        // every candidate keeps both real directories intact.
        config.capacity.folder = 2;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut merged_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let names = tree_names(&candidate.tree);
                let suffixed: Vec<&String> = names
                    .iter()
                    .filter(|name| numeric_suffixed(name.as_str()))
                    .collect();
                assert!(
                    suffixed.is_empty(),
                    "no container may carry a synthetic numeric suffix, got {suffixed:?}"
                );
                let mut folders = Vec::new();
                folder_files(&candidate.tree, &mut folders);
                assert_eq!(
                    folder_of(&folders, "src/adapters/http/client.ts"),
                    Some("http"),
                    "the majority folder keeps its real basename, got {folders:?}"
                );
                let foreign = folder_of(&folders, "src/core/http/util.ts");
                assert!(
                    foreign == Some("http") || foreign == Some("ai/core/http"),
                    "the minority folder must render its real key, got {foreign:?} in {folders:?}"
                );
                merged_seen |= foreign == Some("ai/core/http");
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            merged_seen,
            "expected a merged-domain candidate rendering the foreign folder \
             by its full real key `ai/core/http`"
        );
    }

    #[test]
    fn should_inherit_real_domain_keys_for_domain_rooted_files() {
        // these files sit directly in two packages' domain directories, so
        // there is no deeper folder: each file's real folder IS its domain
        // directory. When coupling merges the two domains into one suggestion
        // the folders must keep those inherited real keys — never collapse
        // into `workspace` fallback twins deduped as `workspace-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "alpha", 3, 10),
                homed(1, "beta", 4, 10),
                homed(2, "gamma", 7, 10),
                homed(3, "delta", 8, 10),
            ],
            vec![edge(0, 2), edge(1, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/app", ScopeLevel::Domain, Some(1)),
                container(3, "src/a1.ts", ScopeLevel::File, Some(2)),
                container(4, "src/a2.ts", ScopeLevel::File, Some(2)),
                container(5, "bi", ScopeLevel::Package, Some(0)),
                container(6, "bi/app", ScopeLevel::Domain, Some(5)),
                container(7, "src/b1.ts", ScopeLevel::File, Some(6)),
                container(8, "src/b2.ts", ScopeLevel::File, Some(6)),
            ],
        );
        let mut config = config_with_k(2);
        // both inherited folders sit at the cap: polish cannot pool the files.
        config.capacity.folder = 2;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut merged_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let names = tree_names(&candidate.tree);
                let suffixed: Vec<&String> = names
                    .iter()
                    .filter(|name| numeric_suffixed(name.as_str()))
                    .collect();
                assert!(
                    suffixed.is_empty(),
                    "no container may carry a synthetic numeric suffix, got {suffixed:?}"
                );
                let mut folders = Vec::new();
                folder_files(&candidate.tree, &mut folders);
                assert!(
                    folders
                        .iter()
                        .all(|(name, _)| ["app", "ai/app", "bi/app"].contains(&name.as_str())),
                    "every folder must carry a real inherited key, got {folders:?}"
                );
                assert!(
                    folders.iter().all(|(_, files)| files.len() == 2),
                    "files of different packages must not pool, got {folders:?}"
                );
                let a = folder_of(&folders, "src/a1.ts");
                let b = folder_of(&folders, "src/b1.ts");
                merged_seen |= a == Some("ai/app") && b == Some("bi/app");
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            merged_seen,
            "expected a merged-domain candidate keeping both inherited real \
             keys `ai/app` and `bi/app` whole"
        );
    }

    #[test]
    fn should_qualify_folder_keys_that_collide_across_nested_packages() {
        // source-root stripping can normalize two different real directories
        // to one folder key: `a/src/b/c` in package `a` and `a/b/src/c` in
        // nested package `a/b` both key as `a/b/c`. The twins must qualify by
        // their real package — never dedupe into a synthetic `c-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "alpha", 4, 10),
                homed(1, "beta", 5, 10),
                homed(2, "gamma", 9, 10),
                homed(3, "delta", 10, 10),
            ],
            vec![edge(0, 2), edge(1, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "a", ScopeLevel::Package, Some(0)),
                container(2, "a/b", ScopeLevel::Domain, Some(1)),
                container(3, "a/b/c", ScopeLevel::Folder, Some(2)),
                container(4, "src/b/c/f1.ts", ScopeLevel::File, Some(3)),
                container(5, "src/b/c/f2.ts", ScopeLevel::File, Some(3)),
                container(6, "a/b", ScopeLevel::Package, Some(0)),
                container(7, "a/b/c", ScopeLevel::Domain, Some(6)),
                container(8, "a/b/c", ScopeLevel::Folder, Some(7)),
                container(9, "src/c/g1.ts", ScopeLevel::File, Some(8)),
                container(10, "src/c/g2.ts", ScopeLevel::File, Some(8)),
            ],
        );
        let mut config = config_with_k(2);
        config.capacity.folder = 2;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut merged_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let names = tree_names(&candidate.tree);
                let suffixed: Vec<&String> = names
                    .iter()
                    .filter(|name| numeric_suffixed(name.as_str()))
                    .collect();
                assert!(
                    suffixed.is_empty(),
                    "no container may carry a synthetic numeric suffix, got {suffixed:?}"
                );
                let mut folders = Vec::new();
                folder_files(&candidate.tree, &mut folders);
                assert!(
                    folders.iter().all(|(name, _)| {
                        ["c", "c (a)", "c (a.b)", "a/b/c (a)", "a/b/c (a.b)"]
                            .contains(&name.as_str())
                    }),
                    "every folder must carry its real or qualified key, got {folders:?}"
                );
                assert!(
                    folders.iter().all(|(_, files)| files.len() == 2),
                    "files of different packages must not pool, got {folders:?}"
                );
                let first = folder_of(&folders, "src/b/c/f1.ts");
                let second = folder_of(&folders, "src/c/g1.ts");
                merged_seen |= first == Some("c (a)") && second == Some("c (a.b)");
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            merged_seen,
            "expected a merged-domain candidate qualifying the twins as \
             `c (a)` and `c (a.b)`"
        );
    }

    #[test]
    fn should_qualify_folder_keys_that_collide_within_one_package() {
        // hand-built snapshots may reuse one bare folder key under two domains
        // of the same package; the twins must qualify by their real location —
        // never dedupe into a synthetic `shared-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "alpha", 4, 10),
                homed(1, "beta", 5, 10),
                homed(2, "gamma", 8, 10),
                homed(3, "delta", 9, 10),
            ],
            vec![edge(0, 2), edge(1, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "pa", ScopeLevel::Package, Some(0)),
                container(2, "pa/d1", ScopeLevel::Domain, Some(1)),
                container(3, "shared", ScopeLevel::Folder, Some(2)),
                container(4, "src/d1/f1.ts", ScopeLevel::File, Some(3)),
                container(5, "src/d1/f2.ts", ScopeLevel::File, Some(3)),
                container(6, "pa/d2", ScopeLevel::Domain, Some(1)),
                container(7, "shared", ScopeLevel::Folder, Some(6)),
                container(8, "src/d2/g1.ts", ScopeLevel::File, Some(7)),
                container(9, "src/d2/g2.ts", ScopeLevel::File, Some(7)),
            ],
        );
        let mut config = config_with_k(2);
        config.capacity.folder = 2;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut merged_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let names = tree_names(&candidate.tree);
                let suffixed: Vec<&String> = names
                    .iter()
                    .filter(|name| numeric_suffixed(name.as_str()))
                    .collect();
                assert!(
                    suffixed.is_empty(),
                    "no container may carry a synthetic numeric suffix, got {suffixed:?}"
                );
                let mut folders = Vec::new();
                folder_files(&candidate.tree, &mut folders);
                assert!(
                    folders.iter().all(|(name, _)| {
                        ["shared", "shared (pa pa.d1)", "shared (pa pa.d2)"]
                            .contains(&name.as_str())
                    }),
                    "every folder must carry its real or qualified key, got {folders:?}"
                );
                let first = folder_of(&folders, "src/d1/f1.ts");
                let second = folder_of(&folders, "src/d2/g1.ts");
                merged_seen |=
                    first == Some("shared (pa pa.d1)") && second == Some("shared (pa pa.d2)");
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            merged_seen,
            "expected a merged-domain candidate qualifying the twins as \
             `shared (pa pa.d1)` and `shared (pa pa.d2)`"
        );
    }

    #[test]
    fn should_keep_files_in_their_real_directories_as_folders() {
        // folders are reality: `b.ts` lives in the real `one` and `d.ts` in
        // the real `two`, so no candidate may pool them into one clustered
        // folder or invent a suffixed synthetic sibling for the overflow.
        let snapshot = over_cap_real_dir_snapshot();
        let mut config = config_with_k(3);
        config.capacity.folder = 2;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let mut folders = Vec::new();
                folder_files(&candidate.tree, &mut folders);
                assert_eq!(
                    folder_of(&folders, "src/svc/one/b.ts"),
                    Some("one"),
                    "b.ts must stay a member of its real directory, got {folders:?}"
                );
                assert_eq!(
                    folder_of(&folders, "src/svc/two/d.ts"),
                    Some("two"),
                    "d.ts must stay a member of its real directory, got {folders:?}"
                );
                assert!(
                    folders
                        .iter()
                        .all(|(name, _)| ["one", "two"].contains(&name.as_str())),
                    "every folder must be one of the real directories, got {folders:?}"
                );
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
    }

    #[test]
    fn should_relieve_an_over_cap_real_directory_through_polish() {
        // the real directory `one` holds three files against a cap of two; its
        // file `c` couples hard to `d` in under-cap `two`, so the polish pass
        // must relieve `one` by moving `c` into the real `two` — never by
        // splitting `one` into a suffixed synthetic sibling.
        let snapshot = over_cap_real_dir_snapshot();
        let mut config = config_with_k(2);
        config.capacity.folder = 2;

        let candidate = analyze(&snapshot, &config)
            .ok()
            .and_then(|result| result.modes.greenfield)
            .and_then(|mode| mode.candidates.into_iter().next());

        let mut folders = Vec::new();
        if let Some(candidate) = &candidate {
            folder_files(&candidate.tree, &mut folders);
        }
        let names: Vec<&str> = folders.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["one", "two"],
            "expected the two real directories as the only folders, got {folders:?}"
        );
        let by_name: BTreeMap<&str, &Vec<String>> = folders
            .iter()
            .map(|(name, files)| (name.as_str(), files))
            .collect();
        assert_eq!(
            by_name.get("one").map(|files| files.as_slice()),
            Some(&["src/svc/one/a.ts".to_owned(), "src/svc/one/b.ts".to_owned()][..]),
            "polish should relieve `one` down to its cap"
        );
        assert_eq!(
            by_name.get("two").map(|files| files.as_slice()),
            Some(&["src/svc/one/c.ts".to_owned(), "src/svc/two/d.ts".to_owned()][..]),
            "the relieving move must land `c` in the real `two`"
        );
        // the relieved layout clears every capacity breach.
        assert_eq!(
            candidate.as_ref().and_then(|candidate| {
                candidate
                    .capacity_remainder
                    .as_ref()
                    .map(|remainder| remainder.remaining)
            }),
            Some(0),
            "the best greenfield candidate must fix the folder breach"
        );
    }

    #[test]
    fn should_read_laminar_home_keys_from_the_container_chain() {
        // the laminar tree keeps source-root-stripped, package-root-resolved
        // name keys; a file under `src/` still keys to the `ai` package and the
        // `ai/adapters` domain/folder, never to a bare `src`.
        let containers = [
            container(0, "ai", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
            container(3, "ai/adapters", ScopeLevel::Folder, Some(2)),
            container(4, "src/adapters/openai.ts", ScopeLevel::File, Some(3)),
        ];
        let by_id: BTreeMap<u32, &Container> = containers.iter().map(|c| (c.id.0, c)).collect();

        let home = laminar_home(&by_id, 4);
        assert_eq!(home.package, "ai");
        assert_eq!(home.domain, "ai/adapters");
        assert_eq!(home.folder, "ai/adapters");
    }

    #[test]
    fn should_key_the_home_to_the_nearest_folder_and_package() {
        // laminar folders and package roots nest; a file's real place is the
        // NEAREST ancestor at each level (folder `…/deeper`, package `a/b`),
        // never the outermost one.
        let containers = [
            container(0, "g", ScopeLevel::PackageGroup, None),
            container(1, "a", ScopeLevel::Package, Some(0)),
            container(2, "a/b", ScopeLevel::Package, Some(1)),
            container(3, "a/b/x", ScopeLevel::Domain, Some(2)),
            container(4, "a/b/x/deep", ScopeLevel::Folder, Some(3)),
            container(5, "a/b/x/deep/deeper", ScopeLevel::Folder, Some(4)),
            container(6, "src/x/deep/deeper/f.ts", ScopeLevel::File, Some(5)),
        ];
        let by_id: BTreeMap<u32, &Container> = containers.iter().map(|c| (c.id.0, c)).collect();

        let home = laminar_home(&by_id, 6);
        assert_eq!(home.package, "a/b", "the nearest package root wins");
        assert_eq!(home.domain, "a/b/x");
        assert_eq!(home.folder, "a/b/x/deep/deeper", "the nearest folder wins");
    }

    #[test]
    fn should_inherit_missing_levels_from_the_nearest_broader_key() {
        // a file directly in its domain directory has no deeper folder: its
        // real folder IS that directory, so the folder key inherits the domain
        // key instead of collapsing to a synthetic `workspace` twin.
        let containers = [
            container(0, "g", ScopeLevel::PackageGroup, None),
            container(1, "p", ScopeLevel::Package, Some(0)),
            container(2, "p/d", ScopeLevel::Domain, Some(1)),
            container(3, "src/f.ts", ScopeLevel::File, Some(2)),
        ];
        let by_id: BTreeMap<u32, &Container> = containers.iter().map(|c| (c.id.0, c)).collect();

        let home = laminar_home(&by_id, 3);
        assert_eq!(home.package, "p");
        assert_eq!(home.domain, "p/d");
        assert_eq!(home.folder, "p/d", "the folder inherits the domain key");
    }

    #[test]
    fn should_not_over_strip_a_user_named_src_module() {
        // a `src` that is a real module name (not a transparent source root) is
        // preserved by the laminar tree as `ai/src`; the engine honors it rather
        // than blindly dropping every `src` segment.
        let containers = [
            container(0, "ai", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/src", ScopeLevel::Domain, Some(1)),
            container(3, "ai/src", ScopeLevel::Folder, Some(2)),
            container(4, "src/src/deep.ts", ScopeLevel::File, Some(3)),
        ];
        let by_id: BTreeMap<u32, &Container> = containers.iter().map(|c| (c.id.0, c)).collect();

        let home = laminar_home(&by_id, 4);
        assert_eq!(home.package, "ai");
        assert_eq!(home.domain, "ai/src");
        assert_eq!(home.folder, "ai/src");
    }

    #[test]
    fn should_name_the_package_from_the_manifest_root_not_the_source_root() {
        // sources live under `src/`, but the laminar tree already resolved the
        // package to the manifest root `ai`; candidate naming must reuse that,
        // so `ai` names the package and `src` never surfaces as a container.
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
            vec![sized(0, "openai", 4, 10), sized(1, "anthropic", 5, 7)],
            vec![edge(0, 1)],
            vec![
                container(0, "ai", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
                container(3, "ai/adapters", ScopeLevel::Folder, Some(2)),
                container(4, "src/adapters/openai.ts", ScopeLevel::File, Some(3)),
                container(5, "src/adapters/anthropic.ts", ScopeLevel::File, Some(3)),
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

        let named: Vec<(&Level, &String)> = levels.iter().zip(names.iter()).collect();
        // the package elects the manifest root, not the transparent source root.
        assert!(
            named
                .iter()
                .any(|(level, name)| **level == Level::Package && name.as_str() == "ai"),
            "expected an `ai` package, got {names:?}"
        );
        // `src` leaks nowhere as an interior container (files keep their paths).
        assert!(
            named
                .iter()
                .all(|(level, name)| **level == Level::File || name.as_str() != "src"),
            "src leaked as an interior container: {names:?}"
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

    /// True when a rendered name is a bare number — every byte a digit.
    fn bare_numeric(name: &str) -> bool {
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
    }

    /// Collects every `(level, rendered name)` pair of a candidate tree,
    /// pre-order.
    fn level_names(tree: &ContainerNode) -> Vec<(Level, String)> {
        let (mut names, mut sloc, mut levels) = (Vec::new(), Vec::new(), Vec::new());
        collect_tree(tree, &mut names, &mut sloc, &mut levels);
        levels.into_iter().zip(names).collect()
    }

    /// Returns the rendered Domain-level names of a candidate tree.
    fn domain_names(tree: &ContainerNode) -> Vec<String> {
        level_names(tree)
            .into_iter()
            .filter(|(level, _)| *level == Level::Domain)
            .map(|(_, name)| name)
            .collect()
    }

    #[test]
    fn should_never_elect_the_mixed_label() {
        // three homes at 40/35/33 SLOC: no strict majority, no shared prefix.
        // The election must produce a real composite name — never the
        // synthetic `mixed` grab-bag label.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 2, 20),
                homed(1, "b", 3, 20),
                homed(2, "c", 5, 18),
                homed(3, "d", 6, 17),
                homed(4, "e", 8, 17),
                homed(5, "f", 9, 16),
            ],
            vec![edge(0, 2), edge(3, 4), edge(5, 1)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "d1", ScopeLevel::Domain, Some(0)),
                container(2, "src/d1/a.ts", ScopeLevel::File, Some(1)),
                container(3, "src/d1/b.ts", ScopeLevel::File, Some(1)),
                container(4, "d2", ScopeLevel::Domain, Some(0)),
                container(5, "src/d2/c.ts", ScopeLevel::File, Some(4)),
                container(6, "src/d2/d.ts", ScopeLevel::File, Some(4)),
                container(7, "d3", ScopeLevel::Domain, Some(0)),
                container(8, "src/d3/e.ts", ScopeLevel::File, Some(7)),
                container(9, "src/d3/f.ts", ScopeLevel::File, Some(7)),
            ],
        );
        let mut config = config_with_k(2);
        // folders sit at the cap so polish cannot cross-pull members, and the
        // domain cap admits all three coupled folders into one cluster.
        config.capacity.folder = 2;
        config.capacity.domain = 3;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut merged_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let names = tree_names(&candidate.tree);
                assert!(
                    names
                        .iter()
                        .all(|name| name != "mixed" && !name.ends_with("/mixed")),
                    "no container may carry the synthetic mixed label, got {names:?}"
                );
                let domains = domain_names(&candidate.tree);
                merged_seen |= domains == ["d1/d2"];
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            merged_seen,
            "expected the merged grab-bag domain to elect the top-two join \
             `d1/d2`"
        );
    }

    /// Builds the all-numeric fixture: real package `2024` holding real
    /// directories `2024/x` and `2024/y`, two cross-coupled files each.
    fn numeric_home_snapshot() -> Snapshot {
        snapshot(
            vec![
                homed(0, "a", 3, 20),
                homed(1, "b", 4, 20),
                homed(2, "c", 6, 20),
                homed(3, "d", 7, 20),
            ],
            vec![edge(0, 2), edge(1, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "2024", ScopeLevel::Package, Some(0)),
                container(2, "2024/x", ScopeLevel::Folder, Some(1)),
                container(3, "src/x/a.ts", ScopeLevel::File, Some(2)),
                container(4, "src/x/b.ts", ScopeLevel::File, Some(2)),
                container(5, "2024/y", ScopeLevel::Folder, Some(1)),
                container(6, "src/y/c.ts", ScopeLevel::File, Some(5)),
                container(7, "src/y/d.ts", ScopeLevel::File, Some(5)),
            ],
        )
    }

    #[test]
    fn should_never_elect_a_bare_numeric_name() {
        // a real package directory named `2024` holds every vote: the elected
        // package and domain names of every emitted candidate must still never
        // surface as a bare number. Identity clones are exempt — they carry
        // the real tree verbatim, and folders always keep their real names.
        let snapshot = numeric_home_snapshot();
        let mut config = config_with_k(2);
        // headroom of one over the two-file directories, so a moved-file
        // partition exists and at least one candidate is emitted (elected)
        // rather than cloned.
        config.capacity.folder = 3;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut emitted = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                if candidate.delta_narration.is_empty() {
                    continue;
                }
                emitted += 1;
                let elected: Vec<(Level, String)> = level_names(&candidate.tree)
                    .into_iter()
                    .filter(|(level, _)| {
                        matches!(level, Level::Domain | Level::Package | Level::PackageGroup)
                    })
                    .collect();
                assert!(
                    elected
                        .iter()
                        .all(|(_, name)| !bare_numeric(name) && !numeric_suffixed(name)),
                    "no elected container may render as a bare number or a \
                     numeric-suffixed twin, got {elected:?}"
                );
            }
        }
        assert!(
            emitted > 0,
            "expected at least one emitted (elected) candidate"
        );
    }

    #[test]
    fn should_elect_the_strict_majority_home_verbatim() {
        // rung R1: `apps/d1` holds 40 of 50 SLOC — a strict majority — so the
        // merged domain takes that home key whole. The old cumulative rewrite
        // truncated a key foreign to its package to `workspace/d1`, rendering
        // `d1` and hiding the real `apps` prefix.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 2, 20),
                homed(1, "b", 3, 20),
                homed(2, "c", 5, 5),
                homed(3, "d", 6, 5),
            ],
            vec![edge(0, 2), edge(1, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "apps/d1", ScopeLevel::Domain, Some(0)),
                container(2, "src/d1/a.ts", ScopeLevel::File, Some(1)),
                container(3, "src/d1/b.ts", ScopeLevel::File, Some(1)),
                container(4, "beta/d2", ScopeLevel::Domain, Some(0)),
                container(5, "src/d2/c.ts", ScopeLevel::File, Some(4)),
                container(6, "src/d2/d.ts", ScopeLevel::File, Some(4)),
            ],
        );
        let mut config = config_with_k(2);
        config.capacity.folder = 2;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut majority_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let domains = domain_names(&candidate.tree);
                assert!(
                    domains.iter().all(|name| name != "d1"),
                    "a majority home must render whole, not truncated to its \
                     last segment, got {domains:?}"
                );
                majority_seen |= domains.iter().any(|name| name == "apps/d1");
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            majority_seen,
            "expected the strict-majority home to render by its full real \
             key `apps/d1`"
        );
    }

    #[test]
    fn should_name_a_balanced_cluster_by_the_shared_home_prefix() {
        // rung R2: `ai/app` and `ai/core` tie at 40 SLOC each — no strict
        // majority — but share the `ai` prefix, so the merged domain is named
        // `ai` rather than misnaming the whole after one tied side.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 3, 20),
                homed(1, "b", 4, 20),
                homed(2, "c", 6, 20),
                homed(3, "d", 7, 20),
            ],
            vec![edge(0, 2), edge(1, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/app", ScopeLevel::Domain, Some(1)),
                container(3, "src/app/a.ts", ScopeLevel::File, Some(2)),
                container(4, "src/app/b.ts", ScopeLevel::File, Some(2)),
                container(5, "ai/core", ScopeLevel::Domain, Some(1)),
                container(6, "src/core/c.ts", ScopeLevel::File, Some(5)),
                container(7, "src/core/d.ts", ScopeLevel::File, Some(5)),
            ],
        );
        let mut config = config_with_k(2);
        config.capacity.folder = 2;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut prefix_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let domains = domain_names(&candidate.tree);
                prefix_seen |= domains == ["ai"];
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            prefix_seen,
            "expected the balanced merged domain to elect the shared home \
             prefix `ai`"
        );
    }

    #[test]
    fn should_join_the_top_two_homes_when_no_prefix_is_shared() {
        // rung R3: `ai/app` (30 SLOC, two files) and `bi/app` (30 SLOC, three
        // files) tie with no shared prefix, so the merged domain joins the two
        // homes — ranked by production SLOC then file count, so the exact SLOC
        // tie falls to file count and `bi/app` leads despite `ai/app` sorting
        // first. The folder cap pins every folder at or over capacity, so the
        // only feasible co-location of the coupled pairs is the domain merge
        // itself.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 3, 15),
                homed(1, "b", 4, 15),
                homed(2, "c", 7, 10),
                homed(3, "d", 8, 10),
                homed(4, "e", 9, 10),
            ],
            vec![edge(0, 2), edge(1, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "ai", ScopeLevel::Package, Some(0)),
                container(2, "ai/app", ScopeLevel::Domain, Some(1)),
                container(3, "src/a.ts", ScopeLevel::File, Some(2)),
                container(4, "src/b.ts", ScopeLevel::File, Some(2)),
                container(5, "bi", ScopeLevel::Package, Some(0)),
                container(6, "bi/app", ScopeLevel::Domain, Some(5)),
                container(7, "src/c.ts", ScopeLevel::File, Some(6)),
                container(8, "src/d.ts", ScopeLevel::File, Some(6)),
                container(9, "src/e.ts", ScopeLevel::File, Some(6)),
            ],
        );
        let mut config = config_with_k(2);
        config.capacity.folder = 2;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut joined_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let containers = level_names(&candidate.tree);
                let joined_domain = containers
                    .iter()
                    .any(|(level, name)| *level == Level::Domain && name == "bi/app/ai/app");
                let joined_package = containers
                    .iter()
                    .any(|(level, name)| *level == Level::Package && name == "bi/ai");
                joined_seen |= joined_domain && joined_package;
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            joined_seen,
            "expected the merged levels to join their top-two homes as \
             `bi/app/ai/app` under `bi/ai`"
        );
    }

    #[test]
    fn should_fall_to_the_dominant_token_when_the_join_is_numeric() {
        // rung R4: year directories `2024/2025` and `2024/2026` outweigh
        // `2024/shared`, but their top-two join is all-digit segments — unfit
        // — so the election falls to the dominant non-numeric token `shared`
        // rather than a bare-number composite or the old `mixed` label.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 3, 20),
                homed(1, "b", 4, 20),
                homed(2, "c", 6, 20),
                homed(3, "d", 7, 20),
                homed(4, "e", 9, 10),
                homed(5, "f", 10, 10),
            ],
            vec![edge(0, 2), edge(3, 4), edge(5, 1)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "2024", ScopeLevel::Package, Some(0)),
                container(2, "2024/2025", ScopeLevel::Domain, Some(1)),
                container(3, "src/2025/a.ts", ScopeLevel::File, Some(2)),
                container(4, "src/2025/b.ts", ScopeLevel::File, Some(2)),
                container(5, "2024/2026", ScopeLevel::Domain, Some(1)),
                container(6, "src/2026/c.ts", ScopeLevel::File, Some(5)),
                container(7, "src/2026/d.ts", ScopeLevel::File, Some(5)),
                container(8, "2024/shared", ScopeLevel::Domain, Some(1)),
                container(9, "src/shared/e.ts", ScopeLevel::File, Some(8)),
                container(10, "src/shared/f.ts", ScopeLevel::File, Some(8)),
            ],
        );
        let mut config = config_with_k(2);
        config.capacity.folder = 2;
        config.capacity.domain = 3;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut token_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let names = tree_names(&candidate.tree);
                assert!(
                    names
                        .iter()
                        .all(|name| name != "mixed" && !name.ends_with("/mixed")),
                    "no container may carry the synthetic mixed label, got {names:?}"
                );
                token_seen |= domain_names(&candidate.tree) == ["shared"];
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            token_seen,
            "expected the numeric grab-bag domain to elect the dominant \
             non-numeric token `shared`"
        );
    }

    #[test]
    fn should_wrap_a_bare_numeric_last_resort_with_its_anchor() {
        // rung R5: every home key is the all-digit `2024`, so no rung can
        // yield a fit name and the last resort wraps the key with its anchor
        // folder — `2024 (2024.x)` — never a bare number. The real `2024/x`
        // and `2024/y` folders keep their real keys.
        let snapshot = numeric_home_snapshot();
        let mut config = config_with_k(2);
        // headroom of one so a moved-file partition exists and at least one
        // candidate is emitted (elected) rather than cloned.
        config.capacity.folder = 3;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut wrapped_seen = false;
        let mut emitted = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                if candidate.delta_narration.is_empty() {
                    continue;
                }
                emitted += 1;
                let containers = level_names(&candidate.tree);
                let package_wrapped = containers
                    .iter()
                    .any(|(level, name)| *level == Level::Package && name == "2024 (2024.x)");
                let domain_wrapped = containers
                    .iter()
                    .any(|(level, name)| *level == Level::Domain && name == "2024 (2024.x)");
                let mut folders = Vec::new();
                folder_files(&candidate.tree, &mut folders);
                let folder_real = folders.iter().any(|(name, _)| name.starts_with("2024/"));
                wrapped_seen |= package_wrapped && domain_wrapped && folder_real;
            }
        }
        assert!(
            emitted > 0,
            "expected at least one emitted (elected) candidate"
        );
        assert!(
            wrapped_seen,
            "expected the all-numeric home to elect the anchored wrap \
             `2024 (2024.x)` at package and domain, keeping the real folders"
        );
    }

    #[test]
    fn should_disambiguate_name_collisions_with_a_home_qualifier_not_an_integer() {
        // a domain cap of one splits the real `pa/app` home into two sibling
        // domain clusters that elect the same name; the twins must qualify by
        // their anchor folders — never dedupe into a synthetic `app-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 3, 10),
                homed(1, "b", 4, 10),
                homed(2, "c", 6, 10),
                homed(3, "d", 7, 10),
            ],
            vec![edge(0, 1), edge(2, 3)],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "pa", ScopeLevel::Package, Some(0)),
                container(2, "pa/app", ScopeLevel::Domain, Some(1)),
                container(3, "pa/app/x", ScopeLevel::Folder, Some(2)),
                container(4, "src/app/x/a.ts", ScopeLevel::File, Some(3)),
                container(5, "src/app/x/b.ts", ScopeLevel::File, Some(3)),
                container(6, "pa/app/y", ScopeLevel::Folder, Some(2)),
                container(7, "src/app/y/c.ts", ScopeLevel::File, Some(6)),
                container(8, "src/app/y/d.ts", ScopeLevel::File, Some(6)),
            ],
        );
        let mut config = config_with_k(1);
        config.capacity.folder = 2;
        config.capacity.domain = 1;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut qualified_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let names = tree_names(&candidate.tree);
                let suffixed: Vec<&String> = names
                    .iter()
                    .filter(|name| numeric_suffixed(name.as_str()))
                    .collect();
                assert!(
                    suffixed.is_empty(),
                    "colliding elected names must qualify by their homes, \
                     never a numeric twin, got {suffixed:?}"
                );
                let domains = domain_names(&candidate.tree);
                qualified_seen |=
                    domains == ["app (pa.app.x)".to_owned(), "app (pa.app.y)".to_owned()];
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            qualified_seen,
            "expected the tied sibling domains to qualify by their anchor \
             folders `app (pa.app.x)` and `app (pa.app.y)`"
        );
    }
}
