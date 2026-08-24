//! The case runner: drives strata in-process, evaluates preconditions and
//! assertions, and reports distance as verdicts.
//!
//! The engine is a library here exactly as the CLI uses it:
//! [`snapshot_from_root`] + [`analyze`] with an explicit config. Nothing shells
//! out; nothing reads a golden score. `STRATA_BLESS_EVAL=1` only regenerates a
//! diagnostic dump — it never rewrites expectations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use strata_engine::config::{AnalyzeConfig, Mode};
use strata_engine::result::{
    AnalyzeResult, Candidate, ContainerNode, Level, ModeResult, Severity as EngineSeverity,
    Violation, ViolationKind as EngineViolationKind,
};
use strata_engine::{analyze, snapshot_from_root};

use crate::error::EvalError;
use crate::metrics;
use crate::target::{
    AssertBlock, BandScope, BucketName, CapacityRelief, ConfigSource, FaceMode, MoveBudget,
    NameAlignment, NonInversion, PathSetAssertion, Precondition, PreconditionKind, PreserveDir,
    PreserveSymbolHome, ReferenceSet, RunMode, SeverityFilter, SizeBand, TargetSpec,
    ViolationClass,
};

/// One measured outcome: what was checked, whether the best state satisfies it,
/// and — on failure — the defect named concretely enough to act on.
#[derive(Debug, Clone)]
pub struct Verdict {
    /// Stable label like `separate(string_utils+charge)#greenfield`.
    pub label: String,
    /// `true` when the asserted best state holds.
    pub passed: bool,
    /// Observed facts; on failure this names the defect, never just "failed".
    pub detail: String,
}

/// The non-gating candidate-distinctness observation (QUAL-P2-1).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModeObservation {
    /// Which mode's candidate set was observed.
    pub face: FaceMode,
    /// Candidates the mode actually returned.
    pub candidates: usize,
    /// The mode's own convergence flag.
    pub solution_space_converged: bool,
    /// Whether candidates 1..k have pairwise-distinct placement signatures.
    pub pairwise_distinct_trees: bool,
}

/// Report-only pair-F1 rows: `(face, precision, recall, f1)`.
type PairF1Report = Vec<(FaceMode, f64, f64, f64)>;

/// Everything one case produced: errors (harness or corpus defects),
/// precondition verdicts, assertion verdicts, report-only pair-F1, and the
/// distinctness observations.
#[derive(Debug, Clone)]
pub struct CaseReport {
    /// The fixture the case ran against.
    pub fixture: String,
    /// Harness or corpus defects; any entry means the verdicts are not
    /// meaningful and the case must fail loudly.
    pub errors: Vec<EvalError>,
    /// Preconditions checked against `current` (all must pass for meaningful
    /// verdicts).
    pub preconditions: Vec<Verdict>,
    /// Distance-to-best-state verdicts; failures are signal, not shame.
    pub verdicts: Vec<Verdict>,
    /// Report-only pair-F1 per face against the reference tree, when present.
    pub pair_f1: PairF1Report,
    /// Non-gating diversity observations per evaluated mode.
    pub observations: Vec<ModeObservation>,
}

impl CaseReport {
    /// A report for a case that aborted before producing verdicts.
    #[must_use]
    pub fn broken(fixture: &str, error: EvalError) -> Self {
        Self {
            fixture: fixture.to_owned(),
            errors: vec![error],
            preconditions: Vec::new(),
            verdicts: Vec::new(),
            pair_f1: Vec::new(),
            observations: Vec::new(),
        }
    }

    /// Aggregates every failure into one loud message for `assert!`.
    ///
    /// Empty when the case is clean; otherwise each line names its defect so a
    /// red run reads like findings, not stack traces.
    #[must_use]
    pub fn failure_message(&self) -> Option<String> {
        let mut lines = Vec::new();
        for error in &self.errors {
            lines.push(format!("ERROR {error}"));
        }
        for verdict in self.preconditions.iter().filter(|verdict| !verdict.passed) {
            lines.push(format!(
                "PRECONDITION {} :: {}",
                verdict.label, verdict.detail
            ));
        }
        for verdict in self.verdicts.iter().filter(|verdict| !verdict.passed) {
            lines.push(format!("ASSERT {} :: {}", verdict.label, verdict.detail));
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n  "))
        }
    }
}

/// The source-root set mirrored from `AdaptersConfig::default`; all first-batch
/// targets run on built-in defaults, so container keys strip exactly these.
const SOURCE_ROOTS: [&str; 7] = ["src", "spec", "test", "tests", "lib", "dist", "__tests__"];

/// One analyzed package scope: the Package node's name and, when that name is
/// also a real directory prefix of census files, the `<root>/` prefix to strip.
#[derive(Debug, Clone)]
struct PackageScope {
    name: String,
    prefix: Option<String>,
}

/// Per-face evaluation inputs: the asserted candidate tree plus how much hard
/// capacity it leaves behind.
struct FaceInputs<'a> {
    tree: &'a ContainerNode,
    capacity_remaining: Option<u32>,
}

/// Everything the pure evaluator needs; built once per case from the engine
/// result, and synthesizable in unit tests without the engine.
struct EvalInputs<'a> {
    current_placement: BTreeMap<String, String>,
    packages: Vec<PackageScope>,
    census: BTreeSet<String>,
    faces: BTreeMap<FaceMode, FaceInputs<'a>>,
}

/// CONTRACT.md's D(P, path): fixture files whose repo-relative path lies under
/// `<P.root>/[<source-root>/]<path>/`, computed per package scope over the
/// census.
fn directory_members(census: &BTreeSet<String>, package: &PackageScope, path: &str) -> Vec<String> {
    census
        .iter()
        .filter(|file| directory_contains(package.prefix.as_deref(), file, path))
        .cloned()
        .collect()
}

/// Every package scope that physically holds `path`, with its member list.
fn packages_holding<'a>(
    census: &BTreeSet<String>,
    packages: &'a [PackageScope],
    path: &str,
) -> Vec<(&'a PackageScope, Vec<String>)> {
    packages
        .iter()
        .map(|package| (package, directory_members(census, package, path)))
        .filter(|(_, members)| !members.is_empty())
        .collect()
}

/// Whether `file` sits under `[<package-root>/][<source-root>/]<path>/`.
fn directory_contains(package_prefix: Option<&str>, file: &str, path: &str) -> bool {
    let mut rest = match package_prefix {
        Some(root) => match file
            .strip_prefix(root)
            .and_then(|tail| tail.strip_prefix('/'))
        {
            Some(tail) => tail,
            None => return false,
        },
        None => file,
    };
    // One leading source-root segment below the package root is transparent.
    if let Some((first, tail)) = rest.split_once('/')
        && SOURCE_ROOTS.contains(&first)
    {
        rest = tail;
    }
    let slash_after_path = || rest.as_bytes().get(path.len()).copied() == Some(b'/');
    rest == path || (rest.starts_with(path) && rest.len() > path.len() && slash_after_path())
}

