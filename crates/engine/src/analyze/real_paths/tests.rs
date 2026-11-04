use std::collections::BTreeMap;

use super::strip_group_prefix;
use crate::analyze::test_support::empty_profile_result;
use crate::config::ProfileName;
use crate::result::{
    Advice, AnalyzeResult, Candidate, CapacityBreach, CapacityUnit, ContainerNode, CurrentTree,
    EvidenceSignals, FileMove, Level, MirrorMove, Move, MoveKind, MoveReason, ProfileAssessment,
    ProfileConflict, Profiles, QualificationThresholds, RESULT_SCHEMA_VERSION, RelocationAdvice,
    RelocationProposal, ScoreBreakdown, Severity, Summary, SymbolKind, SymbolMove, TestSettings,
    Violation, ViolationKind,
};

fn text(value: &str) -> String {
    value.to_owned()
}

fn relief_move() -> Move {
    Move {
        kind: MoveKind::Move,
        files: vec![FileMove {
            path: text("proj/hub/a.py"),
            from: text("proj/hub"),
        }],
        to: text("proj"),
        reason: MoveReason::RelievesOverCap {
            container: text("proj/hub"),
            count: 24,
            cap: 20,
            unit: CapacityUnit::Entries,
            config_key: text("capacity.folder"),
        },
        mirrors: vec![MirrorMove {
            source_path: text("proj/hub/a.py"),
            path: text("proj/spec/hub/a.spec.py"),
            from: text("proj/spec/hub"),
            to: text("proj/proj/inner"),
        }],
    }
}

fn symbol_move() -> SymbolMove {
    SymbolMove {
        symbol: text("run"),
        kind: SymbolKind::Symbol,
        from_path: text("proj/hub/a.py"),
        to_path: text("proj/core/b.py"),
        delta: -1.0,
        broken_imports: 0,
        display: None,
    }
}

fn candidate() -> Candidate {
    Candidate {
        index: 1,
        score: 0.0,
        score_breakdown: ScoreBreakdown {
            cut: 0.0,
            imbalance: 0.0,
            naming: 0.0,
            path: 0.0,
            anchor: 0.0,
            capacity: 0.0,
            dependency_only: 0.0,
            companion_separation: 0.0,
        },
        improvement: 0.0,
        tree: ContainerNode {
            name: text("proj"),
            level: Level::PackageGroup,
            children: None,
            symbols: None,
            production_sloc: None,
        },
        conditional_splits: Vec::new(),
        delta_narration: vec![relief_move()],
        symbol_moves: vec![symbol_move()],
        capacity_remainder: None,
    }
}

fn assessment(destination: &str, alternative: &str) -> ProfileAssessment {
    ProfileAssessment {
        profile: ProfileName::Anchored,
        destination: text(destination),
        evidence: EvidenceSignals {
            unique_owner: 0.0,
            role_affinity: 0.0,
            source_cohesion: 0.0,
            destination_cohesion: 0.0,
            producer_evidence: 0.0,
            architectural_reach: 0.0,
        },
        weighted_score: 0.0,
        structural_score: 0.0,
        ambiguity_margin: 0.0,
        best_alternative: Some(text(alternative)),
        thresholds: QualificationThresholds {
            minimum_evidence: 0.0,
            minimum_structural: 0.0,
            minimum_ambiguity_margin: 0.0,
        },
        qualified: true,
    }
}

fn advice_of(
    proposal: RelocationProposal,
    destination: &str,
    conflict: &str,
    alternative: &str,
) -> RelocationAdvice {
    RelocationAdvice {
        proposal,
        destination: text(destination),
        supporting_profiles: Vec::new(),
        qualified_profiles: Vec::new(),
        absent_profiles: Vec::new(),
        conflicting_destinations: vec![ProfileConflict {
            profile: ProfileName::Greenfield,
            destination: text(conflict),
        }],
        assessments: vec![assessment(destination, alternative)],
        review_reasons: Vec::new(),
    }
}

