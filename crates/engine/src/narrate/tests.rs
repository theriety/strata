//! Narration unit tests.

use smol_str::SmolStr;
use strata_ir::{Container, ContainerId, ContainerTree, ScopeLevel};

use super::*;
use crate::result::MoveReason;

#[test]
fn should_retain_a_real_directory_that_repeats_the_repository_root() {
    let relative = vec!["app".to_owned(), "foo.ts".to_owned()];

    assert_eq!(
        project_physical_path("app", &relative),
        vec!["app".to_owned(), "app".to_owned(), "foo.ts".to_owned()]
    );
    assert_eq!(
        normalize_physical_path(
            "app",
            &[
                "app".to_owned(),
                "candidate".to_owned(),
                "foo.ts".to_owned()
            ]
        ),
        vec![
            "app".to_owned(),
            "candidate".to_owned(),
            "foo.ts".to_owned()
        ]
    );
}

/// Builds a container at a level with an optional parent.
fn container(id: u32, name: &str, level: ScopeLevel, parent: Option<u32>) -> Container {
    Container {
        id: ContainerId(id),
        name: SmolStr::new(name),
        level,
        parent: parent.map(ContainerId),
        synthetic: false,
    }
}

/// A domain root `app` over two folders `src/core` and `src/io` holding the
/// given files (cumulative names, as interning produces them).
fn two_folder_tree(core_files: &[&str], io_files: &[&str]) -> ContainerTree {
    let mut containers = vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/core", ScopeLevel::Folder, Some(0)),
        container(2, "src/io", ScopeLevel::Folder, Some(0)),
    ];
    let mut next = 3;
    for file in core_files {
        containers.push(container(next, file, ScopeLevel::File, Some(1)));
        next += 1;
    }
    for file in io_files {
        containers.push(container(next, file, ScopeLevel::File, Some(2)));
        next += 1;
    }
    ContainerTree::new(containers)
}

