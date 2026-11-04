//! Capacity findings state what they counted: unit, cap, config key, real folders.

use smol_str::SmolStr;
use unlace_ir::{Layout, build_laminar_tree};

use crate::analyze::analyze;
use crate::analyze::test_support::{sloc_node, snapshot};
use crate::config::AnalyzeConfig;
use crate::result::{CapacityBreach, CapacityUnit, Severity, Violation, ViolationKind};

/// Analyzes a laminar tree built over `paths`, each file `sloc` production
/// lines, and returns the current tree's capacity findings.
fn capacity_findings_over(
    paths: &[&str],
    sloc: u32,
    configure: impl FnOnce(&mut AnalyzeConfig),
) -> Vec<Violation> {
    capacity_findings_in("workspace", paths, sloc, configure)
}

/// [`capacity_findings_over`] under the package-group label `group`.
fn capacity_findings_in(
    group: &str,
    paths: &[&str],
    sloc: u32,
    configure: impl FnOnce(&mut AnalyzeConfig),
) -> Vec<Violation> {
    let paths: Vec<SmolStr> = paths.iter().map(|path| SmolStr::new(*path)).collect();
    let layout = Layout {
        package_roots: Vec::new(),
        source_roots: vec![SmolStr::new("src"), SmolStr::new("spec")],
    };
    let built = build_laminar_tree(&paths, group, &layout);
    let nodes = paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            built.files.get(path).map(|container| {
                sloc_node(
                    u32::try_from(index).unwrap_or(u32::MAX),
                    path,
                    *container,
                    sloc,
                )
            })
        })
        .collect();
    let snapshot = snapshot(nodes, Vec::new(), built.tree.containers().to_vec());
    let mut config = AnalyzeConfig::default();
    configure(&mut config);
    let Ok(result) = analyze(&snapshot, &config) else {
        return Vec::new();
    };
    // only the anchored caps are tuned, so its findings are profile-unique.
    let unique = result
        .profiles
        .anchored
        .map(|profile| profile.current.unique_findings)
        .unwrap_or_default();
    result
        .current
        .shared_findings
        .into_iter()
        .chain(unique)
        .filter(|finding| finding.kind == ViolationKind::Capacity)
        .collect()
}

fn first_breach(findings: &[Violation]) -> Option<CapacityBreach> {
    findings
        .first()
        .and_then(|finding| finding.capacity.clone())
}

fn first_detail(findings: &[Violation]) -> Option<&str> {
    findings.first().map(|finding| finding.detail.as_str())
}

#[test]
fn should_state_unit_key_and_real_path_for_a_file_breach() {
    let findings = capacity_findings_over(&["spec/debug/doctor.ts"], 288, |config| {
        config.profiles.anchored.capacity.file = 250;
    });

    assert_eq!(
        first_breach(&findings),
        Some(CapacityBreach {
            measured: 288,
            cap: 250,
            unit: CapacityUnit::Lines,
            config_key: "capacity.file".to_owned(),
            real_paths: vec!["spec/debug/doctor.ts".to_owned()],
            path: Some("spec/debug/doctor.ts".to_owned()),
        })
    );
    assert_eq!(
        first_detail(&findings),
        Some(
            "in `spec/debug/doctor.ts`: 288 production lines; the file cap is 250 (`capacity.file`)."
        )
    );
}

#[test]
fn should_state_unit_key_and_real_folder_for_a_folder_breach() {
    let findings = capacity_findings_over(
        &[
            "src/batch/adapters/a.ts",
            "src/batch/adapters/b.ts",
            "src/batch/adapters/c.ts",
        ],
        10,
        |config| config.profiles.anchored.capacity.folder = 2,
    );

    assert_eq!(
        first_breach(&findings),
        Some(CapacityBreach {
            measured: 3,
            cap: 2,
            unit: CapacityUnit::Entries,
            config_key: "capacity.folder".to_owned(),
            real_paths: vec!["src/batch/adapters".to_owned()],
            path: None,
        })
    );
    assert_eq!(
        first_detail(&findings),
        Some(
            "in `src/batch/adapters`: 3 entries (files and subfolders); the folder cap is 2 (`capacity.folder`)."
        )
    );
}

