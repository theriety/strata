//! The case runner: drives strata in-process, evaluates preconditions and
//! assertions, and reports distance as verdicts.
//!
//! The engine is a library here exactly as the CLI uses it:
//! [`snapshot_from_root`] + [`analyze`](fn@analyze) with an explicit config. Nothing shells
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
    use super::CaseReport;
    use super::assertions::fixtures::{inputs_with, laminar_candidate, torn_candidate};
    use super::assertions::{evaluate_assertions, evaluate_preconditions};
    use crate::target::{AssertBlock, FaceMode, PathSetAssertion, Precondition, PreconditionKind};

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
}
