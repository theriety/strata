#![allow(clippy::assertions_on_constants)]

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_ir::{
    ContainerId, ContainerTree, IntermediateRepresentation, Layout, NodeId, Polarity, ScopeLadder,
    ScopeLevel, Snapshot, build_laminar_tree,
};

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
fn should_omit_a_same_file_cycle_from_shared_and_profile_findings() -> Result<(), String> {
    let snapshot = snapshot(
        vec![
            node(0, "a", 0, Polarity::Production),
            node(1, "b", 0, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![container(0, "file", ScopeLevel::File, None)],
    );

    let result =
        analyze(&snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())?;
    assert!(
        result
            .current
            .shared_findings
            .iter()
            .all(|finding| finding.kind != ViolationKind::Cycle)
    );
    for profile in [&result.profiles.anchored, &result.profiles.greenfield] {
        let profile = profile.as_ref().ok_or("missing requested profile")?;
        assert!(
            profile
                .current
                .unique_findings
                .iter()
                .all(|finding| finding.kind != ViolationKind::Cycle)
        );
    }
    Ok(())
}

#[test]
fn should_emit_identical_findings_once_as_shared() {
    let snapshot = snapshot(
        vec![
            node(0, "left", 0, Polarity::Production),
            node(1, "right", 1, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![
            container(0, "src/left.rs", ScopeLevel::File, None),
            container(1, "src/right.rs", ScopeLevel::File, None),
        ],
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
    let cycle = shared
        .iter()
        .find(|finding| finding.kind == ViolationKind::Cycle);
    assert_eq!(
        cycle.map(|finding| finding.location.as_slice()),
        Some(["src/left.rs".to_owned(), "src/right.rs".to_owned()].as_slice())
    );
    assert!(cycle.is_some_and(|finding| {
        finding
            .detail
            .contains("strata keeps a dependency cycle in one file")
            && finding.detail.contains("to split it, break")
            && !finding.detail.contains("must remain")
    }));
    assert_eq!(anchored_unique, []);
    assert_eq!(greenfield_unique, []);

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

    assert_eq!(shared, []);
    assert_eq!(anchored_unique.len(), 1);
    assert_eq!(greenfield_unique, []);
}

#[test]
fn should_leave_shared_findings_empty_for_a_single_profile_run() {
    let snapshot = snapshot(
        vec![
            node(0, "left", 0, Polarity::Production),
            node(1, "right", 1, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![
            container(0, "src/left.rs", ScopeLevel::File, None),
            container(1, "src/right.rs", ScopeLevel::File, None),
        ],
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

    assert_eq!(shared, []);
    assert_eq!(anchored_unique.len(), 1);
    assert!(result.is_some_and(|result| result.profiles.greenfield.is_none()));
}

#[test]
fn should_retain_a_cycle_between_distinct_files_with_the_same_name() -> Result<(), String> {
    let snapshot = snapshot(
        vec![
            node(0, "left", 0, Polarity::Production),
            node(1, "right", 1, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![
            container(0, "src/item.rs", ScopeLevel::File, None),
            container(1, "src/item.rs", ScopeLevel::File, None),
        ],
    );

    let result =
        analyze(&snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())?;
    assert!(
        result
            .current
            .shared_findings
            .iter()
            .any(|finding| finding.kind == ViolationKind::Cycle)
    );
    Ok(())
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
        && v.detail == "test support `helper` (file) depends on test case `spec` (file)"));
}

#[test]
fn should_locate_a_polarity_breach_by_source_and_target_file_paths() {
    let snapshot = snapshot(
        vec![
            node(0, "same", 0, Polarity::Production),
            node(1, "same", 1, Polarity::TestSupport),
        ],
        vec![edge(0, 1)],
        vec![
            container(0, "src/facade.rs", ScopeLevel::File, None),
            container(1, "src/origin.rs", ScopeLevel::File, None),
        ],
    );

    let violations = analyze(&snapshot, &AnalyzeConfig::default())
        .map(|result| result.current.shared_findings)
        .unwrap_or_default();
    let polarity: Vec<&Violation> = violations
        .iter()
        .filter(|v| v.kind == ViolationKind::Polarity)
        .collect();

    assert_eq!(polarity.len(), 1);
    assert_eq!(
        polarity.first().map(|v| v.location.clone()),
        Some(vec!["src/facade.rs".to_owned(), "src/origin.rs".to_owned()])
    );
    assert_eq!(
        polarity.first().map(|v| v.detail.as_str()),
        Some(
            "production symbol `same` (src/facade.rs) depends on test code `same` \
             (src/origin.rs)"
        )
    );
}

#[test]
fn should_locate_a_same_file_polarity_breach_by_one_path() {
    let snapshot = snapshot(
        vec![
            node(0, "prod", 0, Polarity::Production),
            node(1, "helper", 0, Polarity::TestSupport),
        ],
        vec![edge(0, 1)],
        vec![container(0, "src/a.rs", ScopeLevel::File, None)],
    );

    let violations = analyze(&snapshot, &AnalyzeConfig::default())
        .map(|result| result.current.shared_findings)
        .unwrap_or_default();

    assert!(
        violations
            .iter()
            .any(|v| v.kind == ViolationKind::Polarity && v.location == ["src/a.rs".to_owned()])
    );
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
    assert_eq!(folder_findings, []);
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
fn should_compose_capacity_paths_below_a_nested_package_not_above_it() {
    // ADR-18: a namespace is package-relative, so a file in `crates/core/src`
    // counts toward `strata/crates/core/src`, never `strata/src/crates/core`.
    let tree = ContainerTree::new(vec![
        container(0, "strata", ScopeLevel::PackageGroup, None),
        container(1, "crates/core", ScopeLevel::Package, Some(0)),
        strata_ir::Container {
            synthetic: true,
            ..container(2, "crates/core/workspace", ScopeLevel::Folder, Some(1))
        },
        container(3, "crates/core/cluster", ScopeLevel::Folder, Some(1)),
        container(4, "crates/core/src/lib.rs", ScopeLevel::File, Some(2)),
        container(
            5,
            "crates/core/src/cluster/refine.rs",
            ScopeLevel::File,
            Some(3),
        ),
    ]);
    let namespaces: BTreeMap<ContainerId, SmolStr> = [4, 5]
        .into_iter()
        .map(|id| (ContainerId(id), SmolStr::new("src")))
        .collect();

    let entries = physical_folder_entries(&tree, Some(&namespaces));
    let path = |segments: &[&str]| -> Vec<String> {
        segments
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect()
    };

    assert_eq!(
        entries.get(&path(&["strata", "crates", "core", "src"])),
        Some(&2),
        "`src` holds `lib.rs` and the `cluster` directory: {entries:?}"
    );
    assert_eq!(
        entries.get(&path(&["strata", "crates", "core", "src", "cluster"])),
        Some(&1),
        "{entries:?}"
    );
    assert!(
        entries
            .keys()
            .all(|key| !key.join("/").starts_with("strata/src")),
        "no invented `strata/src/...` directory: {entries:?}"
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
    assert_eq!(domain_findings, []);
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

    assert_eq!(findings, []);
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
fn should_stay_silent_for_a_non_file_need_without_a_ladder() -> Result<(), String> {
    let mut exported = node(0, "orphan", 0, Polarity::Production);
    exported.visibility = ScopeLevel::Package;
    let snapshot = snapshot(
        vec![exported],
        vec![],
        vec![container(0, "folder", ScopeLevel::Folder, None)],
    );
    let result =
        analyze(&snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())?;
    // no ladder: a need above its own file is not known to be spellable.
    assert!(
        !result
            .current
            .shared_findings
            .iter()
            .any(|finding| finding.kind == ViolationKind::Visibility)
    );
    Ok(())
}

/// A ladder verdict for the fixture's `item`: declared at `Package`, needed at
/// `needed` (a strictly narrower rung) with an optional spelling.
fn verdict(needed: Option<ScopeLevel>, spelling: Option<&str>) -> ScopeLadder {
    ScopeLadder {
        node: NodeId(0),
        declared: Some(ScopeLevel::Package),
        needed,
        spelling: spelling.map(smol_str::SmolStr::new),
    }
}

/// Builds a package holding `src/` (files `args`, `main`) and `src/sub/` (files
/// `a`, `b`) with one `Package`-declared `item` in `home` consumed from
/// `consumer_file`, optionally with an adapter ladder verdict for the item.
fn ladder_fixture(
    home: u32,
    consumer_file: Option<u32>,
    ladder: Option<ScopeLadder>,
) -> Result<Snapshot, String> {
    let containers = vec![
        container(0, "pkg", ScopeLevel::Package, None),
        container(1, "src", ScopeLevel::Domain, Some(0)),
        container(2, "src/args.rs", ScopeLevel::File, Some(1)),
        container(3, "src/main.rs", ScopeLevel::File, Some(1)),
        container(4, "src/sub", ScopeLevel::Folder, Some(1)),
        container(5, "src/sub/a.rs", ScopeLevel::File, Some(4)),
        container(6, "src/sub/b.rs", ScopeLevel::File, Some(4)),
    ];
    let mut item = node(0, "item", home, Polarity::Production);
    item.visibility = ScopeLevel::Package;
    let mut nodes = vec![item];
    let mut edges = Vec::new();
    if let Some(file) = consumer_file {
        nodes.push(node(1, "user", file, Polarity::Production));
        edges.push(edge(1, 0));
    }
    let mut ir = IntermediateRepresentation::new(nodes, edges, ContainerTree::new(containers));
    ir.scope_ladders = ladder.into_iter().collect();
    Snapshot::assemble(ir).map_err(|error| error.to_string())
}

#[test]
fn should_not_flag_a_crate_root_sibling_that_no_narrower_spelling_reaches() -> Result<(), String> {
    let snapshot = ladder_fixture(2, Some(3), Some(verdict(None, None)))?;
    assert_eq!(visibility_violations(&snapshot), []);
    Ok(())
}

#[test]
fn should_stay_silent_for_a_laddered_item_the_adapter_could_not_judge() -> Result<(), String> {
    let silent = ScopeLadder {
        declared: None,
        ..verdict(None, None)
    };
    let snapshot = ladder_fixture(2, None, Some(silent))?;
    assert_eq!(visibility_violations(&snapshot), []);
    Ok(())
}

#[test]
fn should_flag_an_item_with_a_narrower_needed_rung_and_name_its_spelling() -> Result<(), String> {
    let ladder = verdict(Some(ScopeLevel::File), Some("private (no `pub`)"));
    let found = visibility_violations(&ladder_fixture(2, None, Some(ladder))?);
    let finding = found.first().ok_or("missing finding")?;
    assert_eq!(
        finding.detail,
        "`item` in `src/args.rs` is exported at Package but needed only at File; narrowest spelling: private (no `pub`)"
    );
    Ok(())
}

#[test]
fn should_stay_silent_without_a_ladder_unless_the_need_is_its_own_file() -> Result<(), String> {
    // no ladder: a cross-file consumer gives no spellable scope, silence.
    assert_eq!(
        visibility_violations(&ladder_fixture(2, Some(3), None)?),
        []
    );
    // no ladder and no outside user: the need is File, so it is flagged.
    let found = visibility_violations(&ladder_fixture(2, None, None)?);
    assert_eq!(found.len(), 1, "{found:?}");
    Ok(())
}

#[test]
fn should_flag_a_nested_item_when_the_ladder_names_a_narrower_folder() -> Result<(), String> {
    let ladder = verdict(Some(ScopeLevel::Folder), None);
    let found = visibility_violations(&ladder_fixture(5, Some(6), Some(ladder))?);
    let finding = found.first().ok_or("missing finding")?;
    assert!(
        finding.detail.contains("Package but needed only at Folder"),
        "{}",
        finding.detail
    );
    Ok(())
}

/// Builds a one-file snapshot where `item` (id 0, `Package`-declared) is used
/// only by a test node (id 1, `test_source`), after `adjust` edits the item and
/// the users.
fn test_only_fixture(
    adjust: impl FnOnce(&mut strata_ir::Node, &mut strata_ir::Node),
) -> Result<Snapshot, String> {
    let mut item = node(0, "item", 0, Polarity::Production);
    item.visibility = ScopeLevel::Package;
    let mut user = node(1, "check", 0, Polarity::TestCase);
    user.test_source = true;
    adjust(&mut item, &mut user);
    let nodes = vec![item, user];
    let ir = IntermediateRepresentation::new(
        nodes,
        vec![edge(1, 0)],
        ContainerTree::new(vec![container(0, "src/a.rs", ScopeLevel::File, None)]),
    );
    Snapshot::assemble(ir).map_err(|error| error.to_string())
}

/// Counts the test-only and visibility findings of `snapshot`.
fn test_only_and_visibility(snapshot: &Snapshot) -> (usize, usize) {
    (
        test_only::test_only_violations(snapshot).len(),
        visibility_violations(snapshot).len(),
    )
}

#[test]
fn should_report_an_item_used_only_by_test_sources_once_and_not_as_visibility() -> Result<(), String>
{
    let snapshot = test_only_fixture(|_, _| {})?;
    assert_eq!(test_only_and_visibility(&snapshot), (1, 0));
    let found = test_only::test_only_violations(&snapshot);
    assert_eq!(
        found
            .iter()
            .map(|v| (v.kind, v.detail.as_str()))
            .collect::<Vec<_>>(),
        [(
            ViolationKind::TestOnly,
            "`item` is used only by tests; move it under test-only code or delete it"
        )]
    );
    Ok(())
}

#[test]
fn should_never_report_test_only_for_a_node_advice_must_not_touch() -> Result<(), String> {
    type Flag = fn(&mut strata_ir::Node);
    let flags: [(&str, Flag); 2] = [
        ("re_export", |node| node.re_export = true),
        ("advice_exempt", |node| node.advice_exempt = true),
    ];
    for (label, flag) in flags {
        let snapshot = test_only_fixture(|item, _| flag(item))?;
        assert_eq!(
            test_only::test_only_violations(&snapshot),
            [],
            "{label} is exempt from test-only advice"
        );
    }
    Ok(())
}

#[test]
fn should_report_a_public_api_item_used_only_by_tests_with_the_public_wording() -> Result<(), String>
{
    let snapshot = test_only_fixture(|item, _| item.public_api = true)?;
    assert_eq!(test_only_and_visibility(&snapshot), (1, 0));
    let found = test_only::test_only_violations(&snapshot);
    assert_eq!(
        found.iter().map(|v| v.detail.as_str()).collect::<Vec<_>>(),
        [
            "Only tests in this repo use `item`. It is public, so code outside this repo may use it too; check before moving or deleting it."
        ]
    );
    Ok(())
}

#[test]
fn should_not_report_test_only_for_test_code_or_a_user_the_source_does_not_call_test()
-> Result<(), String> {
    // the item itself is declared as test code.
    let test_item = test_only_fixture(|item, _| item.test_source = true)?;
    assert_eq!(test_only::test_only_violations(&test_item), []);
    // a user demoted to test support by reachability is not a test in source.
    let demoted_user = test_only_fixture(|_, user| user.test_source = false)?;
    assert_eq!(test_only::test_only_violations(&demoted_user), []);
    Ok(())
}

#[test]
fn should_not_report_test_only_for_a_self_loop_alone_or_an_advisory_use() -> Result<(), String> {
    let mut ir = IntermediateRepresentation::new(
        vec![node(0, "item", 0, Polarity::Production)],
        vec![edge(0, 0)],
        ContainerTree::new(vec![container(0, "src/a.rs", ScopeLevel::File, None)]),
    );
    let loop_only = Snapshot::assemble(ir.clone()).map_err(|error| error.to_string())?;
    assert_eq!(test_only::test_only_violations(&loop_only), []);
    ir.nodes.push({
        let mut user = node(1, "check", 0, Polarity::TestCase);
        user.test_source = true;
        user
    });
    ir.edges = vec![edge(1, 0)];
    ir.advisory_uses = vec![strata_ir::AdvisoryUse {
        user: NodeId(0),
        used: NodeId(0),
        from_test: false,
    }];
    let advised = Snapshot::assemble(ir).map_err(|error| error.to_string())?;
    assert_eq!(test_only::test_only_violations(&advised), []);
    Ok(())
}

#[test]
fn should_not_let_an_advisory_use_from_test_code_silence_test_only() -> Result<(), String> {
    let mut ir = IntermediateRepresentation::new(
        vec![node(0, "item", 0, Polarity::Production), {
            let mut user = node(1, "check", 0, Polarity::TestCase);
            user.test_source = true;
            user
        }],
        vec![edge(1, 0)],
        ContainerTree::new(vec![container(0, "src/a.rs", ScopeLevel::File, None)]),
    );
    ir.advisory_uses = vec![strata_ir::AdvisoryUse {
        user: NodeId(1),
        used: NodeId(0),
        from_test: true,
    }];
    let snapshot = Snapshot::assemble(ir).map_err(|error| error.to_string())?;
    assert_eq!(test_only::test_only_violations(&snapshot).len(), 1);
    Ok(())
}

/// A type `Owner`, its member `Owner::new`, a test, and optionally a
/// production caller of the member.
fn owner_fixture(production_caller: bool) -> Result<Snapshot, String> {
    let mut test = node(2, "check", 0, Polarity::TestCase);
    test.test_source = true;
    let mut owner = node(0, "Owner", 0, Polarity::Production);
    owner.display_name = Some("Owner".into());
    let mut member = node(1, "new", 0, Polarity::Production);
    member.display_name = Some("Owner::new".into());
    member.owner = Some(NodeId(0));
    let mut nodes = vec![owner, member, test];
    let mut edges = vec![edge(1, 0), edge(2, 1), edge(2, 0)];
    if production_caller {
        nodes.push(node(3, "run", 0, Polarity::Production));
        edges.push(edge(3, 1));
    }
    let ir = IntermediateRepresentation::new(
        nodes,
        edges,
        ContainerTree::new(vec![container(0, "src/a.rs", ScopeLevel::File, None)]),
    );
    Snapshot::assemble(ir).map_err(|error| error.to_string())
}

#[test]
fn should_report_a_type_whose_only_other_user_is_its_own_test_only_member() -> Result<(), String> {
    let names: Vec<u32> = test_only::test_only_nodes(&owner_fixture(false)?)
        .into_iter()
        .collect();
    assert_eq!(names, [0, 1]);
    Ok(())
}

#[test]
fn should_not_report_a_type_whose_member_has_a_production_caller() -> Result<(), String> {
    let found: Vec<u32> = test_only::test_only_nodes(&owner_fixture(true)?)
        .into_iter()
        .collect();
    assert_eq!(found, Vec::<u32>::new());
    Ok(())
}
