#![allow(clippy::assertions_on_constants)]

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::score::{
    Candidate as ScoreCandidate, Coefficients, KindWeights, ScoreBreakdown, group_cohesion,
    score_with_naming, weighted_naming,
};
use strata_ir::{
    Affinity, AffinityKind, Container, ContainerId, ContainerTree, Layout, Node, NodeId, NodeKind,
    Polarity, ScopeLevel, Snapshot, build_laminar_tree,
};

use super::*;
use crate::analyze::test_support::*;
use crate::config::{CapacityConfig, ObjectiveConfig};

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

/// A four-file snapshot with calls, a type reference, a duplicate edge, a type
/// node, a test declaration and a companion affinity, plus a candidate tree that
/// moves one file to another folder.
struct Fixture {
    snapshot: Snapshot,
    tree: ContainerTree,
    namespaces: BTreeMap<ContainerId, SmolStr>,
    pass_start: BTreeMap<ContainerId, ContainerId>,
    files: Vec<ContainerId>,
}

fn fixture() -> Fixture {
    let paths: Vec<SmolStr> = [
        "src/ui/button_view.ts",
        "src/ui/panel_view.ts",
        "src/core/engine_core.ts",
        "src/core/store_core.ts",
    ]
    .into_iter()
    .map(SmolStr::new)
    .collect();
    let layout = Layout {
        package_roots: vec![],
        source_roots: vec![SmolStr::new("src")],
    };
    let built = build_laminar_tree(&paths, "workspace", &layout);
    let files: Vec<ContainerId> = paths
        .iter()
        .filter_map(|path| built.files.get(path).copied())
        .collect();

    let mut nodes: Vec<Node> = Vec::new();
    for (index, file) in files.iter().enumerate() {
        for slot in 0..2_u32 {
            // reason: eight fixture nodes index well inside u32.
            #[allow(clippy::cast_possible_truncation)]
            let id = (index as u32) * 2 + slot;
            nodes.push(sloc_node(id, &format!("item_{id}"), *file, 3 + id * 40));
        }
    }
    if let Some(node) = nodes.get_mut(1) {
        node.kind = NodeKind::Type;
    }
    if let Some(node) = nodes.get_mut(5) {
        node.polarity = Polarity::TestCase;
    }
    let edges = vec![
        edge(0, 1),
        edge(0, 2),
        edge(2, 4),
        edge(2, 4),
        type_ref(3, 6),
        edge(5, 7),
        edge(6, 0),
        edge(7, 7),
        inherits(4, 1),
        reexport(6, 3),
    ];
    let affinities = vec![Affinity {
        owner: NodeId(2),
        companion: NodeId(1),
        kind: AffinityKind::CompanionOwner,
    }];
    let containers = built.tree.containers().to_vec();
    let snapshot = snapshot_with_affinities(nodes, edges, affinities, containers.clone());

    // the candidate moves the last file under the first file's folder.
    let first_parent = files
        .first()
        .and_then(|file| containers.iter().find(|c| c.id == *file))
        .and_then(|c| c.parent);
    let moved: Vec<Container> = containers
        .iter()
        .cloned()
        .map(|mut container| {
            if Some(container.id) == files.last().copied() {
                container.parent = first_parent;
            }
            container
        })
        .collect();
    let namespaces = files
        .iter()
        .map(|file| (*file, SmolStr::new("ns")))
        .collect();
    let pass_start = files.iter().map(|file| (*file, *file)).collect();
    Fixture {
        snapshot,
        tree: ContainerTree::new(moved),
        namespaces,
        pass_start,
        files,
    }
}

/// Prices a candidate the straightforward way: naming from each group's own
/// members, then every other term from the candidate's pre-extracted views.
fn reference_score(
    candidate: &ScoreCandidate,
    coefficients: &Coefficients,
    weights: &KindWeights,
) -> ScoreBreakdown {
    let naming = weighted_naming(
        candidate
            .cohesion_groups
            .iter()
            .map(|group| (group.production_sloc, group_cohesion(&group.members))),
    );
    score_with_naming(candidate, naming, coefficients, weights)
}

fn bits(breakdown: &ScoreBreakdown) -> [u64; 9] {
    [
        breakdown.cut,
        breakdown.imbalance,
        breakdown.naming,
        breakdown.path,
        breakdown.anchor,
        breakdown.capacity,
        breakdown.dependency_only,
        breakdown.companion_separation,
        breakdown.total,
    ]
    .map(f64::to_bits)
}

