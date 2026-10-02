#![allow(clippy::assertions_on_constants)]

use std::collections::BTreeMap;

use strata_ir::{
    Affinity, AffinityKind, ContainerId, Node, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot,
};

use crate::config::{AnalyzeConfig, ProfileConfig, ProfileName};
use crate::result::{
    AnalyzeResult, Move, RelocationProposal, ReviewReason, SymbolKind, SymbolMove,
};

use super::*;
use crate::analyze::scoring::*;
use crate::analyze::test_support::*;
use crate::analyze::*;

fn consensus_fixture(with_owner_affinity: bool) -> Snapshot {
    let mut companion = node(0, "AssembleRecordParams", 3, Polarity::Production);
    companion.kind = NodeKind::Type;
    let affinities = with_owner_affinity
        .then_some(Affinity {
            owner: NodeId(2),
            companion: NodeId(0),
            kind: AffinityKind::CompanionOwner,
        })
        .into_iter()
        .collect();

    snapshot_with_affinities(
        vec![
            companion,
            node(1, "origin_resident", 3, Polarity::Production),
            node(2, "assembleRecord", 4, Polarity::Production),
            node(3, "record_producer", 4, Polarity::Production),
            node(4, "record_base", 4, Polarity::Production),
        ],
        vec![inherits(2, 0), inherits(3, 0), edge(4, 2), edge(4, 3)],
        affinities,
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "destination", ScopeLevel::Folder, Some(0)),
            container(3, "origin/helpers.ts", ScopeLevel::File, Some(1)),
            container(
                4,
                "destination/assemble-record.ts",
                ScopeLevel::File,
                Some(2),
            ),
        ],
    )
}

fn consensus_config(anchored_minimum: f64, greenfield_minimum: f64) -> AnalyzeConfig {
    let mut config = config_with_k(1);
    for profile in [
        &mut config.profiles.anchored,
        &mut config.profiles.greenfield,
    ] {
        profile.objective.imbalance = 0.0;
        profile.objective.naming = 0.0;
        profile.objective.path = 0.0;
        profile.objective.anchor = 0.0;
        profile.objective.capacity = 0.0;
        profile.objective.dependency_only = 0.0;
        profile.objective.companion_separation = 1.0;
        profile.qualification.minimum_structural = 0.0;
        profile.qualification.minimum_ambiguity_margin = 0.0;
    }
    config.profiles.anchored.qualification.minimum_evidence = anchored_minimum;
    config.profiles.greenfield.qualification.minimum_evidence = greenfield_minimum;
    config
}

pub(super) fn symbol_advice(symbol: &str, from_path: &str, to_path: &str) -> AdviceProposal {
    AdviceProposal {
        proposal: RelocationProposal::Symbol {
            relocation: SymbolMove {
                symbol: symbol.to_owned(),
                kind: SymbolKind::Type,
                from_path: from_path.to_owned(),
                to_path: to_path.to_owned(),
                delta: -0.1,
                broken_imports: 0,
            },
        },
        subject: format!("symbol:{from_path}:{symbol}"),
        destination: to_path.to_owned(),
    }
}

pub(super) fn typed_node(id: u32, name: &str, container: u32) -> Node {
    let mut value = node(id, name, container, Polarity::Production);
    value.kind = NodeKind::Type;
    value
}

fn file_advice(path: &str, from: &str, to: &str) -> AdviceProposal {
    AdviceProposal {
        proposal: RelocationProposal::File {
            relocation: Move {
                kind: crate::MoveKind::Move,
                files: vec![crate::FileMove {
                    path: path.to_owned(),
                    from: from.to_owned(),
                }],
                to: to.to_owned(),
                reason: crate::MoveReason::Clustering,
                mirrors: Vec::new(),
                blocked_mirrors: Vec::new(),
            },
        },
        subject: format!("file:{path}"),
        destination: to.to_owned(),
    }
}