/// Facts with no edges, no specs, and a roomy folder cap.
fn plain_facts() -> FileFacts {
    FileFacts {
        edge_weights: BTreeMap::new(),
        test_case_files: BTreeSet::new(),
        shadow_test_files: BTreeSet::new(),
        folder_cap: 15,
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn should_tokenize_basenames_on_separators_and_camel_case() {
    let kebab = tokenize("src/core/user-service.ts");
    let camel = tokenize("UserService.spec.ts");

    assert_eq!(kebab, strings(&["user", "service"]).into_iter().collect());
    assert_eq!(
        camel,
        strings(&["user", "service", "spec"]).into_iter().collect()
    );
}

#[test]
fn should_compose_a_destination_from_the_folder_key_not_a_decorated_domain_label() {
    // when clustering merges several real domains into one suggested domain,
    // the elected domain label decorates to `cts (cts.core)` to stay
    // injective. That label is a display overlay, never a path component:
    // the move destination is the folder's own key, the real directory the
    // file lands in. The old fold treated the decorated label as a path
    // ancestor and re-embedded the whole folder key beneath it.
    let current = ContainerTree::new(vec![
        container(0, "cts", ScopeLevel::PackageGroup, None),
        container(1, "cts", ScopeLevel::Package, Some(0)),
        container(2, "cts/util", ScopeLevel::Domain, Some(1)),
        container(3, "cts/util", ScopeLevel::Folder, Some(2)),
        container(4, "src/util/clamp.ts", ScopeLevel::File, Some(3)),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "cts", ScopeLevel::PackageGroup, None),
        container(1, "cts", ScopeLevel::Package, Some(0)),
        container(2, "cts (cts.core)", ScopeLevel::Domain, Some(1)),
        container(3, "cts/core", ScopeLevel::Folder, Some(2)),
        container(4, "src/util/clamp.ts", ScopeLevel::File, Some(3)),
    ]);

    let moves = narrate(&current, &candidate, &plain_facts());

    assert_eq!(moves.len(), 1);
    let entry = moves.first();
    assert_eq!(entry.map(|m| m.to.clone()), Some("cts/src/core".to_owned()));
    assert_eq!(
        entry.map(|m| m.files.iter().map(|f| f.from.clone()).collect::<Vec<_>>()),
        Some(strings(&["cts/src/util"]))
    );
}

#[test]
fn should_group_files_sharing_a_destination_into_one_move() {
    let current = two_folder_tree(&["src/core/a.ts", "src/core/b.ts"], &[]);
    let candidate = two_folder_tree(&[], &["src/core/a.ts", "src/core/b.ts"]);

    let moves = narrate(&current, &candidate, &plain_facts());

    assert_eq!(moves.len(), 1);
    let entry = moves.first();
    assert_eq!(
        entry.map(|m| m.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()),
        Some(strings(&["src/core/a.ts", "src/core/b.ts"]))
    );
    assert_eq!(
        entry.map(|m| m.files.iter().map(|f| f.from.clone()).collect::<Vec<_>>()),
        Some(strings(&["src/core", "src/core"]))
    );
    assert_eq!(entry.map(|m| m.to.clone()), Some("src/io".to_owned()));
    assert_eq!(entry.map(|m| m.kind), Some(MoveKind::Move));
}

#[test]
fn should_preserve_physical_namespaces_in_dataset_qualified_moves() {
    let current = ContainerTree::new(vec![
        container(0, "sample", ScopeLevel::PackageGroup, None),
        container(1, "area", ScopeLevel::Domain, Some(0)),
        container(2, "area/origin", ScopeLevel::Folder, Some(1)),
        container(3, "area/destination", ScopeLevel::Folder, Some(1)),
        container(4, "src/area/origin/item.ts", ScopeLevel::File, Some(2)),
        container(
            5,
            "spec/area/origin/item.spec.ts",
            ScopeLevel::File,
            Some(2),
        ),
        container(
            6,
            "src/area/destination/anchor.ts",
            ScopeLevel::File,
            Some(3),
        ),
        container(
            7,
            "spec/area/destination/anchor.spec.ts",
            ScopeLevel::File,
            Some(3),
        ),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "sample", ScopeLevel::PackageGroup, None),
        container(1, "area", ScopeLevel::Domain, Some(0)),
        container(2, "area/origin", ScopeLevel::Folder, Some(1)),
        container(3, "area/destination", ScopeLevel::Folder, Some(1)),
        container(4, "src/area/origin/item.ts", ScopeLevel::File, Some(3)),
        container(
            5,
            "spec/area/origin/item.spec.ts",
            ScopeLevel::File,
            Some(3),
        ),
        container(
            6,
            "src/area/destination/anchor.ts",
            ScopeLevel::File,
            Some(3),
        ),
        container(
            7,
            "spec/area/destination/anchor.spec.ts",
            ScopeLevel::File,
            Some(3),
        ),
    ]);

    let moves = narrate(&current, &candidate, &plain_facts());

    assert_eq!(moves.len(), 2, "physical namespaces narrate separately");
    let physical_moves: Vec<(String, String, String)> = moves
        .iter()
        .filter_map(|entry| {
            entry
                .files
                .first()
                .map(|file| (file.path.clone(), file.from.clone(), entry.to.clone()))
        })
        .collect();
    assert_eq!(
        physical_moves,
        vec![
            (
                "sample/spec/area/origin/item.spec.ts".to_owned(),
                "sample/spec/area/origin".to_owned(),
                "sample/spec/area/destination".to_owned(),
            ),
            (
                "sample/src/area/origin/item.ts".to_owned(),
                "sample/src/area/origin".to_owned(),
                "sample/src/area/destination".to_owned(),
            ),
        ]
    );
}

#[test]
fn should_split_a_real_directory_into_its_package_relative_namespace() {
    // a nested package file under a see-through root, one directly in it,
    // a single-package file, and a file directly in its package.
    let cases = [
        (
            strings(&["crates", "core", "src", "cluster"]),
            strings(&["crates", "core"]),
            strings(&["crates", "core", "cluster"]),
            strings(&["src"]),
        ),
        (
            strings(&["crates", "core", "src"]),
            strings(&["crates", "core"]),
            strings(&["crates", "core"]),
            strings(&["src"]),
        ),
        (
            strings(&["src", "geometry"]),
            Vec::new(),
            strings(&["geometry"]),
            strings(&["src"]),
        ),
        (
            strings(&["crates", "core"]),
            strings(&["crates", "core"]),
            strings(&["crates", "core"]),
            Vec::new(),
        ),
    ];

    for (directory, package, folder, namespace) in cases {
        assert_eq!(
            physical_namespace(&directory, &package, &folder),
            namespace,
            "namespace of {directory:?}"
        );
        assert_eq!(
            physical_directory(&package, &namespace, &folder),
            directory,
            "composing the namespace back restores {directory:?}"
        );
    }
}

