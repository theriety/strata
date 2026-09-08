#![allow(clippy::assertions_on_constants)]

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_ir::{ContainerTree, Layout, Polarity, ScopeLevel, build_laminar_tree};

use crate::config::{AnalyzeConfig, ProfileName};
use crate::result::{CapacityBreach, Level, Severity, Violation, ViolationKind};

use super::*;
use crate::analyze::test_support::*;
use crate::analyze::*;
use crate::analyze::{rendering::*, scoring::*};

fn config_with_file_cap(cap: u32) -> AnalyzeConfig {
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.file = cap;
    config
}

#[test]
fn should_report_a_cycle_as_a_violation() {
    let snapshot = snapshot(
        vec![
            node(0, "a", 0, Polarity::Production),
            node(1, "b", 0, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![container(0, "file", ScopeLevel::File, None)],
    );

    let result = analyze(&snapshot, &AnalyzeConfig::default());
    let violations = result
        .map(|result| result.current.shared_findings)
        .unwrap_or_default();

    let cycle = violations
        .iter()
        .find(|violation| violation.kind == ViolationKind::Cycle);
    assert_eq!(
        cycle.map(|violation| violation.location.as_slice()),
        Some(["file".to_owned()].as_slice())
    );
    assert!(cycle.is_some_and(|violation| {
        violation.detail.contains("one placement unit")
            && violation.detail.contains("must remain in one file")
            && violation.detail.contains("break")
    }));
}

#[test]
fn should_emit_identical_findings_once_as_shared() {
    let snapshot = snapshot(
        vec![
            node(0, "left", 0, Polarity::Production),
            node(1, "right", 0, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![container(0, "src/pair.rs", ScopeLevel::File, None)],
    );

    let result = analyze(&snapshot, &AnalyzeConfig::default()).ok();
    let shared = result
        .as_ref()
        .map_or(&[][..], |result| result.current.shared_findings.as_slice());
    let anchored_unique = result
        .as_ref()
        .and_then(|result| result.profiles.anchored.as_ref())
        .map_or(&[][..], |profile| {
            profile.current.unique_findings.as_slice()
        });
    let greenfield_unique = result
        .as_ref()
        .and_then(|result| result.profiles.greenfield.as_ref())
        .map_or(&[][..], |profile| {
            profile.current.unique_findings.as_slice()
        });

    assert_eq!(shared.len(), 1);
    assert!(anchored_unique.is_empty());
    assert!(greenfield_unique.is_empty());

    let json = result
        .and_then(|result| serde_json::to_value(result).ok())
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        json.pointer("/current/sharedFindings")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        json.pointer("/profiles/anchored/current/uniqueFindings")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    assert_eq!(
        json.pointer("/profiles/greenfield/current/uniqueFindings")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0)
    );
}

#[test]
fn should_keep_profile_cap_findings_under_only_the_applicable_profile() {
    let snapshot = snapshot(
        vec![
            node(0, "first", 0, Polarity::Production),
            node(1, "second", 0, Polarity::Production),
        ],
        vec![],
        vec![container(0, "src/item.rs", ScopeLevel::File, None)],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.file = 1;
    config.profiles.greenfield.capacity.file = 2;

    let result = analyze(&snapshot, &config).ok();
    let shared = result
        .as_ref()
        .map_or(&[][..], |result| result.current.shared_findings.as_slice());
    let anchored_unique = result
        .as_ref()
        .and_then(|result| result.profiles.anchored.as_ref())
        .map_or(&[][..], |profile| {
            profile.current.unique_findings.as_slice()
        });
    let greenfield_unique = result
        .as_ref()
        .and_then(|result| result.profiles.greenfield.as_ref())
        .map_or(&[][..], |profile| {
            profile.current.unique_findings.as_slice()
        });

    assert!(shared.is_empty());
    assert_eq!(anchored_unique.len(), 1);
    assert!(greenfield_unique.is_empty());
}

#[test]
fn should_leave_shared_findings_empty_for_a_single_profile_run() {
    let snapshot = snapshot(
        vec![
            node(0, "left", 0, Polarity::Production),
            node(1, "right", 0, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![container(0, "src/pair.rs", ScopeLevel::File, None)],
    );
    let mut config = AnalyzeConfig::default();
    config.analysis.profiles = vec![ProfileName::Anchored];

    let result = analyze(&snapshot, &config).ok();
    let shared = result
        .as_ref()
        .map_or(&[][..], |result| result.current.shared_findings.as_slice());
    let anchored_unique = result
        .as_ref()
        .and_then(|result| result.profiles.anchored.as_ref())
        .map_or(&[][..], |profile| {
            profile.current.unique_findings.as_slice()
        });

    assert!(shared.is_empty());
    assert_eq!(anchored_unique.len(), 1);
    assert!(result.is_some_and(|result| result.profiles.greenfield.is_none()));
}

#[test]
fn should_report_a_polarity_breach() {
    let snapshot = snapshot(
        vec![
            node(0, "prod", 0, Polarity::Production),
            node(1, "helper", 0, Polarity::TestSupport),
        ],
        vec![edge(0, 1)],
        vec![container(0, "file", ScopeLevel::File, None)],
    );

    let result = analyze(&snapshot, &AnalyzeConfig::default());
    let violations = result
        .map(|result| result.current.shared_findings)
        .unwrap_or_default();

    assert!(violations.iter().any(|v| v.kind == ViolationKind::Polarity));
}

#[test]
fn should_report_a_test_support_dependency_on_a_test_case() {
    let snapshot = snapshot(
        vec![
            node(0, "helper", 0, Polarity::TestSupport),
            node(1, "spec", 0, Polarity::TestCase),
        ],
        vec![edge(0, 1)],
        vec![container(0, "file", ScopeLevel::File, None)],
    );

    let result = analyze(&snapshot, &AnalyzeConfig::default());
    let violations = result
        .map(|result| result.current.shared_findings)
        .unwrap_or_default();

    assert!(violations.iter().any(|v| v.kind == ViolationKind::Polarity
        && v.detail == "test support `helper` depends on test case `spec`"));
}

#[test]
fn should_locate_a_nested_over_cap_file_by_ancestors_and_basename() {
    // interior DTO names are incremental; the file keeps its full path but
    // its location contributes only the basename (detail has the path).
    let tree = folder(
        "app",
        vec![folder(
            "spec",
            vec![folder(
                "agent",
                vec![folder(
                    "mocks",
                    vec![file("spec/agent/mocks/google/gen.ts", 300)],
                )],
            )],
        )],
    );

    let findings = capacity_violations(&tree, &config_with_file_cap(250));

    let location = findings
        .iter()
        .find(|f| f.severity == Severity::Violation)
        .map(|f| f.location.clone())
        .unwrap_or_default();
    assert_eq!(location, vec!["app", "spec", "agent", "mocks", "gen.ts"]);
}

#[test]
fn should_collapse_repeated_synthetic_levels_in_capacity_locations() {
    // a root-level file hangs under the synthetic workspace chain, whose
    // levels all render the same segment; the location keeps it once.
    let tree = folder(
        "over-capacity",
        vec![folder(
            "workspace",
            vec![folder(
                "workspace",
                vec![folder("workspace", vec![file("huge.py", 300)])],
            )],
        )],
    );

    let findings = capacity_violations(&tree, &config_with_file_cap(250));

    let location = findings
        .iter()
        .find(|f| f.severity == Severity::Violation)
        .map(|f| f.location.clone())
        .unwrap_or_default();
    assert_eq!(location, vec!["over-capacity", "workspace", "huge.py"]);
}

#[test]
fn should_measure_folder_capacity_by_immediate_membership() {
    // each directory counts its immediate files and child directories;
    // deeper descendants do not add to an ancestor's measure.
    let tree = interior(
        "shared",
        Level::Domain,
        vec![
            folder(
                "a",
                vec![folder(
                    "b",
                    vec![folder("c", vec![file("f1", 10), file("f2", 10)])],
                )],
            ),
            folder("x", vec![folder("y", vec![file("g1", 10)])]),
        ],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 1;

    let findings = walk_all_capacity(&tree, &config.profiles.anchored.capacity);

    let folder_findings: Vec<(Vec<String>, Severity)> = findings
        .iter()
        .filter(|(level, _)| *level == Level::Folder)
        .map(|(_, violation)| (violation.location.clone(), violation.severity))
        .collect();
    assert_eq!(
        folder_findings,
        vec![(
            vec![
                "shared".to_owned(),
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
            ],
            Severity::Violation
        )]
    );
}

#[test]
fn should_measure_folder_capacity_by_immediate_entries() {
    let tree = interior(
        "workspace",
        Level::Domain,
        vec![folder(
            "section",
            vec![
                file("entry.ts", 10),
                folder(
                    "nested",
                    vec![file("a.ts", 10), file("b.ts", 10), file("c.ts", 10)],
                ),
            ],
        )],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 2;

    let findings = walk_all_capacity(&tree, &config.profiles.anchored.capacity);

    let folder_findings: Vec<(Vec<String>, Severity)> = findings
        .iter()
        .filter(|(level, _)| *level == Level::Folder)
        .map(|(_, violation)| (violation.location.clone(), violation.severity))
        .collect();
    assert_eq!(
        folder_findings,
        vec![(
            vec![
                "workspace".to_owned(),
                "section".to_owned(),
                "nested".to_owned(),
            ],
            Severity::Violation,
        )]
    );
}

#[test]
fn should_measure_transparent_source_roots_as_distinct_physical_folders() {
    let paths: Vec<SmolStr> = [
        "src/area/first.ts",
        "src/area/second.ts",
        "spec/area/first.spec.ts",
        "spec/area/second.spec.ts",
    ]
    .into_iter()
    .map(SmolStr::new)
    .collect();
    let layout = Layout {
        package_roots: Vec::new(),
        source_roots: vec![SmolStr::new("src"), SmolStr::new("spec")],
    };
    let built = build_laminar_tree(&paths, "workspace", &layout);
    let nodes = paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            built.files.get(path).map(|container| {
                sloc_node(
                    u32::try_from(index).unwrap_or(u32::MAX),
                    path,
                    *container,
                    10,
                )
            })
        })
        .collect();
    let snapshot = snapshot(nodes, Vec::new(), built.tree.containers().to_vec());
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 2;

    let findings = analyze(&snapshot, &config)
        .map(|result| result.current.shared_findings)
        .unwrap_or_default();

    let folder_findings: Vec<(Vec<String>, Severity)> = findings
        .iter()
        .filter(|violation| violation.capacity.is_some())
        .map(|violation| (violation.location.clone(), violation.severity))
        .collect();
    assert!(folder_findings.is_empty());
}

#[test]
fn should_order_nested_package_and_transparent_roots_once_for_capacity() {
    let tree = ContainerTree::new(vec![
        container(0, "workspace/packages/unit", ScopeLevel::PackageGroup, None),
        container(1, "area", ScopeLevel::Folder, Some(0)),
        container(
            2,
            "packages/unit/left/area/item.ts",
            ScopeLevel::File,
            Some(1),
        ),
    ]);

    let entries = physical_folder_entries(&tree, None);

    assert_eq!(
        entries.get(
            &["workspace", "packages", "unit", "left", "area"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        ),
        Some(&1),
        "package and transparent-root segments appear once, in physical order: {entries:?}"
    );
    assert!(
        entries
            .keys()
            .all(|path| path.join("/").matches("packages/unit").count() <= 1),
        "the nested package root is never duplicated: {entries:?}"
    );
}

#[test]
fn should_count_a_root_file_and_direct_child_folder_as_two_entries() {
    let tree = ContainerTree::new(vec![
        container(0, "workspace", ScopeLevel::PackageGroup, None),
        container(1, "workspace/root.ts", ScopeLevel::File, Some(0)),
        container(2, "area", ScopeLevel::Folder, Some(0)),
        container(3, "workspace/area/item.ts", ScopeLevel::File, Some(2)),
    ]);

    let entries = physical_folder_entries(&tree, None);

    assert_eq!(
        entries.get(&vec!["workspace".to_owned()]),
        Some(&2),
        "the package root counts its direct file and direct child folder: {entries:?}"
    );
    let findings = physical_folder_findings(&entries, 1);
    assert!(findings.iter().any(|finding| {
        finding.location == ["workspace"]
            && finding.capacity.as_ref().map(|breach| breach.measured) == Some(2)
    }));
    let pressure = physical_binding_pressure(&tree, &BTreeMap::new(), 1);
    assert!((pressure - 1.0).abs() < f64::EPSILON);
}

#[test]
fn should_count_a_nested_package_file_and_direct_child_folder_as_two_entries() {
    let tree = ContainerTree::new(vec![
        container(0, "workspace/packages/unit", ScopeLevel::PackageGroup, None),
        container(1, "packages/unit/root.ts", ScopeLevel::File, Some(0)),
        container(2, "area", ScopeLevel::Folder, Some(0)),
        container(3, "packages/unit/area/item.ts", ScopeLevel::File, Some(2)),
    ]);

    let entries = physical_folder_entries(&tree, None);

    assert_eq!(
        entries.get(
            [
                "workspace".to_owned(),
                "packages".to_owned(),
                "unit".to_owned(),
            ]
            .as_slice(),
        ),
        Some(&2),
        "the nested package root counts its direct file and child folder: {entries:?}"
    );
    let findings = physical_folder_findings(&entries, 1);
    assert!(findings.iter().any(|finding| {
        finding.location == ["workspace", "packages", "unit"]
            && finding.capacity.as_ref().map(|breach| breach.measured) == Some(2)
    }));
    let pressure = physical_binding_pressure(&tree, &BTreeMap::new(), 1);
    assert!((pressure - 1.0).abs() < f64::EPSILON);
}

#[test]
fn should_count_domain_capacity_by_top_level_directory_subtrees() {
    // a domain holding the single real directory `a/b/c` measures one
    // child subtree, not one per nested segment.
    let tree = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "ws", ScopeLevel::Package, Some(0)),
        container(2, "shared", ScopeLevel::Domain, Some(1)),
        container(3, "a/b/c", ScopeLevel::Folder, Some(2)),
        container(4, "src/a/b/c/f.ts", ScopeLevel::File, Some(3)),
    ]);
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.domain = 1;

    let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();
    let findings = rendered
        .map(|dto| walk_all_capacity(&dto, &config.profiles.anchored.capacity))
        .unwrap_or_default();

    let domain_findings: Vec<Severity> = findings
        .iter()
        .filter(|(level, _)| *level == Level::Domain)
        .map(|(_, violation)| violation.severity)
        .collect();
    assert!(domain_findings.is_empty());
}

#[test]
fn should_report_a_file_over_its_cap_as_a_hard_violation() {
    let tree = file("big", 100);

    let findings = capacity_violations(&tree, &config_with_file_cap(10));

    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings.first().map(|f| f.severity),
        Some(Severity::Violation)
    );
}

#[test]
fn should_report_a_file_within_the_band_as_borderline() {
    // cap 100, file at 105 sits inside the +10% band -> borderline.
    let tree = file("near", 105);

    let findings = capacity_violations(&tree, &config_with_file_cap(100));

    assert_eq!(
        findings.first().map(|f| f.severity),
        Some(Severity::Borderline)
    );
}

#[test]
fn should_not_report_a_file_exactly_at_its_cap() {
    let tree = file("exact", 100);

    let findings = capacity_violations(&tree, &config_with_file_cap(100));

    assert!(findings.is_empty(), "an at-cap file is not over capacity");
}

#[test]
fn should_not_report_a_file_below_its_cap_even_inside_the_old_margin() {
    let tree = file("under", 95);

    let findings = capacity_violations(&tree, &config_with_file_cap(100));

    assert!(findings.is_empty(), "a below-cap file is not a finding");
}

#[test]
fn should_treat_the_ten_percent_overage_boundary_as_borderline() {
    let tree = file("edge", 110);

    let findings = capacity_violations(&tree, &config_with_file_cap(100));

    assert_eq!(
        findings.first().map(|finding| finding.severity),
        Some(Severity::Borderline)
    );
}

#[test]
fn should_treat_an_overage_beyond_ten_percent_as_a_violation() {
    let tree = file("over", 111);

    let findings = capacity_violations(&tree, &config_with_file_cap(100));

    assert_eq!(
        findings.first().map(|finding| finding.severity),
        Some(Severity::Violation)
    );
}

#[test]
fn should_not_report_a_file_well_under_its_cap() {
    let tree = file("small", 10);

    let findings = capacity_violations(&tree, &config_with_file_cap(100));

    assert!(findings.is_empty());
}

#[test]
fn should_count_folder_members_against_the_folder_cap() {
    let children = (0..20).map(|i| file(&format!("f{i}"), 1)).collect();
    let tree = folder("dir", children);
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 5;

    let findings = capacity_violations(&tree, &config);

    assert!(findings.iter().any(|f| f.kind == ViolationKind::Capacity
        && f.severity == Severity::Violation
        && f.location == vec!["dir".to_owned()]));
}

#[test]
fn should_count_only_direct_files_and_immediate_child_folders_for_a_physical_folder() {
    let tree = folder(
        "root",
        vec![
            file("root/direct.ts", 1),
            folder(
                "branch",
                vec![
                    file("root/branch/one.ts", 1),
                    file("root/branch/two.ts", 1),
                    folder("leaf", vec![file("root/branch/leaf/three.ts", 1)]),
                ],
            ),
        ],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 2;

    let findings = capacity_violations(&tree, &config);

    assert!(
        findings.iter().all(|finding| finding.location != ["root"]),
        "root has exactly two immediate entries; descendants must not inflate it: {findings:?}"
    );
}

#[test]
fn should_sort_violations_by_severity_then_kind_then_location() {
    let finding = |kind, severity, location: &str| Violation {
        kind,
        severity,
        location: vec![location.to_owned()],
        detail: String::new(),
        break_suggestions: None,
        capacity: None,
    };
    let mut violations = vec![
        finding(ViolationKind::Capacity, Severity::Borderline, "a"),
        finding(ViolationKind::Visibility, Severity::Violation, "b"),
        finding(ViolationKind::Capacity, Severity::Violation, "z"),
        finding(ViolationKind::Capacity, Severity::Violation, "a"),
        finding(ViolationKind::Cycle, Severity::Violation, "y"),
    ];

    sort_violations(&mut violations);

    let order: Vec<(ViolationKind, Severity, &str)> = violations
        .iter()
        .filter_map(|violation| {
            violation
                .location
                .first()
                .map(|location| (violation.kind, violation.severity, location.as_str()))
        })
        .collect();
    assert_eq!(
        order,
        vec![
            (ViolationKind::Cycle, Severity::Violation, "y"),
            (ViolationKind::Capacity, Severity::Violation, "a"),
            (ViolationKind::Capacity, Severity::Violation, "z"),
            (ViolationKind::Visibility, Severity::Violation, "b"),
            (ViolationKind::Capacity, Severity::Borderline, "a"),
        ]
    );
}

#[test]
fn should_deduplicate_findings_by_complete_serialized_identity() {
    let duplicate = Violation {
        kind: ViolationKind::Capacity,
        severity: Severity::Violation,
        location: vec!["workspace/area".to_owned()],
        detail: "folder holds 3 against a cap of 1".to_owned(),
        break_suggestions: None,
        capacity: Some(CapacityBreach {
            measured: 3,
            cap: 1,
            path: None,
        }),
    };
    let distinct = Violation {
        capacity: Some(CapacityBreach {
            measured: 3,
            cap: 2,
            path: None,
        }),
        ..duplicate.clone()
    };
    let mut findings = vec![duplicate.clone(), distinct, duplicate];

    sort_and_dedup_violations(&mut findings);

    assert_eq!(findings.len(), 2);
    assert!(findings.windows(2).all(|pair| {
        pair.first()
            .zip(pair.get(1))
            .is_some_and(|(left, right)| violation_identity(left) != violation_identity(right))
    }));
}

#[test]
fn should_count_only_hard_capacity_findings_as_breaks() {
    let finding = |kind, severity, location: &str| Violation {
        kind,
        severity,
        location: vec![location.to_owned()],
        detail: String::new(),
        break_suggestions: None,
        capacity: None,
    };
    let violations = vec![
        finding(ViolationKind::Capacity, Severity::Borderline, "warm"),
        finding(ViolationKind::Visibility, Severity::Violation, "b"),
        finding(ViolationKind::Capacity, Severity::Violation, "big_folder"),
        finding(ViolationKind::Capacity, Severity::Violation, "huge_file"),
        finding(ViolationKind::Cycle, Severity::Violation, "y"),
        finding(
            ViolationKind::Capacity,
            Severity::Borderline,
            "another_warm",
        ),
    ];

    assert_eq!(hard_capacity_breaks(&violations), 2);
    assert_eq!(hard_capacity_breaks(&[]), 0);
}

#[test]
fn should_distinguish_same_named_visibility_findings_by_original_file() -> Result<(), String> {
    let mut left = node(0, "duplicate", 1, Polarity::Production);
    let mut right = node(1, "duplicate", 2, Polarity::Production);
    left.visibility = ScopeLevel::Package;
    right.visibility = ScopeLevel::Package;
    let snapshot = snapshot(
        vec![left, right],
        vec![],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "src/left.rs", ScopeLevel::File, Some(0)),
            container(2, "src/right.rs", ScopeLevel::File, Some(0)),
        ],
    );
    let config = AnalyzeConfig::default();
    let result = analyze(&snapshot, &config).map_err(|error| error.to_string())?;
    let visibility: Vec<_> = result
        .current
        .shared_findings
        .iter()
        .filter(|finding| finding.kind == ViolationKind::Visibility)
        .collect();
    assert_eq!(visibility.len(), 2, "same-name findings must not collapse");
    for path in ["src/left.rs", "src/right.rs"] {
        let finding = visibility
            .iter()
            .find(|finding| finding.location == [path, "duplicate"])
            .ok_or_else(|| format!("missing original identity {path}: {visibility:?}"))?;
        assert!(
            finding.detail.contains(&format!("`{path}`")),
            "{}",
            finding.detail
        );
    }
    let repeated = analyze(&snapshot, &config).map_err(|error| error.to_string())?;
    assert_eq!(
        result.current.shared_findings,
        repeated.current.shared_findings
    );
    Ok(())
}

#[test]
fn should_retain_visibility_identity_without_a_file_container() -> Result<(), String> {
    let mut exported = node(0, "orphan", 0, Polarity::Production);
    exported.visibility = ScopeLevel::Package;
    let snapshot = snapshot(
        vec![exported],
        vec![],
        vec![container(0, "folder", ScopeLevel::Folder, None)],
    );
    let result =
        analyze(&snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())?;
    let finding = result
        .current
        .shared_findings
        .iter()
        .find(|finding| finding.kind == ViolationKind::Visibility)
        .ok_or("missing folder-container visibility finding")?;
    assert_eq!(finding.location, ["orphan"]);
    assert_eq!(
        finding.detail,
        "`orphan` is exported at Package but needed only at Folder"
    );
    Ok(())
}
