#![allow(clippy::assertions_on_constants)]

use strata_core::score::KindWeights;

#[cfg(test)]
use crate::narrate::narrate;

use super::*;
use crate::analyze::test_support::*;
use crate::config::AnalyzeConfig;

#[test]
fn should_preserve_nested_package_dependency_facts_for_pull_narration() {
    let (snapshot, candidate) = nested_package_fact_snapshot(false);
    let facts = file_facts(&snapshot, &KindWeights::default(), 15, &[], &[]);

    let moves = narrate(&snapshot.ir().containers, &candidate, &facts);

    assert_eq!(
        moves.first().map(|entry| entry.reason.to_string()),
        Some("pulled by anchor.ts (w 1.0)".to_owned())
    );
}

#[test]
fn should_preserve_nested_package_test_facts_for_follow_narration() {
    let (snapshot, candidate) = nested_package_fact_snapshot(true);
    let facts = file_facts(&snapshot, &KindWeights::default(), 15, &[], &[]);

    let moves = narrate(&snapshot.ir().containers, &candidate, &facts);

    assert_eq!(
        moves.first().map(|entry| entry.reason.to_string()),
        Some("follows unit.ts".to_owned())
    );
}

/// The package container name above every file of `tree`, keyed by file name.
fn package_of_each_file(tree: &ContainerTree) -> BTreeMap<SmolStr, SmolStr> {
    let by_id: BTreeMap<ContainerId, &Container> = tree
        .containers()
        .iter()
        .map(|container| (container.id, container))
        .collect();
    tree.containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .filter_map(|file| {
            let mut current = file.parent;
            while let Some(id) = current {
                let container = by_id.get(&id)?;
                if container.level == ScopeLevel::Package {
                    return Some((file.name.clone(), container.name.clone()));
                }
                current = container.parent;
            }
            None
        })
        .collect()
}

/// A folder cluster holding a cross-package import cycle: `relay.ts` (atlas)
/// is its heaviest member, the two beacon files outweigh it, and a beacon-only
/// pair sits in the same folder as the beacon files.
fn mixed_cycle_snapshot() -> Snapshot {
    snapshot(
        vec![
            sloc_node(0, "relay", ContainerId(6), 10),
            sloc_node(1, "left", ContainerId(7), 7),
            sloc_node(2, "right", ContainerId(8), 7),
            sloc_node(3, "panel", ContainerId(9), 1),
            sloc_node(4, "frame", ContainerId(10), 1),
            sloc_node(5, "alpha", ContainerId(11), 3),
        ],
        vec![edge(0, 1), edge(0, 2), edge(1, 0), edge(2, 0), edge(4, 3)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "atlas", ScopeLevel::Package, Some(0)),
            container(2, "beacon", ScopeLevel::Package, Some(0)),
            container(3, "atlas/hub", ScopeLevel::Folder, Some(1)),
            container(4, "atlas/core", ScopeLevel::Folder, Some(1)),
            container(5, "beacon/widget", ScopeLevel::Folder, Some(2)),
            container(6, "atlas/hub/relay.ts", ScopeLevel::File, Some(3)),
            container(7, "beacon/widget/left.ts", ScopeLevel::File, Some(5)),
            container(8, "beacon/widget/right.ts", ScopeLevel::File, Some(5)),
            container(9, "beacon/widget/panel.ts", ScopeLevel::File, Some(5)),
            container(10, "beacon/widget/frame.ts", ScopeLevel::File, Some(5)),
            container(11, "atlas/core/alpha.ts", ScopeLevel::File, Some(4)),
        ],
    )
}

/// The package container name above every file of the mixed-cycle tree
/// assembled with the package wall up (`allow_cross_package_moves` false) or
/// lifted (true).
fn assembled_package_of_each_file(allow_cross_package_moves: bool) -> BTreeMap<SmolStr, SmolStr> {
    let snapshot = mixed_cycle_snapshot();
    let tests = TestPolicy::defaults();
    let mut config = AnalyzeConfig::default();
    config
        .profiles
        .greenfield
        .relocation
        .allow_cross_package_moves = allow_cross_package_moves;
    let profile = &config.profiles.greenfield;
    let solver = PipelineSolver::new(
        &snapshot,
        profile,
        profile.objective.coefficients(),
        false,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    package_of_each_file(&assembled.tree)
}

#[test]
fn should_draw_each_file_of_a_mixed_cycle_folder_under_its_own_package() {
    // relay.ts (atlas) is the heaviest member of an import cycle with two
    // beacon files, so the cycle sits in atlas/hub's folder cluster. The two
    // beacon members outweigh relay there, but each file must still be drawn
    // under its own manifest package, never the folder's plurality one.
    assert_eq!(
        assembled_package_of_each_file(false),
        package_of_each_file(&mixed_cycle_snapshot().ir().containers),
        "every file is drawn under its own manifest package"
    );
}

#[test]
fn should_keep_the_plain_clustering_of_the_tree_when_the_package_wall_is_lifted() {
    // with `--allow-cross-package-moves` the upper levels are the plain
    // clustering (ADR-17): the cycle's folder is named by its plurality
    // package, so relay.ts is drawn under beacon although it belongs to atlas.
    let lifted = assembled_package_of_each_file(true);

    let expected: BTreeMap<SmolStr, SmolStr> = [
        ("atlas/core/alpha.ts", "atlas"),
        ("atlas/hub/relay.ts", "beacon"),
        ("beacon/widget/frame.ts", "beacon"),
        ("beacon/widget/left.ts", "beacon"),
        ("beacon/widget/panel.ts", "beacon"),
        ("beacon/widget/right.ts", "beacon"),
    ]
    .into_iter()
    .map(|(file, package)| (SmolStr::new(file), SmolStr::new(package)))
    .collect();
    assert_eq!(lifted, expected);
}