#[test]
fn should_compose_a_destination_in_another_package_below_its_namespace() {
    let directory = physical_directory(
        &strings(&["crates", "engine"]),
        &strings(&["src"]),
        &strings(&["crates", "engine", "analyze"]),
    );

    assert_eq!(directory, strings(&["crates", "engine", "src", "analyze"]));
}

/// A multi-package repository: `crates/core` holds a file directly in its
/// see-through `src` root (the synthetic bucket) and one in `src/cluster`;
/// `crates/engine` holds `src/analyze`.
fn multi_package_tree(placements: [(u32, &str); 2]) -> ContainerTree {
    let mut containers = vec![
        container(0, "strata", ScopeLevel::PackageGroup, None),
        container(1, "crates/core", ScopeLevel::Package, Some(0)),
        container(2, "crates/engine", ScopeLevel::Package, Some(0)),
        Container {
            synthetic: true,
            ..container(3, "crates/core/workspace", ScopeLevel::Folder, Some(1))
        },
        container(4, "crates/core/cluster", ScopeLevel::Folder, Some(1)),
        container(5, "crates/engine/analyze", ScopeLevel::Folder, Some(2)),
        container(6, "crates/core/fresh", ScopeLevel::Folder, Some(1)),
        container(
            9,
            "crates/engine/src/analyze/z.rs",
            ScopeLevel::File,
            Some(5),
        ),
    ];
    let [(lib_folder, lib), (refine_folder, refine)] = placements;
    containers.push(container(7, lib, ScopeLevel::File, Some(lib_folder)));
    containers.push(container(8, refine, ScopeLevel::File, Some(refine_folder)));
    containers.sort_by_key(|container| container.id);
    ContainerTree::new(containers)
}

#[test]
fn should_narrate_real_paths_for_files_under_a_nested_package_source_root() {
    // ADR-18: the package path must precede the see-through `src`, never
    // follow it (`strata/crates/core/src/crates/core`).
    let current = multi_package_tree([
        (3, "crates/core/src/lib.rs"),
        (4, "crates/core/src/cluster/refine.rs"),
    ]);
    let candidate = multi_package_tree([
        (5, "crates/core/src/lib.rs"),
        (6, "crates/core/src/cluster/refine.rs"),
    ]);

    let moves = narrate_repository_relative(&current, &candidate, &plain_facts());
    let narrated: Vec<(String, String, String)> = moves
        .iter()
        .flat_map(|entry| {
            entry
                .files
                .iter()
                .map(|file| (file.path.clone(), file.from.clone(), entry.to.clone()))
        })
        .collect();

    assert_eq!(
        narrated,
        vec![
            (
                "strata/crates/core/src/cluster/refine.rs".to_owned(),
                "strata/crates/core/src/cluster".to_owned(),
                "strata/crates/core/src/fresh".to_owned(),
            ),
            (
                "strata/crates/core/src/lib.rs".to_owned(),
                "strata/crates/core/src".to_owned(),
                "strata/crates/engine/src/analyze".to_owned(),
            ),
        ]
    );
}

