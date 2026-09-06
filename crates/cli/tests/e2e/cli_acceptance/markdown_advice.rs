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
    let outcome = run(&[
        "report",
        "--input",
        path.to_str().ok_or("non-UTF8 input path")?,
    ]);
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

    let markdown = report(&result)?;
    let compact = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
    for expected in [
        "Recommended (1):",
        "Review candidate (1):",
        "RecordOptions",
        "shared/owner.ts",
        "supporting [anchored]",
        "qualified [anchored]",
        "absent [greenfield]",
        "greenfield→shared/alternative.ts",
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
        "best alternative shared/alternative.ts",
        "PartialProfileSupport",
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
        markdown.contains(&file_path)
            && markdown.contains("RecordOptions")
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
