//! The case runner: drives strata in-process, evaluates preconditions and
//! assertions, and reports distance as verdicts.
//!
//! The engine is a library here exactly as the CLI uses it:
//! [`snapshot_from_root`] + [`analyze`] with an explicit config. Nothing shells
//! out; nothing reads a golden score. `STRATA_BLESS_EVAL=1` only regenerates a
//! diagnostic dump — it never rewrites expectations.

use std::path::Path;

use strata_engine::config::{AnalyzeConfig, ProfileName};
use strata_engine::{analyze, snapshot_from_root};

use crate::error::EvalError;
use crate::target::{AssertBlock, ConfigSource, FaceMode, RunMode, TargetSpec};

mod assertions;
mod diagnostics;
mod inputs;

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

/// Runs one target against its fixture: analyze in-process, then measure.
///
/// # Errors
///
/// Returns [`EvalError`] when the case aborts before its verdicts are
/// meaningful: invalid target, unreadable fixture, or an unavailable
/// alternative beyond the current best state.
pub fn run_case(
    root: &Path,
    spec: &TargetSpec,
    expected_fixture: &str,
) -> Result<CaseReport, EvalError> {
    spec.validate(expected_fixture)?;

    let mut config = AnalyzeConfig::default();
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
    config.analysis.profiles = match spec.run.mode {
        RunMode::Anchored => vec![ProfileName::Anchored],
        RunMode::Greenfield => vec![ProfileName::Greenfield],
        RunMode::Both => vec![ProfileName::Anchored, ProfileName::Greenfield],
    };
    for profile in [
        &mut config.profiles.anchored,
        &mut config.profiles.greenfield,
    ] {
        profile.candidates = spec.run.candidates;
        profile.seed = spec.run.seed;
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

    let census = inputs::census_of(&result);
    let packages = inputs::discover_packages(&result.current.tree, &census);
    let mut errors = inputs::validate_references(
        spec,
        block,
        &census,
        &packages,
        &result.current.tree,
        expected_fixture,
    );

    // Preconditions are verified against current BEFORE scoring; a failure is a
    // harness or fixture defect, never distance, so it lands in `errors` too.
    let findings = assertions::all_findings(&result);
    let preconditions =
        assertions::evaluate_preconditions(&spec.precondition, result.summary.files, &findings);
    for verdict in preconditions.iter().filter(|verdict| !verdict.passed) {
        errors.push(EvalError::TargetInvalid {
            target: expected_fixture.to_owned(),
            message: format!(
                "precondition `{}` failed against current: {} (harness or fixture defect, never distance)",
                verdict.label, verdict.detail
            ),
        });
    }

    let inputs = inputs::build_inputs(&result, block, expected_fixture)?;
    let (verdicts, pair_f1) =
        assertions::evaluate_assertions(block, &inputs, spec.reference.as_ref());
    let observations = assertions::observe_modes(&result, &inputs.faces);

    let mut report = CaseReport {
        fixture: expected_fixture.to_owned(),
        errors,
        preconditions,
        verdicts,
        pair_f1,
        observations,
    };
    diagnostics::dump_diagnostics(&result, &mut report);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use strata_engine::result::{
        ContainerNode, Level, Severity as EngineSeverity, Violation,
        ViolationKind as EngineViolationKind,
    };

    use super::CaseReport;
    use super::assertions::{
        evaluate_assertions, evaluate_move_budget, evaluate_no_synthetic_bucket,
        evaluate_preconditions, evaluate_size_band, location_suffix_matches, matching_violations,
        reference_pairs, surviving_container,
    };
    use super::inputs::{
        EvalInputs, FaceInputs, discover_packages, find_package_node, packages_holding,
    };
    use crate::metrics::{co_membership_pairs, members, pair_f1, structural_placement};
    use crate::target::{
        AssertBlock, BucketName, FaceMode, MoveBudget, PathSetAssertion, Precondition,
        PreconditionKind, ReferenceSet, SeverityFilter, SizeBand, ViolationClass,
    };

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
