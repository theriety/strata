#![allow(clippy::assertions_on_constants)]

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_ir::{ContainerTree, Polarity, ScopeLevel};

use crate::config::AnalyzeConfig;
use crate::result::{ContainerNode, Level};

use super::*;
use crate::analyze::test_support::*;
use crate::analyze::*;

#[test]
fn should_collapse_the_synthetic_workspace_bucket_under_the_package() {
    // a root-level file hangs off a synthetic `workspace` domain+folder
    // bucket so the internal tree stays strictly level-ascending. The bucket
    // names no real directory, so the DTO collapses it: the file renders
    // directly under its package, with no `workspace` domain or folder node.
    let tree = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "crates/app", ScopeLevel::Package, Some(0)),
        synthetic_container(2, "crates/app/workspace", ScopeLevel::Domain, Some(1)),
        synthetic_container(3, "crates/app/workspace", ScopeLevel::Folder, Some(2)),
        container(4, "crates/app/src/lib.rs", ScopeLevel::File, Some(3)),
    ]);

    let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

    let package = rendered.and_then(only_child);
    let children = package.and_then(|node| node.children).unwrap_or_default();
    let shape: Vec<(&str, Level)> = children
        .iter()
        .map(|node| (node.name.as_str(), node.level))
        .collect();
    assert_eq!(shape, vec![("crates/app/src/lib.rs", Level::File)]);
}

#[test]
fn should_render_root_files_beside_real_folders_when_a_package_has_both() {
    // a package holding both root-level files and a real sub-folder renders
    // the root files as siblings of the real folder directly under the
    // package -- the collapsed synthetic bucket never wraps them in a
    // phantom `workspace` level.
    let tree = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "crates/app", ScopeLevel::Package, Some(0)),
        synthetic_container(2, "crates/app/workspace", ScopeLevel::Domain, Some(1)),
        synthetic_container(3, "crates/app/workspace", ScopeLevel::Folder, Some(2)),
        container(4, "crates/app/src/lib.rs", ScopeLevel::File, Some(3)),
        container(5, "crates/app/io", ScopeLevel::Domain, Some(1)),
        container(6, "crates/app/io", ScopeLevel::Folder, Some(5)),
        container(7, "crates/app/src/io/read.rs", ScopeLevel::File, Some(6)),
    ]);

    let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

    let package = rendered.and_then(only_child);
    let children = package.and_then(|node| node.children).unwrap_or_default();
    let names: Vec<&str> = children.iter().map(|node| node.name.as_str()).collect();
    assert!(
        names.contains(&"crates/app/src/lib.rs"),
        "the root file must sit directly under the package, got {names:?}"
    );
    assert!(
        children.iter().any(|node| node.level == Level::Domain),
        "the real folder's domain must remain, got {names:?}"
    );
    assert!(
        !names.contains(&"workspace"),
        "no synthetic workspace node may survive, got {names:?}"
    );
}

/// Builds a config with the file cap set to `cap`.
#[test]
fn should_nest_a_multi_segment_folder_into_a_directory_chain() {
    // folders are reality: a real directory foreign to its domain renders
    // as one nested folder node per path segment, deepest holding the
    // files, never as a single slash-named node.
    let tree = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "ws", ScopeLevel::Package, Some(0)),
        container(2, "shared", ScopeLevel::Domain, Some(1)),
        container(3, "google/capabilities", ScopeLevel::Folder, Some(2)),
        container(4, "src/google/capabilities/a.ts", ScopeLevel::File, Some(3)),
    ]);

    let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

    let google = rendered
        .and_then(only_child)
        .and_then(only_child)
        .and_then(only_child);
    assert_eq!(
        google.as_ref().map(|node| (node.name.as_str(), node.level)),
        Some(("google", Level::Folder))
    );
    let capabilities = google.and_then(only_child);
    assert_eq!(
        capabilities
            .as_ref()
            .map(|node| (node.name.as_str(), node.level)),
        Some(("capabilities", Level::Folder))
    );
    let files: Vec<String> = capabilities
        .and_then(|node| node.children)
        .unwrap_or_default()
        .into_iter()
        .map(|child| child.name)
        .collect();
    assert_eq!(files, vec!["src/google/capabilities/a.ts".to_owned()]);
}

