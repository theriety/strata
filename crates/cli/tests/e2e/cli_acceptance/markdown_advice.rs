//! Saved-result reporting contracts, exercised through the compiled CLI.

use std::fs;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use strata_engine::{
    AnalyzeResult, EvidenceSignals, ProfileAssessment, ProfileConflict, ProfileName,
    QualificationThresholds, RelocationAdvice, RelocationProposal, ReviewReason, SymbolKind,
    SymbolMove,
};

use super::{analyze_to_file, nanos, run};

fn saved_result() -> Result<AnalyzeResult, String> {
    static RESULT: OnceLock<Result<AnalyzeResult, String>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            let path = analyze_to_file("constellation-ts");
            let contents = fs::read_to_string(&path).map_err(|error| error.to_string());
            let _ = fs::remove_file(path);
            serde_json::from_str(&contents?).map_err(|error| error.to_string())
        })
        .clone()
}

fn report(result: &AnalyzeResult) -> Result<String, String> {
    report_with_detail(result, false)
}

fn report_with_detail(result: &AnalyzeResult, verbose: bool) -> Result<String, String> {
    static NEXT_REPORT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "strata-markdown-{}-{}-{}.json",
        std::process::id(),
        nanos(),
        NEXT_REPORT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(
        &path,
        serde_json::to_vec(result).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let mut args = vec![
        "report",
        "--input",
        path.to_str().ok_or("non-UTF8 input path")?,
    ];
    if verbose {
        args.push("--verbose");
    }
    let outcome = run(&args);
    let _ = fs::remove_file(path);
    if outcome.code != 0 {
        return Err(outcome.stderr);
    }
    Ok(outcome.stdout)
}

fn relocation() -> SymbolMove {
    SymbolMove {
        symbol: "RecordOptions".to_owned(),
        kind: SymbolKind::Type,
        from_path: "origin/options.ts".to_owned(),
        to_path: "shared/owner.ts".to_owned(),
        delta: -0.1,
        broken_imports: 1,
    }
}

fn advice() -> RelocationAdvice {
    RelocationAdvice {
        proposal: RelocationProposal::Symbol {
            relocation: relocation(),
        },
        destination: "shared/owner.ts".to_owned(),
        supporting_profiles: vec![ProfileName::Anchored],
        qualified_profiles: vec![ProfileName::Anchored],
        absent_profiles: vec![ProfileName::Greenfield],
        conflicting_destinations: vec![ProfileConflict {
            profile: ProfileName::Greenfield,
            destination: "shared/alternative.ts".to_owned(),
        }],
        assessments: vec![ProfileAssessment {
            profile: ProfileName::Anchored,
            destination: "shared/owner.ts".to_owned(),
            evidence: EvidenceSignals {
                unique_owner: 1.0,
                role_affinity: 0.8,
                source_cohesion: 0.7,
                destination_cohesion: 0.6,
                producer_evidence: 0.5,
                architectural_reach: 0.4,
            },
            weighted_score: 0.75,
            structural_score: 0.65,
            ambiguity_margin: 0.2,
            best_alternative: Some("shared/alternative.ts".to_owned()),
            thresholds: QualificationThresholds {
                minimum_evidence: 0.6,
                minimum_structural: 0.5,
                minimum_ambiguity_margin: 0.15,
            },
            qualified: true,
        }],
        review_reasons: vec![ReviewReason::PartialProfileSupport],
    }
}

#[test]
fn should_preserve_saved_confidence_and_evidence_in_markdown() -> Result<(), String> {
    let mut result = saved_result()?;
    result.advice.recommended = vec![advice()];
    result.advice.review_candidates = vec![advice()];

    let markdown = report_with_detail(&result, true)?;
    let compact = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
    for expected in [
        "Recommended (1):",
        "Review candidate (1):",
        "RecordOptions",
        "shared/owner.ts",
        "supporting [anchored]",
        "qualified [anchored]",
        "absent [greenfield]",
        "greenfield→`shared/alternative.ts`",
        "owner 1.00",
        "role 0.80",
        "source 0.70",
        "destination 0.60",
        "producer 0.50",
        "reach 0.40",
        "margin 0.20",
        "weighted 0.75/0.60",
        "structural 0.65/0.50",
        "margin threshold 0.15",
        "qualified true",
        "best alternative `shared/alternative.ts`",
        "selected by only some executed profiles",
    ] {
        assert!(
            compact.contains(expected),
            "missing {expected:?} in Markdown: {markdown}"
        );
    }
    Ok(())
}

fn single_candidate() -> Result<AnalyzeResult, String> {
    let mut result = saved_result()?;
    result.profiles.anchored = None;
    result.advice.recommended.clear();
    result.advice.review_candidates.clear();
    let profile = result
        .profiles
        .greenfield
        .as_mut()
        .ok_or("missing greenfield")?;
    profile.candidates.truncate(1);
    if profile.candidates.is_empty() {
        return Err("missing candidate".to_owned());
    }
    Ok(result)
}

#[test]
fn should_report_symbol_only_moves_in_markdown() -> Result<(), String> {
    let mut result = single_candidate()?;
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| profile.candidates.first_mut())
        .ok_or("missing candidate")?;
    candidate.delta_narration.clear();
    candidate.symbol_moves = vec![relocation()];

    let markdown = report(&result)?;

    assert!(
        markdown.contains("RecordOptions")
            && markdown.contains("origin/options.ts")
            && markdown.contains("shared/owner.ts")
            && !markdown.contains("No moves versus"),
        "symbol-only relocation must be reported as a move: {markdown}"
    );
    Ok(())
}