#[test]
fn should_not_carry_a_source_root_into_a_package_that_has_no_manifest() {
    // ADR-18 with the package wall down: clusters elected under `crates`
    // (no manifest) must not print `crates/src/...`. Each cluster here
    // holds files of a single package, so nothing changes package: the
    // lone `lib.rs` stays in `crates/core/src` and `refine.rs` stays in its
    // own `cluster` folder.
    let current = multi_package_tree([
        (3, "crates/core/src/lib.rs"),
        (4, "crates/core/src/cluster/refine.rs"),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "strata", ScopeLevel::PackageGroup, None),
        container(1, "crates", ScopeLevel::Package, Some(0)),
        container(2, "crates/engine", ScopeLevel::Package, Some(0)),
        Container {
            synthetic: true,
            ..container(3, "crates/workspace", ScopeLevel::Folder, Some(1))
        },
        container(4, "crates/core/cluster", ScopeLevel::Folder, Some(1)),
        container(5, "crates/engine/analyze", ScopeLevel::Folder, Some(2)),
        container(7, "crates/core/src/lib.rs", ScopeLevel::File, Some(3)),
        container(
            8,
            "crates/core/src/cluster/refine.rs",
            ScopeLevel::File,
            Some(4),
        ),
        container(
            9,
            "crates/engine/src/analyze/z.rs",
            ScopeLevel::File,
            Some(5),
        ),
    ]);

    let moves = narrate_repository_relative(&current, &candidate, &plain_facts());
    let narrated: Vec<(String, String, String)> = moves
        .iter()
        .flat_map(|entry| {
            entry
                .files
                .iter()
                .map(|file| (file.path.clone(), file.from.clone(), entry.to.clone()))
        })
        .collect();

    assert_eq!(narrated, Vec::<(String, String, String)>::new());
}

#[test]
fn should_keep_a_lone_file_in_its_package_when_upper_levels_merge_packages() {
    // ADR-17: the display-only package level elected `crates/core` above
    // util's untouched `lib.rs`; its cluster holds only util files, so it
    // stays in `crates/util/src` and never lands on core's `lib.rs`.
    let current = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "crates/core", ScopeLevel::Package, Some(0)),
        container(2, "crates/util", ScopeLevel::Package, Some(0)),
        Container {
            synthetic: true,
            ..container(3, "crates/core/workspace", ScopeLevel::Folder, Some(1))
        },
        container(4, "crates/core/beta", ScopeLevel::Folder, Some(1)),
        Container {
            synthetic: true,
            ..container(5, "crates/util/workspace", ScopeLevel::Folder, Some(2))
        },
        container(6, "crates/core/src/lib.rs", ScopeLevel::File, Some(3)),
        container(
            7,
            "crates/core/src/beta/scale.rs",
            ScopeLevel::File,
            Some(4),
        ),
        container(8, "crates/util/src/lib.rs", ScopeLevel::File, Some(5)),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "crates/core", ScopeLevel::Package, Some(0)),
        Container {
            synthetic: true,
            ..container(2, "crates/core/workspace", ScopeLevel::Folder, Some(1))
        },
        container(3, "crates/core/alpha", ScopeLevel::Folder, Some(1)),
        Container {
            synthetic: true,
            ..container(4, "crates/core/workspace", ScopeLevel::Folder, Some(1))
        },
        container(5, "crates/core/src/lib.rs", ScopeLevel::File, Some(2)),
        container(
            6,
            "crates/core/src/beta/scale.rs",
            ScopeLevel::File,
            Some(3),
        ),
        container(7, "crates/util/src/lib.rs", ScopeLevel::File, Some(4)),
    ]);

    let moves = narrate_repository_relative(&current, &candidate, &plain_facts());
    let narrated: Vec<(String, String, String)> = moves
        .iter()
        .flat_map(|entry| {
            entry
                .files
                .iter()
                .map(|file| (file.path.clone(), file.from.clone(), entry.to.clone()))
        })
        .collect();

    assert_eq!(
        narrated,
        vec![(
            "ws/crates/core/src/beta/scale.rs".to_owned(),
            "ws/crates/core/src/beta".to_owned(),
            "ws/crates/core/src/alpha".to_owned(),
        )]
    );
}

