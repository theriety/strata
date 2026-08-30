//! The eval gate: one test per committed target, plus e2e smoke cases and the
//! β ≡ −0.0 witness.
//!
//! Exit state: one standing red by design. Witnesses start red on purpose —
//! each failure message names the defect it measures — and flip exactly
//! themselves green when the matching fix lands. naming-drift's anchored
//! verdicts closed as pins on the scorer's measured, honorable election
//! (deviation D-49), no coefficient touched. cycle-span's stays red as the
//! standing witness for a capability the scorer lacks: no lawful proposal can
//! reproduce a layout that splits an import-cycle atom across directories —
//! a measured impossibility (deviation D-50), and per owner ruling
//! 2026-08-24 the partitioner redesign enabling it ships as its own
//! follow-up stream.

use strata_engine::config::AnalyzeConfig;
use strata_engine::{analyze, snapshot_from_root};
use strata_eval::{EvalError, e2e_fixture_root, eval_fixture_root, harness, load_target};

/// The compile-time-checked case table: every committed target paired with the
/// designed defect its distance is measured against.
const CASES: [(&str, &str); 9] = [
    ("collapse", "workspace collapse"),
    ("large-app", "proposal quality at scale"),
    ("welding", "cross-directory welding"),
    ("tearing", "directory tearing"),
    ("relief", "unrelieved capacity"),
    ("inversion", "anchored inversion"),
    ("naming-drift", "naming incoherence"),
    ("cycle-span", "import-cycle tearing"),
    ("spec-twin-ts", "source-test mixing"),
];

/// The designed defect of one case, read from [`CASES`] without indexing.
fn designed_defect(case_name: &str) -> &'static str {
    CASES
        .iter()
        .find(|(name, _)| *name == case_name)
        .map_or("undescribed", |(_, defect)| *defect)
}

/// Fails the calling test with a witness-prefixed defect report; the
/// non-constant condition keeps clippy's constant-assertion lints quiet while
/// carrying the aggregated message.
fn fail_witness(case_name: &str, defect: &str, detail: &str) {
    assert!(
        detail.is_empty(),
        "witness[{case_name}:{defect}] distance to best state:\n  {detail}"
    );
}

/// Runs one case end to end; any unmet verdict aborts the test loudly.
fn assert_best_state(case_name: &str) {
    let defect = designed_defect(case_name);
    let outcome = load_target(case_name)
        .and_then(|spec| harness::run_case(&eval_fixture_root(case_name), &spec, case_name));
    match outcome {
        Ok(report) => {
            if let Some(message) = report.failure_message() {
                fail_witness(case_name, defect, &message);
            }
        }
        Err(error) => fail_witness(
            case_name,
            defect,
            &format!("case aborted before verdicts were meaningful: {error}"),
        ),
    }
}

#[test]
fn collapse_holds_its_best_state() {
    assert_best_state("collapse");
}

#[test]
fn large_app_holds_its_best_state() {
    assert_best_state("large-app");
}

#[test]
fn welding_holds_its_best_state() {
    assert_best_state("welding");
}

/// RED AS DESIGNED: green when directory tearing stops dissolving billing,
/// telemetry, and pipeline containers in the best candidates.
#[test]
fn tearing_witnesses_directory_tearing() {
    assert_best_state("tearing");
}

/// RED AS DESIGNED: green when the over-cap hub actually relieves.
#[test]
fn relief_witnesses_unrelieved_capacity() {
    assert_best_state("relief");
}

/// RED AS DESIGNED: green when anchored stops moving six structural files on an
/// already-optimal layout.
#[test]
fn inversion_witnesses_anchored_inversion() {
    assert_best_state("inversion");
}

#[test]
fn zero_candidate_best_state_evaluates_the_current_tree() -> Result<(), EvalError> {
    let spec = load_target("inversion")?;
    let report = harness::run_case(&eval_fixture_root("inversion"), &spec, "inversion")?;

    assert!(
        report.errors.is_empty(),
        "zero-candidate best state must not abort: {:?}",
        report.errors
    );
    assert!(
        report
            .observations
            .iter()
            .all(|observation| observation.candidates == 0),
        "the regression requires both parameter profiles to return no alternative"
    );
    assert!(
        report.verdicts.iter().all(|verdict| verdict.passed),
        "the current tree is the defended best state: {:?}",
        report.verdicts
    );
    Ok(())
}

#[test]
fn zero_candidate_result_does_not_fabricate_later_alternatives() -> Result<(), EvalError> {
    let mut spec = load_target("inversion")?;
    let Some(assertion) = spec.assert.first_mut() else {
        return Err(EvalError::TargetInvalid {
            target: "inversion".to_owned(),
            message: "the committed target has no assertion block".to_owned(),
        });
    };
    assertion.candidate = 2;

    match harness::run_case(&eval_fixture_root("inversion"), &spec, "inversion") {
        Ok(_) => {
            return Err(EvalError::TargetInvalid {
                target: "inversion".to_owned(),
                message: "candidate 2 was fabricated from an empty alternative set".to_owned(),
            });
        }
        Err(error) => assert!(
            error.to_string().contains("candidate 2 requested"),
            "the error must identify the unavailable alternative: {error}"
        ),
    }
    Ok(())
}

/// RED AS DESIGNED: green when container names align with their members.
#[test]
fn naming_drift_witnesses_naming_incoherence() {
    assert_best_state("naming-drift");
}

/// RED AS DESIGNED: green when a cross-directory import cycle stops tearing one
/// of its two real directories out of the greenfield proposal (anchored already
/// keeps the layout at zero churn).
#[test]
fn cycle_span_witnesses_import_cycle_tearing() {
    assert_best_state("cycle-span");
}