#[test]
fn should_recommend_a_qualified_move_selected_by_both_profiles() {
    let Ok(result) = analyze(&consensus_fixture(true), &consensus_config(0.0, 0.0)) else {
        assert!(false, "consensus fixture should analyze");
        return;
    };
    let json = serde_json::to_value(result).unwrap_or_default();

    let Some(recommended) = json
        .pointer("/advice/recommended")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items.iter().find(|item| {
                item.pointer("/proposal/relocation/symbol")
                    .and_then(serde_json::Value::as_str)
                    == Some("AssembleRecordParams")
            })
        })
    else {
        assert!(false, "the companion relocation should be recommended");
        return;
    };
    assert_eq!(
        recommended.get("supportingProfiles"),
        recommended.get("qualifiedProfiles")
    );
    for field in ["absentProfiles", "conflictingDestinations"] {
        assert_eq!(
            recommended
                .get(field)
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }
}

#[test]
fn should_keep_unanimous_partial_qualification_out_of_partial_support() {
    let Ok(result) = analyze(&consensus_fixture(true), &consensus_config(0.0, 1.0)) else {
        assert!(false, "consensus fixture should analyze");
        return;
    };
    let json = serde_json::to_value(result).unwrap_or_default();

    assert_eq!(
        json.pointer("/advice/recommended")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    let Some(review) = json
        .pointer("/advice/reviewCandidates")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items.iter().find(|item| {
                item.pointer("/proposal/relocation/symbol")
                    .and_then(serde_json::Value::as_str)
                    == Some("AssembleRecordParams")
            })
        })
    else {
        assert!(
            false,
            "partial profile support should retain the companion as review"
        );
        return;
    };
    assert_eq!(
        review
            .pointer("/qualifiedProfiles/0")
            .and_then(serde_json::Value::as_str),
        Some("anchored")
    );
    assert!(
        review
            .get("reviewReasons")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|reasons| {
                reasons
                    .iter()
                    .all(|reason| reason.as_str() != Some("partialProfileSupport"))
                    && reasons
                        .iter()
                        .any(|reason| reason.as_str() == Some("noMajoritySupport"))
            })
    );
}

#[test]
fn should_mark_a_genuine_one_profile_selection_as_partial_support() {
    let snapshot = consensus_fixture(true);
    let config = consensus_config(0.0, 0.0);
    let Ok(result) = analyze(&snapshot, &config) else {
        assert!(false, "consensus fixture should analyze");
        return;
    };
    let mut profiles = result.profiles;
    let Some(greenfield) = profiles.greenfield.as_mut() else {
        assert!(false, "greenfield profile should exist");
        return;
    };
    let Some(candidate) = greenfield.candidates.first_mut() else {
        assert!(false, "greenfield candidate should exist");
        return;
    };
    candidate.symbol_moves.clear();

    let advice = build_advice(&snapshot, &config, &profiles);
    let review = advice.review_candidates.iter().find(|item| {
        matches!(
            &item.proposal,
            RelocationProposal::Symbol { relocation }
                if relocation.symbol == "AssembleRecordParams"
        )
    });

    assert!(
        review.is_some(),
        "one-profile move remains visible for review"
    );
    if let Some(review) = review {
        assert_eq!(review.supporting_profiles, vec![ProfileName::Anchored]);
        assert_eq!(review.absent_profiles, vec![ProfileName::Greenfield]);
        assert!(
            review
                .review_reasons
                .contains(&ReviewReason::PartialProfileSupport)
        );
    }
}

#[test]
fn should_not_let_lexical_agreement_alone_recommend_a_move() {
    let Ok(result) = analyze(&consensus_fixture(false), &consensus_config(0.60, 0.60)) else {
        assert!(false, "lexical fixture should analyze");
        return;
    };
    let json = serde_json::to_value(result).unwrap_or_default();

    assert_eq!(
        json.pointer("/advice/recommended")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    assert!(
        json.pointer("/advice/reviewCandidates")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().all(|item| item
                .get("assessments")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|assessments| assessments.iter().all(|assessment| {
                    assessment
                        .get("qualified")
                        .and_then(serde_json::Value::as_bool)
                        == Some(false)
                })))),
        "names without ownership evidence must never cross the qualification gate"
    );
    let reviews = json
        .pointer("/advice/reviewCandidates")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        reviews.iter().all(|item| item
            .get("supportingProfiles")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|profiles| profiles.len() == 2)),
        "the weak move is selected by both profiles in this fixture"
    );
    assert!(
        reviews.iter().all(|item| item
            .get("reviewReasons")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|reasons| reasons
                .iter()
                .all(|reason| reason.as_str() != Some("partialProfileSupport")))),
        "unanimous selection with weak evidence is not partial profile support"
    );
}