fn result() -> AnalyzeResult {
    let mut anchored = empty_profile_result();
    anchored.candidates.push(candidate());
    AnalyzeResult {
        schema_version: RESULT_SCHEMA_VERSION,
        snapshot_hash: text("hash"),
        summary: Summary {
            symbols: 0,
            edges: 0,
            files: 0,
            files_by_language: BTreeMap::new(),
        },
        current: CurrentTree {
            tree: ContainerNode {
                name: text("proj"),
                level: Level::PackageGroup,
                children: Some(Vec::new()),
                symbols: None,
                production_sloc: None,
            },
            shared_findings: Vec::new(),
        },
        tests: TestSettings::default(),
        profiles: Profiles {
            anchored: Some(anchored),
            greenfield: None,
        },
        advice: Advice {
            // a file proposal's destination is a group-keyed folder; a symbol
            // proposal's is the real path of the file the symbol lands in.
            recommended: vec![advice_of(
                RelocationProposal::File {
                    relocation: relief_move(),
                },
                "proj/core",
                "proj",
                "proj/other",
            )],
            review_candidates: vec![advice_of(
                RelocationProposal::Symbol {
                    relocation: symbol_move(),
                },
                "proj/core/b.py",
                "proj/core/c.py",
                "proj/other/b.py",
            )],
        },
    }
}

fn real_move() -> Move {
    Move {
        kind: MoveKind::Move,
        files: vec![FileMove {
            path: text("hub/a.py"),
            from: text("hub"),
        }],
        to: text("."),
        reason: MoveReason::RelievesOverCap {
            container: text("hub"),
            count: 24,
            cap: 20,
            unit: CapacityUnit::Entries,
            config_key: text("capacity.folder"),
        },
        mirrors: vec![MirrorMove {
            source_path: text("hub/a.py"),
            path: text("spec/hub/a.spec.py"),
            from: text("spec/hub"),
            to: text("proj/inner"),
        }],
    }
}

#[test]
fn should_strip_the_group_prefix_from_file_moves_reasons_and_mirrors() {
    let mut result = result();

    strip_group_prefix(&mut result);

    let moves = result
        .profiles
        .anchored
        .map(|profile| profile.candidates)
        .unwrap_or_default();
    assert_eq!(
        moves
            .first()
            .map(|candidate| candidate.delta_narration.clone()),
        Some(vec![real_move()])
    );
}

#[test]
fn should_leave_symbol_moves_alone_when_their_real_path_starts_with_the_group_name() {
    let mut result = result();

    strip_group_prefix(&mut result);

    let symbols = result
        .profiles
        .anchored
        .and_then(|profile| profile.candidates.into_iter().next())
        .map(|candidate| candidate.symbol_moves)
        .unwrap_or_default();
    assert_eq!(
        symbols
            .iter()
            .map(|symbol| (symbol.from_path.as_str(), symbol.to_path.as_str()))
            .collect::<Vec<_>>(),
        vec![("proj/hub/a.py", "proj/core/b.py")]
    );
}

#[test]
fn should_strip_the_group_prefix_from_advice_destinations_and_proposals() {
    let mut result = result();

    strip_group_prefix(&mut result);

    let file = result.advice.recommended.first();
    assert_eq!(file.map(|item| item.destination.as_str()), Some("core"));
    assert_eq!(
        file.and_then(|item| item.conflicting_destinations.first())
            .map(|conflict| conflict.destination.as_str()),
        Some(".")
    );
    assert_eq!(
        file.and_then(|item| item.assessments.first())
            .map(|assessment| (
                assessment.destination.as_str(),
                assessment.best_alternative.as_deref()
            )),
        Some(("core", Some("other")))
    );
    assert_eq!(
        file.map(|item| item.proposal.clone()),
        Some(RelocationProposal::File {
            relocation: real_move()
        })
    );
}

#[test]
fn should_leave_symbol_advice_alone_when_its_real_paths_start_with_the_group_name() {
    let mut result = result();
    let before = result.advice.review_candidates.clone();

    strip_group_prefix(&mut result);

    assert_eq!(result.advice.review_candidates, before);
    let symbol = result.advice.review_candidates.first();
    assert_eq!(
        symbol.map(|item| item.destination.as_str()),
        Some("proj/core/b.py")
    );
}

