//! `AnalyzeResult` DTO unit tests.

use std::collections::BTreeMap;

use unlace_ir::ScopeLevel;

use super::*;

#[test]
fn should_serialize_schema_version_eleven_with_two_advice_groups() {
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
        tests: TestSettings::default(),
        profiles: Profiles::default(),
        advice: Advice::default(),
    };

    let json = serde_json::to_value(result).unwrap_or_default();

    assert_eq!(
        json.pointer("/schemaVersion")
            .and_then(serde_json::Value::as_u64),
        Some(11)
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
        unit: CapacityUnit::Lines,
        config_key: "capacity.file".to_owned(),
        real_paths: vec!["src/core/huge.ts".to_owned()],
        path: Some("src/core/huge.ts".to_owned()),
    };

    let json = serde_json::to_string(&breach).unwrap_or_default();

    assert_eq!(
        json,
        "{\"measured\":412,\"cap\":300,\"unit\":\"lines\",\"configKey\":\"capacity.file\",\"realPaths\":[\"src/core/huge.ts\"],\"path\":\"src/core/huge.ts\"}"
    );
    let folder = CapacityBreach {
        measured: 20,
        cap: 15,
        unit: CapacityUnit::Entries,
        config_key: "capacity.folder".to_owned(),
        real_paths: vec!["src/batch/adapters".to_owned()],
        path: None,
    };
    assert_eq!(
        serde_json::to_string(&folder).unwrap_or_default(),
        "{\"measured\":20,\"cap\":15,\"unit\":\"entries\",\"configKey\":\"capacity.folder\",\"realPaths\":[\"src/batch/adapters\"]}"
    );
    let domain = CapacityBreach {
        measured: 21,
        cap: 16,
        unit: CapacityUnit::Folders,
        config_key: "capacity.domain".to_owned(),
        real_paths: vec!["spec/generators".to_owned(), "src/generators".to_owned()],
        path: None,
    };
    assert_eq!(
        serde_json::to_string(&domain).unwrap_or_default(),
        "{\"measured\":21,\"cap\":16,\"unit\":\"folders\",\"configKey\":\"capacity.domain\",\"realPaths\":[\"spec/generators\",\"src/generators\"]}"
    );
}

#[test]
fn should_describe_capacity_breaches_from_structured_fields() {
    let domain = CapacityBreach {
        measured: 21,
        cap: 16,
        unit: CapacityUnit::Folders,
        config_key: "capacity.domain".to_owned(),
        real_paths: vec!["src/generators".to_owned(), "spec/generators".to_owned()],
        path: None,
    };
    let package = CapacityBreach {
        measured: 14,
        cap: 12,
        unit: CapacityUnit::Domains,
        config_key: "capacity.package".to_owned(),
        real_paths: vec!["crates/core".to_owned()],
        path: None,
    };
    let group = CapacityBreach {
        measured: 13,
        cap: 12,
        unit: CapacityUnit::Packages,
        config_key: "capacity.package-group".to_owned(),
        real_paths: vec![".".to_owned()],
        path: None,
    };

    assert_eq!(
        domain.describe(&[]),
        "in `src/generators` + `spec/generators`: 21 folders holding files; the domain cap is 16 (`capacity.domain`). src/ and spec/ count together."
    );
    assert_eq!(
        package.describe(&[]),
        "in `crates/core`: 14 domains holding files; the package cap is 12 (`capacity.package`)."
    );
    assert_eq!(
        group.describe(&[]),
        "in `.`: 13 packages holding files; the package group cap is 12 (`capacity.package-group`)."
    );
}

#[test]
fn should_describe_a_breach_without_real_paths_at_one_fallback_path() {
    let folder = CapacityBreach {
        measured: 24,
        cap: 20,
        unit: CapacityUnit::Entries,
        config_key: "capacity.folder".to_owned(),
        real_paths: Vec::new(),
        path: None,
    };

    let text = folder.describe(&["proj".to_owned(), "hub".to_owned()]);

    assert_eq!(
        text,
        "in `proj/hub`: 24 entries (files and subfolders); the folder cap is 20 (`capacity.folder`)."
    );
    assert!(!text.contains(" + "), "{text}");
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
            unit: CapacityUnit::Entries,
            config_key: "capacity.folder".to_owned(),
        })
        .unwrap_or_default(),
        "{\"kind\":\"relievesOverCap\",\"container\":\"app/src/core\",\"count\":4,\"cap\":3,\"unit\":\"entries\",\"configKey\":\"capacity.folder\"}"
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
                unit: CapacityUnit::Entries,
                config_key: "capacity.folder".to_owned(),
            },
            "relieves app/src/core, which has 4 entries (files and subfolders) against the folder cap of 3 (`capacity.folder`)",
        ),
        (
            MoveReason::RelievesOverCap {
                container: "app/src".to_owned(),
                count: 21,
                cap: 20,
                unit: CapacityUnit::Folders,
                config_key: "capacity.domain".to_owned(),
            },
            "relieves app/src, which has 21 folders holding files against the domain cap of 20 (`capacity.domain`)",
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