#[test]
fn should_render_a_slash_named_domain_as_a_single_node() {
    // domains are the suggestion: an elected join like `d1/d2` is a label,
    // not a directory, so it renders whole and never trie-splits.
    let tree = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "ws", ScopeLevel::Package, Some(0)),
        container(2, "d1/d2", ScopeLevel::Domain, Some(1)),
        container(3, "d1/d2/x", ScopeLevel::Folder, Some(2)),
        container(4, "src/d1/x/a.ts", ScopeLevel::File, Some(3)),
    ]);

    let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

    let domain = rendered.and_then(only_child).and_then(only_child);
    assert_eq!(
        domain.as_ref().map(|node| (node.name.as_str(), node.level)),
        Some(("d1/d2", Level::Domain))
    );
    let folder_names: Vec<String> = domain
        .and_then(|node| node.children)
        .unwrap_or_default()
        .into_iter()
        .map(|child| child.name)
        .collect();
    assert_eq!(folder_names, vec!["x".to_owned()]);
}

#[test]
fn should_not_fabricate_folders_under_a_disambiguated_domain() {
    // a merged domain elects the foreign prefix `cts` under package `pkg`,
    // and `qualify_elected` decorated its display as `cts (cts.core)`. The
    // decorated label is not a prefix of the folder key `cts/core`, so
    // stripping it would leave the whole key to re-embed as a fabricated
    // `cts` → `core` chain. Stripping the domain's real key `cts` (from
    // `key_by_id`) renders the one real directory `core`.
    let tree = ContainerTree::new(vec![
        container(0, "pkg", ScopeLevel::PackageGroup, None),
        container(1, "pkg", ScopeLevel::Package, Some(0)),
        container(2, "cts (cts.core)", ScopeLevel::Domain, Some(1)),
        container(3, "cts/core", ScopeLevel::Folder, Some(2)),
        container(4, "src/core/a.ts", ScopeLevel::File, Some(3)),
    ]);
    let key_by_id: BTreeMap<u32, SmolStr> = [(2, SmolStr::new("cts"))].into_iter().collect();

    let rendered = render_tree(&tree, &[], &|_| None, &key_by_id).ok();

    let domain = rendered.and_then(only_child).and_then(only_child);
    assert_eq!(
        domain.as_ref().map(|node| (node.name.as_str(), node.level)),
        Some(("cts (cts.core)", Level::Domain))
    );
    let folder_names: Vec<String> = domain
        .and_then(|node| node.children)
        .unwrap_or_default()
        .into_iter()
        .map(|child| child.name)
        .collect();
    assert_eq!(folder_names, vec!["core".to_owned()]);
}

#[test]
fn should_suppress_a_domain_whose_key_repeats_its_package_key() {
    // two merged domains both elect the bare package name `cts`; the
    // decorated displays differ (`cts (cts.core)`) but the undecorated key
    // behind each (from `key_by_id`) repeats the package's key, so the
    // level carries no naming information — it is suppressed at the render
    // boundary and the real folders hang directly under the package.
    let tree = ContainerTree::new(vec![
        container(0, "cts", ScopeLevel::PackageGroup, None),
        container(1, "cts", ScopeLevel::Package, Some(0)),
        container(2, "cts (cts.core)", ScopeLevel::Domain, Some(1)),
        container(3, "cts/core", ScopeLevel::Folder, Some(2)),
        container(4, "src/core/a.ts", ScopeLevel::File, Some(3)),
        container(5, "cts (cts.io)", ScopeLevel::Domain, Some(1)),
        container(6, "cts/io", ScopeLevel::Folder, Some(5)),
        container(7, "src/io/b.ts", ScopeLevel::File, Some(6)),
    ]);
    let key_by_id: BTreeMap<u32, SmolStr> = [(2, SmolStr::new("cts")), (5, SmolStr::new("cts"))]
        .into_iter()
        .collect();

    let rendered = render_tree(&tree, &[], &|_| None, &key_by_id).ok();

    let package = rendered.and_then(only_child);
    assert_eq!(
        package
            .as_ref()
            .map(|node| (node.name.as_str(), node.level)),
        Some(("cts", Level::Package))
    );
    let children: Vec<(String, Level)> = package
        .and_then(|node| node.children)
        .unwrap_or_default()
        .into_iter()
        .map(|child| (child.name, child.level))
        .collect();
    assert_eq!(
        children,
        vec![
            ("core".to_owned(), Level::Folder),
            ("io".to_owned(), Level::Folder),
        ]
    );
}