#[test]
fn should_order_nested_package_and_transparent_roots_once_in_narration() {
    let current = ContainerTree::new(vec![
        container(0, "workspace/packages/unit", ScopeLevel::PackageGroup, None),
        container(1, "area", ScopeLevel::Folder, Some(0)),
        container(2, "target", ScopeLevel::Folder, Some(0)),
        container(
            3,
            "packages/unit/left/area/item.ts",
            ScopeLevel::File,
            Some(1),
        ),
        container(
            4,
            "packages/unit/left/target/anchor.ts",
            ScopeLevel::File,
            Some(2),
        ),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "workspace/packages/unit", ScopeLevel::PackageGroup, None),
        container(1, "area", ScopeLevel::Folder, Some(0)),
        container(2, "target", ScopeLevel::Folder, Some(0)),
        container(
            3,
            "packages/unit/left/area/item.ts",
            ScopeLevel::File,
            Some(2),
        ),
        container(
            4,
            "packages/unit/left/target/anchor.ts",
            ScopeLevel::File,
            Some(2),
        ),
    ]);

    let moves = narrate(&current, &candidate, &plain_facts());
    let moved = moves.first().and_then(|entry| {
        entry
            .files
            .first()
            .map(|file| (file.path.as_str(), file.from.as_str(), entry.to.as_str()))
    });

    assert_eq!(
        moved,
        Some((
            "workspace/packages/unit/left/area/item.ts",
            "workspace/packages/unit/left/area",
            "workspace/packages/unit/left/target",
        ))
    );
}

#[test]
fn should_classify_a_two_source_destination_as_a_merge() {
    // one file from each folder converges on a third folder.
    let current = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/core", ScopeLevel::Folder, Some(0)),
        container(2, "src/io", ScopeLevel::Folder, Some(0)),
        container(3, "src/merged", ScopeLevel::Folder, Some(0)),
        container(4, "src/core/a.ts", ScopeLevel::File, Some(1)),
        container(5, "src/io/b.ts", ScopeLevel::File, Some(2)),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/core", ScopeLevel::Folder, Some(0)),
        container(2, "src/io", ScopeLevel::Folder, Some(0)),
        container(3, "src/merged", ScopeLevel::Folder, Some(0)),
        container(4, "src/core/a.ts", ScopeLevel::File, Some(3)),
        container(5, "src/io/b.ts", ScopeLevel::File, Some(3)),
    ]);

    let moves = narrate(&current, &candidate, &plain_facts());

    assert_eq!(moves.len(), 1);
    let entry = moves.first();
    assert_eq!(entry.map(|m| m.kind), Some(MoveKind::Merge));
    // per-file sources are retained: a.ts leaves core, b.ts leaves io.
    let mut origins = entry
        .map(|m| m.files.iter().map(|f| f.from.clone()).collect::<Vec<_>>())
        .unwrap_or_default();
    origins.sort();
    assert_eq!(origins, strings(&["src/core", "src/io"]));
}

#[test]
fn should_classify_a_scattering_source_as_a_split() {
    // both files leave `src/core` for two different folders.
    let current = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/core", ScopeLevel::Folder, Some(0)),
        container(2, "src/io", ScopeLevel::Folder, Some(0)),
        container(3, "src/net", ScopeLevel::Folder, Some(0)),
        container(4, "src/core/a.ts", ScopeLevel::File, Some(1)),
        container(5, "src/core/b.ts", ScopeLevel::File, Some(1)),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/core", ScopeLevel::Folder, Some(0)),
        container(2, "src/io", ScopeLevel::Folder, Some(0)),
        container(3, "src/net", ScopeLevel::Folder, Some(0)),
        container(4, "src/core/a.ts", ScopeLevel::File, Some(2)),
        container(5, "src/core/b.ts", ScopeLevel::File, Some(3)),
    ]);

    let moves = narrate(&current, &candidate, &plain_facts());

    assert_eq!(moves.len(), 2);
    assert!(moves.iter().all(|entry| entry.kind == MoveKind::Split));
}