/// Whether `file` lives directly under directory `dir` (`dir/…`).
fn starts_with_segment(file: &str, dir: &str) -> bool {
    file.strip_prefix(dir)
        .is_some_and(|tail| tail.starts_with('/'))
}

/// Derives the package scopes from the current tree's Package nodes: a name
/// that prefixes census files is a real directory root; a lone package whose
/// name prefixes nothing is the repository-rooted single-package case.
fn discover_packages(current: &ContainerNode, census: &BTreeSet<String>) -> Vec<PackageScope> {
    let mut names = Vec::new();
    collect_package_names(current, &mut names);
    names
        .into_iter()
        .map(|name| {
            let prefix = census
                .iter()
                .any(|file| starts_with_segment(file, &name))
                .then(|| name.clone());
            PackageScope { name, prefix }
        })
        .collect()
}

/// Collects distinct Package-level node names in tree order.
fn collect_package_names(node: &ContainerNode, names: &mut Vec<String>) {
    if node.level == Level::Package && !names.contains(&node.name) {
        names.push(node.name.clone());
    }
    for child in node.children.iter().flatten() {
        collect_package_names(child, names);
    }
}

/// Finds the Package node with `name`, searching the whole tree.
fn find_package_node<'a>(node: &'a ContainerNode, name: &str) -> Option<&'a ContainerNode> {
    if node.level == Level::Package && node.name == name {
        return Some(node);
    }
    node.children
        .iter()
        .flatten()
        .find_map(|child| find_package_node(child, name))
}

/// Runs one target against its fixture: analyze in-process, then measure.
///
/// # Errors
///
/// Returns [`EvalError`] when the case aborts before its verdicts are
/// meaningful: invalid target, unreadable fixture, missing candidate.
pub fn run_case(
    root: &Path,
    spec: &TargetSpec,
    expected_fixture: &str,
) -> Result<CaseReport, EvalError> {
    spec.validate(expected_fixture)?;

    let mut config = AnalyzeConfig::default();
    config.analysis.mode = match spec.run.mode {
        RunMode::Anchored => Mode::Anchored,
        RunMode::Greenfield => Mode::Greenfield,
        RunMode::Both => Mode::Both,
    };
    config.analysis.candidates = spec.run.candidates;
    config.analysis.seed = spec.run.seed;
    match spec.run.config {
        ConfigSource::Defaults => {}
        ConfigSource::Fixture => {
            let fixture_config = root.join("strata.toml");
            config = strata_engine::load_config(&fixture_config).map_err(|error| {
                EvalError::EngineRun {
                    fixture: expected_fixture.to_owned(),
                    message: format!("loading {}: {error}", fixture_config.display()),
                }
            })?;
        }
    }

    let snapshot = snapshot_from_root(root, &config).map_err(|error| EvalError::EngineRun {
        fixture: expected_fixture.to_owned(),
        message: error.to_string(),
    })?;
    let result = analyze(&snapshot, &config).map_err(|error| EvalError::EngineRun {
        fixture: expected_fixture.to_owned(),
        message: error.to_string(),
    })?;

    let block: &AssertBlock = spec
        .assert
        .first()
        .ok_or_else(|| EvalError::TargetInvalid {
            target: expected_fixture.to_owned(),
            message: "exactly one [[assert]] block is required".to_owned(),
        })?;

    let census: BTreeSet<String> = metrics::members(&result.current.tree).into_iter().collect();
    let packages = discover_packages(&result.current.tree, &census);
    let mut errors = validate_references(
        spec,
        block,
        &census,
        &packages,
        &result.current.tree,
        expected_fixture,
    );

    // Preconditions are verified against current BEFORE scoring; a failure is a
    // harness or fixture defect, never distance, so it lands in `errors` too.
    let preconditions = evaluate_preconditions(
        &spec.precondition,
        result.summary.files,
        &result.current.violations,
    );
    for verdict in preconditions.iter().filter(|verdict| !verdict.passed) {
        errors.push(EvalError::TargetInvalid {
            target: expected_fixture.to_owned(),
            message: format!(
                "precondition `{}` failed against current: {} (harness or fixture defect, never distance)",
                verdict.label, verdict.detail
            ),
        });
    }

    let inputs = build_inputs(&result, block, expected_fixture)?;
    let (verdicts, pair_f1) = evaluate_assertions(block, &inputs, spec.reference.as_ref());
    let observations = observe_modes(&result, &inputs.faces);

    let mut report = CaseReport {
        fixture: expected_fixture.to_owned(),
        errors,
        preconditions,
        verdicts,
        pair_f1,
        observations,
    };
    dump_diagnostics(&result, &mut report);
    Ok(report)
}

/// Validates every census-dependent reference before scoring: referenced file
/// paths must exist, and every `preserve_dir` key must physically resolve under
/// at least one analyzed package. A misspelled path is an error, never fake
/// distance.
fn validate_references(
    spec: &TargetSpec,
    block: &AssertBlock,
    census: &BTreeSet<String>,
    packages: &[PackageScope],
    current: &ContainerNode,
    fixture: &str,
) -> Vec<EvalError> {
    let mut errors = Vec::new();
    let mut check_file_set = |kind: &str, paths: &[String]| {
        for path in paths {
            if !census.contains(path) {
                errors.push(EvalError::TargetInvalid {
                    target: fixture.to_owned(),
                    message: format!(
                        "{kind} references {path:?}, which the fixture census does not contain"
                    ),
                });
            }
        }
    };
    for assertion in block.keep_together.iter().chain(block.separate.iter()) {
        check_file_set("keep_together/separate", &assertion.paths);
    }
    if let Some(reference) = &spec.reference {
        for container in &reference.container {
            check_file_set("reference.container", &container.files);
        }
    }

    for preserve in &block.preserve_dir {
        if packages_holding(census, packages, &preserve.path).is_empty() {
            errors.push(EvalError::TargetInvalid {
                target: fixture.to_owned(),
                message: format!(
                    "preserve_dir {:?} resolves under no analyzed package (checked <root>/[<source-root>/]<path>/ against the census)",
                    preserve.path
                ),
            });
        }
    }

    // FIX08: a symbol home must exist in the CURRENT layout — the assertion
    // pins an existing home, so a misspelled symbol or path is a harness
    // error, never permanent fake distance.
    let homes = current_symbol_homes(current);
    for assertion in &block.preserve_symbol_home {
        if !homes
            .get(&assertion.path)
            .is_some_and(|symbols| symbols.contains(&assertion.symbol))
        {
            errors.push(EvalError::TargetInvalid {
                target: fixture.to_owned(),
                message: format!(
                    "preserve_symbol_home pins {:?} in {:?}, which the current layout does not show",
                    assertion.symbol, assertion.path
                ),
            });
        }
    }
    errors
}