#[test]
fn should_render_a_cross_domain_folder_relative_to_its_package() {
    // `atlas/agent` is a real directory clustered into the sibling domain
    // keyed `atlas/core`: its key extends neither the domain key nor the
    // decorated display, so the old whole-key fallback re-embedded the
    // package segment as a fabricated `atlas` → `agent` chain (no `atlas`
    // subdirectory exists under any real `core`). The folder must display
    // relative to its own package: `agent`, directly under the domain.
    let tree = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "atlas", ScopeLevel::Package, Some(0)),
        container(2, "atlas/core", ScopeLevel::Domain, Some(1)),
        container(3, "atlas/agent", ScopeLevel::Folder, Some(2)),
        container(4, "atlas/src/agent/loop.ts", ScopeLevel::File, Some(3)),
    ]);

    let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

    let domain = rendered.and_then(only_child).and_then(only_child);
    assert_eq!(
        domain.as_ref().map(|node| (node.name.as_str(), node.level)),
        Some(("core", Level::Domain))
    );
    let folder = domain.and_then(only_child);
    assert_eq!(
        folder.as_ref().map(|node| (node.name.as_str(), node.level)),
        Some(("agent", Level::Folder))
    );
}

#[test]
fn should_merge_sibling_folders_sharing_a_parent_directory() {
    // two real directories under one parent (`deep/x`, `deep/y`) render
    // as a single `deep` trie holding two subdirectories, never as
    // duplicate `deep` siblings.
    let tree = ContainerTree::new(vec![
        container(0, "ws", ScopeLevel::PackageGroup, None),
        container(1, "ws", ScopeLevel::Package, Some(0)),
        container(2, "shared", ScopeLevel::Domain, Some(1)),
        container(3, "deep/x", ScopeLevel::Folder, Some(2)),
        container(4, "src/deep/x/a.ts", ScopeLevel::File, Some(3)),
        container(5, "deep/y", ScopeLevel::Folder, Some(2)),
        container(6, "src/deep/y/b.ts", ScopeLevel::File, Some(5)),
    ]);

    let rendered = render_tree(&tree, &[], &|_| None, &BTreeMap::new()).ok();

    let deep = rendered
        .and_then(only_child)
        .and_then(only_child)
        .and_then(only_child);
    assert_eq!(deep.as_ref().map(|node| node.name.as_str()), Some("deep"));
    let subdirs: Vec<(String, Vec<String>)> = deep
        .and_then(|node| node.children)
        .unwrap_or_default()
        .into_iter()
        .map(|child| {
            let files = child
                .children
                .iter()
                .flatten()
                .map(|file| file.name.clone())
                .collect();
            (child.name, files)
        })
        .collect();
    assert_eq!(
        subdirs,
        vec![
            ("x".to_owned(), vec!["src/deep/x/a.ts".to_owned()]),
            ("y".to_owned(), vec!["src/deep/y/b.ts".to_owned()]),
        ]
    );
}

/// Builds a production symbol of `sloc` owning `container`.
#[test]
fn should_render_a_nested_tree_with_file_symbols() {
    let snapshot = snapshot(
        vec![node(0, "sym", 1, Polarity::Production)],
        vec![],
        vec![
            container(0, "pkg", ScopeLevel::Package, None),
            container(1, "file", ScopeLevel::File, Some(0)),
        ],
    );

    let result = analyze(&snapshot, &AnalyzeConfig::default());
    let tree = result.map(|result| result.current.tree);

    let root = tree.unwrap_or_else(|_| ContainerNode {
        name: String::new(),
        level: Level::File,
        children: None,
        symbols: None,
        production_sloc: None,
    });
    assert_eq!(root.level, Level::Package);
    let file = root
        .children
        .and_then(|children| children.into_iter().next());
    assert_eq!(file.and_then(|file| file.production_sloc), Some(1));
}

#[test]
fn should_render_a_multi_root_forest_under_a_synthetic_group() {
    let snapshot = snapshot(
        vec![],
        vec![],
        vec![
            container(0, "a", ScopeLevel::Package, None),
            container(1, "b", ScopeLevel::Package, None),
        ],
    );

    let result = analyze(&snapshot, &AnalyzeConfig::default());
    let level = result.map_or(Level::File, |result| result.current.tree.level);

    assert_eq!(level, Level::PackageGroup);
}
