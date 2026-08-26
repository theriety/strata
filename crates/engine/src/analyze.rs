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
    Container, ContainerId, ContainerTree, Edge, Hardness, IntermediateRepresentation, Node,
    NodeId, Polarity, ScopeLevel, Snapshot,
};

use crate::config::{AnalyzeConfig, TestsConfig};
use crate::error::StrataError;
use crate::narrate::{FileFacts, narrate, tokenize};
use crate::result::{
    AnalyzeResult, Candidate, CapacityBreach, CapacityRemainder, ConditionalSplit, ContainerNode,
    CurrentStanding, CurrentTree, EdgeBreak, Level, ModeResult, Modes, RESULT_SCHEMA_VERSION,
    ScoreBreakdown, Severity, Summary, SymbolKind, SymbolMove, SymbolPlacement, Violation,
    ViolationKind,
};
use crate::snapshot::Language;

/// The capacity borderline band: a finding within ±10% of a cap is borderline
/// and never gates CI (reference `BORDERLINE_CAPACITY_MARGIN`).
pub const BORDERLINE_CAPACITY_MARGIN: f64 = 0.1;

/// The compiled `[tests]` policy deciding which files count as tests for the
/// clustering tie-cut and the subject-following shadow pass.
///
/// Built-in detection stays polarity-driven — the adapters already mark
/// symbols from `.spec.`/`.test.` paths, `tests/` directories, and language
/// test attributes. Patterns extend that with glob matching against a file's
/// project-relative place (its container chain joined with `/`, ending in the
/// file name), so `*.spec.*` applies repo-wide while `spec/mocks/**` stays
/// scoped.
#[derive(Debug, Clone)]
struct TestPolicy {
    /// Whether the built-in per-language detection participates.
    builtins: bool,
    /// Compiled patterns containing `/`: matched against the full path.
    path_patterns: Vec<glob::Pattern>,
    /// Compiled bare patterns: matched against the file name alone.
    base_patterns: Vec<glob::Pattern>,
}

impl TestPolicy {
    /// Compiles the configured policy, attributing a failed pattern at its
    /// `tests.patterns[i]` key.
    ///
    /// [`AnalyzeConfig::validate`] compiles every pattern once during loading;
    /// this second compilation covers embedders who build an
    /// [`AnalyzeConfig`] directly and never validate it.
    fn new(config: &TestsConfig) -> Result<Self, StrataError> {
        let mut policy = Self {
            builtins: config.builtins,
            path_patterns: Vec::new(),
            base_patterns: Vec::new(),
        };
        for (index, pattern) in config.patterns.iter().enumerate() {
            let compiled =
                glob::Pattern::new(pattern).map_err(|error| StrataError::ConfigInvalid {
                    key: Some(format!("tests.patterns[{index}]")),
                    reason: error.to_string(),
                })?;
            if pattern.contains('/') {
                policy.path_patterns.push(compiled);
            } else {
                policy.base_patterns.push(compiled);
            }
        }
        Ok(policy)
    }

    /// The shipped default: built-in detection on, no extra globs.
    #[cfg(test)]
    fn defaults() -> Self {
        Self {
            builtins: true,
            path_patterns: Vec::new(),
            base_patterns: Vec::new(),
        }
    }

    /// The inert policy: configuration alone marks nothing as a test.
    #[cfg(test)]
    fn disabled() -> Self {
        Self {
            builtins: false,
            path_patterns: Vec::new(),
            base_patterns: Vec::new(),
        }
    }

    /// Whether `path` matches any configured pattern; bare patterns face the
    /// final segment alone so `*.spec.*` needs no directory knowledge.
    fn matches(&self, path: &str) -> bool {
        let basename = path.rsplit('/').next().unwrap_or(path);
        self.base_patterns
            .iter()
            .any(|pattern| pattern.matches(basename))
            || self
                .path_patterns
                .iter()
                .any(|pattern| pattern.matches(path))
    }
}

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
    let tests = TestPolicy::new(&config.tests)?;
    let cycles = solve_cycles(snapshot, config, &weights);
    let violations = collect_violations(snapshot, config, &current_node, &cycles);
    let current_breakdown = score_current(
        snapshot,
        &config.objective.anchored(),
        &weights,
        config.capacity.folder,
    );

    // identity seeding is anchored-only (AD-2) and requires a cap-clean current
    // tree: a layout that already breaches a capacity cap is not a legal
    // candidate, so it may only serve as the delta baseline. Borderline
    // observations are within tolerance and never gate — the same predicate
    // the DTO reports as `capacity_breaks`.
    let capacity_breaks = hard_capacity_breaks(&violations);
    let capacity_clean = capacity_breaks == 0;

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
                &tests,
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
                &tests,
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
            capacity_breaks,
            violations,
        },
        modes: Modes {
            anchored,
            greenfield,
        },
    })
}