/// Coefficients with every term nonzero and distinct, so no term can hide
/// behind another in the equivalence check.
const fn every_term() -> Coefficients {
    Coefficients {
        lambda: 0.1,
        alpha: 0.3,
        beta: 0.2,
        mu: 1.0,
        gamma: 4.0,
        dependency_only: 0.05,
        companion_separation: 0.07,
    }
}

#[test]
fn should_score_a_placement_exactly_as_the_free_standing_terms_do() {
    let fixture = fixture();
    let capacity = CapacityConfig {
        file: 60,
        folder: 1,
        domain: 1,
        package: 1,
        package_group: 1,
    };
    let lookups = SnapshotScoring::new(&fixture.snapshot, 0.5, 2.0);
    let context = ScoringContext::new(
        &lookups,
        &fixture.tree,
        &fixture.namespaces,
        &fixture.pass_start,
        &capacity,
    );
    let weights = KindWeights::default();
    let ids: Vec<u32> = fixture.snapshot.ir().nodes.iter().map(|n| n.id.0).collect();

    // one context prices many placements: identity, single and compound moves,
    // and a placement that leaves a node out.
    let mut placements: Vec<BTreeMap<u32, ContainerId>> = Vec::new();
    let identity: BTreeMap<u32, ContainerId> = fixture
        .snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id.0, node.container))
        .collect();
    placements.push(identity.clone());
    for (node, target) in [(0_u32, 2_usize), (3, 0), (6, 1), (1, 3)] {
        let mut moved = identity.clone();
        if let Some(file) = fixture.files.get(target) {
            moved.insert(node, *file);
        }
        placements.push(moved);
    }
    let mut compound = identity.clone();
    for (index, id) in ids.iter().enumerate() {
        if let Some(file) = fixture.files.get(index % 3) {
            compound.insert(*id, *file);
        }
    }
    placements.push(compound);
    let mut partial = identity;
    partial.remove(&2);
    partial.remove(&6);
    placements.push(partial);

    for coefficients in [ObjectiveConfig::default().coefficients(), every_term()] {
        for placement in &placements {
            let lookup = |id: u32| placement.get(&id).copied();
            let distance = move_distance(&fixture.snapshot, &fixture.tree, &lookup);
            let oracle = reference_score(
                &score_candidate(
                    &fixture.snapshot,
                    &lookup,
                    &fixture.pass_start,
                    &fixture.tree,
                    &fixture.namespaces,
                    distance,
                    &capacity,
                    0.5,
                    2.0,
                ),
                &coefficients,
                &weights,
            );

            let placed = lookups.place(&lookup);
            let measured = context.move_distance(&placed);
            assert_eq!(measured.to_bits(), distance.to_bits(), "move distance");
            let actual = context.score(&placed, measured, &coefficients, &weights);
            assert_eq!(bits(&actual), bits(&oracle), "breakdown of {placement:?}");
            let direct = context.score_placement(&lookup, &coefficients, &weights);
            assert_eq!(bits(&direct), bits(&oracle), "score_placement");
        }
    }
}

#[test]
fn should_price_an_overlay_set_on_a_dense_placement_like_a_fresh_one() {
    let fixture = fixture();
    let capacity = CapacityConfig::default();
    let lookups = SnapshotScoring::new(&fixture.snapshot, 1.0, 3.0);
    let context = ScoringContext::new(
        &lookups,
        &fixture.tree,
        &fixture.namespaces,
        &fixture.pass_start,
        &capacity,
    );
    let identity = |id: u32| {
        fixture
            .snapshot
            .ir()
            .nodes
            .iter()
            .find(|node| node.id.0 == id)
            .map(|node| node.container)
    };
    let mut incremental = lookups.place(&identity);
    let Some(target) = fixture.files.get(3).copied() else {
        return;
    };
    incremental.set(0, target);
    let moved = |id: u32| if id == 0 { Some(target) } else { identity(id) };
    let fresh = lookups.place(&moved);

    let coefficients = ObjectiveConfig::default().coefficients();
    let weights = KindWeights::default();
    let left = context.score(
        &incremental,
        context.move_distance(&incremental),
        &coefficients,
        &weights,
    );
    let right = context.score(
        &fresh,
        context.move_distance(&fresh),
        &coefficients,
        &weights,
    );
    assert_eq!(bits(&left), bits(&right));
}