#[test]
fn should_strip_only_one_leading_group_segment_of_a_file_move() {
    let mut result = result();
    if let Some(candidate) = result
        .profiles
        .anchored
        .as_mut()
        .and_then(|profile| profile.candidates.first_mut())
    {
        candidate.delta_narration = vec![Move {
            files: vec![FileMove {
                path: text("proj/proj/a.py"),
                from: text("proj/proj"),
            }],
            to: text("elsewhere/proj"),
            ..relief_move()
        }];
    }

    strip_group_prefix(&mut result);

    let moves = result
        .profiles
        .anchored
        .and_then(|profile| profile.candidates.into_iter().next())
        .map(|candidate| candidate.delta_narration)
        .unwrap_or_default();
    assert_eq!(
        moves
            .iter()
            .flat_map(|movement| movement.files.iter())
            .map(|file| (file.path.as_str(), file.from.as_str()))
            .collect::<Vec<_>>(),
        vec![("proj/a.py", "proj")]
    );
    assert_eq!(
        moves.first().map(|movement| movement.to.as_str()),
        Some("elsewhere/proj")
    );
}

fn finding(kind: ViolationKind, location: &[&str]) -> Violation {
    Violation {
        kind,
        severity: Severity::Violation,
        location: location.iter().map(|segment| text(segment)).collect(),
        detail: text("detail"),
        break_suggestions: None,
        capacity: None,
    }
}

fn breach(real_paths: &[&str]) -> CapacityBreach {
    CapacityBreach {
        measured: 24,
        cap: 20,
        unit: CapacityUnit::Entries,
        config_key: text("capacity.folder"),
        real_paths: real_paths.iter().map(|path| text(path)).collect(),
        path: None,
    }
}

/// Every finding of `result`: shared, then each profile's own.
fn findings_of(result: &AnalyzeResult) -> Vec<&Violation> {
    result
        .current
        .shared_findings
        .iter()
        .chain(
            result
                .profiles
                .anchored
                .iter()
                .flat_map(|profile| &profile.current.unique_findings),
        )
        .collect()
}

#[test]
fn should_rewrite_a_capacity_fallback_location_to_the_real_path_and_redescribe_it() {
    let mut result = result();
    // no real path is known, so the finding names its group-keyed ancestry as one joined path.
    let mut fallback = finding(ViolationKind::Capacity, &["proj/src/hub"]);
    fallback.capacity = Some(breach(&[]));
    fallback.detail = fallback
        .capacity
        .as_ref()
        .map(|capacity| capacity.describe(&[text("proj/src/hub")]))
        .unwrap_or_default();
    result.current.shared_findings = vec![fallback];

    strip_group_prefix(&mut result);

    let rewritten = findings_of(&result);
    assert_eq!(
        rewritten
            .first()
            .map(|violation| violation.location.clone()),
        Some(vec![text("src/hub")])
    );
    assert_eq!(
        rewritten.first().map(|violation| violation.detail.as_str()),
        Some(
            "in `src/hub`: 24 entries (files and subfolders); the folder cap is 20 (`capacity.folder`)."
        )
    );
}

#[test]
fn should_name_the_repository_root_when_a_capacity_fallback_is_the_group_itself() {
    let mut result = result();
    let mut fallback = finding(ViolationKind::Capacity, &["proj"]);
    fallback.capacity = Some(breach(&[]));
    result.current.shared_findings = vec![fallback];

    strip_group_prefix(&mut result);

    assert_eq!(
        findings_of(&result)
            .first()
            .map(|violation| violation.location.clone()),
        Some(vec![text(".")])
    );
}

#[test]
fn should_leave_finding_locations_alone_when_their_real_paths_start_with_the_group_name() {
    // flat layout: `proj/` is a real top-level directory as well as the group
    // label, so every one of these already-real locations begins with `proj`.
    let mut capacity = finding(ViolationKind::Capacity, &["proj/hub"]);
    capacity.capacity = Some(breach(&["proj/hub"]));
    let findings = vec![
        finding(ViolationKind::Cycle, &["proj/a.py", "proj/b.py"]),
        finding(
            ViolationKind::Polarity,
            &["proj/service.py", "proj/conftest.py"],
        ),
        finding(ViolationKind::Visibility, &["proj/a.py", "run"]),
        finding(ViolationKind::TestOnly, &["proj/a.py", "helper"]),
        capacity,
    ];
    let mut result = result();
    result.current.shared_findings = findings.clone();

    strip_group_prefix(&mut result);

    assert_eq!(result.current.shared_findings, findings);
}
