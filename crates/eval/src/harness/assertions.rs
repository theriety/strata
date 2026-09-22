//! Preconditions, target assertions, and non-gating observations.

use std::collections::{BTreeMap, BTreeSet};

use strata_engine::result::{
    AnalyzeResult, Candidate, ContainerNode, Level, Severity as EngineSeverity, Violation,
    ViolationKind as EngineViolationKind,
};

use crate::metrics;
use crate::target::{
    AssertBlock, BandScope, BucketName, CapacityRelief, FaceMode, MoveBudget, NameAlignment,
    NonInversion, PathSetAssertion, Precondition, PreconditionKind, PreserveDir,
    PreserveSymbolHome, ReferenceSet, SeverityFilter, SizeBand, ViolationClass,
};

use super::inputs::{EvalInputs, FaceInputs, find_package_node, mode_result_of, packages_holding};
use super::{ModeObservation, PairF1Report, Verdict};

/// Returns the deduplicated findings visible across the executed profiles.
pub(super) fn all_findings(result: &AnalyzeResult) -> Vec<Violation> {
    let mut findings = result.current.shared_findings.clone();
    for profile in [
        result.profiles.anchored.as_ref(),
        result.profiles.greenfield.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        findings.extend(profile.current.unique_findings.iter().cloned());
    }
    let mut unique = Vec::new();
    for finding in findings {
        if !unique.contains(&finding) {
            unique.push(finding);
        }
    }
    unique
}

/// Evaluates every assertion in the block once per face it applies to, then the
/// report-only pair-F1 per evaluated face.
pub(super) fn evaluate_assertions(
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
pub(super) fn reference_pairs(reference: &ReferenceSet) -> BTreeSet<(String, String)> {
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
/// file in `expected` anywhere in its subtree.
///
/// Unlike the counting predicates this is deliberately SUBTREE-scoped:
/// `preserve_dir` pins tearing, and tearing means expected files LEAVING the
/// directory's reach — a directory that keeps its files across nested
/// sub-places inside itself has not been torn, whatever the first-level split
/// beneath it looks like.
pub(super) fn surviving_container(
    package_node: &ContainerNode,
    path: &str,
    expected: &[String],
) -> bool {
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
/// over folder/domain containers only. Membership is first-level: a container
/// holds exactly its direct file children, so nesting one place under another
/// never reads as the two places merging.
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
            let member_list = metrics::direct_members(node);
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

/// `size_band`: every selected container's first-level member count lies
/// within the inclusive band. Only a folder's own children are its members —
/// nested sub-places contribute nothing — so splitting an over-cap folder into
/// halves that nest under it reads as two within-band places.
pub(super) fn evaluate_size_band(
    band: &SizeBand,
    face: FaceMode,
    inputs: &EvalInputs<'_>,
) -> Verdict {
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
            let count = metrics::direct_members(node).len();
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
                    "over-capacity: container {name:?} holds {count} members against {bound} (first-level members only)"
                ),
            }
        }
    }
}

/// `move_budget`: structural moved-file count between current and the asserted
/// candidate, bounded inclusively. Narration never feeds this count.
pub(super) fn evaluate_move_budget(budget: &MoveBudget, inputs: &EvalInputs<'_>) -> Verdict {
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
pub(super) fn evaluate_no_synthetic_bucket(
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
/// first-level members keeps at least `min_ratio` of them sharing a naming
/// token. A folder is judged by its own children; descendants belong to their
/// own places and never dilute an ancestor's alignment.
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
            let members = metrics::direct_members(node);
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
pub(super) fn evaluate_preconditions(
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
pub(super) fn matching_violations<'a>(
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
pub(super) fn location_suffix_matches(location: &str, suffix: &str) -> bool {
    location.ends_with(&format!(".{suffix}"))
        || location.rsplit('.').next().unwrap_or(location) == suffix
}

/// Non-gating diversity observations per evaluated mode.
pub(super) fn observe_modes(
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