#[test]
fn should_follow_a_spec_files_subject_into_its_folder() {
    // the spec starts in io, its subject lives in core; the candidate brings
    // the spec to the subject.
    let current = two_folder_tree(&["src/core/app.ts"], &["src/io/app.spec.ts"]);
    let candidate = two_folder_tree(&["src/core/app.ts", "src/io/app.spec.ts"], &[]);
    let facts = FileFacts {
        edge_weights: [(
            (
                "src/io/app.spec.ts".to_owned(),
                "src/core/app.ts".to_owned(),
            ),
            2.0,
        )]
        .into_iter()
        .collect(),
        test_case_files: ["src/io/app.spec.ts".to_owned()].into_iter().collect(),
        shadow_test_files: BTreeSet::new(),
        folder_cap: 15,
    };

    let moves = narrate(&current, &candidate, &facts);

    assert_eq!(moves.len(), 1);
    let entry = moves.first();
    assert_eq!(
        entry.map(|m| m.reason.clone()),
        Some(MoveReason::Follows {
            subject: "src/core/app.ts".to_owned(),
        })
    );
    assert_eq!(
        entry.map(|m| m.reason.to_string()),
        Some("follows app.ts".to_owned())
    );
}

#[test]
fn should_narrate_a_co_moved_spec_as_following_its_twin() {
    // Subject and spec start together in `src/core`; the candidate moves
    // both into the new nested folder `src/core/app`. Each role narrates on
    // its own: the twin is pulled by its spec, the spec follows back.
    let start = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/core", ScopeLevel::Folder, Some(0)),
        container(2, "src/io", ScopeLevel::Folder, Some(0)),
        container(3, "src/core/app.ts", ScopeLevel::File, Some(1)),
        container(4, "src/core/app.spec.ts", ScopeLevel::File, Some(1)),
        container(5, "src/io/keep.ts", ScopeLevel::File, Some(2)),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/core", ScopeLevel::Folder, Some(0)),
        container(2, "src/io", ScopeLevel::Folder, Some(0)),
        container(3, "src/core/app", ScopeLevel::Folder, Some(1)),
        container(4, "src/core/app.ts", ScopeLevel::File, Some(3)),
        container(5, "src/core/app.spec.ts", ScopeLevel::File, Some(3)),
        container(6, "src/io/keep.ts", ScopeLevel::File, Some(2)),
    ]);
    let facts = FileFacts {
        edge_weights: [(
            (
                "src/core/app.spec.ts".to_owned(),
                "src/core/app.ts".to_owned(),
            ),
            2.0,
        )]
        .into_iter()
        .collect(),
        test_case_files: ["src/core/app.spec.ts".to_owned()].into_iter().collect(),
        shadow_test_files: BTreeSet::new(),
        folder_cap: 15,
    };

    let moves = narrate(&start, &candidate, &facts);

    assert_eq!(moves.len(), 2, "each role narrates as its own entry");
    let followed = moves.iter().find(|entry| {
        entry
            .files
            .iter()
            .any(|file| file.path.ends_with(".spec.ts"))
    });
    assert_eq!(
        followed.map(|entry| entry.reason.to_string()),
        Some("follows app.ts".to_owned())
    );
}

#[test]
fn should_narrate_a_shadow_test_file_as_following_its_subject() {
    // a `[tests]`-pattern match that is not case-only polarity — a test
    // support file — narrates exactly like a spec: it follows its subject.
    let current = two_folder_tree(&["src/core/app.ts"], &["src/io/app.test-utils.ts"]);
    let candidate = two_folder_tree(&["src/core/app.ts", "src/io/app.test-utils.ts"], &[]);
    let facts = FileFacts {
        // narration reads the uncut IR weights, not the tie-cut graph.
        edge_weights: [(
            (
                "src/io/app.test-utils.ts".to_owned(),
                "src/core/app.ts".to_owned(),
            ),
            2.0,
        )]
        .into_iter()
        .collect(),
        test_case_files: BTreeSet::new(),
        shadow_test_files: ["src/io/app.test-utils.ts".to_owned()]
            .into_iter()
            .collect(),
        folder_cap: 15,
    };

    let moves = narrate(&current, &candidate, &facts);

    assert_eq!(moves.len(), 1);
    assert_eq!(
        moves.first().map(|m| m.reason.clone()),
        Some(MoveReason::Follows {
            subject: "src/core/app.ts".to_owned(),
        })
    );
}