#[test]
fn should_report_both_file_and_symbol_moves_in_markdown() -> Result<(), String> {
    let mut result = single_candidate()?;
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| profile.candidates.first_mut())
        .ok_or("missing candidate")?;
    let file_path = candidate
        .delta_narration
        .iter()
        .flat_map(|movement| &movement.files)
        .next()
        .ok_or("fixture needs a file move")?
        .path
        .clone();
    candidate.symbol_moves = vec![relocation()];

    let markdown = report(&result)?;

    assert!(
        markdown.contains(
            file_path
                .strip_prefix("constellation-ts/")
                .unwrap_or(&file_path)
        ) && markdown.contains("RecordOptions")
            && !markdown.contains("No moves versus"),
        "mixed moves must both render: {markdown}"
    );
    Ok(())
}

#[test]
fn should_report_no_moves_only_for_a_truly_empty_candidate() -> Result<(), String> {
    let mut result = single_candidate()?;
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| profile.candidates.first_mut())
        .ok_or("missing candidate")?;
    candidate.delta_narration.clear();
    candidate.symbol_moves.clear();

    let markdown = report(&result)?;

    assert!(
        markdown.contains("No moves versus the current layout.") && !markdown.contains("**Moves**"),
        "empty candidates retain explicit no-moves narration: {markdown}"
    );
    Ok(())
}

#[test]
fn should_explain_every_review_reason_in_saved_markdown() -> Result<(), String> {
    let mut result = saved_result()?;
    let mut item = advice();
    item.review_reasons = vec![
        ReviewReason::PartialProfileSupport,
        ReviewReason::ConflictingDestinations,
        ReviewReason::WeakEvidence,
        ReviewReason::WeakStructuralEvidence,
        ReviewReason::WeakAmbiguityMargin,
        ReviewReason::NoMajoritySupport,
    ];
    result.advice.recommended.clear();
    result.advice.review_candidates = vec![item];
    let markdown = report(&result)?;
    let compact = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
    for explanation in [
        "selected by only some executed profiles",
        "profiles selected different destinations",
        "destination evidence is below the configured minimum",
        "structural evidence is insufficient",
        "the destination is not sufficiently stronger than the best alternative",
        "no strict majority of executed profiles provides qualifying support for this destination",
    ] {
        assert!(
            compact.contains(explanation),
            "missing {explanation:?}: {markdown}"
        );
    }
    for identifier in [
        "PartialProfileSupport",
        "ConflictingDestinations",
        "WeakEvidence",
        "WeakStructuralEvidence",
        "WeakAmbiguityMargin",
        "NoMajoritySupport",
    ] {
        assert!(
            !markdown.contains(identifier),
            "internal reason leaked: {identifier}"
        );
    }
    Ok(())
}