/// Maps every file node of `root` to the set of symbol names it carries.
fn current_symbol_homes(root: &ContainerNode) -> BTreeMap<String, BTreeSet<String>> {
    let mut homes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    metrics::walk_files(root, &mut |node| {
        if let Some(symbols) = &node.symbols {
            let entry = homes.entry(node.name.clone()).or_default();
            for placement in symbols {
                entry.insert(placement.name.clone());
            }
        }
    });
    homes
}

/// Builds the evaluator inputs: current-tree placement plus one face entry per
/// mode any assertion needs, each pointing at its asserted candidate.
fn build_inputs<'a>(
    result: &'a AnalyzeResult,
    block: &AssertBlock,
    fixture: &str,
) -> Result<EvalInputs<'a>, EvalError> {
    let fail = |message: String| EvalError::TargetInvalid {
        target: fixture.to_owned(),
        message,
    };

    let mut needed: Vec<FaceMode> = block.modes.clone();
    for face in block
        .preserve_dir
        .iter()
        .filter_map(|assertion| assertion.mode)
        .chain(block.keep_together.iter().filter_map(|a| a.mode))
        .chain(block.separate.iter().filter_map(|a| a.mode))
        .chain(block.size_band.iter().filter_map(|a| a.mode))
        .chain(block.move_budget.iter().map(|budget| budget.mode))
        .chain(block.no_synthetic_bucket.iter().filter_map(|a| a.mode))
        .chain(block.name_alignment.iter().filter_map(|a| a.mode))
        .chain(block.capacity_relief.iter().map(|relief| relief.mode))
        .chain(
            block
                .preserve_symbol_home
                .iter()
                .filter_map(|assertion| assertion.mode),
        )
    {
        if !needed.contains(&face) {
            needed.push(face);
        }
    }

    let mut faces = BTreeMap::new();
    for face in needed {
        let mode_result = mode_result_of(result, face).ok_or_else(|| {
            fail(format!(
                "assertions need the {face:?} mode but [run].mode did not produce it"
            ))
        })?;
        let index = usize::try_from(block.candidate - 1);
        let candidate = match index {
            Ok(index) => mode_result.candidates.get(index),
            Err(_) => None,
        };
        let candidate = candidate.ok_or_else(|| {
            fail(format!(
                "candidate {} requested but the {face:?} mode returned {} candidates",
                block.candidate,
                mode_result.candidates.len()
            ))
        })?;
        faces.insert(
            face,
            FaceInputs {
                tree: &candidate.tree,
                capacity_remaining: candidate
                    .capacity_remainder
                    .map(|remainder| remainder.remaining),
            },
        );
    }

    Ok(EvalInputs {
        current_placement: metrics::structural_placement(&result.current.tree),
        packages: discover_packages(&result.current.tree, &census_of(result)),
        census: census_of(result),
        faces,
    })
}

/// The analyzed file census, recomputed from the current tree.
fn census_of(result: &AnalyzeResult) -> BTreeSet<String> {
    metrics::members(&result.current.tree).into_iter().collect()
}

/// The mode's [`ModeResult`], or `None` when the run did not produce the face.
fn mode_result_of(result: &AnalyzeResult, face: FaceMode) -> Option<&ModeResult> {
    match face {
        FaceMode::Anchored => result.modes.anchored.as_ref(),
        FaceMode::Greenfield => result.modes.greenfield.as_ref(),
    }
}

/// Evaluates every assertion in the block once per face it applies to, then the
/// report-only pair-F1 per evaluated face.
fn evaluate_assertions(
    block: &AssertBlock,
    inputs: &EvalInputs<'_>,
    reference: Option<&ReferenceSet>,
) -> (Vec<Verdict>, PairF1Report) {
    let mut verdicts = Vec::new();
    for assertion in &block.preserve_dir {
        for face in faces_for(assertion.mode, &block.modes) {
            verdicts.push(evaluate_preserve_dir(assertion, face, inputs));
        }
    }
    for assertion in &block.keep_together {
        for face in faces_for(assertion.mode, &block.modes) {
            verdicts.push(evaluate_path_set(assertion, face, inputs, true));
        }
    }
    for assertion in &block.separate {
        for face in faces_for(assertion.mode, &block.modes) {
            verdicts.push(evaluate_path_set(assertion, face, inputs, false));
        }
    }
    for band in &block.size_band {
        for face in faces_for(band.mode, &block.modes) {
            verdicts.push(evaluate_size_band(band, face, inputs));
        }
    }
    for budget in &block.move_budget {
        verdicts.push(evaluate_move_budget(budget, inputs));
    }
    for bucket in &block.no_synthetic_bucket {
        for face in faces_for(bucket.mode, &block.modes) {
            verdicts.push(evaluate_no_synthetic_bucket(bucket, face, inputs));
        }
    }
    for alignment in &block.name_alignment {
        for face in faces_for(alignment.mode, &block.modes) {
            verdicts.push(evaluate_name_alignment(alignment, face, inputs));
        }
    }
    for relief in &block.capacity_relief {
        verdicts.push(evaluate_capacity_relief(relief, inputs));
    }
    for inversion in &block.non_inversion {
        verdicts.push(evaluate_non_inversion(inversion, inputs));
    }
    for home in &block.preserve_symbol_home {
        for face in faces_for(home.mode, &block.modes) {
            verdicts.push(evaluate_preserve_symbol_home(home, face, inputs));
        }
    }

    let mut pair_f1: PairF1Report = Vec::new();
    if let Some(reference) = reference {
        let universe: BTreeSet<String> = reference
            .container
            .iter()
            .flat_map(|container| container.files.iter().cloned())
            .collect();
        let reference_pairs = reference_pairs(reference);
        for (face, face_inputs) in &inputs.faces {
            let predicted = metrics::co_membership_pairs(face_inputs.tree, &universe);
            let (precision, recall, f1) = metrics::pair_f1(&reference_pairs, &predicted);
            pair_f1.push((*face, precision, recall, f1));
        }
    }
    (verdicts, pair_f1)
}