#[test]
fn should_not_move_a_spec_when_only_its_domain_label_is_regrouped() {
    // the spec's real folder (`nested-ts/__tests__`) is unchanged; only the
    // domain node above it is relabelled as the clusterer regroups the folder
    // to display beside its subject's domain. A domain label is a display
    // overlay, never a path component, so the spec does not move on disk —
    // the regrouping shows as a heading, it is not narrated as a file move.
    let current = ContainerTree::new(vec![
        container(0, "nested-ts", ScopeLevel::Domain, None),
        container(1, "nested-ts/geometry", ScopeLevel::Folder, Some(0)),
        container(2, "nested-ts/__tests__", ScopeLevel::Folder, Some(0)),
        container(3, "app.ts", ScopeLevel::File, Some(1)),
        container(4, "app.spec.ts", ScopeLevel::File, Some(2)),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "nested-ts/geometry", ScopeLevel::Domain, None),
        container(1, "nested-ts/geometry", ScopeLevel::Folder, Some(0)),
        container(2, "nested-ts/__tests__", ScopeLevel::Folder, Some(0)),
        container(3, "app.ts", ScopeLevel::File, Some(1)),
        container(4, "app.spec.ts", ScopeLevel::File, Some(2)),
    ]);
    let facts = FileFacts {
        edge_weights: [(("app.spec.ts".to_owned(), "app.ts".to_owned()), 2.0)]
            .into_iter()
            .collect(),
        test_case_files: ["app.spec.ts".to_owned()].into_iter().collect(),
        shadow_test_files: BTreeSet::new(),
        folder_cap: 15,
    };

    let moves = narrate(&current, &candidate, &facts);

    assert!(moves.is_empty());
}

#[test]
fn should_explain_a_move_out_of_an_over_cap_folder() {
    let core_files: Vec<String> = (0..4).map(|i| format!("src/core/f{i}.ts")).collect();
    let core_refs: Vec<&str> = core_files.iter().map(String::as_str).collect();
    let current = two_folder_tree(&core_refs, &[]);
    // the first file relocates to io; core held 4 files against a cap of 3.
    let mut moved_core = core_refs.clone();
    let mover = moved_core.remove(0);
    let candidate = two_folder_tree(&moved_core, &[mover]);
    let facts = FileFacts {
        edge_weights: BTreeMap::new(),
        test_case_files: BTreeSet::new(),
        shadow_test_files: BTreeSet::new(),
        folder_cap: 3,
    };

    let moves = narrate(&current, &candidate, &facts);

    assert_eq!(
        moves.first().map(|m| m.reason.clone()),
        Some(MoveReason::RelievesOverCap {
            container: "src/core".to_owned(),
            count: 4,
            cap: 3,
        })
    );
    assert_eq!(
        moves.first().map(|m| m.reason.to_string()),
        Some("relieves over-cap folder src/core (4/3 entries)".to_owned())
    );
}

#[test]
fn should_attribute_cap_relief_to_the_dominant_over_cap_origin() {
    // two over-cap folders merge into a third; `src/zebra` sheds three files
    // while `src/alpha` sheds one. Attribution must name zebra — the dominant
    // contributor — not alpha, which sorts first but barely features.
    let current = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/alpha", ScopeLevel::Folder, Some(0)),
        container(2, "src/zebra", ScopeLevel::Folder, Some(0)),
        container(3, "src/dest", ScopeLevel::Folder, Some(0)),
        container(4, "src/alpha/a0.ts", ScopeLevel::File, Some(1)),
        container(5, "src/alpha/a1.ts", ScopeLevel::File, Some(1)),
        container(6, "src/alpha/a2.ts", ScopeLevel::File, Some(1)),
        container(7, "src/alpha/a3.ts", ScopeLevel::File, Some(1)),
        container(8, "src/zebra/z0.ts", ScopeLevel::File, Some(2)),
        container(9, "src/zebra/z1.ts", ScopeLevel::File, Some(2)),
        container(10, "src/zebra/z2.ts", ScopeLevel::File, Some(2)),
        container(11, "src/zebra/z3.ts", ScopeLevel::File, Some(2)),
    ]);
    let candidate = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::Domain, None),
        container(1, "src/alpha", ScopeLevel::Folder, Some(0)),
        container(2, "src/zebra", ScopeLevel::Folder, Some(0)),
        container(3, "src/dest", ScopeLevel::Folder, Some(0)),
        container(4, "src/alpha/a0.ts", ScopeLevel::File, Some(3)),
        container(5, "src/alpha/a1.ts", ScopeLevel::File, Some(1)),
        container(6, "src/alpha/a2.ts", ScopeLevel::File, Some(1)),
        container(7, "src/alpha/a3.ts", ScopeLevel::File, Some(1)),
        container(8, "src/zebra/z0.ts", ScopeLevel::File, Some(3)),
        container(9, "src/zebra/z1.ts", ScopeLevel::File, Some(3)),
        container(10, "src/zebra/z2.ts", ScopeLevel::File, Some(3)),
        container(11, "src/zebra/z3.ts", ScopeLevel::File, Some(2)),
    ]);
    let facts = FileFacts {
        edge_weights: BTreeMap::new(),
        test_case_files: BTreeSet::new(),
        shadow_test_files: BTreeSet::new(),
        folder_cap: 3,
    };

    let moves = narrate(&current, &candidate, &facts);

    assert_eq!(
        moves.first().map(|m| m.reason.clone()),
        Some(MoveReason::RelievesOverCap {
            container: "src/zebra".to_owned(),
            count: 4,
            cap: 3,
        })
    );
}