#[test]
fn should_preserve_physical_source_roots_in_file_advice_alternatives() {
    let snapshot = snapshot(
        vec![
            node(0, "relocate_unit", 5, Polarity::Production),
            node(1, "chosen_owner", 6, Polarity::Production),
            node(2, "alternate_owner", 7, Polarity::Production),
            node(3, "parallel_test", 8, Polarity::TestCase),
        ],
        vec![edge(0, 1), edge(0, 2), edge(3, 0)],
        vec![
            container(0, "dataset", ScopeLevel::PackageGroup, None),
            container(1, "src/origin", ScopeLevel::Folder, Some(0)),
            container(2, "src/chosen", ScopeLevel::Folder, Some(0)),
            container(3, "src/alternative", ScopeLevel::Folder, Some(0)),
            container(4, "spec/alternative", ScopeLevel::Folder, Some(0)),
            container(5, "src/origin/unit.ts", ScopeLevel::File, Some(1)),
            container(6, "src/chosen/owner.ts", ScopeLevel::File, Some(2)),
            container(7, "src/alternative/owner.ts", ScopeLevel::File, Some(3)),
            container(
                8,
                "spec/alternative/owner.spec.ts",
                ScopeLevel::File,
                Some(4),
            ),
        ],
    );
    let proposal = file_advice(
        "dataset/src/origin/unit.ts",
        "dataset/src/origin",
        "dataset/src/chosen",
    );
    let assessment = EvidenceIndex::new(&snapshot).assess(
        &proposal,
        ProfileName::Anchored,
        &ProfileConfig::default(),
    );
    let json = serde_json::to_value(&assessment).unwrap_or_default();

    assert_eq!(
        json.get("bestAlternative")
            .and_then(serde_json::Value::as_str),
        Some("dataset/src/alternative")
    );
    assert_ne!(
        json.get("bestAlternative")
            .and_then(serde_json::Value::as_str),
        Some("dataset/spec/alternative"),
        "parallel source and test folders must retain distinct physical identities"
    );
}