/// `preserve_symbol_home`: the named symbol still resides in the named file of
/// the asserted candidate tree. File identity is the full repo-relative path
/// (stable across trees per the contract), and symbol membership is read off
/// the file's `symbols` list — exactly what FIX08's symbol-grain relocation
/// rewrites, so this is the assertion that catches gratuitous home churn.
fn evaluate_preserve_symbol_home(
    assertion: &PreserveSymbolHome,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let label = format!("preserve_symbol_home({})#{face:?}", assertion.symbol);
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let mut found_home = false;
    let mut now_in: Vec<String> = Vec::new();
    metrics::walk_files(tree, &mut |node| {
        let Some(symbols) = &node.symbols else {
            return;
        };
        let carries = symbols
            .iter()
            .any(|placement| placement.name == assertion.symbol);
        if carries && node.name == assertion.path {
            found_home = true;
        }
        if carries {
            now_in.push(node.name.clone());
        }
    });
    Verdict {
        label,
        passed: found_home,
        detail: if found_home {
            format!("{} still lives in {}", assertion.symbol, assertion.path)
        } else if now_in.is_empty() {
            format!(
                "{} does not appear in any file of the candidate tree",
                assertion.symbol
            )
        } else {
            format!(
                "{} left its pinned home {}: it now appears in {}",
                assertion.symbol,
                assertion.path,
                now_in.join(", ")
            )
        },
    }
}

/// The faces an assertion applies to: its own override, else the block default.
fn faces_for(own: Option<FaceMode>, block_modes: &[FaceMode]) -> Vec<FaceMode> {
    match own {
        Some(face) => vec![face],
        None => block_modes.to_vec(),
    }
}

/// The reference pair set: unordered pairs within each labeled container.
fn reference_pairs(reference: &ReferenceSet) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    for container in &reference.container {
        for (index, left) in container.files.iter().enumerate() {
            for right in container.files.iter().skip(index + 1) {
                pairs.insert(normalize_pair(left, right));
            }
        }
    }
    pairs
}

/// Normalizes an unordered pair so set comparisons never depend on order.
fn normalize_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

/// `preserve_dir`: within each package that physically has the directory, some
/// folder/domain node named exactly `path` must still contain all of D(P, path).
fn evaluate_preserve_dir(
    assertion: &PreserveDir,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let label = format!("preserve_dir({})#{face:?}", assertion.path);
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let holdings = packages_holding(&inputs.census, &inputs.packages, &assertion.path);
    if holdings.is_empty() {
        return Verdict {
            label,
            passed: false,
            detail: format!(
                "directory {:?} resolves under no analyzed package",
                assertion.path
            ),
        };
    }
    let mut missing_reports = Vec::new();
    for (package, expected) in &holdings {
        let satisfied = find_package_node(tree, &package.name).is_some_and(|package_node| {
            surviving_container(package_node, &assertion.path, expected)
        });
        if !satisfied {
            missing_reports.push(format!(
                "package {}: no folder/domain node named {:?} holds all of D = [{}]",
                package.name,
                assertion.path,
                expected.join(", ")
            ));
        }
    }
    Verdict {
        label,
        passed: missing_reports.is_empty(),
        detail: if missing_reports.is_empty() {
            format!(
                "directory {:?} survives as a named container in every holding package",
                assertion.path
            )
        } else {
            format!("directory tearing: {}", missing_reports.join("; "))
        },
    }
}

/// Whether some folder/domain descendant named exactly `path` contains every
/// file in `expected`.
fn surviving_container(package_node: &ContainerNode, path: &str, expected: &[String]) -> bool {
    let expected_set: BTreeSet<&String> = expected.iter().collect();
    let mut found = false;
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            if node.name == path && metrics::is_scoped_container(node) {
                let member_list = metrics::members(node);
                let members: BTreeSet<&String> = member_list.iter().collect();
                if expected_set.is_subset(&members) {
                    found = true;
                }
            }
        };
        metrics::walk_containers(package_node, &mut visit);
    }
    found
}

/// `keep_together` / `separate`: co-location required, or co-location banned,
/// over folder/domain containers only.
fn evaluate_path_set(
    assertion: &PathSetAssertion,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
    keep: bool,
) -> Verdict {
    let kind = if keep { "keep_together" } else { "separate" };
    let label = format!("{kind}({})#{face:?}", assertion.paths.join("+"));
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let wanted: BTreeSet<&String> = assertion.paths.iter().collect();
    let mut offenders: Vec<String> = Vec::new();
    let mut satisfied = false;
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            if !metrics::is_scoped_container(node) {
                return;
            }
            let member_list = metrics::members(node);
            let members: BTreeSet<&String> = member_list.iter().collect();
            let held: Vec<String> = wanted
                .intersection(&members)
                .map(|path| (*path).clone())
                .collect();
            if keep {
                if wanted.is_subset(&members) {
                    satisfied = true;
                }
            } else if held.len() >= 2 {
                offenders.push(format!(
                    "{:?} co-locates {}",
                    node.name,
                    held.join(" with ")
                ));
            }
        };
        metrics::walk_containers(tree, &mut visit);
    }

    if keep {
        Verdict {
            label,
            passed: satisfied,
            detail: if satisfied {
                "some folder/domain container co-locates the set".to_owned()
            } else {
                format!(
                    "no folder/domain container holds all of [{}]",
                    assertion.paths.join(", ")
                )
            },
        }
    } else {
        Verdict {
            label,
            passed: offenders.is_empty(),
            detail: if offenders.is_empty() {
                "no folder/domain container co-locates the set".to_owned()
            } else {
                format!("cross-directory welding: {}", offenders.join("; "))
            },
        }
    }
}

/// `size_band`: every selected container's transitive member count lies within
/// the inclusive band; ancestors bind, so nesting cannot dodge a split.
fn evaluate_size_band(band: &SizeBand, face: FaceMode, inputs: &EvalInputs<'_>) -> Verdict {
    let selector = match (&band.scope, &band.container) {
        (Some(BandScope::AnyContainer), _) => "any_container".to_owned(),
        (_, Some(name)) => format!("container({name})"),
        (None, None) => "unscoped".to_owned(),
    };
    let label = format!("size_band({selector},max={})#{face:?}", band.max_files);
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let mut worst: Option<(String, usize)> = None;
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            let selected = match (&band.scope, &band.container) {
                (Some(BandScope::AnyContainer), _) => metrics::is_scoped_container(node),
                (_, Some(name)) => node.name == *name && node.level != Level::File,
                (None, None) => false,
            };
            if !selected {
                return;
            }
            let count = metrics::members(node).len();
            let over_max = usize::try_from(band.max_files).is_ok_and(|max| count > max);
            let under_min = band
                .min_files
                .and_then(|min| usize::try_from(min).ok())
                .is_some_and(|min| count < min);
            if over_max || under_min {
                let worse = worst
                    .as_ref()
                    .is_none_or(|(_, worst_count)| count > *worst_count);
                if worse {
                    worst = Some((node.name.clone(), count));
                }
            }
        };
        metrics::walk_containers(tree, &mut visit);
    }

    match worst {
        None => Verdict {
            label,
            passed: true,
            detail: "every selected container sits within its band".to_owned(),
        },
        Some((name, count)) => {
            let bound = band.min_files.map_or_else(
                || format!("max {}", band.max_files),
                |min| format!("min {min}/max {}", band.max_files),
            );
            Verdict {
                label,
                passed: false,
                detail: format!(
                    "over-capacity: container {name:?} holds {count} members against {bound} (transitive members bind ancestors too)"
                ),
            }
        }
    }
}