/// Counts the capacity findings that hard-breach their caps: `Severity::Violation`
/// only. Borderline observations sit within the tolerance band, never gate a
/// standing, and never count as breaks; they remain listed in `violations`.
fn hard_capacity_breaks(violations: &[Violation]) -> u32 {
    let count = violations
        .iter()
        .filter(|violation| {
            violation.kind == ViolationKind::Capacity && violation.severity == Severity::Violation
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
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
        // QUAL-P3-2: upper levels count their real structural members deeply —
        // a domain every file-binding folder beneath it, a package every
        // binding domain, a package group every binding package — so nesting
        // through an intermediate level cannot launder binding out of the
        // finding. Only members that actually bind a file count: an interior
        // directory chain (`a/b/c`) is one real place, never three. The folder
        // arm stays a direct file count (files bind exactly once at their own
        // folder).
        Level::Domain => (
            count_bound_members(node, Level::Folder).0,
            config.capacity.domain,
        ),
        Level::Package => (
            count_bound_members(node, Level::Domain).0,
            config.capacity.package,
        ),
        Level::PackageGroup => (
            count_bound_members(node, Level::Package).0,
            config.capacity.package_group,
        ),
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

/// Counts, in one pass, the descendants of `node` at `member_level` whose
/// subtree binds at least one file — the real places beneath it — plus
/// whether `node` itself binds one (QUAL-P3-2). An intermediate level can no
/// longer launder binding out of the finding. A rendered directory chain
/// interleaves one Folder node per path segment, so a folder counts as a real
/// place only when it *directly* holds a file — otherwise every ancestor
/// segment of `a/b/c` would price the single directory three times. Upper
/// member levels (domain, package) have no such chaining, so their transitive
/// binding stands.
fn count_bound_members(node: &ContainerNode, member_level: Level) -> (u32, bool) {
    if node.level == Level::File {
        return (0, true);
    }
    let mut members = 0_u32;
    let mut holds = false;
    for child in node.children.iter().flatten() {
        let (child_members, child_holds) = count_bound_members(child, member_level);
        members = members.saturating_add(child_members);
        holds |= child_holds;
    }
    if node.level == member_level {
        let binds = if member_level == Level::Folder {
            node.children
                .as_ref()
                .is_some_and(|children| children.iter().any(|child| child.level == Level::File))
        } else {
            holds
        };
        if binds {
            members = members.saturating_add(1);
        }
    }
    (members, holds)
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
    folder_budget: u32,
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
        folder_budget,
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
    tests: &TestPolicy,
) -> Result<ModeResult, StrataError> {
    let solver = PipelineSolver::new(snapshot, config, *coefficients, seed_identity, tests);
    let mode_config = mode_config(config);
    let CoreModeResult {
        candidates,
        solution_space_converged,
    } = diversify(&solver, &mode_config);

    let current_breakdown = score_current(
        snapshot,
        coefficients,
        &config.weights.kind_weights(),
        config.capacity.folder,
    );
    // A suggestion must at least tie keeping today's layout: the diversifier's
    // tolerance band measures against the pool's own best, so a diverse shape can
    // clear that bar yet still price above the current tree (the tie-cut withdrew
    // the test-edge pulls that used to keep every diversifier ahead). Candidates
    // scored above the current layout under this mode's own objective are
    // therefore never offered — `improvement` stays non-negative by construction,
    // which is the reliability contract the acceptance suite states. When nothing
    // clears the bar — an infeasible tree whose every restructuring costs more
    // than it saves — the least-bad shape is still offered, because for a
    // cap-breached layout doing nothing is not on the table.
    let mut offered: Vec<SolvedCandidate> = candidates
        .iter()
        .filter(|solved| solved.score <= current_breakdown.total)
        .cloned()
        .collect();
    if offered.is_empty() {
        offered = candidates.first().cloned().into_iter().collect();
    }
    let current_tree = &snapshot.ir().containers;
    let mut built = Vec::with_capacity(offered.len());
    for (index, solved) in offered.iter().enumerate() {
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

    let pairwise_distance = pairwise_distances(&offered);
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

/// Bound on symbol-polish sweeps (FIX08): the same two-pass shape as the file
/// polish — a second pass catches relocations the first pass unlocked — with an
/// early stop once a sweep relocates nothing.
const SYMBOL_SWEEPS: usize = 2;

/// Candidate destination FILES examined per symbol during the symbol polish.
const SYMBOL_TARGETS: usize = 4;

/// Floor on symbol-polish acceptance (FIX08): an improvement smaller than this
/// is float dust, not signal. Accepting it would fabricate movement — the very
/// thing D-47 forbids — so the pass demands a real margin before relocating a
/// symbol. The file polish does not need this: its moves are whole files, whose
/// deltas dwarf any rounding error.
const SYMBOL_MIN_IMPROVEMENT: f64 = 1e-12;

/// Coherence floor under which a folder's residual population is held to be
/// misdescribed by its own roof (FIX09): when fewer than half the files that
/// would remain under a real directory share a basename token with it, the
/// directory is the naming defect itself, and the synthesis dissolves it into
/// evidence-backed places instead of leaving a misleading label over bonded
/// company. Same majority semantics the eval harness's `name_alignment`
/// verdict applies, so synthesis and measurement agree on what "misnamed"
/// means.
const ROOF_COHERENCE_FLOOR: f64 = 0.5;

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
    /// Whether each file-graph vertex is a test-zone file under the `[tests]`
    /// policy — polarity detection plus configured patterns. Drives the shadow
    /// pass that follows subjects.
    test_zone: Vec<bool>,
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
    /// The production-SLOC cap per file (`capacity.file`); the symbol polish
    /// (FIX08) vetoes any relocation that would push its destination file over
    /// it. The file polish never needed it — it moves whole files, whose SLOC
    /// travels with them.
    file_cap: u32,
    /// The objective coefficients for this mode.
    coefficients: Coefficients,
    /// The configured edge-kind weights pricing the cut term.
    weights: KindWeights,
    /// The identity partition (anchored mode on a cap-clean tree), else `None`.
    /// Cloned before relief, so it always mirrors the current tree exactly.
    identity: Option<Partition>,
    /// Whether relief left the search's real-directory partition identical to
    /// the current tree — the mode-independent "nothing changed yet" shape. The
    /// faithful candidate exit (FIX05) keys on this so greenfield reports an
    /// unchanged layout through the same truthful render anchored uses, instead
    /// of re-assembling reality and pricing its own fabrication.
    real_is_identity: bool,
    /// The relieved real-directory folder partition: each file SCC starts in
    /// the cluster of its current parent folder, except that an over-capacity
    /// real folder's SCCs are pre-split along priced connectivity into
    /// cap-respecting halves ([`relieve_over_capacity`]). Folders stay reality,
    /// so this is still the one folder-grain start every non-identity seed
    /// shares; only the identity entry bypasses it.
    real_partition: Partition,
    /// Each relieved folder cluster's directory name, indexed by cluster id;
    /// split halves extend the table with `{folder}/{token}` labels that path-
    /// extend their base folder at render time.
    real_folder_names: Vec<SmolStr>,
    /// Whether each relieved folder cluster is the synthetic `workspace`
    /// bucket, indexed by cluster id in lockstep with `real_folder_names`.
    /// Carries the current tree's collapse marker onto the candidate folder it
    /// induces; split halves are always real places, so they carry `false`.
    real_folder_synthetic: Vec<bool>,
    /// The configured base seed; seed offset 0 selects the identity entry.
    base_seed: u64,
    /// The current root's name, reused for candidate package groups.
    root_name: SmolStr,
    /// The per-file facts narration consults when explaining moves.
    facts: FileFacts,
    /// FIX09 (naming-incoherence): the synthesized roof-rebuild start, present
    /// exactly when some real folder hosts a mixed population — bonded files
    /// plus a token-coherent group of zero-priced strangers — under a roof the
    /// evidence says it does not describe. Seed offset 1 starts from this
    /// partition instead of reality, giving the pool a genuinely different
    /// shape that the ordinary polish/score path then ratifies or rejects; no
    /// other seed or fixture is perturbed.
    roof_rebuild: Option<Partition>,
}

/// Inventories the current tree's file containers in file-graph vertex order:
/// every file with its laminar home keys, ascending container id, paired with
/// the container-to-vertex index. Production SLOC is folded into each entry so
/// relief and narration can weigh files without re-walking the IR nodes.
fn file_inventory(ir: &IntermediateRepresentation) -> (Vec<FileInfo>, BTreeMap<u32, u32>) {
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
    (files, index_of)
}

impl<'a> PipelineSolver<'a> {
    /// Builds the solver, computing every seed-independent pipeline input once.
    fn new(
        snapshot: &'a Snapshot,
        config: &AnalyzeConfig,
        coefficients: Coefficients,
        seed_identity: bool,
        tests: &'a TestPolicy,
    ) -> Self {
        let ir = snapshot.ir();
        let weights = config.weights.kind_weights();
        let (files, index_of) = file_inventory(ir);

        let test_zone = test_zone_marks(tests, &files, &ir.nodes);

        let file_graph = build_file_graph(
            &ir.edges,
            &ir.nodes,
            &index_of,
            files.len(),
            &weights,
            &test_zone,
        );
        let condensation = condense(&file_graph);
        let caps = level_caps(config);
        let reverse_dag = reverse_csr(&condensation.dag);

        // folders are reality: the identity layout and the search's folder
        // partition start as the same object — each file SCC in its real
        // directory — so anchored seeding just clones it.
        let (identity_partition, identity_names, identity_synthetic) =
            real_dir_partition(&files, &condensation);
        let identity = seed_identity.then(|| identity_partition.clone());

        // FIX03 relieves over-capacity binding in the search grain itself: a
        // real folder holding more files than its budget is pre-split along
        // priced connectivity into cap-respecting halves named to path-extend
        // their base folder. The objective prices what this creates, so every
        // non-identity seed starts from a layout the split can win from instead
        // of only being able to shed files out of the over-cap folder.
        let (relieved_files, search_partition, mut folder_names, mut folder_synthetic) =
            relieve_over_capacity(
                files,
                &condensation,
                &file_graph,
                &identity_partition,
                &identity_names,
                &identity_synthetic,
                caps.folder,
            );
        // FIX09 (naming-incoherence): a misnamed roof is invisible to edge-driven
        // search — the strangers under it carry no priced edge to pull them out,
        // so every seed converges on the same welded layout and no candidate can
        // ever propose the split. Where the signature fires, this synthesizes one
        // alternative start: zero-priced strangers regrouped by their own shared
        // tokens, a residual roof that misdescribes its remaining residents
        // dissolved into priced-connected places, each new place labeled from
        // member names to path-extend its base folder.
        // The proposal enters the ordinary pool at offset 1 — polish still runs,
        // the vetoes still bind, the objective still decides — and when it does
        // not fire, nothing downstream changes at all.
        let roof_rebuild = synthesize_roof_rebuild(
            &relieved_files,
            &condensation,
            &file_graph,
            &test_zone,
            &search_partition,
            &mut folder_names,
            &mut folder_synthetic,
        );
        let root_name = ir
            .containers
            .containers()
            .iter()
            .find(|container| container.level == ScopeLevel::PackageGroup)
            .map_or_else(|| SmolStr::new("workspace"), |group| group.name.clone());
        let facts = file_facts(
            snapshot,
            &weights,
            config.capacity.folder,
            &relieved_files,
            &test_zone,
        );
        let real_is_identity = search_partition == identity_partition;

        Self {
            snapshot,
            test_zone,
            files: relieved_files,
            index_of,
            condensation,
            reverse_dag,
            caps,
            file_cap: config.capacity.file,
            coefficients,
            weights,
            identity,
            real_is_identity,
            real_partition: search_partition,
            real_folder_names: folder_names,
            real_folder_synthetic: folder_synthetic,
            base_seed: config.analysis.seed,
            root_name,
            facts,
            roof_rebuild,
        }
    }

    /// Scores the five-level layout `parts` induces under this mode's
    /// coefficients.
    fn evaluate(&self, parts: &Partition) -> f64 {
        let assembled = self.assemble(parts);
        let placement = |id: u32| assembled.placement.get(&id).copied();
        // the assembly's placement maps every node to its current file's
        // candidate id, so this distance is pure file-grain movement.
        let distance = move_distance(self.snapshot, &assembled.tree, &placement);
        let candidate = score_candidate(
            self.snapshot,
            &placement,
            &assembled.tree,
            distance,
            self.caps.folder,
        );
        score(&candidate, &self.coefficients, &self.weights).total
    }

    /// The J(T)-polish pass: sweeps every file SCC in deterministic order and
    /// greedily relocates it to the strongest-pulling folder whenever the move
    /// strictly lowers the full objective. Capacity (files per
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
                    // FIX05 (WS-D anchored-inversion): a bridge is not a member of
                    // the thing it bridges. When an SCC's priced edges reach a
                    // folder besides the pair (current, target) — main.py importing
                    // three features, pipeline.py bridging billing and telemetry —
                    // absorbing it into one side strands the rest of its boundary,
                    // yet every locally-scored statistic of the absorber improves:
                    // the adopted edges drop to folder height while the abandoned
                    // ones keep whatever height they already had. The objective
                    // alone therefore ratifies the absorption and greenfield
                    // out-churns anchored, inverting the product promise (the
                    // eval corpus's inversion witness). The veto is structural,
                    // not scored: it fires before evaluation, needs no reference
                    // to the current layout, and so binds both modes equally —
                    // the FIX04 pattern of enforcing contract intent where
                    // admission cannot see it. Zero-priced edges nominate nothing
                    // here either: they never bind placement.
                    if self.absorbs_a_foreign_anchor(parts, scc32, source, target) {
                        continue;
                    }
                    // FIX05 (companion veto): the mirror image of bridge
                    // absorption. An SCC whose current folder pulls at least as
                    // hard as the destination is being torn from measured
                    // company for speculative proximity — the channel that
                    // survived the bridge veto: with the facade unabsorbable,
                    // greedy polish instead walked the feature members out of
                    // their real directories toward it, one transiently cheap
                    // step at a time. Relocation is honest only when the
                    // destination out-pulls what would be stranded (the
                    // satellite joining its sole anchor); a tie resolves to
                    // staying, because folders are reality until priced
                    // evidence says otherwise.
                    if self.strands_a_comparable_anchor(parts, scc32, source, target) {
                        continue;
                    }
                    // FIX05 (third veto): the synthetic bucket is not a place.
                    // `workspace` is the fallback name for files whose real
                    // directory is the project root — an absence of structure,
                    // not a structure. Once the first two vetoes sealed the
                    // feature folders, greedy polish found the remaining exit:
                    // feature members fleeing their real directories INTO the
                    // bucket, because sitting beside the unabsorbable hub
                    // cheapens their hub edges while the bucket prices nothing
                    // back. That flight is the collapse defect itself (real
                    // directories swallowed by an invented container). A file
                    // with priced company in its own folder therefore may not
                    // relocate into the bucket at all; only files reality left
                    // loose belong there, and they are already home.
                    if self.flees_into_the_synthetic_bucket(parts, scc32, source, target) {
                        continue;
                    }
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
    /// first, ties broken by the lower cluster id. Zero-priced edges nominate
    /// nothing: they never bind placement (the FIX04 doctrine), so a folder
    /// connected only through re-exports is never offered as a move target.
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
                let weight = weights.get(slot).copied().unwrap_or(0.0);
                if weight <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                *pull.entry(cluster).or_insert(0.0) += f64::from(weight);
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

    /// The shadow pass: every test-zone SCC follows its unique source twin.
    ///
    /// The tie-cut left test files with zero-priced edges and zero production
    /// SLOC, so the polish had no priced reason to move them — they sit where
    /// reality put them while everything around them re-groups. Left there, a
    /// spec strands away from the subject it exists to test once that subject
    /// relocates. This pass closes the gap by placement, not price: each
    /// test-zone file pairs with the one production file it was named after
    /// (same package home, same stem once the test markers strip away), and
    /// its whole SCC moves into the twin's cluster — production placements
    /// themselves never move here. The move serves exactly the coupling the
    /// cut removed from pricing, so acceptance is structural only: the folder
    /// cap holds and the quotient gains no cycles, mirroring the polish's
    /// vetoes. A file with no twin, or more than one equally-named candidate,
    /// stays put (ADR-0002 rule 1: unchanged placements are never advice).
    /// Shadow-assigns test-zone files to their unique subject twin's placement.
    ///
    /// Runs after the polish so a test file follows the placement its subject
    /// actually earned. Production placements are never moved: only whole-SCC
    /// units made up entirely of test-zone files are relocated (an SCC mixing
    /// production members is a production unit and stays where the search put
    /// it), and the folder cap vetoes an overflowing move exactly like it does
    /// for polish moves.
    ///
    /// No cyclicity veto: every edge touching a test-zone file is priced zero
    /// (the tie-cut in [`build_file_graph`]), so a moved unit carries no priced
    /// edges at all — the *priced* quotient the objective reads is invariant
    /// under this pass, and FIX04 doctrine says a zero-priced edge binds
    /// nothing, the cyclicity veto included. The structural quotient may still
    /// cycle through cut edges (`spec` → `support` → subject), which is exactly
    /// the weld this pass exists to undo; counting it would veto every follow
    /// in a mirrored test tree.
    fn shadow_tests(&self, parts: &mut Partition) {
        // live per-cluster file counts, mirroring the polish's bookkeeping:
        // clusters size in SCCs, caps count in files.
        let mut file_count: Vec<u32> = vec![0; parts.cluster_count()];
        for (scc, members) in self.condensation.members.iter().enumerate() {
            let Some(cluster) = parts.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX)) else {
                continue;
            };
            if let Some(slot) = file_count.get_mut(cluster.0 as usize) {
                *slot = slot.saturating_add(u32::try_from(members.len()).unwrap_or(u32::MAX));
            }
        }

        for (vertex, zone) in self.test_zone.iter().enumerate() {
            if !zone {
                continue;
            }
            let Some(file) = self.files.get(vertex) else {
                continue;
            };
            let Some(twin_vertex) = self.unique_subject_twin(file) else {
                continue;
            };
            let unit = self
                .condensation
                .membership
                .get(vertex)
                .copied()
                .map_or(u32::MAX, |scc| scc.0);
            // never shadow-move an SCC that carries production code: its
            // placement was earned by the priced search, not by the tie-cut.
            let unit_members = self.condensation.members.get(unit as usize);
            if unit_members.is_none_or(|unit_members| {
                unit_members.iter().any(|member| {
                    !self
                        .test_zone
                        .get(member.0 as usize)
                        .copied()
                        .unwrap_or(true)
                })
            }) {
                continue;
            }
            let subject = self
                .condensation
                .membership
                .get(twin_vertex)
                .copied()
                .map_or(u32::MAX, |scc| scc.0);
            let (Some(source), Some(target)) = (parts.cluster_of(unit), parts.cluster_of(subject))
            else {
                continue;
            };
            if source == target {
                continue;
            }
            let unit_files = self
                .condensation
                .members
                .get(unit as usize)
                .map_or(0, |unit_scc_files| {
                    u32::try_from(unit_scc_files.len()).unwrap_or(u32::MAX)
                });
            let target_files = file_count
                .get(target.0 as usize)
                .copied()
                .unwrap_or(u32::MAX);
            if target_files.saturating_add(unit_files) > self.caps.folder {
                continue;
            }
            if !parts.move_node(unit, target) {
                continue;
            }
            if let Some(slot) = file_count.get_mut(source.0 as usize) {
                *slot = slot.saturating_sub(unit_files);
            }
            if let Some(slot) = file_count.get_mut(target.0 as usize) {
                *slot = slot.saturating_add(unit_files);
            }
        }
    }

    /// Returns the file-graph vertex of the unique production-zone file this
    /// test-zone file was named after: same package home, same stem once the
    /// test markers strip away. Two equally-named candidates are no twin at
    /// all — ambiguity keeps the test file where reality has it.
    fn unique_subject_twin(&self, file: &FileInfo) -> Option<usize> {
        let stem = subject_stem(&file.name);
        if stem.is_empty() {
            return None;
        }
        let mut twin: Option<usize> = None;
        for (index, candidate) in self.files.iter().enumerate() {
            if candidate.container == file.container
                || candidate.home.package != file.home.package
                || self.test_zone.get(index).copied().unwrap_or(true)
                || subject_stem(&candidate.name) != stem
            {
                continue;
            }
            if twin.is_some() {
                return None;
            }
            twin = Some(index);
        }
        twin
    }

    /// Whether relocating `scc` from `source` into `target` would leave part of
    /// its priced neighborhood behind in some third folder — the bridge-absorption
    /// shape the polish veto bars (see the FIX05 comment at the call site). An
    /// SCC every one of whose priced edges terminates inside {source, target} is
    /// consolidating; one that also reaches elsewhere is orchestrating.
    fn absorbs_a_foreign_anchor(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                if weights.get(slot).copied().unwrap_or(0.0) <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement,
                    // so it cannot anchor a bridge either.
                    continue;
                }
                let Some(cluster) = parts.cluster_of(neighbour) else {
                    continue;
                };
                if cluster != source && cluster != target {
                    return true;
                }
            }
        }
        false
    }

    /// Whether relocating `scc` from `source` into `target` would strand priced
    /// pull in `source` at least equal to what awaits in `target` — the
    /// tearing-side veto (see the FIX05 companion comment at the call site).
    /// Only strictly stronger destinations justify leaving.
    fn strands_a_comparable_anchor(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        let mut stranded = 0.0_f64;
        let mut awaiting = 0.0_f64;
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                let weight = f64::from(weights.get(slot).copied().unwrap_or(0.0));
                if weight <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                match parts.cluster_of(neighbour) {
                    Some(cluster) if cluster == source => stranded += weight,
                    Some(cluster) if cluster == target => awaiting += weight,
                    _ => {}
                }
            }
        }
        awaiting <= stranded
    }

    /// Whether `target` is the synthetic `workspace` bucket and `scc` would have
    /// to abandon priced company in its own folder to get there — the
    /// bucket-flight veto (see the FIX05 third-veto comment at the call site).
    fn flees_into_the_synthetic_bucket(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        if !self
            .real_folder_synthetic
            .get(target.0 as usize)
            .copied()
            .unwrap_or(false)
        {
            return false;
        }
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                if weights.get(slot).copied().unwrap_or(0.0) <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                if parts.cluster_of(neighbour) == Some(source) {
                    return true;
                }
            }
        }
        false
    }

    /// The FIX08 symbol-grain polish pass: sweeps every symbol over the
    /// already-polished layout's EXISTING files and greedily relocates each to
    /// the file whose residents pull it hardest whenever the move strictly
    /// lowers the full objective J under this mode's coefficients. Trees never
    /// change — only narrations grow — because v1 constrains relocation to
    /// between existing files.
    ///
    /// Acceptance rides the same veto family as the file polish, adapted to
    /// symbol grain (all structural vetoes fire before any evaluation):
    ///
    /// - **no empty shells** — the origin file must retain at least one
    ///   production symbol; draining a file whole would be a file move wearing
    ///   a symbol costume, and v1 does not propose those;
    /// - **source/test boundary** — a relocation whose origin and destination
    ///   sit on opposite sides of the test zone is barred outright (FIX11), in
    ///   either direction; the incidence map already zero-prices every
    ///   zone-touching edge so spec twins nominate nothing in the first place;
    /// - **SLOC cap** — the destination file must absorb the symbol's
    ///   production SLOC without breaching `capacity.file`;
    /// - **no new cycles** — relative, like the file pass: barred only when the
    ///   crossing graph over effectively-placed symbols grows its cyclic
    ///   population against the pass's own baseline;
    /// - **visibility floor** — the derived-visibility over-export finding
    ///   count must not grow: pulling a symbol out of the LCA of its consumers
    ///   must not manufacture an over-export;
    /// - **strict J improvement** — ties resolve to staying.
    ///
    /// Zero-priced edges nominate nothing ([`absorbs_a_foreign_anchor`]'s FIX04
    /// doctrine at symbol grain): an edge priced 0.0 never binds placement, so
    /// it neither pulls a symbol nor counts toward a destination's pull.
    ///
    /// Deterministic end to end: symbols sweep in ascending id order,
    /// destinations rank by summed two-way pull with ties broken toward the
    /// lower file id, and every tie elsewhere resolves to staying. The pass is
    /// skipped entirely on identity-equal layouts — `solve` routes those to the
    /// faithful exit before this runs, so "already optimal" never fabricates
    /// movement (FIX05/D-47).
    fn symbol_polish(&self, parts: &Partition) -> SymbolOutcome {
        let ir = self.snapshot.ir();
        let assembled = self.assemble(parts);
        let mut pass = SymbolPass::new(
            self.snapshot,
            &self.coefficients,
            &self.weights,
            self.caps.folder,
            self.file_cap,
            &assembled,
            &ir.nodes,
            &ir.edges,
        );
        pass.run();
        SymbolOutcome {
            overlay: pass.overlay,
            relocations: pass.relocations,
            total: pass.best,
        }
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
            score: score_current(
                self.snapshot,
                &self.coefficients,
                &self.weights,
                self.caps.folder,
            )
            .total,
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
                zone_by_file: BTreeMap::new(),
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
        let mut zone_by_file: BTreeMap<ContainerId, bool> = BTreeMap::new();
        for (&folder, members) in members_of {
            let Some(&(domain, _, _)) = chain_of.get(&folder) else {
                continue;
            };
            // folders are reality: the cluster keeps its full real key — one
            // container per distinct real location with an injective name
            // (`qualify_folder_names`), so sibling folders never collide and no
            // synthetic `-N` twin can arise. A key that doesn't path-extend its
            // elected domain displays relative to its enclosing package at the
            // render boundary (`folder_increment`), the honest display of a
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
                zone_by_file.insert(
                    id,
                    self.test_zone
                        .get(vertex as usize)
                        .copied()
                        .unwrap_or(false),
                );
            }
        }

        CandidateTree {
            tree: ContainerTree::new(arena.containers),
            placement: self.placements(&file_ids),
            zone_by_file,
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
        // FIX05: the faithful exit keys on the layout being reality, not on the
        // analysis mode. Gating it on `identity` alone (anchored-only) meant a
        // greenfield candidate whose partition is byte-for-byte the real
        // directory layout still went through assemble() — which re-elects
        // upper-level containers and nests them differently from the current
        // tree — so "change nothing" rendered as structural moves for every
        // file whose fabricated chain differed. That phantom churn is what
        // inverted anchored against greenfield: the unbiased mode was billed
        // for movement it never proposed. When relief split an over-capacity
        // folder, the search's start is no longer reality, so the assemble
        // path stays (the split is exactly what the proposal must show).
        let faithful = self.identity.as_ref() == Some(&solved.partition)
            || (self.real_is_identity && self.real_partition == solved.partition);
        if faithful {
            let breakdown = score_current(
                self.snapshot,
                &self.coefficients,
                &self.weights,
                self.caps.folder,
            );
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
                symbol_moves: Vec::new(),
                capacity_remainder: None,
            });
        }

        let assembled = self.assemble(&solved.partition);
        // FIX08: re-run the deterministic symbol pass on this exact partition.
        // `solve` already priced its result into the ranking score, so the DTO
        // score here matches what ranked this candidate by construction.
        let symbols = self.symbol_polish(&solved.partition);
        let merged = |id: u32| {
            symbols
                .overlay
                .get(&id)
                .copied()
                .or_else(|| assembled.placement.get(&id).copied())
        };
        let distance = move_distance(self.snapshot, &assembled.tree, &merged);
        let breakdown = score(
            &score_candidate(
                self.snapshot,
                &merged,
                &assembled.tree,
                distance,
                self.caps.folder,
            ),
            &self.coefficients,
            &self.weights,
        );
        let placement_of = |node: &Node| merged(node.id.0);
        let node = render_tree(&assembled.tree, nodes, &placement_of, &assembled.key_by_id)?;
        let delta = narrate(current_tree, &assembled.tree, &self.facts);
        let symbol_moves = self.symbol_narrate(&assembled, &symbols);

        Ok(Candidate {
            index,
            score: breakdown.total,
            score_breakdown: ScoreBreakdown::from(breakdown),
            improvement: 0.0,
            tree: node,
            conditional_splits: splits.to_vec(),
            delta_narration: delta,
            symbol_moves,
            capacity_remainder: None,
        })
    }

    /// Narrates the accepted symbol relocations as scored [`SymbolMove`] DTOs.
    ///
    /// `from_path` reads the CURRENT tree's file name for the symbol's home
    /// container; `to_path` reads the candidate file's name off the assembled
    /// tree. `broken_imports` counts the distinct files other than origin and
    /// destination that house a direct caller or callee under the final
    /// overlay — those imports must be re-pointed once the move applies, while
    /// edges landing in the destination become co-location (no import at all)
    /// and edges inside the origin never cross a file boundary.
    fn symbol_narrate(
        &self,
        assembled: &CandidateTree,
        symbols: &SymbolOutcome,
    ) -> Vec<SymbolMove> {
        if symbols.relocations.is_empty() {
            return Vec::new();
        }
        let ir = self.snapshot.ir();
        let base = &assembled.placement;
        let effective = |id: u32| -> Option<ContainerId> {
            symbols
                .overlay
                .get(&id)
                .copied()
                .or_else(|| base.get(&id).copied())
        };
        let current_name: BTreeMap<u32, &str> = ir
            .containers
            .containers()
            .iter()
            .map(|container| (container.id.0, container.name.as_str()))
            .collect();
        let candidate_name: BTreeMap<u32, &str> = assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id.0, container.name.as_str()))
            .collect();
        let kind_of = |node: &Node| match node.kind {
            strata_ir::NodeKind::Symbol => SymbolKind::Symbol,
            strata_ir::NodeKind::Type => SymbolKind::Type,
        };

        let by_node: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();
        let mut moves = Vec::with_capacity(symbols.relocations.len());
        for relocation in &symbols.relocations {
            let Some(symbol) = by_node.get(&relocation.node).copied() else {
                continue;
            };
            let Some(&home) = base.get(&relocation.node) else {
                continue;
            };
            debug_assert_eq!(
                home, relocation.from_file,
                "origin file is the symbol's base placement"
            );
            let mut severed: BTreeSet<ContainerId> = BTreeSet::new();
            for edge in &ir.edges {
                let neighbour = if edge.source.0 == relocation.node {
                    edge.target.0
                } else if edge.target.0 == relocation.node {
                    edge.source.0
                } else {
                    continue;
                };
                let Some(place) = effective(neighbour) else {
                    continue;
                };
                if place == relocation.to_file || place == relocation.from_file || place == home {
                    continue;
                }
                severed.insert(place);
            }
            moves.push(SymbolMove {
                symbol: symbol.name.to_string(),
                kind: kind_of(symbol),
                from_path: current_name
                    .get(&symbol.container.0)
                    .copied()
                    .unwrap_or_default()
                    .to_owned(),
                to_path: candidate_name
                    .get(&relocation.to_file.0)
                    .copied()
                    .unwrap_or_default()
                    .to_owned(),
                delta: -relocation.delta,
                broken_imports: u32::try_from(severed.len()).unwrap_or(u32::MAX),
            });
        }
        moves
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
        // identity plus one improvement candidate. FIX09 re-sources diversity at
        // exactly one point: when the naming-incoherence signature fired at
        // construction, offset 1 starts from the synthesized roof rebuild and
        // runs the identical polish/score path on it, so the pool carries a
        // genuinely different shape that the objective ratifies or rejects like
        // any other. Every other offset polishes reality unchanged.
        let start = match (&self.roof_rebuild, offset) {
            (Some(rebuild), 1) => rebuild,
            _ => &self.real_partition,
        };
        let mut parts = start.clone();
        self.polish(&mut parts);
        // The shadow pass runs on the polished layout so a test file follows
        // the placement its subject actually earned, not the one reality
        // suggested; production placements are never moved by it.
        self.shadow_tests(&mut parts);
        // FIX08: the file polish's layout is refined by the symbol-grain pass
        // before scoring, so pool ranking prices symbol relocation too. The
        // outcome itself is not threaded out — `build_candidate` re-runs this
        // pure, deterministic pass on the identical partition and gets the
        // identical overlay, so ranking score and DTO score agree by
        // construction. (The polish's own total is subsumed: the symbol pass
        // re-prices the identical layout before improving on it.)
        let symbols = self.symbol_polish(&parts);
        self.finish(parts, symbols.total)
    }
}

/// One symbol relocation the FIX08 symbol polish accepted, in candidate-tree
/// file ids. `delta` is the strict J improvement it earned at acceptance time.
struct SymbolRelocation {
    /// The relocated node's id.
    node: u32,
    /// The candidate file container the symbol leaves.
    from_file: ContainerId,
    /// The candidate file container the symbol joins.
    to_file: ContainerId,
    /// The objective improvement contributed (positive; J drops by this much).
    delta: f64,
}

/// The outcome of one symbol polish pass: the effective placement overlay, the
/// accepted relocations in acceptance order, and the final objective total.
struct SymbolOutcome {
    /// Node id → candidate file container for relocated symbols only.
    overlay: BTreeMap<u32, ContainerId>,
    /// Accepted relocations in acceptance order.
    relocations: Vec<SymbolRelocation>,
    /// The objective total after all accepted relocations.
    total: f64,
}