#[test]
fn should_explain_a_move_by_its_strongest_dependency_pull() {
    let current = two_folder_tree(&["src/core/engine.ts"], &["src/io/mover.ts"]);
    let candidate = two_folder_tree(&["src/core/engine.ts", "src/io/mover.ts"], &[]);
    let facts = FileFacts {
        edge_weights: [
            (
                (
                    "src/io/mover.ts".to_owned(),
                    "src/core/engine.ts".to_owned(),
                ),
                1.5,
            ),
            (
                (
                    "src/core/engine.ts".to_owned(),
                    "src/io/mover.ts".to_owned(),
                ),
                1.0,
            ),
        ]
        .into_iter()
        .collect(),
        test_case_files: BTreeSet::new(),
        shadow_test_files: BTreeSet::new(),
        folder_cap: 15,
    };

    let moves = narrate(&current, &candidate, &facts);

    assert!(matches!(
        moves.first().map(|m| &m.reason),
        Some(MoveReason::PulledBy { partner, .. }) if partner == "src/core/engine.ts"
    ));
    assert_eq!(
        moves.first().map(|m| m.reason.to_string()),
        Some("pulled by engine.ts (w 2.5)".to_owned())
    );
}

#[test]
fn should_explain_a_move_by_naming_cohesion_with_the_destination() {
    // no edges; {user, service} vs {user, service, api} → Jaccard 2/3.
    let current = two_folder_tree(
        &["src/core/user-service-api.ts"],
        &["src/io/user-service.ts"],
    );
    let candidate = two_folder_tree(
        &["src/core/user-service-api.ts", "src/io/user-service.ts"],
        &[],
    );

    let moves = narrate(&current, &candidate, &plain_facts());

    assert!(matches!(
        moves.first().map(|m| &m.reason),
        Some(MoveReason::NamingCohesion { .. })
    ));
    assert_eq!(
        moves.first().map(|m| m.reason.to_string()),
        Some("naming cohesion 0.67 with destination".to_owned())
    );
}

#[test]
fn should_fall_back_to_the_clustering_reason() {
    let current = two_folder_tree(&["src/core/alpha.ts"], &["src/io/zulu.ts"]);
    let candidate = two_folder_tree(&["src/core/alpha.ts", "src/io/zulu.ts"], &[]);

    let moves = narrate(&current, &candidate, &plain_facts());

    assert_eq!(
        moves.first().map(|m| m.reason.clone()),
        Some(MoveReason::Clustering)
    );
    assert_eq!(
        moves.first().map(|m| m.reason.to_string()),
        Some("regrouped by clustering".to_owned())
    );
}

#[test]
fn should_narrate_nothing_for_identical_trees() {
    let tree = two_folder_tree(&["src/core/a.ts"], &["src/io/b.ts"]);

    let moves = narrate(&tree, &tree, &plain_facts());

    assert!(moves.is_empty());
}