/// `move_budget`: structural moved-file count between current and the asserted
/// candidate, bounded inclusively. Narration never feeds this count.
fn evaluate_move_budget(budget: &MoveBudget, inputs: &EvalInputs<'_>) -> Verdict {
    let face = budget.mode;
    let bound_label = match budget.max_moved_files {
        Some(max) => format!("max_moved_files={max}"),
        None => format!(
            "min_moved_files={}",
            budget.min_moved_files.unwrap_or_default()
        ),
    };
    let label = format!("move_budget({face:?},{bound_label})");

    let placement = face_placement(inputs, face);
    let moved = metrics::moved_files(&inputs.current_placement, &placement);
    let count = moved.len();
    let within = if let Some(max) = budget
        .max_moved_files
        .and_then(|max| usize::try_from(max).ok())
    {
        count <= max
    } else {
        budget
            .min_moved_files
            .and_then(|min| usize::try_from(min).ok())
            .is_some_and(|min| count >= min)
    };
    Verdict {
        label,
        passed: within,
        detail: if within {
            format!("{face:?} moves {count} files structurally, within its budget")
        } else {
            format!(
                "{face:?} moves {count} files structurally against its budget ({bound_label}): [{}]",
                moved.join(", ")
            )
        },
    }
}

/// The structural placement of a face's asserted candidate tree.
fn face_placement(inputs: &EvalInputs<'_>, face: FaceMode) -> BTreeMap<String, String> {
    inputs
        .faces
        .get(&face)
        .map(|face_inputs| metrics::structural_placement(face_inputs.tree))
        .unwrap_or_default()
}

/// `no_synthetic_bucket`: no non-file node may carry the forbidden last segment.
fn evaluate_no_synthetic_bucket(
    bucket: &BucketName,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let label = format!("no_synthetic_bucket({})#{face:?}", bucket.name);
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let mut offenders: Vec<String> = Vec::new();
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            if node.level != Level::File && metrics::last_segment(&node.name) == bucket.name {
                offenders.push(format!("{:?} ({:?})", node.name, node.level));
            }
        };
        metrics::walk_containers(tree, &mut visit);
    }
    Verdict {
        label,
        passed: offenders.is_empty(),
        detail: if offenders.is_empty() {
            format!("no node carries the synthetic bucket {:?}", bucket.name)
        } else {
            format!(
                "workspace collapse: synthetic bucket {:?} appears at {}",
                bucket.name,
                offenders.join(", ")
            )
        },
    }
}

/// `name_alignment`: every folder/domain container with at least `min_members`
/// members keeps at least `min_ratio` of them sharing a naming token.
fn evaluate_name_alignment(
    alignment: &NameAlignment,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
    let min_ratio = alignment.min_ratio;
    let min_members_requested = alignment.min_members;
    let label = format!(
        "name_alignment(min_ratio={min_ratio},min_members={min_members_requested})#{face:?}"
    );
    let Some(tree) = inputs.faces.get(&face).map(|face_inputs| face_inputs.tree) else {
        return Verdict {
            label: format!("#{face:?}"),
            passed: false,
            detail: format!("the {face:?} mode was not evaluated"),
        };
    };
    let min_members = usize::try_from(alignment.min_members).unwrap_or(usize::MAX);
    let mut worst: Option<(String, usize, usize)> = None;
    {
        let mut visit = |node: &ContainerNode, _chain: &str| {
            if !metrics::is_scoped_container(node) {
                return;
            }
            let members = metrics::members(node);
            if members.len() < min_members || members.is_empty() {
                return;
            }
            let (aligned, total) = metrics::alignment_counts(&node.name, &members);
            let below_floor =
                metrics::alignment_sharing(&node.name, &members) < min_ratio - f64::EPSILON;
            let worse = worst
                .as_ref()
                .is_none_or(|(_, _, worst_aligned)| aligned < *worst_aligned);
            if below_floor && worse {
                worst = Some((node.name.clone(), aligned, total));
            }
        };
        metrics::walk_containers(tree, &mut visit);
    }

    match worst {
        None => Verdict {
            label,
            passed: true,
            detail: "every qualifying container aligns with its members".to_owned(),
        },
        Some((name, aligned, total)) => Verdict {
            label,
            passed: false,
            detail: format!(
                "naming incoherence: container {name:?} aligns {aligned}/{total} members below min_ratio {min_ratio}"
            ),
        },
    }
}

/// `capacity_relief`: the asserted candidate leaves no hard capacity finding.
fn evaluate_capacity_relief(relief: &CapacityRelief, inputs: &EvalInputs<'_>) -> Verdict {
    let face = relief.mode;
    let label = format!("capacity_relief({face:?})");
    let remaining = inputs
        .faces
        .get(&face)
        .and_then(|face_inputs| face_inputs.capacity_remaining);
    let passed = remaining.is_none_or(|count| count == 0);
    let detail = match remaining {
        None => format!("{face:?} candidate leaves no capacityRemainder"),
        Some(0) => {
            format!("{face:?} candidate leaves capacityRemainder.remaining = 0")
        }
        Some(count) => format!(
            "unrelieved capacity: {face:?} candidate still carries capacityRemainder.remaining = {count} hard findings"
        ),
    };
    Verdict {
        label,
        passed,
        detail,
    }
}

/// `non_inversion`: anchored may never move more files structurally than
/// greenfield on the same snapshot.
fn evaluate_non_inversion(inversion: &NonInversion, inputs: &EvalInputs<'_>) -> Verdict {
    let label = "non_inversion".to_owned();
    let anchored = face_placement(inputs, FaceMode::Anchored);
    let greenfield = face_placement(inputs, FaceMode::Greenfield);
    let anchored_moved = metrics::moved_files(&inputs.current_placement, &anchored).len();
    let greenfield_moved = metrics::moved_files(&inputs.current_placement, &greenfield).len();
    let passed = anchored_moved <= greenfield_moved;
    let detail = if passed {
        format!(
            "anchored moves {anchored_moved} files, greenfield moves {greenfield_moved}: no inversion ({})",
            inversion.because
        )
    } else {
        format!(
            "anchored inversion: anchored moves {anchored_moved} files while greenfield moves {greenfield_moved} on the same snapshot"
        )
    };
    Verdict {
        label,
        passed,
        detail,
    }
}