#[test]
fn should_list_both_real_folders_of_a_domain_spanning_src_and_spec() {
    let findings = capacity_findings_over(
        &[
            "src/generators/a/x.ts",
            "src/generators/b/y.ts",
            "spec/generators/c/z.ts",
            "spec/generators/d/w.ts",
        ],
        10,
        |config| config.profiles.anchored.capacity.domain = 3,
    );

    assert_eq!(
        first_breach(&findings),
        Some(CapacityBreach {
            measured: 4,
            cap: 3,
            unit: CapacityUnit::Folders,
            config_key: "capacity.domain".to_owned(),
            real_paths: vec!["spec/generators".to_owned(), "src/generators".to_owned()],
            path: None,
        })
    );
    assert_eq!(
        first_detail(&findings),
        Some(
            "in `spec/generators` + `src/generators`: 4 sub-folders directly inside; the domain cap is 3 (`capacity.domain`). spec/ and src/ count together."
        )
    );
}

#[test]
fn should_omit_the_merge_note_when_a_domain_has_one_real_folder() {
    let findings = capacity_findings_over(&["src/gen/a/x.ts", "src/gen/b/y.ts"], 10, |config| {
        config.profiles.anchored.capacity.domain = 1;
    });

    assert_eq!(
        first_detail(&findings),
        Some(
            "in `src/gen`: 2 sub-folders directly inside; the domain cap is 1 (`capacity.domain`)."
        )
    );
}

