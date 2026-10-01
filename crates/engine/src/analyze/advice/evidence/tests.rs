use std::collections::BTreeSet;

use strata_ir::{ContainerId, NodeId, Polarity, ScopeLevel};

use crate::analyze::advice::evidence::EvidenceIndex;
use crate::analyze::advice::evidence::EvidencePlace;
use crate::analyze::advice::tests::{symbol_advice, typed_node};
use crate::analyze::test_support::*;
use crate::config::{ProfileConfig, ProfileName};

#[test]
fn should_measure_ambiguity_against_the_actual_best_alternative() {
    let snapshot = snapshot(
        vec![
            typed_node(0, "AssembleRecordParams", 4),
            node(1, "assembleRecord", 5, Polarity::Production),
            node(2, "assembleRecord", 6, Polarity::Production),
        ],
        vec![type_ref(1, 0), type_ref(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "first", ScopeLevel::Folder, Some(0)),
            container(3, "second", ScopeLevel::Folder, Some(0)),
            container(4, "origin/model.ts", ScopeLevel::File, Some(1)),
            container(5, "first/assemble.ts", ScopeLevel::File, Some(2)),
            container(6, "second/assemble.ts", ScopeLevel::File, Some(3)),
        ],
    );
    let index = EvidenceIndex::new(&snapshot);
    let proposal = symbol_advice(
        "AssembleRecordParams",
        "origin/model.ts",
        "first/assemble.ts",
    );
    let assessment = index.assess(&proposal, ProfileName::Anchored, &ProfileConfig::default());

    let subjects = index.subject_nodes(&proposal);
    assert!(
        index
            .alternatives(
                &subjects,
                &EvidencePlace::Container(ContainerId(5)),
                &proposal,
                &ProfileConfig::default()
            )
            .contains(&EvidencePlace::Container(ContainerId(4))),
        "the immutable source/stay placement must be an ambiguity alternative"
    );
    assert_eq!(
        assessment.best_alternative.as_deref(),
        Some("second/assemble.ts")
    );
    assert!(assessment.ambiguity_margin.abs() < f64::EPSILON);
    assert!(
        !assessment.qualified,
        "a tied destination must stay in review"
    );
}

#[test]
fn should_resolve_same_basename_files_by_exact_pass_start_path() {
    let snapshot = snapshot(
        vec![
            typed_node(0, "RecordParams", 3),
            typed_node(1, "RecordParams", 4),
            node(2, "recordOwner", 5, Polarity::Production),
        ],
        vec![type_ref(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "left", ScopeLevel::Folder, Some(0)),
            container(2, "right", ScopeLevel::Folder, Some(0)),
            container(3, "left/model.ts", ScopeLevel::File, Some(1)),
            container(4, "right/model.ts", ScopeLevel::File, Some(2)),
            container(5, "right/owner.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let proposal = symbol_advice("RecordParams", "left/model.ts", "right/owner.ts");

    assert_eq!(
        EvidenceIndex::new(&snapshot).subject_nodes(&proposal),
        BTreeSet::from([NodeId(0)])
    );
    let prefixed = symbol_advice("RecordParams", "arbitrary/left/model.ts", "right/owner.ts");
    assert!(
        EvidenceIndex::new(&snapshot)
            .subject_nodes(&prefixed)
            .is_empty(),
        "an arbitrary first segment must not be stripped to manufacture file or type identity"
    );
}

#[test]
fn should_keep_a_conceptual_consumer_lca_when_no_container_exists_there() {
    let snapshot = snapshot(
        vec![
            typed_node(0, "RecordState", 4),
            node(1, "leftConsumer", 5, Polarity::Production),
            node(2, "rightConsumer", 6, Polarity::Production),
        ],
        vec![type_ref(1, 0), type_ref(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "shared/left", ScopeLevel::Folder, Some(0)),
            container(3, "shared/right", ScopeLevel::Folder, Some(0)),
            container(4, "origin/state.ts", ScopeLevel::File, Some(1)),
            container(5, "shared/left/consumer.ts", ScopeLevel::File, Some(2)),
            container(6, "shared/right/consumer.ts", ScopeLevel::File, Some(3)),
        ],
    );
    let index = EvidenceIndex::new(&snapshot);
    let proposal = symbol_advice("RecordState", "origin/state.ts", "shared/left/consumer.ts");
    let subjects = index.subject_nodes(&proposal);
    let alternatives = index.alternatives(
        &subjects,
        &EvidencePlace::Container(ContainerId(5)),
        &proposal,
        &ProfileConfig::default(),
    );

    assert!(alternatives.contains(&EvidencePlace::ConceptualFolder("shared".to_owned())));
}

#[test]
fn should_reward_only_the_common_ancestor_of_sibling_consumer_branches() {
    let snapshot = snapshot(
        vec![
            typed_node(0, "RecordState", 5),
            node(1, "leftConsumer", 6, Polarity::Production),
            node(2, "rightConsumer", 7, Polarity::Production),
            node(3, "sharedOwner", 8, Polarity::Production),
        ],
        vec![type_ref(1, 0), type_ref(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "shared", ScopeLevel::Domain, Some(0)),
            container(3, "shared/left", ScopeLevel::Folder, Some(2)),
            container(4, "shared/right", ScopeLevel::Folder, Some(2)),
            container(5, "origin/state.ts", ScopeLevel::File, Some(1)),
            container(6, "shared/left/consumer.ts", ScopeLevel::File, Some(3)),
            container(7, "shared/right/consumer.ts", ScopeLevel::File, Some(4)),
            container(8, "shared/owner.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let index = EvidenceIndex::new(&snapshot);
    let common = index.assess(
        &symbol_advice("RecordState", "origin/state.ts", "shared/owner.ts"),
        ProfileName::Anchored,
        &ProfileConfig::default(),
    );
    let branch = index.assess(
        &symbol_advice("RecordState", "origin/state.ts", "shared/left/consumer.ts"),
        ProfileName::Anchored,
        &ProfileConfig::default(),
    );

    assert!((common.evidence.architectural_reach - 1.0).abs() < f64::EPSILON);
    assert!(branch.evidence.architectural_reach.abs() < f64::EPSILON);
    let symbol_proposal =
        symbol_advice("RecordState", "origin/state.ts", "shared/left/consumer.ts");
    let symbol_subjects = index.subject_nodes(&symbol_proposal);
    assert!(
        index
            .alternatives(
                &symbol_subjects,
                &EvidencePlace::Container(ContainerId(6)),
                &symbol_proposal,
                &ProfileConfig::default()
            )
            .contains(&EvidencePlace::PhysicalFolder {
                container: ContainerId(2),
                path: "shared".to_owned(),
            }),
        "a shared symbol's sibling-consumer LCA must enter the ambiguity pool"
    );
}