#[test]
fn should_define_evidence_before_scores_and_quote_saved_move_paths() -> Result<(), String> {
    let mut result = single_candidate()?;
    result.advice.review_candidates = vec![advice()];
    let candidate = result
        .profiles
        .greenfield
        .as_mut()
        .and_then(|profile| profile.candidates.first_mut())
        .ok_or("missing candidate")?;
    candidate.delta_narration.clear();
    candidate.symbol_moves = vec![relocation()];
    let markdown = report_with_detail(&result, true)?;
    let compact = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
    let first_score = compact
        .find("owner 1.00")
        .ok_or("missing first evidence score")?;
    for definition in [
        "owner means unique ownership",
        "role means role affinity",
        "source and destination mean cohesion at each side",
        "producer means producer evidence",
        "reach means architectural reach",
        "Weighted is the normalized score across all six signals",
        "structural excludes role affinity",
        "margin is the selected destination's lead over the best alternative",
        "configured minimums",
        "Profiles share analysis-start evidence but apply their own weights and thresholds",
    ] {
        let position = compact
            .find(definition)
            .ok_or_else(|| format!("missing definition {definition:?}: {markdown}"))?;
        assert!(
            position < first_score,
            "definition must precede first score: {definition}"
        );
    }
    for path in [
        "greenfield→`shared/alternative.ts`",
        "best alternative `shared/alternative.ts`",
        "from `origin/options.ts` → `shared/owner.ts`",
    ] {
        assert!(
            compact.contains(path),
            "missing quoted path {path:?}: {markdown}"
        );
    }
    assert!(compact.contains("Objective delta -0.1000; 1 imports to repoint."));
    Ok(())
}