#[test]
fn should_keep_borderline_severity_while_rewording_a_domain_breach() {
    let paths: Vec<String> = (0..11)
        .map(|index| format!("src/gen/f{index}/x.ts"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let findings = capacity_findings_over(&refs, 10, |config| {
        config.profiles.anchored.capacity.domain = 10;
    });

    assert_eq!(
        findings
            .iter()
            .map(|finding| (
                finding.severity,
                finding.capacity.as_ref().map(|breach| breach.measured)
            ))
            .collect::<Vec<_>>(),
        vec![(Severity::Borderline, Some(11))]
    );
}

fn location_of(findings: &[Violation]) -> Option<Vec<String>> {
    findings.first().map(|finding| finding.location.clone())
}

/// A capacity finding's location: the one whole real path.
fn path(path: &str) -> Vec<String> {
    vec![path.to_owned()]
}

#[test]
fn should_locate_a_file_breach_by_its_real_path() {
    let findings = capacity_findings_over(&["spec/debug/doctor.ts"], 288, |config| {
        config.profiles.anchored.capacity.file = 250;
    });

    assert_eq!(location_of(&findings), Some(path("spec/debug/doctor.ts")));
}

#[test]
fn should_locate_a_folder_breach_without_the_group_name() {
    let findings = capacity_findings_over(
        &[
            "src/batch/adapters/a.ts",
            "src/batch/adapters/b.ts",
            "src/batch/adapters/c.ts",
        ],
        10,
        |config| config.profiles.anchored.capacity.folder = 2,
    );

    assert_eq!(location_of(&findings), Some(path("src/batch/adapters")));
}

#[test]
fn should_locate_a_merged_domain_by_its_first_real_path() {
    let findings = capacity_findings_over(
        &[
            "src/generators/a/x.ts",
            "src/generators/b/y.ts",
            "spec/generators/c/z.ts",
            "spec/generators/d/w.ts",
        ],
        10,
        |config| config.profiles.anchored.capacity.domain = 3,
    );

    // the lexicographically first of `realPaths`, which lists the rest.
    assert_eq!(location_of(&findings), Some(path("spec/generators")));
}

#[test]
fn should_keep_a_leading_directory_named_like_the_group_in_every_capacity_location() {
    // flat layout: the group label `proj` is also the real top-level directory.
    let folder = capacity_findings_in(
        "proj",
        &["proj/hub/a.ts", "proj/hub/b.ts", "proj/hub/c.ts"],
        10,
        |config| config.profiles.anchored.capacity.folder = 2,
    );
    let domain = capacity_findings_in("proj", &["proj/gen/x.ts", "proj/lib/y.ts"], 10, |config| {
        config.profiles.anchored.capacity.domain = 1;
    });
    let file = capacity_findings_in("proj", &["proj/big.ts"], 288, |config| {
        config.profiles.anchored.capacity.file = 250;
    });

    assert_eq!(location_of(&folder), Some(path("proj/hub")));
    assert_eq!(
        folder.first().map(|finding| finding.detail.as_str()),
        Some(
            "in `proj/hub`: 3 entries (files and subfolders); the folder cap is 2 (`capacity.folder`)."
        )
    );
    // the laminar domain is the top directory `proj` itself, which really exists.
    assert_eq!(location_of(&domain), Some(path("proj")));
    assert_eq!(location_of(&file), Some(path("proj/big.ts")));
}

#[test]
fn should_count_a_same_named_src_and_spec_child_of_a_domain_once() {
    let findings = capacity_findings_over(
        &[
            "src/generators/analysts/a.ts",
            "spec/generators/analysts/b.ts",
            "src/generators/embedders/c.ts",
            "spec/generators/navigators/d.ts",
        ],
        10,
        |config| config.profiles.anchored.capacity.domain = 2,
    );

    assert_eq!(
        first_breach(&findings).map(|breach| breach.measured),
        Some(3)
    );
    assert_eq!(
        first_detail(&findings),
        Some(
            "in `spec/generators` + `src/generators`: 3 sub-folders directly inside; the domain cap is 2 (`capacity.domain`). spec/ and src/ count together."
        )
    );
}

#[test]
fn should_count_only_direct_subfolders_of_a_domain_not_deeper_folders() {
    // two direct children; the eight folders beneath them do not count.
    let findings = capacity_findings_over(
        &[
            "src/gen/a/b1/x.ts",
            "src/gen/a/b2/x.ts",
            "src/gen/a/b3/x.ts",
            "src/gen/a/b4/x.ts",
            "src/gen/c/d1/x.ts",
            "src/gen/c/d2/x.ts",
            "src/gen/c/d3/x.ts",
            "src/gen/c/d4/x.ts",
        ],
        10,
        |config| config.profiles.anchored.capacity.domain = 1,
    );

    assert_eq!(
        first_breach(&findings).map(|breach| breach.measured),
        Some(2)
    );
}

#[test]
fn should_count_a_real_same_named_subfolder_but_not_the_domains_own_directory() {
    // `gen/gen/` is a real sub-folder of the domain `gen`; `gen/top.ts` sits in
    // the domain's own directory and is no sub-folder.
    let findings = capacity_findings_over(
        &["src/gen/gen/g.ts", "src/gen/top.ts", "src/gen/a/x.ts"],
        10,
        |config| config.profiles.anchored.capacity.domain = 1,
    );
    assert_eq!(
        first_breach(&findings).map(|breach| breach.measured),
        Some(2)
    );

    let own_only = capacity_findings_over(&["src/gen/top.ts", "src/gen/other.ts"], 10, |config| {
        config.profiles.anchored.capacity.domain = 0;
    });
    assert!(own_only.is_empty(), "{own_only:?}");
}

#[test]
fn should_count_an_interior_chain_under_a_domain_as_one_subfolder() {
    let findings = capacity_findings_over(
        &["src/gen/a/b/c/x.ts", "src/gen/d/y.ts", "src/gen/d/e/z.ts"],
        10,
        |config| config.profiles.anchored.capacity.domain = 1,
    );

    assert_eq!(
        first_breach(&findings).map(|breach| breach.measured),
        Some(2)
    );
}