#[test]
fn should_use_a_domain_backed_physical_folder_as_the_file_destination() {
    let snapshot = snapshot(
        vec![
            node(0, "relocate_unit", 3, Polarity::Production),
            node(1, "chosen_consumer", 4, Polarity::Production),
            node(2, "second_consumer", 5, Polarity::Production),
        ],
        vec![edge(1, 0), edge(2, 0)],
        vec![
            container(0, "dataset", ScopeLevel::PackageGroup, None),
            container(1, "origin-domain", ScopeLevel::Domain, Some(0)),
            container(2, "chosen-domain", ScopeLevel::Domain, Some(0)),
            container(3, "src/origin/unit.ts", ScopeLevel::File, Some(1)),
            container(4, "src/chosen/consumer.ts", ScopeLevel::File, Some(2)),
            container(5, "src/chosen/second.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let assessment = EvidenceIndex::new(&snapshot).assess(
        &file_advice(
            "dataset/src/origin/unit.ts",
            "dataset/src/origin",
            "dataset/src/chosen",
        ),
        ProfileName::Anchored,
        &ProfileConfig::default(),
    );

    assert!((assessment.evidence.architectural_reach - 1.0).abs() < f64::EPSILON);
    assert_eq!(
        assessment.best_alternative.as_deref(),
        Some("dataset/src/origin"),
        "a Domain-backed physical folder is explained by its exact path, not its semantic parent"
    );
}

#[test]
fn should_keep_parallel_source_and_test_folders_distinct_under_one_semantic_parent() {
    let snapshot = snapshot(
        vec![
            node(0, "relocate_unit", 3, Polarity::Production),
            node(1, "source_consumer", 4, Polarity::Production),
            node(2, "spec_consumer", 5, Polarity::Production),
        ],
        vec![edge(1, 0), edge(2, 0)],
        vec![
            container(0, "dataset", ScopeLevel::PackageGroup, None),
            container(1, "origin-domain", ScopeLevel::Domain, Some(0)),
            container(2, "shared-semantic-domain", ScopeLevel::Domain, Some(0)),
            container(3, "src/origin/unit.ts", ScopeLevel::File, Some(1)),
            container(4, "src/x/consumer.ts", ScopeLevel::File, Some(2)),
            container(5, "spec/x/consumer.spec.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let assessment = EvidenceIndex::new(&snapshot).assess(
        &file_advice(
            "dataset/src/origin/unit.ts",
            "dataset/src/origin",
            "dataset/src/x",
        ),
        ProfileName::Anchored,
        &ProfileConfig::default(),
    );

    assert_eq!(
        assessment.best_alternative.as_deref(),
        Some("dataset/spec/x")
    );
    assert!(assessment.ambiguity_margin.abs() < f64::EPSILON);
    assert!(
        !assessment.qualified,
        "equally supported physical folders under one semantic parent remain ambiguous"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn should_keep_same_named_type_and_runtime_relocations_as_distinct_advice() {
    let snapshot = snapshot(
        vec![
            typed_node(0, "Record", 3),
            node(1, "Record", 3, Polarity::Production),
            node(2, "type_owner", 4, Polarity::Production),
            node(3, "runtime_owner", 5, Polarity::Production),
        ],
        vec![inherits(2, 0), edge(3, 1)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "destination", ScopeLevel::Folder, Some(0)),
            container(3, "origin/records.ts", ScopeLevel::File, Some(1)),
            container(4, "destination/types.ts", ScopeLevel::File, Some(2)),
            container(5, "destination/runtime.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let config = consensus_config(0.0, 0.0);
    let Ok(template) = analyze(&consensus_fixture(true), &config) else {
        assert!(false, "profile template should analyze");
        return;
    };
    let mut profiles = template.profiles;
    let make_relocation = |kind, to_path: &str| SymbolMove {
        symbol: "Record".to_owned(),
        kind,
        from_path: "origin/records.ts".to_owned(),
        to_path: to_path.to_owned(),
        delta: -0.1,
        broken_imports: 1,
    };
    let (Some(anchored), Some(initial_greenfield)) =
        (profiles.anchored.as_mut(), profiles.greenfield.as_mut())
    else {
        assert!(false, "both profiles should exist");
        return;
    };
    for result in [anchored, initial_greenfield] {
        let Some(candidate) = result.candidates.first_mut() else {
            assert!(false, "candidate should exist");
            return;
        };
        candidate.delta_narration.clear();
        candidate.symbol_moves = vec![
            make_relocation(SymbolKind::Type, "destination/types.ts"),
            make_relocation(SymbolKind::Symbol, "destination/runtime.ts"),
        ];
    }

    let advice = build_advice(&snapshot, &config, &profiles);

    assert_eq!(advice.recommended.len(), 2);
    assert!(advice.review_candidates.is_empty());
    for item in &advice.recommended {
        assert_eq!(item.supporting_profiles.len(), 2);
        assert_eq!(item.qualified_profiles.len(), 2);
        assert!(item.absent_profiles.is_empty());
        assert!(item.conflicting_destinations.is_empty());
    }
    let identities = advice
        .recommended
        .iter()
        .map(|item| match &item.proposal {
            RelocationProposal::Symbol { relocation } => {
                (relocation.kind, relocation.to_path.as_str())
            }
            RelocationProposal::File { .. } => (SymbolKind::Symbol, ""),
        })
        .collect::<Vec<_>>();
    assert!(identities.contains(&(SymbolKind::Type, "destination/types.ts")));
    assert!(identities.contains(&(SymbolKind::Symbol, "destination/runtime.ts")));

    let Some(greenfield) = profiles.greenfield.as_mut() else {
        assert!(false, "greenfield profile should exist");
        return;
    };
    let Some(candidate) = greenfield.candidates.first_mut() else {
        assert!(false, "candidate should exist");
        return;
    };
    candidate.symbol_moves = vec![
        make_relocation(SymbolKind::Type, "destination/runtime.ts"),
        make_relocation(SymbolKind::Symbol, "destination/types.ts"),
    ];
    let conflicting = build_advice(&snapshot, &config, &profiles);

    assert_eq!(conflicting.recommended.len(), 0);
    assert_eq!(conflicting.review_candidates.len(), 4);
    for item in &conflicting.review_candidates {
        assert_eq!(item.supporting_profiles.len(), 1);
        assert_eq!(item.conflicting_destinations.len(), 1);
        let RelocationProposal::Symbol { relocation: moved } = &item.proposal else {
            assert!(false, "expected symbol advice");
            continue;
        };
        let expected_conflict = match moved.kind {
            SymbolKind::Type => {
                if moved.to_path == "destination/types.ts" {
                    "destination/runtime.ts"
                } else {
                    "destination/types.ts"
                }
            }
            SymbolKind::Symbol => {
                if moved.to_path == "destination/runtime.ts" {
                    "destination/types.ts"
                } else {
                    "destination/runtime.ts"
                }
            }
        };
        assert_eq!(
            item.conflicting_destinations
                .first()
                .map(|value| value.destination.as_str()),
            Some(expected_conflict)
        );
    }
}

#[test]
fn should_project_a_real_symbol_destination_file_to_its_direct_physical_folder_for_reach() {
    let snapshot = snapshot(
        vec![
            typed_node(0, "RecordState", 6),
            node(1, "leftConsumer", 7, Polarity::Production),
            node(2, "rightConsumer", 8, Polarity::Production),
            node(3, "sharedOwner", 9, Polarity::Production),
        ],
        vec![type_ref(1, 0), type_ref(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "shared", ScopeLevel::Domain, Some(0)),
            container(3, "shared/left", ScopeLevel::Folder, Some(2)),
            container(4, "shared/right", ScopeLevel::Folder, Some(2)),
            container(5, "origin/state.ts", ScopeLevel::File, Some(1)),
            container(6, "origin/model.ts", ScopeLevel::File, Some(1)),
            container(7, "shared/left/consumer.ts", ScopeLevel::File, Some(3)),
            container(8, "shared/right/consumer.ts", ScopeLevel::File, Some(4)),
            container(9, "shared/owner.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let assessment = EvidenceIndex::new(&snapshot).assess(
        &symbol_advice("RecordState", "origin/model.ts", "shared/owner.ts"),
        ProfileName::Anchored,
        &ProfileConfig::default(),
    );

    assert!((assessment.evidence.architectural_reach - 1.0).abs() < f64::EPSILON);
}

#[test]
fn should_allow_structural_and_role_evidence_without_an_explicit_owner() {
    let snapshot = consensus_fixture(false);
    let mut profile = ProfileConfig::default();
    profile.qualification.minimum_evidence = 0.0;
    profile.qualification.minimum_structural = 0.0;
    profile.qualification.minimum_ambiguity_margin = 0.0;
    let assessment = EvidenceIndex::new(&snapshot).assess(
        &symbol_advice(
            "AssembleRecordParams",
            "origin/helpers.ts",
            "destination/assemble-record.ts",
        ),
        ProfileName::Anchored,
        &profile,
    );

    assert!(assessment.evidence.role_affinity > 0.0);
    assert!(assessment.structural_score > 0.0);
    assert!(
        assessment.qualified,
        "adequate structural evidence must not require explicit owner affinity"
    );
}

#[test]
fn should_exclude_edges_internal_to_a_file_proposal_from_evidence() {
    let snapshot = snapshot(
        vec![
            node(0, "recordBuilder", 3, Polarity::Production),
            node(1, "recordHelper", 3, Polarity::Production),
            node(2, "recordOwner", 4, Polarity::Production),
        ],
        vec![edge(0, 1), edge(0, 2)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "destination", ScopeLevel::Folder, Some(0)),
            container(3, "origin/record.ts", ScopeLevel::File, Some(1)),
            container(4, "destination/owner.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let proposal = file_advice("origin/record.ts", "origin", "destination");
    let assessment = EvidenceIndex::new(&snapshot).assess(
        &proposal,
        ProfileName::Anchored,
        &ProfileConfig::default(),
    );

    assert!(
        (assessment.evidence.destination_cohesion - 0.5).abs() < f64::EPSILON,
        "the internal edge must neither add evidence nor dilute the sole external neighbor: {assessment:?}"
    );
}

#[test]
fn should_apply_profile_specific_affinity_only_to_pass_start_same_file_edges() {
    let mut input_type = node(0, "input", 1, Polarity::Production);
    input_type.kind = NodeKind::Type;
    let snapshot = snapshot(
        vec![input_type, node(1, "execute", 1, Polarity::Production)],
        vec![type_ref(1, 0)],
        vec![container(0, "src/run.rs", ScopeLevel::File, None)],
    );
    let tree = snapshot.ir().containers.clone();
    let placement = |_| Some(ContainerId(0));
    let config = AnalyzeConfig::default();

    let anchored = score_candidate(
        &snapshot,
        &placement,
        &BTreeMap::from([(ContainerId(0), ContainerId(0))]),
        &tree,
        &BTreeMap::new(),
        0.0,
        &config.profiles.anchored.capacity,
        config.profiles.anchored.weights.same_file_symbol,
        config.profiles.anchored.weights.same_file_type,
    );
    let mut greenfield_config = config.clone();
    greenfield_config.profiles.greenfield.weights.same_file_type = 1.0;
    let greenfield = score_candidate(
        &snapshot,
        &placement,
        &BTreeMap::from([(ContainerId(0), ContainerId(0))]),
        &tree,
        &BTreeMap::new(),
        0.0,
        &greenfield_config.profiles.greenfield.capacity,
        greenfield_config
            .profiles
            .greenfield
            .weights
            .same_file_symbol,
        greenfield_config.profiles.greenfield.weights.same_file_type,
    );

    let anchored_affinity = anchored.edges.first().map_or(0.0, |edge| edge.affinity);
    let greenfield_affinity = greenfield.edges.first().map_or(0.0, |edge| edge.affinity);
    assert!((anchored_affinity - 3.0).abs() < f64::EPSILON);
    assert!((greenfield_affinity - 1.0).abs() < f64::EPSILON);
    assert!((config.profiles.anchored.weights.same_file_type - 3.0).abs() < f64::EPSILON);
}

#[test]
fn should_keep_runtime_same_file_affinity_at_one_by_default() {
    let snapshot = snapshot(
        vec![
            node(0, "helper", 1, Polarity::Production),
            node(1, "execute", 1, Polarity::Production),
        ],
        vec![edge(1, 0)],
        vec![container(0, "src/run.rs", ScopeLevel::File, None)],
    );
    let tree = snapshot.ir().containers.clone();
    let profile = &AnalyzeConfig::default().profiles.anchored;

    let scored = score_candidate(
        &snapshot,
        &|_| Some(ContainerId(0)),
        &BTreeMap::from([(ContainerId(0), ContainerId(0))]),
        &tree,
        &BTreeMap::new(),
        0.0,
        &profile.capacity,
        profile.weights.same_file_symbol,
        profile.weights.same_file_type,
    );

    let affinity = scored.edges.first().map_or(0.0, |edge| edge.affinity);
    assert!((affinity - 1.0).abs() < f64::EPSILON);
}

#[test]
fn should_change_only_greenfield_scoring_when_only_its_type_affinity_changes() {
    let mut input_type = node(0, "input", 1, Polarity::Production);
    input_type.kind = NodeKind::Type;
    let snapshot = snapshot(
        vec![
            input_type,
            node(1, "execute", 1, Polarity::Production),
            node(2, "forward", 2, Polarity::Production),
        ],
        vec![type_ref(1, 0), type_ref(2, 0)],
        vec![
            container(0, "src/run.rs", ScopeLevel::File, None),
            container(1, "src/forward.rs", ScopeLevel::File, None),
        ],
    );
    let baseline = analyze(&snapshot, &AnalyzeConfig::default()).ok();
    let mut changed_config = AnalyzeConfig::default();
    changed_config.profiles.greenfield.weights.same_file_type = 1.0;
    let changed = analyze(&snapshot, &changed_config).ok();

    let score = |result: &AnalyzeResult, profile: ProfileName| {
        let profile = match profile {
            ProfileName::Anchored => result.profiles.anchored.as_ref(),
            ProfileName::Greenfield => result.profiles.greenfield.as_ref(),
        };
        profile.map(|profile| profile.current.score_breakdown.cut)
    };
    let anchored_before = baseline
        .as_ref()
        .and_then(|result| score(result, ProfileName::Anchored));
    let anchored_after = changed
        .as_ref()
        .and_then(|result| score(result, ProfileName::Anchored));
    let greenfield_before = baseline
        .as_ref()
        .and_then(|result| score(result, ProfileName::Greenfield));
    let greenfield_after = changed
        .as_ref()
        .and_then(|result| score(result, ProfileName::Greenfield));

    assert_eq!(anchored_before, anchored_after);
    assert_ne!(greenfield_before, greenfield_after);
}

#[test]
fn should_keep_lexical_only_consensus_in_review_at_zero_thresholds() -> Result<(), String> {
    let mut config = consensus_config(0.0, 0.0);
    for profile in [
        &mut config.profiles.anchored,
        &mut config.profiles.greenfield,
    ] {
        profile.qualification.weights = crate::config::QualificationWeightsConfig {
            unique_owner: 0.0,
            role_affinity: 1.0,
            source_cohesion: 0.0,
            destination_cohesion: 0.0,
            producer_evidence: 0.0,
            architectural_reach: 0.0,
        };
    }
    let mut ir = consensus_fixture(false).ir().clone();
    for node in &mut ir.nodes {
        if node.id == NodeId(1) {
            node.name = "unrelated_cache_transport_scheduler".into();
        }
        if node.id == NodeId(3) || node.id == NodeId(4) {
            node.name = "assembleRecordParams".into();
        }
    }
    let snapshot = Snapshot::assemble(ir).map_err(|error| error.to_string())?;
    let result = analyze(&snapshot, &config).map_err(|error| error.to_string())?;
    let item = result.advice.recommended.iter().chain(&result.advice.review_candidates).find(|item| {
        matches!(&item.proposal, RelocationProposal::Symbol { relocation } if relocation.symbol == "AssembleRecordParams" && relocation.to_path == "destination/assemble-record.ts")
    }).ok_or("fixture must produce the actual companion advice")?;
    assert!(!item.assessments.is_empty());
    assert!(
        result.advice.recommended.is_empty(),
        "lexical-only evidence cannot recommend: {:?}",
        result.advice
    );
    assert!(
        item.assessments
            .iter()
            .all(|assessment| assessment.structural_score == 0.0 && !assessment.qualified)
    );
    assert!(
        item.review_reasons
            .contains(&ReviewReason::WeakStructuralEvidence)
    );
    Ok(())
}

#[test]
fn should_qualify_positive_structural_consensus_at_zero_thresholds() -> Result<(), String> {
    let result = analyze(&consensus_fixture(false), &consensus_config(0.0, 0.0))
        .map_err(|error| error.to_string())?;
    let item = result.advice.recommended.iter().find(|item| {
        matches!(&item.proposal, RelocationProposal::Symbol { relocation } if relocation.symbol == "AssembleRecordParams" && relocation.to_path == "destination/assemble-record.ts")
    }).ok_or("structurally supported companion must remain recommended")?;
    assert!(
        item.assessments
            .iter()
            .any(|assessment| assessment.structural_score > 0.0 && assessment.qualified)
    );
    Ok(())
}