#[test]
fn should_explain_advice_in_the_public_summary() -> Result<(), String> {
    let root = super::fixture("constellation-ts");
    let outcome = run(&[
        "analyze",
        "--root",
        root.to_str().ok_or("non-UTF8 fixture")?,
        "--config",
        super::PURE_DEFAULTS,
        "--format",
        "summary",
        "--verbose",
    ]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let compact = outcome
        .stdout
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for expected in [
        "Review candidate (1)",
        "owner means unique ownership",
        "Profiles share analysis-start evidence",
        "selected by only some executed profiles",
        "no strict majority of executed profiles provides qualifying support for this destination",
        "best alternative `src/util`",
    ] {
        assert!(
            compact.contains(expected),
            "missing summary explanation {expected:?}: {}",
            outcome.stdout
        );
    }
    Ok(())
}

#[test]
fn should_distinguish_qualifying_support_from_selection_majority() -> Result<(), String> {
    let mut result = saved_result()?;
    let mut item = advice();
    item.supporting_profiles = vec![ProfileName::Anchored, ProfileName::Greenfield];
    item.absent_profiles.clear();
    item.conflicting_destinations.clear();
    let mut unqualified = item
        .assessments
        .first()
        .ok_or("missing assessment")?
        .clone();
    unqualified.profile = ProfileName::Greenfield;
    unqualified.weighted_score = 0.4;
    unqualified.qualified = false;
    item.assessments.push(unqualified);
    item.review_reasons = vec![ReviewReason::NoMajoritySupport];
    result.advice.recommended.clear();
    result.advice.review_candidates = vec![item];

    let markdown = report(&result)?;
    let compact = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
    for expected in [
        "supporting [anchored, greenfield]",
        "qualified [anchored]",
        "Review candidate (1)",
        "no strict majority of executed profiles provides qualifying support for this destination",
    ] {
        assert!(
            compact.contains(expected),
            "missing {expected:?}: {markdown}"
        );
    }
    Ok(())
}

#[test]
fn should_roundtrip_finite_analyzed_advice_through_saved_report() -> Result<(), String> {
    let result = saved_result()?;
    let assessments: Vec<_> = result
        .advice
        .recommended
        .iter()
        .chain(&result.advice.review_candidates)
        .flat_map(|item| &item.assessments)
        .collect();
    assert!(
        !assessments.is_empty(),
        "analyzed fixture must exercise scored advice"
    );
    assert!(assessments.iter().all(|assessment| {
        [
            assessment.weighted_score,
            assessment.structural_score,
            assessment.ambiguity_margin,
            assessment.evidence.unique_owner,
            assessment.evidence.role_affinity,
            assessment.evidence.source_cohesion,
            assessment.evidence.destination_cohesion,
            assessment.evidence.producer_evidence,
            assessment.evidence.architectural_reach,
        ]
        .into_iter()
        .all(f64::is_finite)
    }));
    let encoded = serde_json::to_vec(&result).map_err(|error| error.to_string())?;
    let restored: AnalyzeResult =
        serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
    let markdown = report_with_detail(&restored, true)?;
    assert!(markdown.contains("Review candidate (1)") && markdown.contains("weighted 0.70/0.60"));
    Ok(())
}

#[test]
fn should_preserve_distinct_visibility_sources_in_cli_outputs() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "strata-visibility-{}-{}",
        std::process::id(),
        nanos()
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"visibility-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), "mod left;\nmod right;\n")
        .map_err(|error| error.to_string())?;
    for name in ["left", "right"] {
        fs::write(
            root.join(format!("src/{name}.rs")),
            "pub fn duplicate() {}\n",
        )
        .map_err(|error| error.to_string())?;
    }
    let json = run(&[
        "analyze",
        "--root",
        root.to_str().ok_or("non-UTF8 root")?,
        "--config",
        super::PURE_DEFAULTS,
        "--format",
        "json",
    ]);
    let summary = run(&[
        "analyze",
        "--root",
        root.to_str().ok_or("non-UTF8 root")?,
        "--config",
        super::PURE_DEFAULTS,
        "--format",
        "summary",
    ]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(json.code, 0, "{}", json.stderr);
    assert_eq!(summary.code, 0, "{}", summary.stderr);
    let result: AnalyzeResult =
        serde_json::from_str(&json.stdout).map_err(|error| error.to_string())?;
    let markdown = report(&result)?;
    let findings: Vec<_> = result
        .current
        .shared_findings
        .iter()
        .filter(|finding| finding.kind == strata_engine::ViolationKind::Visibility)
        .collect();
    assert_eq!(findings.len(), 2, "{findings:?}");
    for path in ["src/left.rs", "src/right.rs"] {
        assert!(
            findings
                .iter()
                .any(|finding| finding.location == [path, "duplicate"]),
            "{findings:?}"
        );
        for output in [&summary.stdout, &markdown] {
            assert!(
                output.contains(&format!("`{path}`")),
                "missing quoted {path}: {output}"
            );
        }
    }
    Ok(())
}

#[test]
fn should_warn_about_correlated_profiles_in_default_markdown() -> Result<(), String> {
    let result = saved_result()?;

    let markdown = report(&result)?;
    let compact = markdown.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(
        compact.contains("Profiles share analysis-start evidence"),
        "missing confidence qualification: {markdown}"
    );
    assert!(
        !compact.contains("owner means unique ownership"),
        "numerical evidence remains verbose-only"
    );
    Ok(())
}

#[test]
fn should_warn_about_correlated_profiles_in_default_terminal() -> Result<(), String> {
    let root = super::fixture("constellation-ts");
    let outcome = run(&[
        "analyze",
        "--root",
        root.to_str().ok_or("non-UTF8 fixture")?,
        "--config",
        super::PURE_DEFAULTS,
        "--format",
        "summary",
    ]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let compact = outcome
        .stdout
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        compact.contains("Profiles share analysis-start evidence"),
        "missing confidence qualification: {}",
        outcome.stdout
    );
    assert!(
        !compact.contains("owner means unique ownership"),
        "numerical evidence remains verbose-only"
    );
    Ok(())
}