/// RED AS DESIGNED: green when the symbol pass stops relocating production
/// symbols across the source/test boundary into their mirrored spec twins.
#[test]
fn spec_twin_ts_witnesses_source_test_mixing() {
    assert_best_state("spec-twin-ts");
}

/// EVL04 coverage guard: the table covers exactly the committed target files,
/// so a fixture without a case — or a case without a fixture — fails here
/// instead of silently narrowing the gate.
#[test]
fn cases_table_covers_exactly_the_committed_targets() {
    let targets_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("targets");
    let mut problems = Vec::new();
    match std::fs::read_dir(&targets_directory) {
        Err(error) => problems.push(format!("reading {}: {error}", targets_directory.display())),
        Ok(entries) => {
            let mut committed: Vec<String> = entries
                .filter_map(std::result::Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|file_name| {
                    std::path::Path::new(file_name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
                })
                .map(|file_name| file_name.trim_end_matches(".toml").to_owned())
                .collect();
            committed.sort();

            let mut tabulated: Vec<&str> = CASES.iter().map(|(name, _)| *name).collect();
            tabulated.sort_unstable();

            if committed.len() != tabulated.len()
                || committed
                    .iter()
                    .zip(tabulated.iter())
                    .any(|(committed_name, tabulated_name)| committed_name != tabulated_name)
            {
                problems.push(format!(
                    "target files {committed:?} do not equal CASES entries {tabulated:?}"
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "eval gate coverage broken:\n  {}",
        problems.join("\n  ")
    );
}

/// EVL01 smoke: the python e2e fixture analyzes in-process with defaults.
#[test]
fn smoke_python_fixture_analyzes_in_process() {
    analyze_e2e_smoke("python");
}

/// EVL01 smoke: the typescript e2e fixture analyzes in-process with defaults.
#[test]
fn smoke_typescript_fixture_analyzes_in_process() {
    analyze_e2e_smoke("ts");
}

/// Drives `snapshot_from_root` + `analyze` directly — no CLI subprocess — and
/// asserts only sanity: files found, both faces produced results, and any
/// alternatives returned are strict improvements over their own baselines.
fn analyze_e2e_smoke(fixture_name: &str) {
    let config = AnalyzeConfig::default();
    let root = e2e_fixture_root(fixture_name);
    let mut problems = Vec::new();

    match snapshot_from_root(&root, &config) {
        Err(error) => problems.push(format!("snapshot failed: {error}")),
        Ok(snapshot) => match analyze(&snapshot, &config) {
            Err(error) => problems.push(format!("analyze failed: {error}")),
            Ok(result) => {
                if result.summary.files == 0 {
                    problems.push("found no files".to_owned());
                }
                for (name, profile) in [
                    ("anchored", result.profiles.anchored.as_ref()),
                    ("greenfield", result.profiles.greenfield.as_ref()),
                ] {
                    match profile {
                        None => problems.push(format!("{name} produced no profile result")),
                        Some(profile)
                            if profile
                                .candidates
                                .iter()
                                .any(|candidate| candidate.improvement <= 0.0) =>
                        {
                            problems.push(format!("{name} returned a non-improving alternative"));
                        }
                        Some(_) => {}
                    }
                }
            }
        },
    }

    assert!(
        problems.is_empty(),
        "smoke[{fixture_name}] in-process drive broken:\n  {}",
        problems.join("\n  ")
    );
}

/// RED-AS-DESIGNED β ≡ −0.0 witness: on testsupport-leak's unchanged layout the
/// path-cohesion term reads signed zero, contradicting the engine's documented
/// invariant that an unchanged layout keeps full cohesion credit (the FIX01
/// key-vs-display sites). Green when the term carries a strictly negative bonus
/// magnitude again.
#[test]
fn beta_witness_path_term_is_signed_zero_on_unchanged_layout() {
    let config = AnalyzeConfig::default();
    let root = e2e_fixture_root("testsupport-leak");

    let mut problems = Vec::new();
    match snapshot_from_root(&root, &config) {
        Err(error) => problems.push(format!("snapshot failed: {error}")),
        Ok(snapshot) => match analyze(&snapshot, &config) {
            Err(error) => problems.push(format!("analyze failed: {error}")),
            Ok(result) => {
                let current_path_term = result
                    .profiles
                    .anchored
                    .as_ref()
                    .map_or(0.0, |profile| profile.current.score_breakdown.path);
                if current_path_term >= 0.0 {
                    problems.push(format!(
                        "scoreBreakdown.path reads {current_path_term:+} on the unchanged \
                         current layout — zero path-cohesion credit where a strictly negative \
                         bonus belongs (FIX01 cohesion_inputs key-vs-display)"
                    ));
                }

                // The same invariant holds for any anchored candidate that
                // proposes no moves at all: identical layout, identical credit.
                let unchanged_terms = result
                    .profiles
                    .anchored
                    .as_ref()
                    .into_iter()
                    .flat_map(|mode| mode.candidates.iter())
                    .filter(|candidate| candidate.delta_narration.is_empty())
                    .map(|candidate| candidate.score_breakdown.path);
                for term in unchanged_terms {
                    if term >= 0.0 {
                        problems.push(format!(
                            "scoreBreakdown.path reads {term:+} on an unchanged-layout \
                             anchored candidate (empty deltaNarration) — FIX01 \
                             cohesion_inputs key-vs-display"
                        ));
                    }
                }
            }
        },
    }

    assert!(
        problems.is_empty(),
        "beta ≡ −0.0 witness:\n  {}",
        problems.join("\n  ")
    );
}
