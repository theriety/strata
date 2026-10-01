//! `AnalyzeResult` DTO unit tests.

use std::collections::BTreeMap;

use strata_ir::ScopeLevel;

use super::*;

#[test]
fn should_serialize_schema_version_nine_with_two_advice_groups() {
    let result = AnalyzeResult {
        schema_version: RESULT_SCHEMA_VERSION,
        snapshot_hash: "snapshot".to_owned(),
        summary: Summary {
            symbols: 0,
            edges: 0,
            files: 0,
            files_by_language: BTreeMap::new(),
        },
        current: CurrentTree {
            tree: ContainerNode {
                name: "workspace".to_owned(),
                level: Level::PackageGroup,
                children: Some(Vec::new()),
                symbols: None,
                production_sloc: None,
            },
            shared_findings: Vec::new(),
        },
        profiles: Profiles::default(),
        advice: Advice::default(),
    };

    let json = serde_json::to_value(result).unwrap_or_default();

    assert_eq!(
        json.pointer("/schemaVersion")
            .and_then(serde_json::Value::as_u64),
        Some(9)
    );
    assert!(json.get("profiles").is_some());
    assert!(json.get("modes").is_none());
    assert!(json.pointer("/current/sharedFindings").is_some());
    assert!(json.pointer("/advice/recommended").is_some());
    assert!(json.pointer("/advice/reviewCandidates").is_some());
    assert!(json.pointer("/advice/rejected").is_none());
}

#[test]
fn should_serialize_a_breakdown_as_camel_case() {
    let breakdown = ScoreBreakdown {
        cut: 1.0,
        imbalance: 0.0,
        naming: -0.5,
        path: 0.0,
        anchor: 0.25,
        capacity: 0.75,
        dependency_only: 0.05,
        companion_separation: 0.05,
    };

    let json = serde_json::to_string(&breakdown).unwrap_or_default();

    assert!(json.contains("\"cut\":1.0"));
    assert!(json.contains("\"anchor\":0.25"));
    assert!(json.contains("\"capacity\":0.75"));
    assert!(json.contains("\"dependencyOnly\":0.05"));
    assert!(json.contains("\"companionSeparation\":0.05"));
}

#[test]
fn should_serialize_package_group_level_as_camel_case() {
    let json = serde_json::to_string(&Level::PackageGroup).unwrap_or_default();

    assert_eq!(json, "\"packageGroup\"");
}

#[test]
fn should_map_every_scope_level_to_its_dto_level() {
    assert_eq!(Level::from(ScopeLevel::File), Level::File);
    assert_eq!(Level::from(ScopeLevel::PackageGroup), Level::PackageGroup);
}

#[test]
fn should_omit_modes_that_were_not_requested() {
    let modes = Modes {
        anchored: None,
        greenfield: None,
    };

    let json = serde_json::to_string(&modes).unwrap_or_default();

    assert_eq!(json, "{}");
}

#[test]
fn should_serialize_current_standing_as_camel_case() {
    let json = serde_json::to_string(&CurrentStanding::Outscored).unwrap_or_default();

    assert_eq!(json, "\"outscored\"");
}

#[test]
fn should_serialize_a_capacity_breach_as_camel_case() {
    let breach = CapacityBreach {
        measured: 412,
        cap: 300,
        path: Some("src/core/huge.ts".to_owned()),
    };

    let json = serde_json::to_string(&breach).unwrap_or_default();

    assert_eq!(
        json,
        "{\"measured\":412,\"cap\":300,\"path\":\"src/core/huge.ts\"}"
    );
    let folder = CapacityBreach {
        measured: 20,
        cap: 15,
        path: None,
    };
    assert_eq!(
        serde_json::to_string(&folder).unwrap_or_default(),
        "{\"measured\":20,\"cap\":15}"
    );
}

#[test]
fn should_serialize_a_move_reason_with_its_kind_tag() {
    let reason = MoveReason::Follows {
        subject: "src/core/app.ts".to_owned(),
    };

    let json = serde_json::to_string(&reason).unwrap_or_default();

    assert_eq!(
        json,
        "{\"kind\":\"follows\",\"subject\":\"src/core/app.ts\"}"
    );
    assert_eq!(
        serde_json::to_string(&MoveReason::Clustering).unwrap_or_default(),
        "{\"kind\":\"clustering\"}"
    );
    assert_eq!(
        serde_json::to_string(&MoveReason::RelievesOverCap {
            container: "app/src/core".to_owned(),
            count: 4,
            cap: 3,
        })
        .unwrap_or_default(),
        "{\"kind\":\"relievesOverCap\",\"container\":\"app/src/core\",\"count\":4,\"cap\":3}"
    );
}

#[test]
fn should_display_every_reason_as_its_prose() {
    let cases = vec![
        (
            MoveReason::Follows {
                subject: "src/core/app.ts".to_owned(),
            },
            "follows app.ts",
        ),
        (
            MoveReason::RelievesOverCap {
                container: "app/src/core".to_owned(),
                count: 4,
                cap: 3,
            },
            "relieves over-cap folder app/src/core (4/3 entries)",
        ),
        (
            MoveReason::PulledBy {
                partner: "src/core/engine.ts".to_owned(),
                weight: 2.5,
            },
            "pulled by engine.ts (w 2.5)",
        ),
        (
            MoveReason::NamingCohesion {
                cohesion: 2.0 / 3.0,
            },
            "naming cohesion 0.67 with destination",
        ),
        (MoveReason::Clustering, "regrouped by clustering"),
    ];

    for (reason, prose) in cases {
        assert_eq!(reason.to_string(), prose);
    }
}