/// Preconditions verified against the current layout before scoring.
fn evaluate_preconditions(
    preconditions: &[Precondition],
    files: u32,
    violations: &[Violation],
) -> Vec<Verdict> {
    preconditions
        .iter()
        .map(|precondition| {
            let label = precondition_label(precondition);
            let (passed, observed) = match precondition.kind {
                PreconditionKind::FileCount => {
                    let min = precondition.min.unwrap_or(0);
                    let max = precondition.max.unwrap_or(u32::MAX);
                    (
                        files >= min && files <= max,
                        format!("summary.files = {files} against [{min}, {max}]"),
                    )
                }
                PreconditionKind::ViolationPresent | PreconditionKind::ViolationAbsent => {
                    let matches = matching_violations(precondition, violations);
                    let present = !matches.is_empty();
                    let expect_present = precondition.kind == PreconditionKind::ViolationPresent;
                    let classes: Vec<String> = matches
                        .iter()
                        .map(|violation| violation_class_name(violation.kind))
                        .collect();
                    let observed = if present {
                        format!("found [{}]", classes.join(", "))
                    } else {
                        "found none".to_owned()
                    };
                    (present == expect_present, observed)
                }
            };
            Verdict {
                label,
                passed,
                detail: format!("{observed}; because {}", precondition.because),
            }
        })
        .collect()
}

/// A precondition's stable label.
fn precondition_label(precondition: &Precondition) -> String {
    let filters = class_filters(precondition);
    match precondition.kind {
        PreconditionKind::FileCount => "file_count".to_owned(),
        PreconditionKind::ViolationPresent => {
            format!("violation_present({filters})")
        }
        PreconditionKind::ViolationAbsent => {
            format!("violation_absent({filters})")
        }
    }
}

/// The violation class plus optional suffix/severity a precondition filters on.
fn class_filters(precondition: &Precondition) -> String {
    let class = match precondition.violation {
        Some(ViolationClass::Cycle) => violation_class_name(EngineViolationKind::Cycle),
        Some(ViolationClass::Polarity) => violation_class_name(EngineViolationKind::Polarity),
        Some(ViolationClass::Capacity) => violation_class_name(EngineViolationKind::Capacity),
        Some(ViolationClass::Visibility) => violation_class_name(EngineViolationKind::Visibility),
        None => "any".to_owned(),
    };
    let suffix = precondition
        .location_suffix
        .as_ref()
        .map(|suffix| format!("@{suffix}"))
        .unwrap_or_default();
    let severity = precondition
        .severity
        .map(|severity| match severity {
            SeverityFilter::Violation => "::violation",
            SeverityFilter::Borderline => "::borderline",
        })
        .unwrap_or_default();
    format!("{class}{suffix}{severity}")
}

/// The DTO violation kind rendered for messages.
fn violation_class_name(kind: EngineViolationKind) -> String {
    match kind {
        EngineViolationKind::Cycle => "cycle".to_owned(),
        EngineViolationKind::Polarity => "polarity".to_owned(),
        EngineViolationKind::Capacity => "capacity".to_owned(),
        EngineViolationKind::Visibility => "visibility".to_owned(),
    }
}

/// Class equality between the target's vocabulary and the DTO's.
fn class_matches(precondition: &Precondition, violation: &Violation) -> bool {
    let Some(wanted) = precondition.violation else {
        return true;
    };
    matches!(
        (wanted, violation.kind),
        (ViolationClass::Cycle, EngineViolationKind::Cycle)
            | (ViolationClass::Polarity, EngineViolationKind::Polarity)
            | (ViolationClass::Capacity, EngineViolationKind::Capacity)
            | (ViolationClass::Visibility, EngineViolationKind::Visibility)
    )
}

/// Severity filter; absent means any severity qualifies.
fn severity_matches(precondition: &Precondition, violation: &Violation) -> bool {
    match precondition.severity {
        None => true,
        Some(SeverityFilter::Violation) => violation.severity == EngineSeverity::Violation,
        Some(SeverityFilter::Borderline) => violation.severity == EngineSeverity::Borderline,
    }
}

/// The violations matching a precondition's class, severity, and dot-segment
/// location filters.
fn matching_violations<'a>(
    precondition: &Precondition,
    violations: &'a [Violation],
) -> Vec<&'a Violation> {
    violations
        .iter()
        .filter(|violation| {
            class_matches(precondition, violation)
                && severity_matches(precondition, violation)
                && precondition
                    .location_suffix
                    .as_deref()
                    .is_none_or(|suffix| {
                        violation
                            .location
                            .iter()
                            .any(|location| location_suffix_matches(location, suffix))
                    })
        })
        .collect()
}

/// Dot-segment location matching: the location's last dot-segment equals the
/// suffix, or the location ends with `.<suffix>`.
fn location_suffix_matches(location: &str, suffix: &str) -> bool {
    location.ends_with(&format!(".{suffix}"))
        || location.rsplit('.').next().unwrap_or(location) == suffix
}

/// Non-gating diversity observations per evaluated mode.
fn observe_modes(
    result: &AnalyzeResult,
    faces: &BTreeMap<FaceMode, FaceInputs<'_>>,
) -> Vec<ModeObservation> {
    faces
        .keys()
        .filter_map(|face| {
            let mode_result = mode_result_of(result, *face)?;
            let signatures: Vec<Vec<(String, String)>> = mode_result
                .candidates
                .iter()
                .map(|candidate: &Candidate| metrics::placement_signature(&candidate.tree))
                .collect();
            Some(ModeObservation {
                face: *face,
                candidates: mode_result.candidates.len(),
                solution_space_converged: mode_result.solution_space_converged,
                pairwise_distinct_trees: all_pairs_distinct(&signatures),
            })
        })
        .collect()
}

/// Whether every pair of placement signatures differs.
fn all_pairs_distinct(signatures: &[Vec<(String, String)>]) -> bool {
    for (index, left) in signatures.iter().enumerate() {
        for right in signatures.iter().skip(index + 1) {
            if left == right {
                return false;
            }
        }
    }
    true
}

/// A serializable diagnostics dump written when `STRATA_BLESS_EVAL` is set.
///
/// This regenerates evidence only — expectations live in the targets, never in
/// a golden file, so there is no bless path that could rewrite them.
#[derive(serde::Serialize)]
struct DumpVerdict {
    label: String,
    passed: bool,
    detail: String,
}

/// The dump document: the case's verdicts plus the full engine result.
#[derive(serde::Serialize)]
struct DumpDoc<'a> {
    fixture: &'a str,
    preconditions: Vec<DumpVerdict>,
    verdicts: Vec<DumpVerdict>,
    observations: Vec<ModeObservation>,
    pair_f1: PairF1Report,
    result: &'a AnalyzeResult,
}