/// A reconstructed candidate tree plus the placement of every symbol node.
struct CandidateTree {
    /// The candidate container tree: package groups over packages, domains, and
    /// folders derived per level, each folder holding whole current files.
    tree: ContainerTree,
    /// The file container each symbol node lands in, keyed by node id.
    placement: BTreeMap<u32, ContainerId>,
    /// Whether each candidate file sits inside the test zone (FIX11). Candidate
    /// file ids are fresh arena ids, so the file graph's vertex-parallel zone
    /// marks cannot be consulted directly at symbol grain — they ride here,
    /// populated where files are emitted, so every grain shares one boundary.
    zone_by_file: BTreeMap<ContainerId, bool>,
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
/// Marks each file-graph vertex whose clustering edges are priced to zero.
/// Built-in detection marks a file that holds at least one symbol and nothing
/// but non-production symbols — test cases or test support — reusing the
/// polarity adapters already compute; configured `[tests]` patterns mark a
/// file by repo-relative path regardless of its symbols. Disabling builtins
/// leaves only the patterns to decide. The returned slice is parallel to
/// `files`, i.e. to the file graph's vertices.
fn test_zone_marks(tests: &TestPolicy, files: &[FileInfo], nodes: &[Node]) -> Vec<bool> {
    let mut marks: Vec<bool> = files.iter().map(|file| tests.matches(&file.name)).collect();
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

/// Strips the test markers from a path's basename down to its subject stem:
/// the final extension goes (`openai.spec.ts` → `openai.spec`), then a
/// `.spec`/`.test` infix (`openai.spec` → `openai`), then `test_`/`_test`
/// affixes (`test_openai`, `openai_test` → `openai`). Comparison is
/// byte-wise and conservative: anything that does not reduce cleanly pairs
/// with nothing.
fn subject_stem(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    let mut stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
    if let Some(without_marker) = stem
        .strip_suffix(".spec")
        .or_else(|| stem.strip_suffix(".test"))
    {
        stem = without_marker;
    }
    let stem = stem.strip_prefix("test_").unwrap_or(stem);
    stem.strip_suffix("_test").unwrap_or(stem)
}

fn build_file_graph(
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

/// Builds the per-file facts narration consults: config-priced edge weights
/// summed per directed file pair (every edge, the objective's currency),
/// spec files (symbols exclusively test cases), the rest of the tie-cut zone
/// (`[tests]`-pattern matches and test-support helpers), and the folder cap.
fn file_facts(
    snapshot: &Snapshot,
    weights: &KindWeights,
    folder_cap: u32,
    files: &[FileInfo],
    test_zone: &[bool],
) -> FileFacts {
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

    // the tie-cut zone beyond the case-only set: pattern-marked paths and
    // support-polarity helpers narrate their moves as following a subject
    // exactly like spec files do.
    let mut shadow_test_files = BTreeSet::new();
    for (index, file) in files.iter().enumerate() {
        let zone = test_zone.get(index).copied().unwrap_or(false);
        let only_cases = case_only.get(&file.container).copied().unwrap_or(false);
        if zone && !only_cases {
            shadow_test_files.insert(file.name.to_string());
        }
    }

    FileFacts {
        edge_weights,
        test_case_files,
        shadow_test_files,
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

/// Splits every over-capacity real folder cluster into cap-respecting halves
/// and overrides each split file's domain home key so the halves stay apart.
///
/// The split runs on priced connectivity between the folder's SCCs: two SCCs
/// end up in the same pile only when a positively-priced file edge joins them
/// ([`connected_piles`]), so each half stays internally connected and the
/// quotient DAG never gains a cycle (whole SCCs always move together). Only
/// evidence-backed halves split off: a pile qualifies when its members are
/// actually joined (at least two SCCs or two files); unrelated singletons stay
/// glued to the original cluster, because inventing halves for unconnected
/// files would erase the merge pressure that cross-folder coupling otherwise
/// exerts on the search. A connected pile that alone exceeds the budget is
/// chunked contiguously along its own connectivity; a single oversized SCC
/// cannot be split at all and is left whole — the objective still prices it,
/// which is honest.
///
/// Pile 0 of each folder keeps the folder's original cluster id; every later
/// pile gets a fresh cluster id, a `{folder}/{token}` name that path-extends
/// its base folder at render time, and a `false` synthetic marker. Members'
/// `home.domain` keys stay untouched: the halves already sit in one domain —
/// their base folder's — and the slash in the label is what nests the half
/// under it, so no home rewriting can weld or split anything here. Returns the
/// files plus the relieved partition and its extended tables; the
/// identity entry must be cloned before this runs (the caller does).
fn relieve_over_capacity(
    files: Vec<FileInfo>,
    condensation: &Condensation,
    graph: &Csr,
    base: &Partition,
    base_names: &[SmolStr],
    base_synthetic: &[bool],
    cap: u32,
) -> (Vec<FileInfo>, Partition, Vec<SmolStr>, Vec<bool>) {
    let mut assignment = base.assignment().to_vec();
    let mut names = base_names.to_vec();
    let mut synthetic = base_synthetic.to_vec();
    if cap == 0 {
        // a disabled budget binds nothing, so there is nothing to relieve.
        return (files, base.clone(), names, synthetic);
    }
    let base_count = base.cluster_count();
    let mut next_cluster = u64::from(u32::try_from(base_count).unwrap_or(u32::MAX));
    let mut used_labels: BTreeSet<SmolStr> = names.iter().cloned().collect();
    for cluster_index in 0..base_count {
        if next_cluster > u64::from(u32::MAX) {
            break;
        }
        let cluster = ClusterId(u32::try_from(cluster_index).unwrap_or(u32::MAX));
        let sccs: Vec<u32> = (0..u32::try_from(condensation.members.len()).unwrap_or(u32::MAX))
            .filter(|scc| base.cluster_of(*scc) == Some(cluster))
            .collect();
        let bound: u32 = sccs
            .iter()
            .map(|&scc| scc_file_count(condensation, scc))
            .sum();
        if bound <= cap {
            continue;
        }
        let piles: Vec<Vec<u32>> = connected_piles(&sccs, condensation, graph, cap)
            .into_iter()
            .filter(|pile| {
                // evidence rule: only a joined pile earns its own half.
                pile.len() >= 2
                    || pile
                        .first()
                        .is_some_and(|&scc| scc_file_count(condensation, scc) >= 2)
            })
            .collect();
        if piles.is_empty() {
            continue;
        }
        let base_name = base_names
            .get(cluster_index)
            .cloned()
            .unwrap_or_else(|| SmolStr::new("workspace"));
        for (ordinal, pile) in piles.iter().enumerate().skip(1) {
            let token = elect_split_token(pile, condensation, &files);
            // half names path-extend the real directory so they render nested
            // under it and can never collide with it or a sibling; a repeated
            // dominant token falls back to the numeric form.
            let mut label = token.map_or_else(
                || format!("{base_name}/{ordinal}"),
                |stem| format!("{base_name}/{stem}"),
            );
            if used_labels.contains(label.as_str()) {
                label = format!("{base_name}/{ordinal}");
            }
            used_labels.insert(SmolStr::from(label.clone()));
            for &scc in pile {
                if let Some(slot) = assignment.get_mut(scc as usize) {
                    *slot = ClusterId(u32::try_from(next_cluster).unwrap_or(u32::MAX));
                }
            }
            names.push(SmolStr::from(label));
            synthetic.push(false);
            next_cluster += 1;
        }
    }
    (
        files,
        Partition::from_assignment(
            assignment,
            usize::try_from(next_cluster).unwrap_or(base_count),
        ),
        names,
        synthetic,
    )
}

/// Counts the files an SCC holds.
fn scc_file_count(condensation: &Condensation, scc: u32) -> u32 {
    condensation.members.get(scc as usize).map_or(0, |members| {
        u32::try_from(members.len()).unwrap_or(u32::MAX)
    })
}

/// Follows `parent` links from `start` to its union-find root.
fn union_root(parent: &BTreeMap<u32, u32>, start: u32) -> u32 {
    let mut node = start;
    while let Some(&up) = parent.get(&node) {
        if up == node {
            return node;
        }
        node = up;
    }
    start
}

/// Groups `sccs` into connectivity piles joined by positively-priced edges
/// between their member files: every connected component becomes one pile,
/// and a component that alone exceeds `cap` is chunked by breadth-first
/// traversal from its smallest SCC, closing a chunk just before the next SCC
/// would push it past the budget (a lone oversized SCC stays whole). No
/// packing happens across components — unrelated files must not be invented
/// into a shared half. Deterministic: CSR rows are ascending, union roots are
/// the smaller id, and traversal order follows ascending neighbor ids.
fn connected_piles(
    sccs: &[u32],
    condensation: &Condensation,
    graph: &Csr,
    cap: u32,
) -> Vec<Vec<u32>> {
    let wanted: BTreeSet<u32> = sccs.iter().copied().collect();
    let mut parent: BTreeMap<u32, u32> = wanted.iter().map(|&scc| (scc, scc)).collect();
    let mut adjacency: BTreeMap<u32, BTreeSet<u32>> =
        wanted.iter().map(|&scc| (scc, BTreeSet::new())).collect();
    for &scc in sccs {
        let Some(members) = condensation.members.get(scc as usize) else {
            continue;
        };
        for member in members {
            let vertex = member.0;
            for (&neighbor, weight) in graph.neighbors(vertex).iter().zip(graph.weights(vertex)) {
                if *weight <= 0.0 {
                    // zero-priced edges never bind placement (FIX04 doctrine).
                    continue;
                }
                let Some(&other_scc) = condensation.membership.get(neighbor as usize) else {
                    continue;
                };
                let other_scc = other_scc.0;
                if !wanted.contains(&other_scc) || other_scc == scc {
                    continue;
                }
                adjacency.entry(scc).or_default().insert(other_scc);
                adjacency.entry(other_scc).or_default().insert(scc);
                let (a, b) = (union_root(&parent, scc), union_root(&parent, other_scc));
                if a != b {
                    let (keep, move_) = if a <= b { (a, b) } else { (b, a) };
                    parent.insert(move_, keep);
                }
            }
        }
    }

    // gather connected components, each ordered ascending by SCC id.
    let mut components: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &scc in sccs {
        components
            .entry(union_root(&parent, scc))
            .or_default()
            .push(scc);
    }

    let mut piles: Vec<Vec<u32>> = Vec::new();
    for members in components.values_mut() {
        members.sort_unstable();
        let total: u32 = members
            .iter()
            .map(|&scc| scc_file_count(condensation, scc))
            .sum();
        let chunks: Vec<Vec<u32>> = if total <= cap {
            vec![members.clone()]
        } else {
            let mut visited: BTreeSet<u32> = BTreeSet::new();
            let mut queue: std::collections::VecDeque<u32> =
                members.first().copied().into_iter().collect();
            let mut chunk: Vec<u32> = Vec::new();
            let mut chunk_size = 0_u32;
            let mut pieces: Vec<Vec<u32>> = Vec::new();
            while let Some(scc) = queue.pop_front() {
                if !visited.insert(scc) {
                    continue;
                }
                let scc_size = scc_file_count(condensation, scc);
                if !chunk.is_empty() && chunk_size + scc_size > cap {
                    pieces.push(std::mem::take(&mut chunk));
                    chunk_size = 0;
                }
                chunk.push(scc);
                chunk_size += scc_size;
                for neighbor in adjacency.get(&scc).into_iter().flatten() {
                    if !visited.contains(neighbor) {
                        queue.push_back(*neighbor);
                    }
                }
            }
            if !chunk.is_empty() {
                pieces.push(chunk);
            }
            pieces
        };
        piles.extend(chunks);
    }
    piles
}

/// Elects a split-half suffix token from the dominant alphabetic basename stem
/// among the pile's files, weighted by production SLOC then file count, with
/// lexicographic order breaking exact ties. Returns `None` when no file has a
/// usable stem, falling back to numeric labels.
fn elect_split_token(
    pile: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> Option<String> {
    #[derive(Clone, Copy, Default)]
    struct Tally {
        sloc: u64,
        count: u32,
    }
    let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
    for &scc in pile {
        let Some(members) = condensation.members.get(scc as usize) else {
            continue;
        };
        for member in members {
            let Some(file) = files.get(member.0 as usize) else {
                continue;
            };
            if let Some(token) = basename_stem(&file.name) {
                let entry = tally.entry(token).or_default();
                entry.sloc += u64::from(file.production_sloc);
                entry.count += 1;
            }
        }
    }
    tally
        .into_iter()
        .max_by(|a, b| {
            (a.1.sloc, a.1.count)
                .cmp(&(b.1.sloc, b.1.count))
                .then_with(|| b.0.cmp(&a.0))
        })
        .map(|(token, _)| token)
}

/// Extracts a file path's lowercase leading-alphabetic basename stem:
/// `hub/ingest_00.py` elects `ingest`, `emit_00.ts` elects `emit`. Numeric or
/// punctuation-only stems yield `None`.
fn basename_stem(name: &str) -> Option<String> {
    let basename = name.rsplit('/').next().unwrap_or(name);
    let stem = basename.split_once('.').map_or(basename, |(stem, _)| stem);
    let run: String = stem
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_lowercase();
    if run.is_empty() { None } else { Some(run) }
}

/// FIX09 (naming-incoherence): synthesizes one alternative search start that
/// rebuilds a misnamed real roof into evidence-backed places, returning the
/// rebuilt partition — or `None` when no folder carries the signature, leaving
/// every downstream byte unchanged.
///
/// The signature, per real non-synthetic folder cluster: a group of at least
/// two zero-priced files (no incident edge prices above zero anywhere in the
/// graph — D-46 makes them structurally unanchored, hence invisible to every
/// pull-driven move) whose basenames share tokens pairwise-connectedly, hosted
/// alongside at least one other file. Those strangers are exactly the
/// population edge-driven relocation can never nominate, so without synthesis
/// the pool collapses onto layouts that keep them welded under a label that
/// describes someone else.
///
/// The rebuild follows the [`relieve_over_capacity`] pattern: whole SCCs move
/// (the quotient DAG never gains a cycle), each new place gets a fresh cluster
/// id and a `{folder}/…` name grounded in member names that path-extends its
/// base folder at render time; member `home.domain` keys stay untouched. Two
/// shapes emerge, all deterministic:
///
/// 1. stranger groups leave for places named after their own shared word;
/// 2. when what remains covers fewer than half its files under the original
///    roof's name ([`ROOF_COHERENCE_FLOOR`]), the ENTIRE residual becomes one
///    rebuilt place named after its own heaviest member stems — a roof that
///    misdescribes its residents is replaced wholesale, not subdivided on
///    evidence the graph cannot price. The joined-stem name stays honest even
///    if polish later absorbs another consumer into the place: two of three
///    differently-named members still clear the majority floor.
///
/// Anything else — coherent residuals, lone stragglers, quiet all-unbonded
/// folders — stays put: naming alone never tears a folder that carries no
/// mixed-population signature.
// One function because the trigger, the stranger regroup, and the wholesale
// rebuild share one pass over the base partition; splitting them would either
// duplicate the bond scan or thread four pieces of mutable state through
// helpers. The length is documentation and the two place-naming branches.
#[allow(clippy::too_many_lines)]
fn synthesize_roof_rebuild(
    files: &[FileInfo],
    condensation: &Condensation,
    graph: &Csr,
    test_zone: &[bool],
    base: &Partition,
    names: &mut Vec<SmolStr>,
    synthetic: &mut Vec<bool>,
) -> Option<Partition> {
    // which file vertices carry priced company at all — the bond evidence the
    // trigger reads. Zero-priced edges stay in the graph but bind nothing
    // (D-46), so a vertex whose every incident edge prices zero is exactly the
    // "unnominatable" population this synthesis exists for.
    let mut bonded = vec![false; graph.vertex_count()];
    // CSR neighbors are valid vertex ids by construction — `Csr` admits only
    // in-range endpoints — so every slot below exists; `.get_mut` keeps the
    // bound explicit and the loop total.
    for vertex in 0..graph.vertex_count() {
        let from = u32::try_from(vertex).unwrap_or(u32::MAX);
        for (&neighbor, weight) in graph.neighbors(from).iter().zip(graph.weights(from)) {
            if *weight > 0.0 {
                if let Some(slot) = bonded.get_mut(vertex) {
                    *slot = true;
                }
                if let Some(slot) = bonded.get_mut(neighbor as usize) {
                    *slot = true;
                }
            }
        }
    }
    // an SCC is unanchored only when EVERY file inside is: whole SCCs move, so
    // partial bonds keep the component glued to its measured company. A member
    // id outside the bond table counts as bonded — absence of evidence of
    // unanchorage keeps the component put.
    let unbonded_scc = |scc: u32| -> bool {
        condensation
            .members
            .get(scc as usize)
            .is_some_and(|members| {
                members
                    .iter()
                    .all(|member| bonded.get(member.0 as usize).is_some_and(|slot| !*slot))
            })
    };

    let base_count = base.cluster_count();
    let mut assignment = base.assignment().to_vec();
    let mut used_labels: BTreeSet<SmolStr> = names.iter().cloned().collect();
    let mut next_cluster = u64::from(u32::try_from(names.len()).unwrap_or(u32::MAX));
    let mut fired = false;

    for cluster_index in 0..base_count {
        if next_cluster > u64::from(u32::MAX) {
            break;
        }
        if synthetic.get(cluster_index).copied().unwrap_or(false) {
            // the workspace bucket is an absence of structure, not a roof to
            // rebuild (the FIX05 doctrine).
            continue;
        }
        let cluster = ClusterId(u32::try_from(cluster_index).unwrap_or(u32::MAX));
        let sccs: Vec<u32> = (0..u32::try_from(condensation.members.len()).unwrap_or(u32::MAX))
            .filter(|&scc| base.cluster_of(scc) == Some(cluster))
            .collect();
        if sccs.is_empty() {
            continue;
        }

        // strangers: unanchored SCCs grouped by shared basename tokens, keeping
        // groups of at least two files — a lone stray earns no invented place.
        // FIX11: an all-test-zone SCC is the spec-twin population; zone edges
        // price zero so such an SCC always looks unanchored, and the roof
        // rebuild must never sweep it into an invented production place.
        let strangers: Vec<u32> = sccs
            .iter()
            .copied()
            .filter(|&scc| unbonded_scc(scc))
            .filter(|&scc| {
                condensation
                    .members
                    .get(scc as usize)
                    .is_some_and(|members| {
                        members.iter().all(|member| {
                            !test_zone.get(member.0 as usize).copied().unwrap_or(false)
                        })
                    })
            })
            .collect();
        let groups: Vec<Vec<u32>> = token_groups(&strangers, condensation, files)
            .into_iter()
            .filter(|group| {
                group
                    .iter()
                    .map(|&scc| scc_file_count(condensation, scc))
                    .sum::<u32>()
                    >= 2
            })
            .collect();
        if groups.is_empty() {
            continue;
        }
        let exiled: BTreeSet<u32> = groups.iter().flatten().copied().collect();
        let residual: Vec<u32> = sccs
            .iter()
            .copied()
            .filter(|scc| !exiled.contains(scc))
            .collect();
        let residual_files: u32 = residual
            .iter()
            .map(|&scc| scc_file_count(condensation, scc))
            .sum();
        let base_name = names
            .get(cluster_index)
            .cloned()
            .unwrap_or_else(|| SmolStr::new("workspace"));

        // strangers first, each a fresh cluster id.
        for group in &groups {
            let label = rebuild_label(
                &base_name,
                group,
                condensation,
                files,
                &mut used_labels,
                next_cluster,
            );
            for &scc in group {
                if let Some(slot) = assignment.get_mut(scc as usize) {
                    *slot = ClusterId(u32::try_from(next_cluster).unwrap_or(u32::MAX));
                }
            }
            names.push(label);
            synthetic.push(false);
            next_cluster += 1;
            fired = true;
        }

        // then the residual roof: when its own name covers fewer than half of
        // what remains, replace the roof wholesale with one place named after
        // the residents themselves. A lone straggler keeps the original roof —
        // one file under any name is vacuously covered.
        if residual_files >= 2
            && roof_coherence(&base_name, &residual, condensation, files) < ROOF_COHERENCE_FLOOR
        {
            let label = rebuild_label(
                &base_name,
                &residual,
                condensation,
                files,
                &mut used_labels,
                next_cluster,
            );
            for &scc in &residual {
                if let Some(slot) = assignment.get_mut(scc as usize) {
                    *slot = ClusterId(u32::try_from(next_cluster).unwrap_or(u32::MAX));
                }
            }
            names.push(label);
            synthetic.push(false);
            next_cluster += 1;
            fired = true;
        }
    }

    fired.then(|| {
        Partition::from_assignment(
            assignment,
            usize::try_from(next_cluster).unwrap_or(base_count),
        )
    })
}

/// Fraction of `member_files` whose basename shares a token with the last `/`
/// segment of `container_name` — the engine-side twin of the eval harness's
/// alignment metric, so the synthesis trigger and the verdict measure the same
/// coherence and can never disagree about what "misnamed" means.
fn roof_coherence(
    container_name: &str,
    residual: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> f64 {
    let last = container_name.rsplit('/').next().unwrap_or(container_name);
    let container_tokens = tokenize(last);
    let member_files: Vec<String> = residual
        .iter()
        .flat_map(|&scc| scc_file_names(condensation, files, scc))
        .collect();
    let total = member_files.len();
    if total == 0 {
        return 1.0;
    }
    let aligned = member_files
        .iter()
        .filter(|file| {
            let base = file.rsplit('/').next().unwrap_or(file.as_str());
            let stem = base.split('.').next().unwrap_or(base);
            tokenize(stem)
                .iter()
                .any(|token| container_tokens.contains(token))
        })
        .count();
    // reason: member counts are corpus-sized; the f64 mantissa loses nothing
    #[allow(clippy::cast_precision_loss)]
    let sharing = aligned as f64 / total as f64;
    sharing
}

/// Collects the file paths inside one SCC, ascending.
fn scc_file_names(condensation: &Condensation, files: &[FileInfo], scc: u32) -> Vec<String> {
    condensation
        .members
        .get(scc as usize)
        .into_iter()
        .flatten()
        .filter_map(|member| files.get(member.0 as usize))
        .map(|file| file.name.to_string())
        .collect()
}

/// Basename tokens of one file path, per the CONTRACT tokenization scope: the
/// basename minus extension only — the directory prefix never counts toward a
/// container's claim on a file.
fn basename_tokens(name: &str) -> BTreeSet<String> {
    let basename = name.rsplit('/').next().unwrap_or(name);
    let stem = basename.split('.').next().unwrap_or(basename);
    tokenize(stem)
}

/// Groups `sccs` into token-connected components: two SCCs join when any pair
/// of their files shares a basename token. This is naming evidence only — no
/// edge is priced or fabricated (D-46 holds: absence of priced edges is read
/// as separation evidence, and shared words group the separated). Deterministic:
/// ascending SCC pairs, union roots the smaller id, components emitted by
/// smallest member ascending, each sorted ascending.
fn token_groups(sccs: &[u32], condensation: &Condensation, files: &[FileInfo]) -> Vec<Vec<u32>> {
    let tokens_of: BTreeMap<u32, BTreeSet<String>> = sccs
        .iter()
        .map(|&scc| {
            let tokens: BTreeSet<String> = scc_file_names(condensation, files, scc)
                .iter()
                .flat_map(|name| basename_tokens(name))
                .collect();
            (scc, tokens)
        })
        .collect();
    let mut parent: BTreeMap<u32, u32> = sccs.iter().map(|&scc| (scc, scc)).collect();
    for (index, &left) in sccs.iter().enumerate() {
        for &right in sccs.iter().skip(index + 1) {
            let joined = tokens_of.get(&left).is_some_and(|left_tokens| {
                tokens_of
                    .get(&right)
                    .is_some_and(|right_tokens| !left_tokens.is_disjoint(right_tokens))
            });
            if joined {
                let (a, b) = (union_root(&parent, left), union_root(&parent, right));
                if a != b {
                    let (keep, move_) = if a <= b { (a, b) } else { (b, a) };
                    parent.insert(move_, keep);
                }
            }
        }
    }
    let mut components: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &scc in sccs {
        components
            .entry(union_root(&parent, scc))
            .or_default()
            .push(scc);
    }
    let mut groups: Vec<Vec<u32>> = components.into_values().collect();
    for group in &mut groups {
        group.sort_unstable();
    }
    groups
}

/// Names one proposed place after its members: a token shared by EVERY file in
/// the group wins (`helpers/utils`); otherwise the two heaviest distinct
/// basename stems join (`helpers/charge-refund`) — the [`join_top_two`]
/// honesty, so a name covering two stems survives later absorption of a third
/// differently-named file without falling under half-aligned. The separator
/// between base and suffix is a slash — the label path-extends its base folder
/// at render time — while a joined stem pair stays dash-joined inside the last
/// segment. Collisions fall back to the numeric form, mirroring
/// [`relieve_over_capacity`]. Returns the label inserted into `used`.
fn rebuild_label(
    base_name: &str,
    group: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
    used: &mut BTreeSet<SmolStr>,
    ordinal: u64,
) -> SmolStr {
    // per-file token sets, for the everyone-shares-it intersection.
    let per_file: Vec<BTreeSet<String>> = group
        .iter()
        .flat_map(|&scc| scc_file_names(condensation, files, scc))
        .map(|name| basename_tokens(&name))
        .collect();
    let common: Option<String> = per_file
        .first()
        .map(|first| {
            per_file.iter().skip(1).fold(first.clone(), |held, set| {
                held.intersection(set).cloned().collect()
            })
        })
        // an all-digit token would mint a label indistinguishable from the
        // numeric fallback (`helpers/2024` vs `helpers/3`) and would never
        // align under the contract tokenizer, which drops digit tokens — skip
        // it and let the stem path or the fallback name the place.
        .and_then(|tokens| {
            tokens
                .into_iter()
                .find(|token| token.chars().any(char::is_alphabetic))
        });
    let candidate = if let Some(token) = common {
        format!("{base_name}/{token}")
    } else {
        // heaviest distinct stems by production SLOC then stem order.
        #[derive(Default)]
        struct Tally {
            sloc: u64,
        }
        let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
        for &scc in group {
            let Some(members) = condensation.members.get(scc as usize) else {
                continue;
            };
            for member in members {
                let Some(file) = files.get(member.0 as usize) else {
                    continue;
                };
                let Some(stem) = basename_stem(&file.name) else {
                    continue;
                };
                tally.entry(stem).or_default().sloc += u64::from(file.production_sloc);
            }
        }
        let mut ranked: Vec<(String, u64)> =
            tally.into_iter().map(|(stem, t)| (stem, t.sloc)).collect();
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let stems: Vec<String> = ranked.into_iter().map(|(stem, _)| stem).take(2).collect();
        if stems.is_empty() {
            // no alphabetic stem anywhere in the group: nothing honest to
            // name it after — the numeric form is the only fit left.
            format!("{base_name}/{ordinal}")
        } else {
            format!("{base_name}/{}", stems.join("-"))
        }
    };
    let label = if used.contains(candidate.as_str()) {
        format!("{base_name}/{ordinal}")
    } else {
        candidate
    };
    used.insert(SmolStr::from(label.clone()));
    SmolStr::from(label)
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
    folder_budget: u32,
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
    let capacity_pressure = binding_pressure(tree, folder_budget);

    ScoreCandidate {
        edges,
        containers,
        cohesion_groups,
        path_cohesion,
        move_distance,
        capacity_pressure,
    }
}

/// Sums the scoped over-capacity binding pressure of a rendered tree: over
/// every folder and domain container, the share its transitively-bound file
/// count exceeds `folder_budget` — `Σ max(0, bound − budget) / budget` (FIX03).
///
/// Ancestors bind, so nesting cannot dodge the budget: a domain whose folders
/// together hold more than the budget pays for the whole binding even when
/// each folder sits within it. The rendered tree is the flat IR form, so the
/// count accumulates upward: each container folds its subtree total into its
/// parent, charging folders and domains on the way. This is the priced
/// counterpart of the eval's ancestor-binding rule; the configured per-level
/// caps stay findings-only semantics (`walk_capacity`). A zero budget disables
/// the term.
fn binding_pressure(tree: &ContainerTree, folder_budget: u32) -> f64 {
    if folder_budget == 0 {
        return 0.0;
    }
    // Subtree file totals keyed by container id. Iterating in descending id
    // order visits every child before its parent (ids are dense and parents
    // precede children), so one pass settles every container exactly once.
    let mut totals: BTreeMap<u32, u32> = BTreeMap::new();
    let mut pressure = 0.0_f64;
    for container in tree.containers().iter().rev() {
        match container.level {
            ScopeLevel::File => {
                if let Some(parent) = container.parent {
                    *totals.entry(parent.0).or_insert(0) += 1;
                }
            }
            level => {
                let bound = totals.remove(&container.id.0).unwrap_or(0);
                if matches!(level, ScopeLevel::Folder | ScopeLevel::Domain) {
                    let over = bound.saturating_sub(folder_budget);
                    pressure += f64::from(over) / f64::from(folder_budget);
                }
                if let Some(parent) = container.parent {
                    *totals.entry(parent.0).or_insert(0) += bound;
                }
            }
        }
    }
    pressure
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
        // No production SLOC exists under this placement (an all-test fixture,
        // say), so no file could demonstrably have left its folder: the
        // weighted fraction has an empty denominator, and the documented
        // invariant credits such a layout in full instead of collapsing the
        // empty ratio to a signed-zero term that reads as zero cohesion.
        1.0
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
/// owning file changes real folder — or, since FIX08, whose effective placement
/// lands it in a DIFFERENT file than the one that houses it today.
///
/// A file's location is exactly its folder key — the path it would be moved to —
/// so the comparison reads folder keys on both sides and never composes a
/// root-to-leaf path. Labels above the folder are display, not location:
/// renaming a domain relocates nothing and must not register here. The second,
/// placement-aware rule prices symbol-grain relocation: `placement` maps each
/// node to the candidate file container it occupies, and a node whose placed
/// file carries another path has left its home even when its folder key is
/// unchanged. File-only layouts pass the assembly's own placement, which maps
/// every node to its current file's candidate id, so the extension changes
/// nothing for them — μ and β stop being blind to symbol moves without any
/// coefficient moving.
fn move_distance(
    snapshot: &Snapshot,
    candidate: &ContainerTree,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
) -> f64 {
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
    let candidate_file_name: BTreeMap<u32, &str> = candidate
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container.name.as_str()))
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
            // FIX08: a symbol whose effective placement sits in a file of
            // another path has relocated between files, whatever its folder key
            // did. Unplaced nodes fall through to the file-key rule alone.
            let left_file = placement(node.id.0).is_some_and(|file| {
                candidate_file_name
                    .get(&file.0)
                    .is_some_and(|placed| *placed != name.as_str())
            });
            left_file
                || candidate_folder
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
            RenderScope::default(),
            key_by_id,
        )),
        many => Ok(ContainerNode {
            name: "workspace".to_owned(),
            level: Level::PackageGroup,
            children: Some(
                many.iter()
                    .map(|root| {
                        render_node(
                            root,
                            &children_by_parent,
                            &contents,
                            RenderScope::default(),
                            key_by_id,
                        )
                    })
                    .collect(),
            ),
            symbols: None,
            production_sloc: None,
        }),
    }
}

/// The naming context a render walk threads from a parent to its children: the
/// parent's cumulative display name, the parent's undecorated *key*, and the
/// undecorated key of the nearest enclosing package (empty above the package
/// level). The package key backs the folder-increment fallback — a real
/// directory foreign to its elected domain still displays relative to its own
/// package rather than re-embedding the package segment as a fabricated folder.
#[derive(Clone, Copy, Default)]
struct RenderScope<'tree> {
    /// The parent's cumulative display name (empty at the root).
    parent_name: &'tree str,
    /// The parent's undecorated key (empty at the root).
    parent_key: &'tree str,
    /// The undecorated key of the nearest enclosing package, or empty when no
    /// package has been descended yet.
    package_key: &'tree str,
}

/// Renders the container nodes `container` contributes to its parent's child
/// list, collapsing two kinds of redundant levels at the render boundary.
///
/// A synthetic bucket names no real directory — it exists only so the internal
/// tree stays strictly level-ascending over a root-level file — so it
/// contributes no node of its own: its children rise to sit directly under the
/// nearest real ancestor (a package's root files become siblings of its real
/// folders). A domain whose undecorated key repeats its package's key carries
/// no naming information of its own either — the elected label merely echoes
/// the level above — so it is suppressed the same way: its folders and files
/// hang directly under the package. The internal tree keeps both containers;
/// only the DTO drops them. Every other container contributes itself.
fn render_contributions(
    container: &Container,
    children_by_parent: &BTreeMap<u32, Vec<&Container>>,
    contents: &BTreeMap<u32, FileContents>,
    scope: RenderScope<'_>,
    key_by_id: &BTreeMap<u32, SmolStr>,
) -> Vec<ContainerNode> {
    let own_key = key_by_id
        .get(&container.id.0)
        .map_or(container.name.as_str(), SmolStr::as_str);
    let echoes_package = container.level == ScopeLevel::Domain && own_key == scope.package_key;
    if container.synthetic || echoes_package {
        return children_by_parent
            .get(&container.id.0)
            .map(|children| {
                children
                    .iter()
                    .flat_map(|child| {
                        render_contributions(child, children_by_parent, contents, scope, key_by_id)
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    vec![render_node(
        container,
        children_by_parent,
        contents,
        scope,
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
/// A folder's increment strips its parent's *key* (`scope.parent_key`), not the
/// parent's rendered display name: a domain whose display label
/// `qualify_elected` decorated (`core (constellation-ts.core)`) is no longer a
/// prefix of the folder key, so stripping the label would leave the whole key to
/// re-embed as a fabricated directory chain. `key_by_id` supplies the
/// undecorated key of any decorated ancestor; every other container keys on its
/// own name, so the two coincide and the render is unchanged. A folder key
/// foreign to its domain falls back to stripping the enclosing *package* key
/// ([`folder_increment`]), so a cross-domain real directory displays relative
/// to its own package instead of re-embedding the package segment as a
/// fabricated directory.
fn render_node(
    container: &Container,
    children_by_parent: &BTreeMap<u32, Vec<&Container>>,
    contents: &BTreeMap<u32, FileContents>,
    scope: RenderScope<'_>,
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
    let child_scope = RenderScope {
        parent_name: &container.name,
        parent_key: own_key,
        // descending a package establishes the fallback key its folders strip
        // against; every other level threads the enclosing package unchanged.
        package_key: if container.level == ScopeLevel::Package {
            own_key
        } else {
            scope.package_key
        },
    };
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
                        child_scope,
                        key_by_id,
                    )
                })
                .collect();
            merge_sibling_folders(rendered)
        })
        .unwrap_or_default();

    let increment = if container.level == ScopeLevel::Folder {
        folder_increment(&container.name, scope.parent_key, scope.package_key)
    } else {
        increment_name(&container.name, scope.parent_name)
    };
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

/// Returns a folder's display increment: its increment over the parent domain's
/// key when the two relate, else its increment over the enclosing package's key.
///
/// A folder key that path-extends neither ancestor renders whole — the honest
/// display of a directory foreign to the whole package. The domain strip stays
/// first so the clean case (a folder under a same-key domain) is untouched; the
/// package fallback only replaces the old whole-key fallback, which re-embedded
/// the package segment as a fabricated directory chain (`atlas/agent` under the
/// domain keyed `atlas/core` rendered `atlas` → `agent`, but no `atlas`
/// subdirectory exists under any real `core`). Stripping the package key
/// displays the folder relative to its own package: `agent`.
fn folder_increment(name: &str, parent_key: &str, package_key: &str) -> String {
    let against_parent = increment_name(name, parent_key);
    if against_parent != name {
        return against_parent;
    }
    increment_name(name, package_key)
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

/// One deterministic run of the FIX08 symbol polish over an assembled tree.
///
/// Owns every piece of mutable trial state — the placement overlay, the
/// running-best score, the cycle and visibility baselines, the working
/// visibility copy of the node table, and the incremental per-file SLOC and
/// occupancy ledgers — so [`PipelineSolver::symbol_polish`] stays a thin
/// orchestrator. Determinism is structural: symbols sweep in ascending id
/// order, destinations rank by summed two-way priced pull with ties broken
/// toward the lower file id, and every tie elsewhere resolves to staying.
struct SymbolPass<'a> {
    snapshot: &'a Snapshot,
    coefficients: &'a Coefficients,
    weights: &'a KindWeights,
    folder_cap: u32,
    file_cap: u32,
    assembled: &'a CandidateTree,
    base: &'a BTreeMap<u32, ContainerId>,
    nodes: &'a [Node],
    edges: &'a [Edge],
    /// Both-direction priced incidence per node, computed once: an edge
    /// priced 0.0 never nominates a destination (FIX04).
    incident: BTreeMap<u32, Vec<(u32, f64)>>,
    /// Per-file production SLOC under the assembly, maintained
    /// incrementally across acceptances.
    sloc: BTreeMap<ContainerId, u32>,
    /// Per-file production occupancy, maintained alongside [`Self::sloc`].
    residents: BTreeMap<ContainerId, u32>,
    /// Dense vertex per candidate FILE, for the crossing graph.
    file_vertices: BTreeMap<ContainerId, u32>,
    /// Working copy of the node table whose containers track the overlay,
    /// so the visibility floor is derived over trial placements.
    visibility_nodes: Vec<Node>,
    /// Effective placement overrides accepted so far.
    overlay: BTreeMap<u32, ContainerId>,
    /// Running best objective value; acceptance must beat it by more than
    /// [`SYMBOL_MIN_IMPROVEMENT`].
    best: f64,
    /// Cycle baseline: relocation may never raise the cycle count.
    cyclic_base: usize,
    /// Visibility baseline: relocation may never raise the finding count.
    vis_base: usize,
    relocations: Vec<SymbolRelocation>,
}

impl<'a> SymbolPass<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        snapshot: &'a Snapshot,
        coefficients: &'a Coefficients,
        weights: &'a KindWeights,
        folder_cap: u32,
        file_cap: u32,
        assembled: &'a CandidateTree,
        nodes: &'a [Node],
        edges: &'a [Edge],
    ) -> Self {
        let base = &assembled.placement;
        let mut sloc: BTreeMap<ContainerId, u32> = BTreeMap::new();
        let mut residents: BTreeMap<ContainerId, u32> = BTreeMap::new();
        for node in nodes {
            if node.polarity != Polarity::Production {
                continue;
            }
            let Some(file) = base.get(&node.id.0).copied() else {
                continue;
            };
            *sloc.entry(file).or_insert(0) += node.effective_size;
            *residents.entry(file).or_insert(0) += 1;
        }
        let mut incident: BTreeMap<u32, Vec<(u32, f64)>> = BTreeMap::new();
        // FIX11: the test-zone tie-cut rides placement into symbol grain. A
        // node's zone is its placed file's mark; an edge with either endpoint
        // inside the zone prices to zero exactly as `build_file_graph` prices
        // it — the single-pricing choke point, mirrored so no grain disagrees
        // about what binds placement.
        let touches_zone = |node: u32| -> bool {
            base.get(&node)
                .and_then(|file| assembled.zone_by_file.get(file))
                .copied()
                .unwrap_or(false)
        };
        for edge in edges {
            let weight = weights.edge_weight(edge.kind, edge.confidence);
            if weight <= 0.0 || touches_zone(edge.source.0) || touches_zone(edge.target.0) {
                continue;
            }
            incident
                .entry(edge.source.0)
                .or_default()
                .push((edge.target.0, weight));
            incident
                .entry(edge.target.0)
                .or_default()
                .push((edge.source.0, weight));
        }
        let file_vertices: BTreeMap<ContainerId, u32> = assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .enumerate()
            .map(|(index, container)| (container.id, u32::try_from(index).unwrap_or(u32::MAX)))
            .collect();
        let mut pass = Self {
            snapshot,
            coefficients,
            weights,
            folder_cap,
            file_cap,
            assembled,
            base,
            nodes,
            edges,
            incident,
            sloc,
            residents,
            file_vertices,
            visibility_nodes: nodes.to_vec(),
            overlay: BTreeMap::new(),
            best: 0.0,
            cyclic_base: 0,
            vis_base: 0,
            relocations: Vec::new(),
        };
        pass.best = pass.score_with(&pass.overlay);
        pass.cyclic_base = cyclic_vertex_count(&pass.crossing_csr());
        pass.vis_base = pass.refresh_visibility();
        pass
    }

    /// Sweeps every symbol in ascending id order, at most
    /// [`SYMBOL_SWEEPS`] times, stopping early once a sweep relocates
    /// nothing.
    fn run(&mut self) {
        for _ in 0..SYMBOL_SWEEPS {
            let mut improved = false;
            for node in self.nodes {
                improved |= self.try_relocate(node);
            }
            if !improved {
                break;
            }
        }
    }

    /// Placement of one node: the overlay wins, the assembly fills the rest.
    fn effective(&self, id: u32) -> Option<ContainerId> {
        self.overlay
            .get(&id)
            .copied()
            .or_else(|| self.base.get(&id).copied())
    }

    /// Full objective value of the layout `state` describes.
    fn score_with(&self, state: &BTreeMap<u32, ContainerId>) -> f64 {
        let placement = |id: u32| {
            state
                .get(&id)
                .copied()
                .or_else(|| self.base.get(&id).copied())
        };
        let distance = move_distance(self.snapshot, &self.assembled.tree, &placement);
        let candidate = score_candidate(
            self.snapshot,
            &placement,
            &self.assembled.tree,
            distance,
            self.folder_cap,
        );
        score(&candidate, self.coefficients, self.weights).total
    }

    /// The crossing graph over candidate FILES induced by the current
    /// overlay, condensed-ready exactly as the file polish builds its
    /// quotient.
    fn crossing_csr(&self) -> Csr {
        let mut pairs: BTreeSet<(u32, u32)> = BTreeSet::new();
        for edge in self.edges {
            let (Some(source), Some(target)) =
                (self.effective(edge.source.0), self.effective(edge.target.0))
            else {
                continue;
            };
            if source == target {
                continue;
            }
            let (Some(source), Some(target)) = (
                self.file_vertices.get(&source),
                self.file_vertices.get(&target),
            ) else {
                continue;
            };
            pairs.insert((*source, *target));
        }
        let sorted: Vec<(u32, u32)> = pairs.into_iter().collect();
        Csr::from_sorted_edges(self.file_vertices.len(), &sorted)
    }

    /// Applies the whole overlay to the working visibility copy, then
    /// counts findings — the initial baseline path.
    fn refresh_visibility(&mut self) -> usize {
        for node in &mut self.visibility_nodes {
            if let Some(file) = self.overlay.get(&node.id.0)
                && node.container != *file
            {
                node.container = *file;
            }
        }
        self.count_findings()
    }

    /// Points one node's working-copy container at `container`.
    fn set_visible(&mut self, id: u32, container: ContainerId) {
        for node in &mut self.visibility_nodes {
            if node.id == NodeId(id) {
                node.container = container;
            }
        }
    }

    /// Visibility finding count over the working copy as it stands.
    fn count_findings(&self) -> usize {
        derive_visibility(&self.assembled.tree, &self.visibility_nodes, self.edges)
            .findings
            .len()
    }

    /// Offers one symbol its strongest-pulling destinations under the full
    /// veto family; records an accepted relocation and returns whether the
    /// sweep made progress.
    fn try_relocate(&mut self, node: &Node) -> bool {
        let Some(&source_file) = self.base.get(&node.id.0) else {
            return false;
        };
        // Rank destination files by summed two-way priced pull from their
        // effective residents; strongest first, ties toward the lower id.
        let mut pull: BTreeMap<ContainerId, f64> = BTreeMap::new();
        if let Some(links) = self.incident.get(&node.id.0) {
            for &(neighbour, weight) in links {
                let Some(place) = self.effective(neighbour) else {
                    continue;
                };
                if place == source_file {
                    continue;
                }
                *pull.entry(place).or_insert(0.0) += weight;
            }
        }
        let mut ranked: Vec<(ContainerId, f64)> = pull.into_iter().collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.cmp(&right.0))
        });
        for (destination, _) in ranked.into_iter().take(SYMBOL_TARGETS) {
            // No empty shells: the origin keeps at least one production
            // resident.
            if self.residents.get(&source_file).copied().unwrap_or(0) <= 1 {
                break;
            }
            // FIX11 source/test boundary: a relocation whose origin and
            // destination sit on opposite sides of the test zone is barred
            // outright, in either direction. This static check runs before any
            // evaluation; nomination should already keep zone files out of
            // `ranked` because the incidence map zero-prices every
            // zone-touching edge, so the veto is defense in depth against
            // future nomination paths. Every emitted file gets a zone entry,
            // so the map is empty only on the no-files early return — where
            // placement is empty and `try_relocate` declines before ever
            // reaching this check; lookups nonetheless default to false on
            // both sides.
            if self
                .assembled
                .zone_by_file
                .get(&source_file)
                .copied()
                .unwrap_or(false)
                != self
                    .assembled
                    .zone_by_file
                    .get(&destination)
                    .copied()
                    .unwrap_or(false)
            {
                continue;
            }
            // SLOC cap on the destination, priced in production SLOC.
            let destination_sloc = self.sloc.get(&destination).copied().unwrap_or(0);
            let moving_sloc =
                (node.polarity == Polarity::Production).then_some(node.effective_size);
            if let Some(size) = moving_sloc
                && destination_sloc.saturating_add(size) > self.file_cap
            {
                continue;
            }

            // Tentatively relocate, then run the structural vetoes.
            let previous = self.overlay.insert(node.id.0, destination);
            let cyclic_now = cyclic_vertex_count(&self.crossing_csr());
            if cyclic_now > self.cyclic_base {
                Self::undo(&mut self.overlay, node.id.0, previous);
                continue;
            }
            self.set_visible(node.id.0, destination);
            let vis_now = self.count_findings();
            if vis_now > self.vis_base {
                Self::undo(&mut self.overlay, node.id.0, previous);
                self.set_visible(node.id.0, source_file);
                continue;
            }

            let total = self.score_with(&self.overlay);
            // Strict improvement with a real margin: float-dust gains are
            // rejected, not accepted (see SYMBOL_MIN_IMPROVEMENT).
            if self.best - total > SYMBOL_MIN_IMPROVEMENT {
                let delta = self.best - total;
                self.best = total;
                self.cyclic_base = cyclic_now;
                self.vis_base = vis_now;
                if node.polarity == Polarity::Production {
                    if let Some(slot) = self.sloc.get_mut(&source_file) {
                        *slot = slot.saturating_sub(node.effective_size);
                    }
                    *self.sloc.entry(destination).or_insert(0) += node.effective_size;
                    if let Some(slot) = self.residents.get_mut(&source_file) {
                        *slot = slot.saturating_sub(1);
                    }
                    *self.residents.entry(destination).or_insert(0) += 1;
                }
                self.relocations.push(SymbolRelocation {
                    node: node.id.0,
                    from_file: source_file,
                    to_file: destination,
                    delta,
                });
                return true;
            }
            // Rejected: undo the tentative relocation.
            Self::undo(&mut self.overlay, node.id.0, previous);
            self.set_visible(node.id.0, source_file);
        }
        false
    }

    /// Restores the prior overlay entry for one node.
    fn undo(overlay: &mut BTreeMap<u32, ContainerId>, id: u32, previous: Option<ContainerId>) {
        match previous {
            Some(place) => {
                overlay.insert(id, place);
            }
            None => {
                overlay.remove(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use strata_core::condense::SccId;

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

    /// Builds a soft type-reference edge (weak priced pull, `0.3`).
    fn type_ref(source: u32, target: u32) -> Edge {
        Edge {
            kind: EdgeKind::TypeReference,
            hardness: Hardness::Soft,
            ..edge(source, target)
        }
    }

    /// Builds a hard inheritance edge (the heaviest priced pull, `1.5`).
    fn inherits(source: u32, target: u32) -> Edge {
        Edge {
            kind: EdgeKind::Inheritance,
            ..edge(source, target)
        }
    }

    /// Builds a zero-priced re-export edge (the barrel-file shape).
    fn reexport(source: u32, target: u32) -> Edge {
        Edge {
            kind: EdgeKind::ReExport,
            ..edge(source, target)
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

    /// Assembles a snapshot from parts, panicking loudly with the assemble
    /// error — a broken test fixture must surface, never hide behind a
    /// minimal fallback.
    #[allow(clippy::panic)] // loud failure is the point of this test helper
    fn snapshot(nodes: Vec<Node>, edges: Vec<Edge>, containers: Vec<Container>) -> Snapshot {
        let ir = IntermediateRepresentation::new(nodes, edges, ContainerTree::new(containers));
        Snapshot::assemble(ir)
            .unwrap_or_else(|error| panic!("test snapshot failed to assemble: {error}"))
    }

    /// Returns a minimal valid snapshot: a single empty root file container.
    ///
    /// Kept despite gaining no remaining callers (the loud-assert change
    /// removed its only use): a legitimate builder retained by owner ruling,
    /// not dead weight to delete.
    #[allow(dead_code)]
    #[allow(clippy::expect_used)] // loud failure is the point of this test helper
    fn minimal_snapshot() -> Snapshot {
        let ir = IntermediateRepresentation::new(
            vec![],
            vec![],
            ContainerTree::new(vec![container(0, "root", ScopeLevel::File, None)]),
        );
        Snapshot::assemble(ir).expect("minimal snapshot must always assemble")
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
        // a merged domain elects the foreign prefix `cts` under package `pkg`,
        // and `qualify_elected` decorated its display as `cts (cts.core)`. The
        // decorated label is not a prefix of the folder key `cts/core`, so
        // stripping it would leave the whole key to re-embed as a fabricated
        // `cts` → `core` chain. Stripping the domain's real key `cts` (from
        // `key_by_id`) renders the one real directory `core`.
        let tree = ContainerTree::new(vec![
            container(0, "pkg", ScopeLevel::PackageGroup, None),
            container(1, "pkg", ScopeLevel::Package, Some(0)),
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
    fn should_suppress_a_domain_whose_key_repeats_its_package_key() {
        // two merged domains both elect the bare package name `cts`; the
        // decorated displays differ (`cts (cts.core)`) but the undecorated key
        // behind each (from `key_by_id`) repeats the package's key, so the
        // level carries no naming information — it is suppressed at the render
        // boundary and the real folders hang directly under the package.
        let tree = ContainerTree::new(vec![
            container(0, "cts", ScopeLevel::PackageGroup, None),
            container(1, "cts", ScopeLevel::Package, Some(0)),
            container(2, "cts (cts.core)", ScopeLevel::Domain, Some(1)),
            container(3, "cts/core", ScopeLevel::Folder, Some(2)),
            container(4, "src/core/a.ts", ScopeLevel::File, Some(3)),
            container(5, "cts (cts.io)", ScopeLevel::Domain, Some(1)),
            container(6, "cts/io", ScopeLevel::Folder, Some(5)),
            container(7, "src/io/b.ts", ScopeLevel::File, Some(6)),
        ]);
        let key_by_id: BTreeMap<u32, SmolStr> =
            [(2, SmolStr::new("cts")), (5, SmolStr::new("cts"))]
                .into_iter()
                .collect();

        let rendered = render_tree(&tree, &[], &|_| None, &key_by_id).ok();

        let package = rendered.and_then(only_child);
        assert_eq!(
            package
                .as_ref()
                .map(|node| (node.name.as_str(), node.level)),
            Some(("cts", Level::Package))
        );
        let children: Vec<(String, Level)> = package
            .and_then(|node| node.children)
            .unwrap_or_default()
            .into_iter()
            .map(|child| (child.name, child.level))
            .collect();
        assert_eq!(
            children,
            vec![
                ("core".to_owned(), Level::Folder),
                ("io".to_owned(), Level::Folder),
            ]
        );
    }

    #[test]
    fn should_render_a_cross_domain_folder_relative_to_its_package() {
        // `atlas/agent` is a real directory clustered into the sibling domain
        // keyed `atlas/core`: its key extends neither the domain key nor the
        // decorated display, so the old whole-key fallback re-embedded the
        // package segment as a fabricated `atlas` → `agent` chain (no `atlas`
        // subdirectory exists under any real `core`). The folder must display
        // relative to its own package: `agent`, directly under the domain.
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "atlas", ScopeLevel::Package, Some(0)),
            container(2, "atlas/core", ScopeLevel::Domain, Some(1)),
            container(3, "atlas/agent", ScopeLevel::Folder, Some(2)),
            container(4, "atlas/src/agent/loop.ts", ScopeLevel::File, Some(3)),
        ]);

        let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

        let domain = rendered.and_then(only_child).and_then(only_child);
        assert_eq!(
            domain.as_ref().map(|node| (node.name.as_str(), node.level)),
            Some(("core", Level::Domain))
        );
        let folder = domain.and_then(only_child);
        assert_eq!(
            folder.as_ref().map(|node| (node.name.as_str(), node.level)),
            Some(("agent", Level::Folder))
        );
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

        let distance = move_distance(&snapshot, &ContainerTree::new(relabelled), &|_| None);

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
                    capacity: 0.0,
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

    /// Builds the gate-probe snapshot: two edgeless production files. Polish
    /// nominates moves only along priced pulls (D-46), so with no edges every
    /// seed converges back to reality under either objective and the identity
    /// layout wins whichever pool is allowed to carry it.
    fn gate_fixture() -> Snapshot {
        snapshot(
            vec![
                node(0, "alpha", 0, Polarity::Production),
                node(1, "beta", 1, Polarity::Production),
            ],
            vec![],
            vec![
                container(0, "alpha.py", ScopeLevel::File, None),
                container(1, "beta.py", ScopeLevel::File, None),
            ],
        )
    }

    #[test]
    fn should_seed_identity_only_into_the_anchored_pool() {
        // AD-2's seeding gate as documented at `analyze_inner`: the identity
        // entry ("change nothing" guaranteed a pool slot at the true current
        // score) is anchored-only on a cap-clean tree. On this fixture the
        // searches cannot move anything, so the gate shows as the standings
        // asymmetry: anchored reports optimal because its identity entry won
        // the pool; greenfield has no such entry and can never claim optimal,
        // yet still reports candidates against its own current-score baseline.
        let anchored = analyze(&gate_fixture(), &config_with_k(2))
            .ok()
            .and_then(|result| result.modes.anchored);
        assert!(
            anchored.is_some_and(|mode| {
                mode.current_standing == CurrentStanding::Optimal
                    && mode.candidates.first().is_some_and(|candidate| {
                        candidate.delta_narration.is_empty()
                            && candidate.improvement.abs() < f64::EPSILON
                    })
            }),
            "anchored must carry the identity entry: standing optimal with a \
             zero-move candidate at +0 improvement"
        );

        let greenfield = analyze(&gate_fixture(), &config_with_k(2))
            .ok()
            .and_then(|result| result.modes.greenfield);
        assert!(
            greenfield.is_some_and(|mode| {
                mode.current_standing == CurrentStanding::Outscored && !mode.candidates.is_empty()
            }),
            "greenfield never seeds from the current layout (AD-2): no identity \
             entry means no optimal standing, while candidates still report"
        );
    }

    #[test]
    fn should_gate_identity_seeding_on_the_solver_flag() {
        // the mechanism behind the wiring: `PipelineSolver` constructs the
        // identity entry only when asked, and only then does the offset-0 seed
        // short-circuit to it — re-priced at the true current tree rather than
        // the assembled search view.
        let snapshot = gate_fixture();
        let config = AnalyzeConfig::default();
        let weights = config.weights.kind_weights();

        let anchored_tests = TestPolicy::defaults();
        let anchored = PipelineSolver::new(
            &snapshot,
            &config,
            config.objective.anchored(),
            true,
            &anchored_tests,
        );
        let seeded = anchored.solve(config.analysis.seed);
        let current_total = score_current(
            &snapshot,
            &config.objective.anchored(),
            &weights,
            level_caps(&config).folder,
        )
        .total;
        let identity_matched = anchored.identity.as_ref().is_some_and(|identity| {
            seeded.partition == *identity && (seeded.score - current_total).abs() < f64::EPSILON
        });
        assert!(
            identity_matched,
            "seed_identity=true carries the identity partition and solve(base) \
             returns it at the true current-tree score"
        );

        let greenfield_tests = TestPolicy::defaults();
        let greenfield = PipelineSolver::new(
            &snapshot,
            &config,
            config.objective.greenfield(),
            false,
            &greenfield_tests,
        );
        let searched = greenfield.solve(config.analysis.seed);
        let covered = (0..greenfield.condensation.members.len()).all(|scc| {
            searched
                .partition
                .cluster_of(u32::try_from(scc).unwrap_or(u32::MAX))
                .is_some()
        });
        assert!(
            greenfield.identity.is_none(),
            "seed_identity=false constructs no identity entry at all"
        );
        assert!(covered, "closing the gate must not break the search");
    }

    #[test]
    fn should_report_both_modes_infeasible_when_capacity_breaches() {
        // the cap-clean arm of the gate: a hard breach makes the current layout
        // an illegal candidate, so neither mode may report optimal even though
        // both searches still run and still emit candidates measured against
        // the (illegal) baseline. Borderline observations never reach this arm:
        // the same hard-breaks predicate feeds both the DTO count and the gate.
        let mut dirty = config_with_k(2);
        dirty.capacity.file = 0;

        let analyzed = analyze(&gate_fixture(), &dirty);
        let breaks = analyzed
            .as_ref()
            .map_or(0, |result| result.current.capacity_breaks);
        assert!(breaks > 0, "a zero file cap counts as a hard break");

        let modes = analyzed.map(|result| result.modes).unwrap_or_default();
        let anchored_ok = modes.anchored.as_ref().is_some_and(|mode| {
            mode.current_standing == CurrentStanding::Infeasible && !mode.candidates.is_empty()
        });
        let greenfield_ok = modes
            .greenfield
            .as_ref()
            .is_some_and(|mode| mode.current_standing == CurrentStanding::Infeasible);
        assert!(
            anchored_ok,
            "a cap-breaching current layout holds anchored at infeasible while \
             candidates still report"
        );
        assert!(
            greenfield_ok,
            "a cap-breaching current layout holds greenfield at infeasible"
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
    fn should_count_only_hard_capacity_findings_as_breaks() {
        let finding = |kind, severity, location: &str| Violation {
            kind,
            severity,
            location: vec![location.to_owned()],
            detail: String::new(),
            break_suggestions: None,
            capacity: None,
        };
        let violations = vec![
            finding(ViolationKind::Capacity, Severity::Borderline, "warm"),
            finding(ViolationKind::Visibility, Severity::Violation, "b"),
            finding(ViolationKind::Capacity, Severity::Violation, "big_folder"),
            finding(ViolationKind::Capacity, Severity::Violation, "huge_file"),
            finding(ViolationKind::Cycle, Severity::Violation, "y"),
            finding(
                ViolationKind::Capacity,
                Severity::Borderline,
                "another_warm",
            ),
        ];

        assert_eq!(hard_capacity_breaks(&violations), 2);
        assert_eq!(hard_capacity_breaks(&[]), 0);
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
    fn should_split_an_over_cap_directory_into_named_halves_along_connectivity() {
        // one real directory (`openai`) holds two priced file pairs — more
        // than the folder cap of two — and no sibling directory exists to
        // relieve into. FIX03 prices that binding in the objective, so the
        // search grain itself relieves it: the directory splits along its
        // priced connectivity into cap-respecting halves — the first keeps the
        // real directory's name, later halves extend it with their dominant
        // basename stem, keeping every name a real place.
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
            2,
            "the over-cap directory must relieve into two halves, got {folders:?}"
        );
        // the first pile keeps the real directory's own name — folders are
        // reality, so the original place never renames; only a new pile earns
        // an extended `{dir}/{stem}` label, which the render boundary nests
        // under the same directory instead of minting a dash-joined sibling.
        assert!(
            folders.iter().any(|name| name.as_str() == "openai"),
            "the original directory must keep its real name, got {folders:?}"
        );
        assert!(
            folders.iter().any(|name| name.as_str() == "dalle"),
            "the second half must nest its dominant stem under the base \
             directory's place, got {folders:?}"
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
        // different real places. The satellite `nu` carries a sole inheritance
        // anchor into `adapters/http`, so polish genuinely consolidates it
        // there; the alpha/mu and gamma/delta spines pin every heavyweight,
        // and mu's weak type-reference to delta keeps both directories one
        // merged-domain suggestion. Each folder must still surface its own
        // real key — never a truncated twin deduped into a synthetic `http-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "alpha", 4, 40),
                homed(1, "mu", 5, 30),
                homed(2, "gamma", 8, 20),
                homed(3, "delta", 9, 10),
                homed(4, "nu", 10, 5),
            ],
            vec![
                edge(0, 1),
                inherits(1, 0),
                edge(2, 3),
                inherits(3, 2),
                inherits(4, 0),
                type_ref(1, 3),
            ],
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
                container(10, "src/core/http/net.ts", ScopeLevel::File, Some(7)),
            ],
        );
        let mut config = config_with_k(2);
        // headroom for the genuine consolidation: `adapters/http` absorbs the
        // sole-anchored satellite without ever exceeding the cap.
        config.capacity.folder = 3;

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
                    foreign == Some("http") || foreign == Some("core/http"),
                    "the minority folder must render its package-relative key, \
                     got {foreign:?} in {folders:?}"
                );
                merged_seen |= foreign == Some("core/http");
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            merged_seen,
            "expected a merged-domain candidate rendering the foreign folder \
             by its package-relative key `core/http`, never re-embedding the \
             package segment as a fabricated `ai` directory"
        );
    }

    #[test]
    fn should_inherit_real_domain_keys_for_domain_rooted_files() {
        // these files sit directly in two packages' domain directories, so
        // there is no deeper folder: each file's real folder IS its domain
        // directory. The satellite `a2` carries a sole inheritance anchor into
        // `bi/app`, so polish genuinely consolidates it there and coupling
        // merges the two domains into one suggestion — whose folders must keep
        // those inherited real keys, never collapse into `workspace` fallback
        // twins deduped as `workspace-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "alpha", 3, 10),
                homed(1, "beta", 4, 10),
                homed(2, "gamma", 7, 10),
                homed(3, "delta", 8, 10),
            ],
            vec![
                type_ref(0, 1),
                inherits(1, 2),
                inherits(1, 3),
                inherits(2, 3),
                edge(3, 2),
                type_ref(0, 3),
            ],
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
        // headroom for the genuine consolidation: `bi/app` absorbs the
        // sole-anchored satellite without ever exceeding the cap.
        config.capacity.folder = 3;

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
                    folders.iter().all(|(_, files)| !files.is_empty()),
                    "every rendered folder holds its real members, got {folders:?}"
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
        // nested package `a/b` both key as `a/b/c`. The satellite `f2` carries
        // a sole inheritance anchor into the nested package's `c`, so polish
        // genuinely consolidates it there and coupling merges the two domains
        // into one suggestion — whose twins must qualify by their real
        // package, never dedupe into a synthetic `c-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "alpha", 4, 10),
                homed(1, "beta", 5, 10),
                homed(2, "gamma", 9, 10),
                homed(3, "delta", 10, 10),
            ],
            vec![
                type_ref(0, 1),
                inherits(1, 2),
                inherits(1, 3),
                inherits(2, 3),
                edge(3, 2),
                type_ref(0, 3),
            ],
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
        // headroom for the genuine consolidation: the nested package's `c`
        // absorbs the sole-anchored satellite without exceeding the cap.
        config.capacity.folder = 3;

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
                    folders.iter().all(|(_, files)| !files.is_empty()),
                    "every rendered folder holds its real members, got {folders:?}"
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
        // of the same package. The satellite `f2` carries a sole inheritance
        // anchor into `d2`'s `shared`, so polish genuinely consolidates it
        // there and coupling merges the two domains into one suggestion —
        // whose twins must qualify by their real location, never dedupe into
        // a synthetic `shared-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "alpha", 4, 10),
                homed(1, "beta", 5, 10),
                homed(2, "gamma", 8, 10),
                homed(3, "delta", 9, 10),
            ],
            vec![
                type_ref(0, 1),
                inherits(1, 2),
                inherits(1, 3),
                inherits(2, 3),
                edge(3, 2),
                type_ref(0, 3),
            ],
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
        // headroom for the genuine consolidation: `d2`'s `shared` absorbs the
        // sole-anchored satellite without exceeding the cap.
        config.capacity.folder = 3;

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
        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(
            snapshot,
            &AnalyzeConfig::default(),
            Coefficients::anchored(),
            true,
            &tests,
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
        // three homes with no strict majority and no shared prefix. The
        // satellite `s` carries a sole inheritance anchor into `d2`, so polish
        // genuinely consolidates it there, and the weak type-reference triangle
        // (kept acyclic so the file graph never welds the folders into one
        // immovable atom) couples all three folders into one grab-bag domain.
        // Votes follow real homes — {d1:48, d2:40, d3:38}, no strict majority —
        // so the election must reach the top-two join and produce a real
        // composite name — never the synthetic `mixed` label.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 2, 24),
                homed(1, "b", 3, 24),
                homed(2, "c", 5, 22),
                homed(3, "d", 6, 18),
                homed(4, "e", 8, 15),
                homed(5, "f", 9, 14),
                homed(6, "s", 10, 9),
            ],
            vec![
                edge(0, 1),
                edge(2, 3),
                inherits(3, 2),
                edge(4, 5),
                type_ref(0, 2),
                type_ref(1, 4),
                type_ref(5, 2),
                inherits(6, 2),
            ],
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
                container(10, "src/d3/s.ts", ScopeLevel::File, Some(7)),
            ],
        );
        let mut config = config_with_k(2);
        // headroom for the genuine consolidation, and a domain cap that
        // admits all three coupled folders into one cluster.
        config.capacity.folder = 3;
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
        // rung R2: `ai/app` and `ai/core` tie at 40 SLOC of real homes — no
        // strict majority — but share the `ai` prefix, so the merged domain
        // elects `ai` rather than misnaming the whole after one tied side.
        // The satellite `b` carries a sole call anchor into `ai/core`, so
        // polish genuinely consolidates it there while every vote keeps its
        // real home. The elected name repeats the package's key, so the
        // redundant domain level is suppressed at the render boundary: the
        // merged candidate shows no domain node at all, its folders hanging
        // directly under the `ai` package.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 3, 20),
                homed(1, "b", 4, 20),
                homed(2, "c", 6, 20),
                homed(3, "d", 7, 20),
            ],
            vec![type_ref(0, 1), type_ref(0, 2), edge(1, 2), inherits(2, 3)],
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
        // headroom for the genuine consolidation: `ai/core` absorbs the
        // sole-anchored satellite without exceeding the cap.
        config.capacity.folder = 3;

        let modes = analyze(&snapshot, &config)
            .map(|result| result.modes)
            .unwrap_or_default();

        let mut prefix_seen = false;
        let mut seen = 0;
        for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
            for candidate in &mode.candidates {
                seen += 1;
                let domains = domain_names(&candidate.tree);
                prefix_seen |= domains.is_empty();
            }
        }
        assert!(seen > 0, "expected at least one candidate across the modes");
        assert!(
            prefix_seen,
            "expected the balanced merged domain to elect the shared home \
             prefix `ai` and be suppressed as a redundant echo of the package"
        );
    }

    #[test]
    fn should_join_the_top_two_homes_when_no_prefix_is_shared() {
        // rung R3: `ai/app` and `bi/app` tie at 30 SLOC of real homes with no
        // shared prefix, so the merged domain joins the two homes — ranked by
        // production SLOC then file count, so the exact SLOC tie falls to file
        // count and `bi/app` leads despite `ai/app` sorting first. The
        // satellite `d` carries a sole call anchor into `ai/app`, so polish
        // genuinely consolidates it there while every vote keeps its real
        // home, and the weak `a`-to-`c` reference keeps the packages coupled
        // into one suggestion.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 3, 15),
                homed(1, "b", 4, 15),
                homed(2, "c", 7, 10),
                homed(3, "d", 8, 10),
                homed(4, "e", 9, 10),
            ],
            vec![
                inherits(0, 1),
                type_ref(0, 2),
                edge(3, 1),
                inherits(2, 4),
                edge(4, 2),
                type_ref(4, 3),
            ],
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
        // headroom for the genuine consolidation: `ai/app` absorbs the
        // sole-anchored satellite without exceeding the cap.
        config.capacity.folder = 3;

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
        // rather than a bare-number composite or the old `mixed` label. The
        // satellite `e` carries a sole inheritance anchor into `2024/2026`,
        // so polish genuinely consolidates it there while the `a`-to-`c` and
        // `f`-to-`c` references keep all three folders one coupled suggestion.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 3, 20),
                homed(1, "b", 4, 20),
                homed(2, "c", 6, 20),
                homed(3, "d", 7, 20),
                homed(4, "e", 9, 10),
                homed(5, "f", 10, 10),
                homed(6, "g", 11, 10),
            ],
            vec![
                edge(0, 1),
                type_ref(0, 2),
                inherits(2, 3),
                edge(3, 2),
                inherits(4, 3),
                edge(5, 6),
                inherits(6, 5),
                type_ref(5, 2),
            ],
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
                container(11, "src/shared/g.ts", ScopeLevel::File, Some(8)),
            ],
        );
        let mut config = config_with_k(2);
        // headroom for the genuine consolidation, and a domain cap that
        // admits all three coupled folders into one cluster.
        config.capacity.folder = 3;
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
        // and `2024/y` folders keep their real keys. The domain elects the
        // same wrap as its package, so the redundant domain level is
        // suppressed at the render boundary; the wrap survives at the package.
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
                let domain_suppressed =
                    !containers.iter().any(|(level, _)| *level == Level::Domain);
                let mut folders = Vec::new();
                folder_files(&candidate.tree, &mut folders);
                let folder_real = folders.iter().any(|(name, _)| name.starts_with("2024/"));
                wrapped_seen |= package_wrapped && domain_suppressed && folder_real;
            }
        }
        assert!(
            emitted > 0,
            "expected at least one emitted (elected) candidate"
        );
        assert!(
            wrapped_seen,
            "expected the all-numeric home to elect the anchored wrap \
             `2024 (2024.x)` at the package, suppress the echoing domain, and \
             keep the real folders"
        );
    }

    #[test]
    fn should_disambiguate_name_collisions_with_a_home_qualifier_not_an_integer() {
        // a domain cap of one splits the real `pa/app` home into two sibling
        // domain clusters that elect the same name. The satellite `b` carries
        // a sole call anchor into `y`, so polish genuinely consolidates it
        // there while the `a`-`e` spine pins `x`'s stayers — and the twins
        // must still qualify by their anchor folders, never dedupe into a
        // synthetic `app-2`.
        let snapshot = snapshot(
            vec![
                homed(0, "a", 4, 10),
                homed(1, "b", 5, 10),
                homed(2, "e", 6, 10),
                homed(3, "c", 8, 10),
                homed(4, "d", 9, 10),
            ],
            vec![
                edge(0, 2),
                inherits(2, 0),
                type_ref(0, 1),
                edge(1, 3),
                inherits(3, 4),
            ],
            vec![
                container(0, "ws", ScopeLevel::PackageGroup, None),
                container(1, "pa", ScopeLevel::Package, Some(0)),
                container(2, "pa/app", ScopeLevel::Domain, Some(1)),
                container(3, "pa/app/x", ScopeLevel::Folder, Some(2)),
                container(4, "src/app/x/a.ts", ScopeLevel::File, Some(3)),
                container(5, "src/app/x/b.ts", ScopeLevel::File, Some(3)),
                container(6, "src/app/x/e.ts", ScopeLevel::File, Some(3)),
                container(7, "pa/app/y", ScopeLevel::Folder, Some(2)),
                container(8, "src/app/y/c.ts", ScopeLevel::File, Some(7)),
                container(9, "src/app/y/d.ts", ScopeLevel::File, Some(7)),
            ],
        );
        let mut config = config_with_k(1);
        // headroom for the genuine consolidation, and a domain cap that
        // forces the two sibling clusters apart.
        config.capacity.folder = 3;
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

    /// Builds an over-capacity fixture file living in the synthetic `hub`
    /// folder with a `{stem}_{index}.py` basename.
    fn relief_file(index: usize, stem: &str) -> FileInfo {
        FileInfo {
            container: u32::try_from(index).unwrap_or(u32::MAX),
            name: SmolStr::new(format!("{stem}_{index:02}.py")),
            production_sloc: 10,
            home: LaminarHome {
                folder: SmolStr::new("hub"),
                domain: SmolStr::new("hub"),
                package: SmolStr::new("app"),
                synthetic: false,
            },
        }
    }

    /// Builds an all-singleton condensation over `file_count` files.
    fn singleton_condensation(file_count: usize) -> Condensation {
        Condensation {
            dag: Csr::from_sorted_edges(file_count, &[]),
            membership: (0..file_count)
                .map(|index| SccId(u32::try_from(index).unwrap_or(u32::MAX)))
                .collect(),
            members: (0..file_count)
                .map(|index| vec![NodeId(u32::try_from(index).unwrap_or(u32::MAX))])
                .collect(),
        }
    }

    #[test]
    fn should_accumulate_transitive_binding_over_the_flat_candidate_tree() {
        let tree = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "hub", ScopeLevel::Domain, Some(0)),
            container(2, "hub/ingest", ScopeLevel::Folder, Some(1)),
            container(3, "hub/ingest/a.py", ScopeLevel::File, Some(2)),
            container(4, "hub/ingest/b.py", ScopeLevel::File, Some(2)),
            container(5, "hub/ingest/c.py", ScopeLevel::File, Some(2)),
            container(6, "hub/emit", ScopeLevel::Folder, Some(1)),
            container(7, "hub/emit/d.py", ScopeLevel::File, Some(6)),
        ]);

        // `ingest` binds 3 files → one over-budget share against budget 2;
        // `emit` stays within budget; the domain binds all 4 → two shares.
        // Ancestor binding is the point: nesting cannot dodge the budget.
        assert!((binding_pressure(&tree, 2) - 1.5).abs() < 1e-9);
        // everything within budget charges nothing.
        assert!(binding_pressure(&tree, 4).abs() < 1e-9);
        // a zero budget disables the term entirely.
        assert!(binding_pressure(&tree, 0).abs() < 1e-9);
    }

    #[test]
    fn should_count_binding_folders_through_an_intermediate_domain() {
        // QUAL-P3-2 witness: `dom` directly holds only one child (the nested
        // `sub`), but three file-binding folders live beneath it. Direct-child
        // counting stayed silent at cap 2; deep counting fires on both levels.
        let tree = interior(
            "root",
            Level::PackageGroup,
            vec![interior(
                "pkg",
                Level::Package,
                vec![interior(
                    "dom",
                    Level::Domain,
                    vec![interior(
                        "sub",
                        Level::Domain,
                        vec![
                            folder("x", vec![file("f1", 10)]),
                            folder("y", vec![file("f2", 10)]),
                            folder("z", vec![file("f3", 10)]),
                        ],
                    )],
                )],
            )],
        );
        let mut config = AnalyzeConfig::default();
        config.capacity.domain = 2;

        let findings = walk_all_capacity(&tree, &config);

        let domains: Vec<Vec<String>> = findings
            .iter()
            .filter(|(level, _)| *level == Level::Domain)
            .map(|(_, violation)| violation.location.clone())
            .collect();
        assert_eq!(
            domains,
            vec![
                vec!["root".to_owned(), "pkg".to_owned(), "dom".to_owned()],
                vec![
                    "root".to_owned(),
                    "pkg".to_owned(),
                    "dom".to_owned(),
                    "sub".to_owned()
                ],
            ]
        );
    }

    #[test]
    fn should_split_an_over_capacity_folder_along_priced_connectivity() {
        // 24 files in one real-folder cluster against budget 20: two priced
        // 12-file chains (`ingest`, `emit`) with no cross-chain edges, so
        // connectivity elects exactly the two halves a human would cut.
        let file_count = 24;
        let files: Vec<FileInfo> = (0..file_count)
            .map(|index| {
                if index < 12 {
                    relief_file(index, "ingest")
                } else {
                    relief_file(index, "emit")
                }
            })
            .collect();
        let condensation = singleton_condensation(file_count);
        let chain = |start: u32| {
            (start..start + 11)
                .map(|vertex| (vertex, vertex + 1, 1.0_f32))
                .collect::<Vec<_>>()
        };
        let mut edges = chain(0);
        edges.extend(chain(12));
        let graph = Csr::from_weighted_edges(file_count, &edges);
        let base = Partition::from_assignment(vec![ClusterId(0); file_count], 1);

        let (relieved, partition, names, synthetic) = relieve_over_capacity(
            files,
            &condensation,
            &graph,
            &base,
            &[SmolStr::new("hub")],
            &[false],
            20,
        );

        assert_eq!(partition.cluster_count(), 2);
        for index in 0..12usize {
            let scc = u32::try_from(index).unwrap_or(u32::MAX);
            assert_eq!(
                partition.cluster_of(scc),
                Some(ClusterId(0)),
                "pile 0 keeps the original cluster id"
            );
            let home = relieved.get(index).map(|file| file.home.domain.as_str());
            assert_eq!(home, Some("hub"));
        }
        for index in 12..24usize {
            let scc = u32::try_from(index).unwrap_or(u32::MAX);
            assert_eq!(
                partition.cluster_of(scc),
                Some(ClusterId(1)),
                "the later pile gets the fresh cluster id"
            );
            let home = relieved.get(index).map(|file| file.home.domain.as_str());
            assert_eq!(
                home,
                Some("hub"),
                "split halves never rewrite member homes — the '/' label alone \
                 nests the half under its base folder"
            );
        }
        assert_eq!(names, vec![SmolStr::new("hub"), SmolStr::new("hub/emit")]);
        assert_eq!(synthetic, vec![false, false]);
    }

    #[test]
    fn should_chunk_an_oversized_component_and_fall_back_to_numeric_labels() {
        // one 22-file priced chain against budget 20 is a single oversized
        // component: BFS chunks it into 20 + 2, and the numeric-only basenames
        // yield no alphabetic stem, so the overflow chunk qualifies numerically.
        let file_count = 22;
        let files: Vec<FileInfo> = (0..file_count)
            .map(|index| relief_file(index, "123"))
            .collect();
        let condensation = singleton_condensation(file_count);
        let edges: Vec<(u32, u32, f32)> = (0..21).map(|v| (v, v + 1, 1.0_f32)).collect();
        let graph = Csr::from_weighted_edges(file_count, &edges);
        let base = Partition::from_assignment(vec![ClusterId(0); file_count], 1);

        let (relieved, partition, names, _) = relieve_over_capacity(
            files,
            &condensation,
            &graph,
            &base,
            &[SmolStr::new("hub")],
            &[false],
            20,
        );

        assert_eq!(partition.cluster_count(), 2);
        let moved: Vec<usize> = (0..u32::try_from(file_count).unwrap_or(u32::MAX))
            .filter(|scc| partition.cluster_of(*scc) != Some(ClusterId(0)))
            .map(|scc| usize::try_from(scc).unwrap_or(usize::MAX))
            .collect();
        assert_eq!(moved, vec![20, 21], "only the overflow chunk moves");
        assert!(
            moved.iter().all(|&index| relieved
                .get(index)
                .is_some_and(|file| file.home.domain == "hub")),
            "the numeric fallback half also keeps its base domain"
        );
        assert_eq!(names.get(1), Some(&SmolStr::new("hub/1")));
    }

    #[test]
    fn should_not_invent_halves_for_unconnected_members() {
        // 22 mutually unconnected files against budget 20: no pile is joined
        // by priced evidence, so the folder stays whole — the objective prices
        // the binding and the search may still relieve it by moving files into
        // folders that actually pull them.
        let file_count = 22;
        let files: Vec<FileInfo> = (0..file_count)
            .map(|index| relief_file(index, "solo"))
            .collect();
        let condensation = singleton_condensation(file_count);
        let graph = Csr::from_weighted_edges(file_count, &[]);
        let base = Partition::from_assignment(vec![ClusterId(0); file_count], 1);

        let (relieved, partition, names, _) = relieve_over_capacity(
            files,
            &condensation,
            &graph,
            &base,
            &[SmolStr::new("hub")],
            &[false],
            20,
        );

        assert_eq!(partition.cluster_count(), 1);
        assert!(relieved.iter().all(|file| file.home.domain == "hub"));
        assert_eq!(names, vec![SmolStr::new("hub")]);
    }

    #[test]
    fn should_never_nominate_a_folder_connected_only_by_zero_priced_edges() {
        // the welding shape: `barrel/index.ts` re-exports two unrelated
        // directories while one priced import ties render to auth. A free edge
        // carries no evidence two folders belong together (the FIX04 doctrine),
        // so it must nominate nothing: polish is never offered a move that
        // welds files from different real directories into one folder.
        let snapshot = snapshot(
            vec![
                node(0, "login", 3, Polarity::Production),
                node(1, "session", 4, Polarity::Production),
                node(2, "canvas", 5, Polarity::Production),
                node(3, "index", 6, Polarity::Production),
            ],
            vec![
                edge(0, 1),     // login -> session: priced, inside auth.
                edge(2, 0),     // canvas -> login: priced, render pulls on auth.
                reexport(3, 0), // barrel -> login: free.
                reexport(3, 1), // barrel -> session: free.
                reexport(3, 2), // barrel -> canvas: free.
            ],
            vec![
                container(0, "auth", ScopeLevel::Folder, None),
                container(1, "render", ScopeLevel::Folder, None),
                container(2, "barrel", ScopeLevel::Folder, None),
                container(3, "auth/login.ts", ScopeLevel::File, Some(0)),
                container(4, "auth/session.ts", ScopeLevel::File, Some(0)),
                container(5, "render/canvas.ts", ScopeLevel::File, Some(1)),
                container(6, "barrel/index.ts", ScopeLevel::File, Some(2)),
            ],
        );

        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(
            &snapshot,
            &AnalyzeConfig::default(),
            Coefficients::anchored(),
            false,
            &tests,
        );
        let parts = &solver.real_partition;
        let scc_of = |file_container: u32| -> u32 {
            let vertex = solver
                .index_of
                .get(&file_container)
                .copied()
                .unwrap_or(u32::MAX);
            solver
                .condensation
                .membership
                .get(vertex as usize)
                .map_or(0, |scc| scc.0)
        };

        // three real directories start in three distinct clusters.
        let auth = parts.cluster_of(scc_of(3)).unwrap_or(ClusterId(0));
        let render = parts.cluster_of(scc_of(5)).unwrap_or(ClusterId(0));
        let barrel = parts.cluster_of(scc_of(6)).unwrap_or(ClusterId(0));
        assert_eq!(parts.cluster_count(), 3);
        assert_ne!(auth, render);
        assert_ne!(auth, barrel);
        assert_ne!(render, barrel);

        // login's cross-folder pull comes only from the priced import: render
        // is nominated, but the free barrel edge nominates nothing.
        let targets = solver.pull_targets(parts, scc_of(3), auth);
        assert_eq!(targets, vec![render]);

        // the barrel's own folder connects only through free re-exports: no
        // move target exists for it at all.
        let barrel_targets = solver.pull_targets(parts, scc_of(6), barrel);
        assert!(
            barrel_targets.is_empty(),
            "zero-priced edges must not nominate any move target"
        );
    }

    #[test]
    fn should_never_absorb_a_multi_folder_bridge_into_one_side() {
        // the inversion shape under greenfield coefficients (the mode where the
        // FIX05 defect churns): `main.ts` calls into two sibling features while
        // each feature is internally cohesive. Absorbing the facade into one
        // feature strands its edges to the other at package height, yet every
        // locally-scored statistic of the absorber improves — so the unguarded
        // objective ratifies the fold and greenfield out-churns anchored,
        // inverting the product promise. Bridge integrity vetoes the fold: a
        // folder that does not already contain an SCC's whole priced
        // neighborhood may not absorb it.
        let snapshot = snapshot(
            vec![
                node(0, "Shape", 4, Polarity::Production),
                node(1, "area_of", 5, Polarity::Production),
                node(2, "scale", 6, Polarity::Production),
                node(3, "Counter", 7, Polarity::Production),
                node(4, "run", 8, Polarity::Production),
                node(5, "per_second", 9, Polarity::Production),
            ],
            vec![
                edge(1, 0), // area_of -> Shape: priced, inside geometry.
                edge(1, 2), // area_of -> scale: priced, geometry coheres.
                edge(2, 0), // scale -> Shape: priced, geometry coheres.
                edge(5, 3), // per_second -> Counter: priced, metrics coheres.
                edge(4, 1), // run -> area_of: priced, facade reaches geometry.
                edge(4, 5), // run -> per_second: priced, facade reaches metrics.
            ],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "geometry", ScopeLevel::Folder, Some(0)),
                container(2, "metrics", ScopeLevel::Folder, Some(0)),
                container(3, "workspace", ScopeLevel::Folder, Some(0)),
                container(4, "shape.ts", ScopeLevel::File, Some(1)),
                container(5, "area.ts", ScopeLevel::File, Some(1)),
                container(6, "units.ts", ScopeLevel::File, Some(1)),
                container(7, "counter.ts", ScopeLevel::File, Some(2)),
                container(8, "main.ts", ScopeLevel::File, Some(3)),
                container(9, "rate.ts", ScopeLevel::File, Some(2)),
            ],
        );

        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(
            &snapshot,
            &AnalyzeConfig::default(),
            AnalyzeConfig::default().objective.greenfield(),
            false,
            &tests,
        );
        let scc_of = |file_container: u32| -> u32 {
            let vertex = solver
                .index_of
                .get(&file_container)
                .copied()
                .unwrap_or(u32::MAX);
            solver
                .condensation
                .membership
                .get(vertex as usize)
                .map_or(0, |scc| scc.0)
        };

        let mut polished = solver.real_partition.clone();
        let before = polished.clone();
        solver.polish(&mut polished);

        assert_eq!(
            polished.cluster_of(scc_of(8)),
            before.cluster_of(scc_of(8)),
            "the facade bridges geometry and metrics; absorbing it into either \
             side makes the bridge a member of the thing it bridges"
        );
        assert_eq!(
            polished, before,
            "every move this layout offers is a bridge fold, so polish must hold"
        );
    }

    /// A misfiled symbol with one honest destination: `s` lives in `a.ts`, but
    /// its inheritors `c1`/`c2` sit in `b.ts`. The co-resident `mate` keeps
    /// `a.ts` from emptying, so the shell veto never fires. FIX08: the symbol
    /// pass relocates exactly `s` between the two existing files, and the
    /// narration carries an improvement past the float-dust floor and zero
    /// severed imports.
    ///
    /// The assertions run against the greenfield mode deliberately: the cut
    /// term normalizes by the candidate's own edge mass, capping any single
    /// move's cut gain near `1/MAX_CROSSING_HEIGHT`, so on a five-node fixture
    /// the anchored `mu * d` price exceeds every cut gain available and an
    /// anchored symbol pass rightly holds still (D-47: no fabricated
    /// movement). Greenfield drops that price, letting pure structure decide.
    #[test]
    fn should_relocate_a_misfiled_symbol_between_existing_files() {
        let snapshot = snapshot(
            vec![
                node(0, "s", 3, Polarity::Production),
                node(1, "mate", 3, Polarity::Production),
                node(2, "c1", 4, Polarity::Production),
                node(3, "c2", 4, Polarity::Production),
                node(4, "base", 4, Polarity::Production),
            ],
            vec![
                inherits(2, 0), // c1 extends s: strong pull toward b.ts …
                inherits(3, 0), // … twice over.
                edge(4, 2),     // base calls c1/c2: migrating them would
                edge(4, 3),     // re-sever more than following s gains.
            ],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "keep", ScopeLevel::Folder, Some(0)),
                container(2, "sink", ScopeLevel::Folder, Some(0)),
                container(3, "a.ts", ScopeLevel::File, Some(1)),
                container(4, "b.ts", ScopeLevel::File, Some(2)),
            ],
        );

        let mut config = AnalyzeConfig::default();
        config.analysis.candidates = 1;
        let moves = analyze(&snapshot, &config)
            .ok()
            .and_then(|result| result.modes.greenfield)
            .and_then(|mode| mode.candidates.into_iter().next())
            .map(|candidate| candidate.symbol_moves)
            .unwrap_or_default();

        // The symbol itself relocates between the two existing files, with an
        // improvement past the float-dust floor and no severed imports.
        let s_move = moves.iter().find(|entry| entry.symbol == "s");
        assert!(
            s_move.is_some_and(|entry| {
                entry.from_path == "a.ts"
                    && entry.to_path == "b.ts"
                    && entry.kind == SymbolKind::Symbol
                    && entry.delta < -SYMBOL_MIN_IMPROVEMENT
                    && entry.broken_imports == 0
            }),
            "the symbol pass must relocate s into its consumers' file with real \
             improvement and nothing severed; got {moves:?}"
        );
        // Nothing else relocates: migrating the consumers would re-sever their
        // calls to base, and a.ts keeps its co-resident either way.
        assert!(
            moves.iter().all(|entry| entry.symbol == "s"),
            "only s has pull justifying relocation; got {moves:?}"
        );
    }

    /// The FIX04 doctrine at symbol grain: zero-priced edges nominate nothing,
    /// so a symbol connected only through re-exports is never relocated, no
    /// matter how many of them point across folders.
    #[test]
    fn should_let_zero_priced_edges_nominate_no_symbol_target() {
        let snapshot = snapshot(
            vec![
                node(0, "s", 3, Polarity::Production),
                node(1, "fill", 3, Polarity::Production),
                node(2, "c1", 4, Polarity::Production),
                node(3, "c2", 4, Polarity::Production),
            ],
            vec![reexport(2, 0), reexport(3, 0)],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "keep", ScopeLevel::Folder, Some(0)),
                container(2, "sink", ScopeLevel::Folder, Some(0)),
                container(3, "a.ts", ScopeLevel::File, Some(1)),
                container(4, "b.ts", ScopeLevel::File, Some(2)),
            ],
        );

        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(
            &snapshot,
            &AnalyzeConfig::default(),
            AnalyzeConfig::default().objective.greenfield(),
            false,
            &tests,
        );
        let outcome = solver.symbol_polish(&solver.real_partition.clone());

        assert!(
            outcome.relocations.is_empty() && outcome.overlay.is_empty(),
            "re-export edges are priced 0.0 and never bind placement"
        );
    }

    /// The no-empty-shells veto: a file's last production resident stays home
    /// even when an out-of-file pull exists — draining the file would be a
    /// file move wearing a symbol costume, which v1 does not propose.
    #[test]
    fn should_not_drain_a_file_of_its_last_resident() {
        let snapshot = snapshot(
            vec![
                node(0, "s", 3, Polarity::Production),
                node(1, "c1", 4, Polarity::Production),
                node(2, "c2", 4, Polarity::Production),
                node(3, "base", 4, Polarity::Production),
            ],
            vec![
                type_ref(1, 0), // the consumers do pull s across …
                type_ref(2, 0), //
                edge(3, 1),     // … but base binds them home, so the only
                edge(3, 2),     // candidate move is s's, which must be vetoed.
            ],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "keep", ScopeLevel::Folder, Some(0)),
                container(2, "sink", ScopeLevel::Folder, Some(0)),
                container(3, "a.ts", ScopeLevel::File, Some(1)),
                container(4, "b.ts", ScopeLevel::File, Some(2)),
            ],
        );

        let mut config = AnalyzeConfig::default();
        config.analysis.candidates = 1;
        let undrained = analyze(&snapshot, &config).is_ok_and(|result| {
            [&result.modes.anchored, &result.modes.greenfield]
                .into_iter()
                .flatten()
                .flat_map(|mode| &mode.candidates)
                .all(|candidate| {
                    candidate
                        .symbol_moves
                        .iter()
                        .all(|mv| mv.from_path != "a.ts")
                })
        });
        assert!(
            undrained,
            "a.ts holds s alone; relocating it empties the file, so the veto \
             must hold"
        );
    }

    /// The placement-aware move_distance extension (FIX08): a node whose
    /// effective placement lands in a different FILE counts as moved even when
    /// both files keep their folder keys, while the identical layout with
    /// home-file placements measures zero.
    #[test]
    fn should_count_a_file_grain_identity_placement_as_unmoved() {
        let ir = IntermediateRepresentation::new(
            vec![
                node(0, "s", 3, Polarity::Production),
                node(1, "t", 4, Polarity::Production),
            ],
            vec![],
            ContainerTree::new(vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "keep", ScopeLevel::Folder, Some(0)),
                container(2, "sink", ScopeLevel::Folder, Some(0)),
                container(3, "a.ts", ScopeLevel::File, Some(1)),
                container(4, "b.ts", ScopeLevel::File, Some(2)),
            ]),
        );
        // The fixture layout is static, so a failed assemble means the fixture
        // itself rotted — surface that loudly rather than testing a fallback.
        let assembled = Snapshot::assemble(ir);
        assert!(assembled.is_ok(), "fixture layout must assemble");
        let Ok(snap) = assembled else {
            return;
        };

        // Candidate tree mirrors the current folders; placement sends s from
        // a.ts (container 3) into b.ts (container 4).
        let candidate = ContainerTree::new(snap.ir().containers.containers().to_vec());
        let distance = move_distance(&snap, &candidate, &|id| (id == 0).then_some(ContainerId(4)));
        assert!(
            distance > 0.0,
            "s left its home file, so μ must see the move: measured {distance}"
        );

        let home = move_distance(&snap, &candidate, &|_| None);
        assert!(
            home.abs() < f64::EPSILON,
            "unplaced nodes fall back to folder keys, which did not change: \
             measured {home}"
        );
    }

    /// Builds a minimal [`FileInfo`] owning `container`, whose naming evidence
    /// is just its path; laminar home keys never feed label formation, so they
    /// stay blank.
    fn file_info(container: u32, path: &str, sloc: u32) -> FileInfo {
        FileInfo {
            container,
            name: SmolStr::new(path),
            production_sloc: sloc,
            home: LaminarHome {
                folder: SmolStr::new(""),
                domain: SmolStr::new(""),
                package: SmolStr::new(""),
                synthetic: false,
            },
        }
    }

    /// An all-digit shared token must never name a rebuilt place (FIX09 label
    /// honesty): `helpers/2024` is indistinguishable from the numeric fallback
    /// and can never align under the contract tokenizer, which drops digit
    /// tokens. The stem path names the place instead.
    #[test]
    fn should_skip_a_digit_token_when_naming_a_rebuilt_place() {
        let condensation = singleton_condensation(2);
        let files = vec![
            file_info(0, "report_2024.py", 10),
            file_info(0, "audit_2024.py", 10),
        ];
        let mut used = BTreeSet::new();

        let label = rebuild_label("helpers", &[0, 1], &condensation, &files, &mut used, 9);

        assert_eq!(
            label, "helpers/audit-report",
            "the only shared token is the digit run '2024'; the joined stems \
             must name the place instead"
        );
    }

    /// Token grouping joins two SCCs only on a genuinely shared basename
    /// token; unrelated files stay in their own groups.
    #[test]
    fn should_group_only_sccs_sharing_a_basename_token() {
        let condensation = singleton_condensation(3);
        let files = vec![
            file_info(0, "alpha.py", 1),
            file_info(0, "alpha_beta.py", 1),
            file_info(0, "gamma.py", 1),
        ];

        let groups = token_groups(&[0, 1, 2], &condensation, &files);

        assert_eq!(
            groups,
            vec![vec![0, 1], vec![2]],
            "'alpha' joins the first pair; 'gamma' shares nothing and stays alone"
        );
    }

    /// A file with any priced incident edge is bonded, and its whole SCC stays
    /// glued: bonded company never enters the stranger population, so a folder
    /// holding only bonded files and ungroupable strays fires nothing.
    #[test]
    fn should_keep_a_priced_bond_out_of_the_stranger_population() {
        // beta carries the only priced edge, so alpha and gamma are strangers —
        // but they share no token, so no group of two forms and nothing fires.
        let graph = Csr::from_weighted_edges(3, &[(0_u32, 1_u32, 1.0_f32)]);
        let condensation = singleton_condensation(3);
        let base = Partition::from_assignment(vec![ClusterId(0), ClusterId(0), ClusterId(0)], 1);
        let files = vec![
            file_info(0, "alpha.py", 1),
            file_info(0, "beta.py", 1),
            file_info(0, "gamma.py", 1),
        ];
        let mut names = vec![SmolStr::new("helpers")];
        let mut synthetic = vec![false];

        let rebuilt = synthesize_roof_rebuild(
            &files,
            &condensation,
            &graph,
            &[false, false, false],
            &base,
            &mut names,
            &mut synthetic,
        );

        assert!(
            rebuilt.is_none(),
            "no token group of two forms among the strangers, and the bonded \
             residual is a lone file, so the folder must stay put"
        );
    }

    /// The end-to-end FIX09 win: zero-priced strangers sharing a basename token
    /// under a misnamed roof leave for a place named after their own shared
    /// word, so the greenfield pool carries a genuinely different shape.
    #[test]
    fn should_synthesize_a_place_named_after_its_strangers() {
        // checkout calls into helpers/charge; every other helpers file prices
        // zero anywhere, so refund/string/date are the unanchored population.
        let snapshot = snapshot(
            vec![
                node(0, "pay", 1, Polarity::Production),
                node(1, "charge_card", 3, Polarity::Production),
                node(2, "refund_card", 4, Polarity::Production),
                node(3, "slugify", 5, Polarity::Production),
                node(4, "parse_iso", 6, Polarity::Production),
            ],
            vec![edge(0, 1)],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "checkout.py", ScopeLevel::File, Some(0)),
                container(2, "helpers", ScopeLevel::Folder, Some(0)),
                container(3, "charge.py", ScopeLevel::File, Some(2)),
                container(4, "refund.py", ScopeLevel::File, Some(2)),
                container(5, "string_utils.py", ScopeLevel::File, Some(2)),
                container(6, "date_utils.py", ScopeLevel::File, Some(2)),
            ],
        );
        let config = config_with_k(3);

        // analysis failures surface as an empty candidate list, so the
        // carries_rebuild assertion below fails informatively (house style:
        // no panic!/expect in tests).
        let greenfield = analyze(&snapshot, &config)
            .ok()
            .and_then(|result| result.modes.greenfield)
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
                    capacity: 0.0,
                },
                current_standing: CurrentStanding::Outscored,
            });

        let carries_rebuild = greenfield.candidates.iter().any(|candidate| {
            // the rebuilt label path-extends its base folder, so the rendered
            // tree nests `utils` under `helpers` rather than naming one node
            // with the slash — match the pair structurally.
            find_named(&candidate.tree, "helpers")
                .and_then(|helpers| {
                    helpers
                        .children
                        .as_ref()
                        .and_then(|children| children.iter().find(|child| child.name == "utils"))
                })
                .is_some_and(|place| {
                    let paths = descendant_files(place);
                    paths.iter().any(|path| path.ends_with("string_utils.py"))
                        && paths.iter().any(|path| path.ends_with("date_utils.py"))
                        && paths.len() == 2
                })
        });
        assert!(
            carries_rebuild,
            "string_utils and date_utils share only the roof's misnomer; the \
             synthesized helpers/utils place must reach the greenfield pool \
             nested under helpers, holding exactly those two files"
        );
    }

    /// Depth-first search for a non-file container whose name matches exactly.
    fn find_named<'a>(node: &'a ContainerNode, name: &str) -> Option<&'a ContainerNode> {
        if node.level != Level::File && node.name == name {
            return Some(node);
        }
        node.children
            .iter()
            .flatten()
            .find_map(|child| find_named(child, name))
    }

    /// Collects every descendant file name beneath one rendered container.
    fn descendant_files(node: &ContainerNode) -> Vec<String> {
        let mut paths = Vec::new();
        accumulate_files(node, &mut paths);
        paths
    }

    /// Depth-first accumulation of descendant file names.
    fn accumulate_files(node: &ContainerNode, out: &mut Vec<String>) {
        if node.level == Level::File {
            out.push(node.name.clone());
            return;
        }
        for child in node.children.iter().flatten() {
            accumulate_files(child, out);
        }
    }

    /// Compiles a `[tests]` section into a policy, failing loudly on an
    /// invalid fixture pattern — a broken test must surface, never hide.
    #[allow(clippy::panic)] // loud failure is the point of this test helper
    fn policy(tests: &TestsConfig) -> TestPolicy {
        match TestPolicy::new(tests) {
            Ok(compiled) => compiled,
            Err(error) => panic!("test policy failed to compile: {error}"),
        }
    }

    /// Returns the weight of the `from -> to` edge in a CSR, or `None` when no
    /// such edge exists.
    fn edge_weight(graph: &Csr, from: u32, to: u32) -> Option<f32> {
        let slot = graph
            .neighbors(from)
            .iter()
            .position(|&target| target == to)?;
        graph.weights(from).get(slot).copied()
    }

    #[test]
    fn should_mark_a_polarity_detected_spec_as_the_test_zone() {
        // the spec holds one test-case symbol; its subject is production.
        let nodes = vec![
            node(0, "openai", 1, Polarity::Production),
            node(1, "openai_spec", 2, Polarity::TestCase),
        ];
        let files = [
            file_info(1, "src/openai.ts", 1),
            file_info(2, "src/openai.spec.ts", 0),
        ];

        assert_eq!(
            test_zone_marks(&TestPolicy::defaults(), &files, &nodes),
            vec![false, true]
        );
    }

    #[test]
    fn should_not_mark_a_file_holding_any_production_symbol() {
        let nodes = vec![
            node(0, "helper", 1, Polarity::TestCase),
            node(1, "real", 1, Polarity::Production),
        ];
        let files = [file_info(1, "src/mixed.ts", 1)];

        assert_eq!(
            test_zone_marks(&TestPolicy::defaults(), &files, &nodes),
            vec![false]
        );
    }

    #[test]
    fn should_mark_a_pattern_matched_file_even_when_production_polarity() {
        let tests = TestsConfig {
            patterns: vec!["*.custom-test.*".to_owned()],
            ..TestsConfig::default()
        };
        let nodes = vec![node(0, "weird", 1, Polarity::Production)];
        let files = [
            file_info(1, "src/plain.ts", 1),
            file_info(2, "src/weird.custom-test.ts", 1),
        ];

        assert_eq!(
            test_zone_marks(&policy(&tests), &files, &nodes),
            vec![false, true]
        );
    }

    #[test]
    fn should_leave_the_zone_inert_when_builtins_are_off_and_no_patterns_given() {
        let nodes = vec![node(0, "spec", 1, Polarity::TestCase)];
        let files = [file_info(1, "src/x.spec.ts", 0)];

        assert_eq!(
            test_zone_marks(&TestPolicy::disabled(), &files, &nodes),
            vec![false]
        );
    }

    #[test]
    fn should_zero_price_every_edge_touching_the_test_zone() {
        // spec imports both production files; the two production files bond
        // with each other. Only edges touching vertex 1 (the spec) may cut.
        let nodes = vec![
            node(0, "alpha", 1, Polarity::Production),
            node(1, "beta", 2, Polarity::Production),
            node(2, "alpha_spec", 3, Polarity::TestCase),
        ];
        let edges = vec![edge(0, 1), edge(2, 0)];
        let index_of = BTreeMap::from([(1_u32, 0_u32), (2, 1), (3, 2)]);
        let weights = AnalyzeConfig::default().weights.kind_weights();
        let test_zone = vec![false, false, true];

        let graph = build_file_graph(&edges, &nodes, &index_of, 3, &weights, &test_zone);

        // FIX04 doctrine: cut edges stay in the graph at price zero — the CSR
        // shape must not change, only the binding.
        #[allow(clippy::cast_possible_truncation)] // mirrors the production narrowing
        let call = weights.call as f32;
        assert_eq!(graph.edge_count(), 2);
        assert_eq!(edge_weight(&graph, 0, 1), Some(call));
        assert_eq!(edge_weight(&graph, 1, 0), None); // never priced in reverse
        assert_eq!(edge_weight(&graph, 2, 0), Some(0.0)); // spec -> subject: cut
    }

    #[test]
    fn should_zero_price_test_to_test_edges_in_both_directions() {
        let nodes = vec![
            node(0, "one", 1, Polarity::TestCase),
            node(1, "two", 2, Polarity::TestCase),
        ];
        let edges = vec![edge(0, 1), edge(1, 0)];
        let index_of = BTreeMap::from([(1_u32, 0_u32), (2, 1_u32)]);
        let weights = AnalyzeConfig::default().weights.kind_weights();
        let test_zone = vec![true, true];

        let graph = build_file_graph(&edges, &nodes, &index_of, 2, &weights, &test_zone);

        assert_eq!(graph.edge_count(), 2);
        assert_eq!(edge_weight(&graph, 0, 1), Some(0.0));
        assert_eq!(edge_weight(&graph, 1, 0), Some(0.0));
    }

    #[test]
    fn should_give_a_spec_no_priced_pull_toward_its_subject() {
        // The exact openai.spec.ts shape: a mirrored spec tree importing its
        // production twin. Condensation stays structural (Tarjan folds real
        // cycles whatever they cost), but every stage that moves files —
        // relief piles, polish pulls, heavy-edge matching — reads prices and
        // skips zero, so the cut must leave no priced edge anywhere between
        // the pair's components.
        let nodes = vec![
            node(0, "openai", 1, Polarity::Production),
            node(1, "openai_spec", 2, Polarity::TestCase),
        ];
        let edges = vec![edge(1, 0)];
        let containers = vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "src/openai.ts", ScopeLevel::File, Some(0)),
            container(2, "spec/openai.spec.ts", ScopeLevel::File, Some(0)),
        ];
        let snap = snapshot(nodes, edges, containers);

        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(
            &snap,
            &AnalyzeConfig::default(),
            Coefficients::anchored(),
            true,
            &tests,
        );

        let mut priced: Vec<(u32, u32)> = Vec::new();
        for vertex in 0..solver.condensation.dag.vertex_count() {
            let from = u32::try_from(vertex).unwrap_or(u32::MAX);
            let weights = solver.condensation.dag.weights(from);
            for (slot, &to) in solver.condensation.dag.neighbors(from).iter().enumerate() {
                if weights.get(slot).copied().unwrap_or(0.0) > 0.0 {
                    priced.push((from, to));
                }
            }
        }

        assert!(
            priced.is_empty(),
            "the tie-cut left a priced edge between spec and subject: {priced:?}"
        );
    }

    /// The FIX11 repro, mirrored on the `ai` codec.ts ↔ codec.spec.ts shape: a
    /// production subject whose spec twin pulls hardest (two inherited
    /// extensions) must never be relocated across the source/test boundary —
    /// the file-graph tie-cut prices that bond at zero, so the symbol pass
    /// must read the same price and nominate nothing across it, in either
    /// direction.
    #[test]
    fn should_not_relocate_a_production_symbol_into_its_spec_twin() {
        let snapshot = snapshot(
            vec![
                node(0, "to_gemini_image_response", 3, Polarity::Production),
                node(1, "codec_helper", 3, Polarity::Production),
                node(2, "google_codec", 4, Polarity::Production),
                node(3, "batch_codec", 4, Polarity::Production),
                node(4, "codec_spec_case", 5, Polarity::TestCase),
            ],
            vec![
                edge(2, 0),     // consumers import the subject, but the
                edge(3, 0),     // spec twin pulls harder: the mirrored
                inherits(4, 0), // spec extends it twice over, the heaviest
                inherits(4, 0), // priced pull this graph can carry.
            ],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "src", ScopeLevel::Folder, Some(0)),
                container(2, "spec", ScopeLevel::Folder, Some(0)),
                container(3, "src/codec.ts", ScopeLevel::File, Some(1)),
                container(4, "src/consumers.ts", ScopeLevel::File, Some(1)),
                container(5, "spec/codec.spec.ts", ScopeLevel::File, Some(2)),
            ],
        );

        // The pass driven straight on the real layout: no relocation may
        // touch the spec twin, in either direction.
        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(
            &snapshot,
            &AnalyzeConfig::default(),
            AnalyzeConfig::default().objective.greenfield(),
            false,
            &tests,
        );
        let assembled = solver.assemble(&solver.real_partition);
        let spec_file = assembled
            .tree
            .containers()
            .iter()
            .find(|file| {
                file.level == ScopeLevel::File && file.name.as_str() == "spec/codec.spec.ts"
            })
            .map(|file| file.id);
        let outcome = solver.symbol_polish(&solver.real_partition);
        let crossings: Vec<String> = outcome
            .relocations
            .iter()
            .filter(|relocation| {
                Some(relocation.to_file) == spec_file || Some(relocation.from_file) == spec_file
            })
            .map(|relocation| {
                format!(
                    "node {} across file {}",
                    relocation.node, relocation.to_file.0
                )
            })
            .collect();
        assert!(
            crossings.is_empty(),
            "the symbol pass must never relocate across the source/test \
             boundary in either direction; crossed {crossings:?}"
        );

        // End to end: no candidate of either mode narrates a move whose
        // destination is the spec twin.
        let mut config = AnalyzeConfig::default();
        config.analysis.candidates = 1;
        let offenders: Vec<String> = analyze(&snapshot, &config)
            .ok()
            .map(|result| {
                [&result.modes.anchored, &result.modes.greenfield]
                    .into_iter()
                    .flatten()
                    .flat_map(|mode| &mode.candidates)
                    .flat_map(|candidate| &candidate.symbol_moves)
                    .filter(|move_entry| move_entry.to_path.ends_with(".spec.ts"))
                    .map(|move_entry| {
                        format!(
                            "{}: {} -> {}",
                            move_entry.symbol, move_entry.from_path, move_entry.to_path
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            offenders.is_empty(),
            "no candidate may suggest moving a production symbol into its \
             spec twin; suggested {offenders:?}"
        );
    }

    /// The FIX11 incidence tie-cut at symbol grain: an edge with either
    /// endpoint resident in a test-zone file prices zero in the pass's
    /// incident map, exactly as `build_file_graph` prices it — while priced
    /// production bonds survive untouched.
    #[test]
    fn should_zero_price_incident_edges_touching_the_test_zone() {
        let snapshot = snapshot(
            vec![
                node(0, "gemini_image_codec", 3, Polarity::Production),
                node(1, "codec_helper", 3, Polarity::Production),
                node(2, "google_codec", 4, Polarity::Production),
                node(3, "codec_spec_case", 5, Polarity::TestCase),
            ],
            vec![edge(2, 0), edge(2, 1), inherits(3, 0)],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "src", ScopeLevel::Folder, Some(0)),
                container(2, "spec", ScopeLevel::Folder, Some(0)),
                container(3, "src/codec.ts", ScopeLevel::File, Some(1)),
                container(4, "src/google.ts", ScopeLevel::File, Some(1)),
                container(5, "spec/codec.spec.ts", ScopeLevel::File, Some(2)),
            ],
        );
        let tests = TestPolicy::defaults();
        let config = AnalyzeConfig::default();
        let solver = PipelineSolver::new(
            &snapshot,
            &config,
            config.objective.greenfield(),
            false,
            &tests,
        );
        let ir = snapshot.ir();
        let assembled = solver.assemble(&solver.real_partition);
        let pass = SymbolPass::new(
            &snapshot,
            &solver.coefficients,
            &solver.weights,
            solver.caps.folder,
            solver.file_cap,
            &assembled,
            &ir.nodes,
            &ir.edges,
        );

        // the spec twin nominates nothing: no incident slot at all.
        assert!(
            pass.incident.get(&3).is_none_or(Vec::is_empty),
            "the test-zone resident must carry no priced incident edges"
        );
        // and no priced production link points at it either way.
        for id in [0u32, 1, 2] {
            let links = pass.incident.get(&id).map_or(&[][..], Vec::as_slice);
            assert!(
                links.iter().all(|&(neighbour, _)| neighbour != 3),
                "node {id} still carries a priced link into the test zone"
            );
        }
        // positive control: the production bond survived the cut.
        let subject_links = pass.incident.get(&0).map_or(&[][..], Vec::as_slice);
        assert!(
            subject_links.iter().any(|&(neighbour, _)| neighbour == 2),
            "the codec's priced bond to its consumer must survive the cut"
        );
    }

    /// Defense in depth: even when a priced link smuggles a cross-boundary
    /// destination into a symbol's nomination ranking, the static boundary
    /// veto bars the relocation before any evaluation runs.
    #[test]
    fn should_veto_a_cross_boundary_destination_even_when_priced() {
        let snapshot = snapshot(
            vec![
                node(0, "gemini_image_codec", 3, Polarity::Production),
                node(1, "codec_helper", 3, Polarity::Production),
                node(2, "codec_spec_case", 4, Polarity::TestCase),
            ],
            vec![inherits(2, 0)],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "src", ScopeLevel::Folder, Some(0)),
                container(2, "spec", ScopeLevel::Folder, Some(0)),
                container(3, "src/codec.ts", ScopeLevel::File, Some(1)),
                container(4, "spec/codec.spec.ts", ScopeLevel::File, Some(2)),
            ],
        );
        let tests = TestPolicy::defaults();
        let config = AnalyzeConfig::default();
        let solver = PipelineSolver::new(
            &snapshot,
            &config,
            config.objective.greenfield(),
            false,
            &tests,
        );
        let ir = snapshot.ir();
        let assembled = solver.assemble(&solver.real_partition);
        let mut pass = SymbolPass::new(
            &snapshot,
            &solver.coefficients,
            &solver.weights,
            solver.caps.folder,
            solver.file_cap,
            &assembled,
            &ir.nodes,
            &ir.edges,
        );
        // simulate a future nomination path that prices the twin pull despite
        // the tie-cut: the strongest possible lure across the boundary.
        pass.incident.entry(0).or_default().push((2, 5.0));
        // Float surgery: hold strict-J aside so a J-cost rejection cannot
        // masquerade as the veto — with best at infinity, any destination
        // that survives the gate chain is deterministically accepted, so
        // `!accepted` proves THIS veto fired.
        pass.best = f64::INFINITY;

        let accepted = ir
            .nodes
            .first()
            .is_some_and(|subject| pass.try_relocate(subject));
        assert!(
            !accepted && pass.relocations.is_empty(),
            "a cross-boundary destination must be vetoed even when priced; \
             relocations {:?}",
            pass.relocations.iter().map(|r| r.node).collect::<Vec<_>>()
        );
    }

    /// A production symbol misfiled INSIDE the test zone stays there: the
    /// outward crossing is barred the same as the inward one.
    #[test]
    fn should_keep_a_test_zone_resident_inside_the_test_zone() {
        let snapshot = snapshot(
            vec![
                node(0, "stray_helper", 4, Polarity::Production),
                node(1, "fellow_stray", 4, Polarity::Production),
                node(2, "greeter", 3, Polarity::Production),
            ],
            vec![],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "src", ScopeLevel::Folder, Some(0)),
                container(2, "spec", ScopeLevel::Folder, Some(0)),
                container(3, "src/home.ts", ScopeLevel::File, Some(1)),
                container(4, "spec/legacy.spec.ts", ScopeLevel::File, Some(2)),
            ],
        );
        let tests = policy(&TestsConfig {
            patterns: vec![String::from("*.spec.ts")],
            ..TestsConfig::default()
        });
        let config = AnalyzeConfig::default();
        let solver = PipelineSolver::new(
            &snapshot,
            &config,
            config.objective.greenfield(),
            false,
            &tests,
        );
        let ir = snapshot.ir();
        let assembled = solver.assemble(&solver.real_partition);
        let mut pass = SymbolPass::new(
            &snapshot,
            &solver.coefficients,
            &solver.weights,
            solver.caps.folder,
            solver.file_cap,
            &assembled,
            &ir.nodes,
            &ir.edges,
        );
        // a priced-looking lure out toward the production home file.
        pass.incident.entry(0).or_default().push((2, 3.0));
        // Float surgery: hold strict-J aside so a J-cost rejection cannot
        // masquerade as the veto — with best at infinity, any destination
        // that survives the gate chain is deterministically accepted, so
        // `!accepted` proves THIS veto fired.
        pass.best = f64::INFINITY;

        let accepted = ir
            .nodes
            .first()
            .is_some_and(|stray| pass.try_relocate(stray));
        assert!(
            !accepted && pass.relocations.is_empty(),
            "a test-zone resident must never relocate out of the zone; \
             relocations {:?}",
            pass.relocations.iter().map(|r| r.node).collect::<Vec<_>>()
        );
    }

    /// The boundary vetoes crossings, not motion: with the objective held
    /// aside, a misfiled symbol pulled toward callers inside the SAME zone
    /// clears the whole veto family — the veto never fires on intra-zone
    /// pairs.
    #[test]
    fn should_still_allow_moves_inside_one_zone() {
        let snapshot = snapshot(
            vec![
                node(0, "misfiled_formatter", 2, Polarity::Production),
                node(1, "zone_bystander", 2, Polarity::Production),
                node(2, "format_caller", 3, Polarity::Production),
                node(3, "second_caller", 3, Polarity::Production),
            ],
            vec![edge(2, 0), edge(3, 0)],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "spec", ScopeLevel::Folder, Some(0)),
                container(2, "spec/generated.spec.ts", ScopeLevel::File, Some(1)),
                container(3, "spec/callers.spec.ts", ScopeLevel::File, Some(1)),
            ],
        );
        let tests = policy(&TestsConfig {
            patterns: vec![String::from("*.spec.ts")],
            ..TestsConfig::default()
        });
        let config = AnalyzeConfig::default();
        let solver = PipelineSolver::new(
            &snapshot,
            &config,
            config.objective.greenfield(),
            false,
            &tests,
        );
        let ir = snapshot.ir();
        let assembled = solver.assemble(&solver.real_partition);
        let mut pass = SymbolPass::new(
            &snapshot,
            &solver.coefficients,
            &solver.weights,
            solver.caps.folder,
            solver.file_cap,
            &assembled,
            &ir.nodes,
            &ir.edges,
        );
        // the tie-cut zeroed the zone edges, so nomination needs the simulated
        // priced pull; the destination sits inside the same zone.
        pass.incident.entry(0).or_default().push((2, 5.0));
        // Float surgery: hold strict-J aside so THIS test exercises the veto
        // family alone — with zone edges priced zero everywhere, an intra-zone
        // move can never pay its own displacement under J, and that pricing
        // doctrine is covered by the zero-price tests, not here.
        pass.best = f64::INFINITY;

        let accepted = ir
            .nodes
            .first()
            .is_some_and(|misfiled| pass.try_relocate(misfiled));
        assert!(
            accepted,
            "an intra-zone relocation must clear the veto family; relocations \
             {:?}",
            pass.relocations.iter().map(|r| r.node).collect::<Vec<_>>()
        );
        assert_eq!(pass.relocations.len(), 1);
        assert_eq!(pass.relocations.first().map(|r| r.node), Some(0));
    }

    /// Zoning by `[tests]` patterns alone (no polarity hint): a file matching
    /// only the configured pattern is equally out of bounds as a destination.
    #[test]
    fn should_veto_entry_into_a_pattern_marked_test_zone() {
        let snapshot = snapshot(
            vec![
                node(0, "hero_widget", 2, Polarity::Production),
                node(1, "rival_widget", 3, Polarity::Production),
                node(2, "probe_case", 4, Polarity::TestCase),
            ],
            vec![edge(1, 0), type_ref(2, 0)],
            vec![
                container(0, "app", ScopeLevel::PackageGroup, None),
                container(1, "src", ScopeLevel::Folder, Some(0)),
                container(2, "src/hero.ts", ScopeLevel::File, Some(1)),
                container(3, "src/rival.ts", ScopeLevel::File, Some(1)),
                container(4, "src/probe.custom-test.ts", ScopeLevel::File, Some(1)),
            ],
        );
        let tests = policy(&TestsConfig {
            patterns: vec![String::from("*.custom-test.ts")],
            ..TestsConfig::default()
        });
        let config = AnalyzeConfig::default();
        let solver = PipelineSolver::new(
            &snapshot,
            &config,
            config.objective.greenfield(),
            false,
            &tests,
        );
        // Fresh candidate ids are arena-issued, so locate the zone file by its
        // snapshot name rather than assuming a literal id.
        let assembled = solver.assemble(&solver.real_partition);
        let probe_file = assembled
            .tree
            .containers()
            .iter()
            .find(|file| {
                file.level == ScopeLevel::File && file.name.as_str() == "src/probe.custom-test.ts"
            })
            .map(|file| file.id);
        // the pattern itself must be what marked the file: pin the zone map
        // before asserting anything about relocations.
        assert_eq!(
            probe_file.and_then(|id| assembled.zone_by_file.get(&id)),
            Some(&true),
            "the [tests] pattern must mark probe.custom-test.ts into the zone"
        );
        let outcome = solver.symbol_polish(&solver.real_partition);
        let crossings: Vec<String> = outcome
            .relocations
            .iter()
            .filter(|relocation| {
                Some(relocation.to_file) == probe_file || Some(relocation.from_file) == probe_file
            })
            .map(|relocation| {
                format!(
                    "node {} across file {}",
                    relocation.node, relocation.to_file.0
                )
            })
            .collect();
        assert!(
            crossings.is_empty(),
            "a pattern-marked test file is the same boundary; crossed \
             {crossings:?}"
        );
    }

    /// The R6 companion guard at roof grain: two all-test-zone SCCs sharing a
    /// basename token are exactly who the stranger sweep would group — and the
    /// rebuild must never sweep spec twins into an invented production place.
    #[test]
    fn should_not_sweep_a_test_zone_stranger_into_a_token_group() {
        // no priced edges anywhere: both zone files look unanchored, and their
        // shared `case` token would form a group of two without the guard.
        let graph = Csr::from_weighted_edges(2, &[]);
        let condensation = singleton_condensation(2);
        let base = Partition::from_assignment(vec![ClusterId(0), ClusterId(0)], 1);
        let files = vec![
            file_info(0, "refund_case.py", 1),
            file_info(0, "date_case.py", 1),
        ];
        let mut names = vec![SmolStr::new("helpers")];
        let mut synthetic = vec![false];

        let rebuilt = synthesize_roof_rebuild(
            &files,
            &condensation,
            &graph,
            &[true, true],
            &base,
            &mut names,
            &mut synthetic,
        );

        assert!(
            rebuilt.is_none(),
            "all-test-zone SCCs must never enter the stranger population, no \
             matter what token they share"
        );
    }

    /// The spec lives in its own real directory, so the real-dir partition
    /// starts it apart from its twin; the tie-cut prices their bond to zero
    /// so polish never pulls them together. The shadow pass is what joins
    /// them.
    #[test]
    fn should_shadow_follow_a_spec_to_its_subjects_cluster() {
        let nodes = vec![
            node(0, "openai", 3, Polarity::Production),
            node(1, "openai_spec", 4, Polarity::TestCase),
        ];
        let edges = vec![edge(1, 0)];
        let containers = vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "spec", ScopeLevel::Folder, Some(0)),
            container(3, "src/openai.ts", ScopeLevel::File, Some(1)),
            container(4, "spec/openai.spec.ts", ScopeLevel::File, Some(2)),
        ];
        let snap = snapshot(nodes, edges, containers);

        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(
            &snap,
            &AnalyzeConfig::default(),
            Coefficients::anchored(),
            true,
            &tests,
        );

        let scc_of = |file_container: u32| -> u32 {
            let vertex = solver
                .index_of
                .get(&file_container)
                .copied()
                .unwrap_or(u32::MAX);
            solver
                .condensation
                .membership
                .get(vertex as usize)
                .map_or(u32::MAX, |scc| scc.0)
        };

        let mut parts = solver.real_partition.clone();
        let unit = scc_of(4); // the spec
        let subject = scc_of(3); // the twin
        assert_ne!(
            parts.cluster_of(unit),
            parts.cluster_of(subject),
            "setup: real dirs must start the pair apart"
        );

        solver.shadow_tests(&mut parts);

        assert_eq!(parts.cluster_of(unit), parts.cluster_of(subject));
    }

    #[test]
    fn should_keep_an_ambiguously_twinned_spec_where_it_is() {
        // Two production files reduce to the same stem in the same package —
        // no unique twin, so the shadow pass must leave the spec alone (ADR-0002
        // rule 1: unchanged placements are never listed).
        let nodes = vec![
            node(0, "one", 4, Polarity::Production),
            node(1, "two", 5, Polarity::Production),
            node(2, "spec", 3, Polarity::TestCase),
        ];
        let edges = vec![edge(2, 0)];
        let containers = vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "spec", ScopeLevel::Folder, Some(0)),
            container(2, "alt", ScopeLevel::Folder, Some(0)),
            container(3, "spec/openai.spec.ts", ScopeLevel::File, Some(1)),
            container(4, "alt/openai.ts", ScopeLevel::File, Some(2)),
            container(5, "alt/openai.tsx", ScopeLevel::File, Some(2)),
        ];
        let snap = snapshot(nodes, edges, containers);

        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(
            &snap,
            &AnalyzeConfig::default(),
            Coefficients::anchored(),
            true,
            &tests,
        );

        let scc_of = |file_container: u32| -> u32 {
            let vertex = solver
                .index_of
                .get(&file_container)
                .copied()
                .unwrap_or(u32::MAX);
            solver
                .condensation
                .membership
                .get(vertex as usize)
                .map_or(u32::MAX, |scc| scc.0)
        };

        let mut parts = solver.real_partition.clone();
        let unit = scc_of(3);
        let before = parts.cluster_of(unit);
        assert!(before.is_some(), "setup: the spec starts placed");
        assert_ne!(
            before,
            parts.cluster_of(scc_of(4)),
            "setup: the spec starts apart from its would-be twins"
        );

        solver.shadow_tests(&mut parts);

        assert_eq!(parts.cluster_of(unit), before);
    }

    #[test]
    fn should_veto_a_shadow_move_that_would_overflow_the_folder_cap() {
        // A folder cap of one leaves no room next to the twin; the cap veto
        // binds exactly like it does for polish moves.
        let config = AnalyzeConfig {
            capacity: crate::CapacityConfig {
                folder: 1,
                ..crate::CapacityConfig::default()
            },
            ..AnalyzeConfig::default()
        };
        let nodes = vec![
            node(0, "openai", 3, Polarity::Production),
            node(1, "openai_spec", 4, Polarity::TestCase),
        ];
        let edges = vec![edge(1, 0)];
        let containers = vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "spec", ScopeLevel::Folder, Some(0)),
            container(3, "src/openai.ts", ScopeLevel::File, Some(1)),
            container(4, "spec/openai.spec.ts", ScopeLevel::File, Some(2)),
        ];
        let snap = snapshot(nodes, edges, containers);

        let tests = TestPolicy::defaults();
        let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);

        let scc_of = |file_container: u32| -> u32 {
            let vertex = solver
                .index_of
                .get(&file_container)
                .copied()
                .unwrap_or(u32::MAX);
            solver
                .condensation
                .membership
                .get(vertex as usize)
                .map_or(u32::MAX, |scc| scc.0)
        };

        let mut parts = solver.real_partition.clone();
        let unit = scc_of(4);
        let subject = scc_of(3);
        assert_ne!(
            parts.cluster_of(unit),
            parts.cluster_of(subject),
            "setup: real dirs must start the pair apart"
        );

        solver.shadow_tests(&mut parts);

        assert_ne!(
            parts.cluster_of(unit),
            parts.cluster_of(subject),
            "the cap veto binds even against a unique twin"
        );
    }
}
