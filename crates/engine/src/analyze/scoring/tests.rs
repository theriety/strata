#![allow(clippy::assertions_on_constants)]

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_ir::{
    Container, ContainerId, ContainerTree, Layout, Node, ScopeLevel, build_laminar_tree,
};

use super::*;
use crate::analyze::test_support::*;

#[test]
fn should_measure_full_path_cohesion_for_the_unchanged_laminar_layout() {
    // `cohesion_inputs` documents the unchanged layout as scoring ~1.0: every
    // file still sits in the folder its own directory names, so nothing is
    // relocated and nothing dilutes the fraction. Build the tree the way the
    // real analysis does -- via `build_laminar_tree`, over a nested directory
    // -- and score the identity placement, where each symbol stays in the file
    // container the tree already put it in.
    let paths: Vec<SmolStr> = vec![
        SmolStr::new("src/core/engine/a.ts"),
        SmolStr::new("src/core/engine/b.ts"),
    ];
    let layout = Layout {
        package_roots: vec![],
        source_roots: vec![SmolStr::new("src")],
    };
    let built = build_laminar_tree(&paths, "workspace", &layout);

    let nodes: Vec<Node> = paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            let container = *built.files.get(path)?;
            // reason: two fixture files index well inside u32.
            #[allow(clippy::cast_possible_truncation)]
            let id = index as u32;
            Some(sloc_node(id, path.as_str(), container, 10))
        })
        .collect();
    let container_of: BTreeMap<u32, ContainerId> = nodes
        .iter()
        .map(|node| (node.id.0, node.container))
        .collect();
    let snapshot = snapshot(nodes, vec![], built.tree.containers().to_vec());
    let tree = snapshot.ir().containers.clone();

    let (_, path_cohesion) =
        cohesion_inputs(&snapshot, &|id| container_of.get(&id).copied(), &tree);

    assert!(
        (path_cohesion - 1.0).abs() < f64::EPSILON,
        "the unchanged layout must be fully path-cohesive, measured {path_cohesion}"
    );
}

#[test]
fn should_not_count_a_file_as_moved_when_only_a_display_name_above_its_folder_changes() {
    // a file's real location is its folder key: `src/core/engine/a.ts` lives
    // in `workspace/core/engine` no matter what label the domain above it
    // wears. Naming a domain is a display act, never a path component, so a
    // relabelled ancestor must not register as a relocation.
    let paths: Vec<SmolStr> = vec![SmolStr::new("src/core/engine/a.ts")];
    let layout = Layout {
        package_roots: vec![],
        source_roots: vec![SmolStr::new("src")],
    };
    let built = build_laminar_tree(&paths, "workspace", &layout);
    // `build_laminar_tree` interns every input path, so the lookup always
    // hits; the fallback is unreachable and only keeps the strict lint clean.
    let file_id = built
        .files
        .get(&SmolStr::new("src/core/engine/a.ts"))
        .copied()
        .unwrap_or(ContainerId(0));
    let containers = built.tree.containers().to_vec();
    let snapshot = snapshot(
        vec![sloc_node(0, "src/core/engine/a.ts", file_id, 10)],
        vec![],
        containers.clone(),
    );

    // the same tree, with only the domain container's display label decorated.
    let relabelled: Vec<Container> = containers
        .iter()
        .cloned()
        .map(|mut container| {
            if container.level == ScopeLevel::Domain {
                container.name = SmolStr::new(format!("{} (workspace.core)", container.name));
            }
            container
        })
        .collect();

    let distance = move_distance(&snapshot, &ContainerTree::new(relabelled), &|_| None);

    assert!(
        distance.abs() < f64::EPSILON,
        "relabelling a domain moves no file, measured {distance}"
    );
}