/// Writes the diagnostic dump when the environment asks for one; a dump
/// failure surfaces as a case error, never silently.
fn dump_diagnostics(result: &AnalyzeResult, report: &mut CaseReport) {
    if std::env::var_os("STRATA_BLESS_EVAL").is_none() {
        return;
    }
    let fixture = report.fixture.clone();
    let doc = DumpDoc {
        fixture: &fixture,
        preconditions: dump_verdicts(&report.preconditions),
        verdicts: dump_verdicts(&report.verdicts),
        observations: report.observations.clone(),
        pair_f1: report.pair_f1.clone(),
        result,
    };
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or(Path::new(env!("CARGO_MANIFEST_DIR")));
    let directory = workspace.join("target").join("strata-eval-diagnostics");
    let outcome = std::fs::create_dir_all(&directory).and_then(|()| {
        let path = directory.join(format!("{fixture}.json"));
        std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap_or_default())
    });
    if let Err(error) = outcome {
        report.errors.push(EvalError::DumpFailed {
            fixture,
            message: error.to_string(),
        });
    }
}

/// Converts verdicts to their serializable shape.
fn dump_verdicts(verdicts: &[Verdict]) -> Vec<DumpVerdict> {
    verdicts
        .iter()
        .map(|verdict| DumpVerdict {
            label: verdict.label.clone(),
            passed: verdict.passed,
            detail: verdict.detail.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{co_membership_pairs, members, pair_f1, structural_placement};

    fn file(name: &str) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: None,
            production_sloc: None,
        }
    }

    fn node(level: Level, name: &str, children: Vec<ContainerNode>) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level,
            children: Some(children),
            symbols: None,
            production_sloc: None,
        }
    }

    /// A candidate tree where `billing` dissolved: invoice and pricing welded
    /// into a domain named `helpers`, telemetry intact.
    fn torn_candidate() -> ContainerNode {
        node(
            Level::PackageGroup,
            "root",
            vec![node(
                Level::Package,
                "app",
                vec![node(
                    Level::Domain,
                    "helpers",
                    vec![
                        file("billing/invoice.py"),
                        file("billing/pricing.py"),
                        file("pipeline.py"),
                    ],
                )],
            )],
        )
    }

    /// A candidate tree that keeps real directories as folders.
    fn laminar_candidate() -> ContainerNode {
        node(
            Level::PackageGroup,
            "root",
            vec![node(
                Level::Package,
                "app",
                vec![
                    node(
                        Level::Folder,
                        "billing",
                        vec![file("billing/invoice.py"), file("billing/pricing.py")],
                    ),
                    node(Level::Folder, "telemetry", vec![file("telemetry/sink.py")]),
                    file("pipeline.py"),
                ],
            )],
        )
    }

    fn inputs_with(candidate: &ContainerNode) -> EvalInputs<'_> {
        let current = laminar_candidate();
        let census: BTreeSet<String> = members(&current).into_iter().collect();
        EvalInputs {
            current_placement: structural_placement(&current),
            packages: discover_packages(&current, &census),
            census,
            faces: BTreeMap::from([(
                FaceMode::Anchored,
                FaceInputs {
                    tree: candidate,
                    capacity_remaining: None,
                },
            )]),
        }
    }

    fn block(modes: Vec<FaceMode>) -> AssertBlock {
        AssertBlock {
            modes,
            candidate: 1,
            preserve_dir: Vec::new(),
            keep_together: Vec::new(),
            separate: Vec::new(),
            size_band: Vec::new(),
            move_budget: Vec::new(),
            no_synthetic_bucket: Vec::new(),
            name_alignment: Vec::new(),
            capacity_relief: Vec::new(),
            non_inversion: Vec::new(),
            preserve_symbol_home: Vec::new(),
        }
    }

    #[test]
    fn separate_names_the_welding_offender_over_scoped_containers_only() {
        let candidate = torn_candidate();
        let inputs = inputs_with(&candidate);
        let (verdicts, _) = evaluate_assertions(
            &AssertBlock {
                separate: vec![PathSetAssertion {
                    paths: vec!["billing/invoice.py".to_owned(), "pipeline.py".to_owned()],
                    mode: None,
                    because: "billing never welds with pipeline".to_owned(),
                }],
                ..block(vec![FaceMode::Anchored])
            },
            &inputs,
            None,
        );
        assert_eq!(verdicts.len(), 1);
        let separate_verdict = verdicts.first();
        assert!(
            separate_verdict.is_some_and(|verdict| !verdict.passed),
            "the torn layout must fail separate"
        );
        assert!(separate_verdict.is_some_and(|verdict| verdict.detail.contains("helpers")));
        assert!(separate_verdict.is_some_and(|verdict| verdict.label.contains("#Anchored")));
    }

    #[test]
    fn keep_together_holds_when_a_folder_keeps_the_set() {
        let candidate = laminar_candidate();
        let inputs = inputs_with(&candidate);
        let (verdicts, _) = evaluate_assertions(
            &AssertBlock {
                keep_together: vec![PathSetAssertion {
                    paths: vec![
                        "billing/invoice.py".to_owned(),
                        "billing/pricing.py".to_owned(),
                    ],
                    mode: None,
                    because: "billing survives".to_owned(),
                }],
                ..block(vec![FaceMode::Anchored])
            },
            &inputs,
            None,
        );
        assert!(
            verdicts.first().is_some_and(|verdict| verdict.passed),
            "the laminar layout keeps billing together"
        );
    }

    #[test]
    fn preserve_dir_requires_a_named_container_holding_every_member() {
        // The torn candidate has no `billing` container, so D(P, billing) =
        // [invoice, pricing] cannot be found under any folder/domain node.
        let torn = torn_candidate();
        let torn_inputs = inputs_with(&torn);
        let torn_packages = packages_holding(&torn_inputs.census, &torn_inputs.packages, "billing");
        assert_eq!(
            torn_packages.len(),
            1,
            "exactly the repository-rooted package holds billing/"
        );
        let anchored_tree = torn_inputs
            .faces
            .get(&FaceMode::Anchored)
            .map(|face| face.tree);
        let torn_satisfied = anchored_tree.is_some_and(|tree| {
            torn_packages.first().is_some_and(|(package, expected)| {
                find_package_node(tree, &package.name).is_some_and(|package_node| {
                    surviving_container(package_node, "billing", expected)
                })
            })
        });
        assert!(!torn_satisfied, "torn layout must dissolve billing");

        let laminar = laminar_candidate();
        let laminar_inputs = inputs_with(&laminar);
        let laminar_packages =
            packages_holding(&laminar_inputs.census, &laminar_inputs.packages, "billing");
        let laminar_tree = laminar_inputs
            .faces
            .get(&FaceMode::Anchored)
            .map(|face| face.tree);
        let laminar_satisfied = laminar_tree.is_some_and(|tree| {
            laminar_packages.first().is_some_and(|(package, expected)| {
                find_package_node(tree, &package.name).is_some_and(|package_node| {
                    surviving_container(package_node, "billing", expected)
                })
            })
        });
        assert!(laminar_satisfied, "laminar layout keeps billing");
    }

    #[test]
    fn move_budget_counts_structural_placement_changes_only() {
        let torn = torn_candidate();
        let inputs = inputs_with(&torn);
        let budget = MoveBudget {
            mode: FaceMode::Anchored,
            max_moved_files: Some(0),
            min_moved_files: None,
            because: "a best state leaves billing alone".to_owned(),
        };
        let verdict = evaluate_move_budget(&budget, &inputs);
        assert!(!verdict.passed);
        // invoice + pricing + pipeline changed placement into helpers, and
        // telemetry/sink.py exists only in current (vanished counts as moved).
        assert!(verdict.detail.contains("moves 4 files structurally"));

        let laminar = laminar_candidate();
        let laminar_inputs = inputs_with(&laminar);
        let laminar_verdict = evaluate_move_budget(&budget, &laminar_inputs);
        assert!(
            laminar_verdict.passed,
            "identical placement moves nothing: {}",
            laminar_verdict.detail
        );
    }

    #[test]
    fn no_synthetic_bucket_reads_last_segments_at_any_level() {
        let bucketed = node(
            Level::PackageGroup,
            "root",
            vec![node(
                Level::Package,
                "app",
                vec![node(Level::Domain, "workspace", vec![file("loose.py")])],
            )],
        );
        let bucketed_inputs = inputs_with(&bucketed);
        let bucket = BucketName {
            name: "workspace".to_owned(),
            mode: None,
            because: "real directories never collapse into workspace".to_owned(),
        };
        let verdict = evaluate_no_synthetic_bucket(&bucket, FaceMode::Anchored, &bucketed_inputs);
        assert!(!verdict.passed);
        assert!(verdict.detail.contains("workspace"));

        let laminar = laminar_candidate();
        let laminar_inputs = inputs_with(&laminar);
        let laminar_verdict =
            evaluate_no_synthetic_bucket(&bucket, FaceMode::Anchored, &laminar_inputs);
        assert!(laminar_verdict.passed);
    }

    #[test]
    fn size_band_container_selector_matches_full_prefix_names_any_level() {
        let wide = node(
            Level::PackageGroup,
            "root",
            vec![node(
                Level::Package,
                "app",
                vec![node(
                    Level::Folder,
                    "hub",
                    vec![
                        file("hub/a.py"),
                        file("hub/b.py"),
                        file("hub/c.py"),
                        file("hub/d.py"),
                    ],
                )],
            )],
        );
        let inputs = inputs_with(&wide);
        let band = SizeBand {
            max_files: 2,
            min_files: None,
            scope: None,
            container: Some("hub".to_owned()),
            mode: None,
            because: "hub splits".to_owned(),
        };
        let verdict = evaluate_size_band(&band, FaceMode::Anchored, &inputs);
        assert!(!verdict.passed);
        assert!(verdict.detail.contains("holds 4 members"));
    }

    #[test]
    fn violation_suffix_matches_on_dot_segment_boundaries() {
        assert!(location_suffix_matches("a.hub", "hub"));
        assert!(location_suffix_matches("hub", "hub"));
        assert!(!location_suffix_matches("hub.sub", "hu"));
        let violations = vec![
            Violation {
                kind: EngineViolationKind::Capacity,
                severity: EngineSeverity::Borderline,
                location: vec!["src".to_owned(), "hub".to_owned()],
                detail: "over cap".to_owned(),
                break_suggestions: None,
                capacity: None,
            },
            Violation {
                kind: EngineViolationKind::Capacity,
                severity: EngineSeverity::Borderline,
                location: vec!["ingest.hub".to_owned()],
                detail: "dotted container".to_owned(),
                break_suggestions: None,
                capacity: None,
            },
        ];
        let precondition = Precondition {
            kind: PreconditionKind::ViolationAbsent,
            min: None,
            max: None,
            violation: Some(ViolationClass::Capacity),
            location_suffix: Some("hub".to_owned()),
            severity: Some(SeverityFilter::Borderline),
            because: "no borderline hub breach".to_owned(),
        };
        let matches = matching_violations(&precondition, &violations);
        assert_eq!(
            matches.len(),
            2,
            "dot-segment suffix 'hub' matches the bare segment and the dotted tail"
        );
        let precondition_narrower = Precondition {
            kind: PreconditionKind::ViolationPresent,
            severity: Some(SeverityFilter::Violation),
            ..precondition
        };
        assert!(matching_violations(&precondition_narrower, &violations).is_empty());
    }

    #[test]
    fn preconditions_failures_surface_in_failure_message() {
        let verdicts = evaluate_preconditions(
            &[Precondition {
                kind: PreconditionKind::FileCount,
                min: Some(10),
                max: Some(20),
                violation: None,
                location_suffix: None,
                severity: None,
                because: "the fixture carries 12 files".to_owned(),
            }],
            3,
            &[],
        );
        assert!(
            verdicts.first().is_some_and(|verdict| !verdict.passed),
            "file_count below its floor must fail the precondition"
        );
        let report = CaseReport {
            fixture: "synthetic".to_owned(),
            errors: Vec::new(),
            preconditions: verdicts,
            verdicts: Vec::new(),
            pair_f1: Vec::new(),
            observations: Vec::new(),
        };
        let message = report.failure_message();
        assert!(message.is_some_and(|text| text.contains("PRECONDITION file_count")));
    }

    #[test]
    fn pair_f1_excludes_envelope_pairs_and_is_report_only() {
        let universe: BTreeSet<String> = ["billing/invoice.py", "billing/pricing.py"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let reference = ReferenceSet {
            container: vec![crate::target::ReferenceContainer {
                path: "ref/billing".to_owned(),
                files: vec![
                    "billing/invoice.py".to_owned(),
                    "billing/pricing.py".to_owned(),
                ],
            }],
        };
        let pairs = reference_pairs(&reference);
        let predicted = co_membership_pairs(&laminar_candidate(), &universe);
        assert_eq!(
            predicted.len(),
            1,
            "folder-level pair only; package envelope excluded"
        );
        let (_, _, f1) = pair_f1(&pairs, &predicted);
        assert!((f1 - 1.0).abs() < 1e-12);
    }
}
