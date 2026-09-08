#![allow(clippy::assertions_on_constants)]

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};
use strata_core::condense::Condensation;
use strata_core::condense::SccId;
use strata_core::graph::csr::Csr;
use strata_core::score::Coefficients;
use strata_ir::{
    Container, ContainerId, ContainerTree, Layout, Node, NodeId, NodeKind, Polarity, ScopeLevel,
    Snapshot, build_laminar_tree,
};

use crate::config::AnalyzeConfig;
use crate::result::{ContainerNode, Level, Profiles};

use super::*;
use crate::analyze::test_support::*;
use crate::analyze::*;
use crate::analyze::{findings::*, relocation::*, scoring::*};

#[test]
fn should_elect_names_by_production_sloc_before_file_count() {
    // two production files under src outweigh five zero-SLOC spec files.
    let mut tally: NameTally = BTreeMap::new();
    for _ in 0..5 {
        vote(&mut tally, 0, SmolStr::new("spec/adapters"), 0);
    }
    vote(&mut tally, 0, SmolStr::new("src/adapters"), 60);
    vote(&mut tally, 0, SmolStr::new("src/adapters"), 60);
    let elected = tally.get(&0).map(plurality).unwrap_or_default();
    assert_eq!(elected, "src/adapters");

    // an all-test cluster has zero SLOC everywhere and degrades to the
    // old file-count plurality.
    let mut tests_only: NameTally = BTreeMap::new();
    for _ in 0..3 {
        vote(&mut tests_only, 0, SmolStr::new("spec/agent"), 0);
    }
    vote(&mut tests_only, 0, SmolStr::new("spec/batch"), 0);
    let fallback = tests_only.get(&0).map(plurality).unwrap_or_default();
    assert_eq!(fallback, "spec/agent");
}

#[test]
fn should_elect_real_home_names_never_a_neutral_label() {
    let anchor = SmolStr::new("src/core");

    // a coherent cluster: one origin holds the SLOC majority, so it names.
    let mut coherent: NameTally = BTreeMap::new();
    vote(&mut coherent, 0, SmolStr::new("src/core"), 200);
    vote(&mut coherent, 0, SmolStr::new("src/io"), 30);
    vote(&mut coherent, 0, SmolStr::new("src/net"), 20);
    assert_eq!(
        coherent
            .get(&0)
            .map(|tally| elect(tally, &anchor))
            .unwrap_or_default(),
        "src/core"
    );

    // a grab-bag: no origin reaches half the weight, so the ladder falls
    // to the homes' shared prefix — a real place, never a neutral label.
    let mut grabbag: NameTally = BTreeMap::new();
    vote(&mut grabbag, 0, SmolStr::new("src/openai"), 40);
    vote(&mut grabbag, 0, SmolStr::new("src/google"), 35);
    vote(&mut grabbag, 0, SmolStr::new("src/anthropic"), 33);
    assert_eq!(
        grabbag
            .get(&0)
            .map(|tally| elect(tally, &anchor))
            .unwrap_or_default(),
        "src"
    );

    // a production home keeps its name despite many companion spec files,
    // because SLOC — not file count — decides the majority.
    let mut with_specs: NameTally = BTreeMap::new();
    vote(&mut with_specs, 0, SmolStr::new("src/adapters"), 120);
    for _ in 0..5 {
        vote(&mut with_specs, 0, SmolStr::new("spec/adapters"), 0);
    }
    assert_eq!(
        with_specs
            .get(&0)
            .map(|tally| elect(tally, &anchor))
            .unwrap_or_default(),
        "src/adapters"
    );
}

#[test]
fn should_join_top_two_homes_by_production_sloc_not_file_count() {
    // three divergent production homes plus a spec dump that wins on file
    // count alone: no home holds a strict SLOC majority and the keys share
    // no prefix, so the join fires — and the two production-SLOC-heaviest
    // homes must lead the composite. A zero-SLOC spec dump outnumbering
    // them in files can never lead the joined name.
    let mut votes: NameTally = BTreeMap::new();
    vote(&mut votes, 0, SmolStr::new("lib/core"), 300);
    vote(&mut votes, 0, SmolStr::new("vendor/util"), 250);
    vote(&mut votes, 0, SmolStr::new("tools/gen"), 200);
    for _ in 0..20 {
        vote(&mut votes, 0, SmolStr::new("spec/everything"), 0);
    }
    let anchor = SmolStr::new("lib/core");

    assert_eq!(
        votes
            .get(&0)
            .map(|tally| elect(tally, &anchor))
            .unwrap_or_default(),
        "lib/core/vendor/util"
    );
}

#[test]
fn should_relativize_the_shared_prefix_when_joining_top_two_homes() {
    // package-qualified homes share their package root; the join must
    // relativize the second home against the first instead of re-embedding
    // the root — `ai/adapters/ai/model` repeats the root mid-path, an
    // incoherent nesting no human would ever propose.
    let anchor = SmolStr::new("ai/adapters");
    let mut votes: NameTally = BTreeMap::new();
    vote(&mut votes, 0, SmolStr::new("ai/adapters"), 300);
    vote(&mut votes, 0, SmolStr::new("ai/model"), 250);
    vote(&mut votes, 0, SmolStr::new("workspace"), 200);

    assert_eq!(
        votes
            .get(&0)
            .map(|tally| elect(tally, &anchor))
            .unwrap_or_default(),
        "ai/adapters/model"
    );

    // when one home is the other's ancestor, the ancestor already covers
    // both and elects alone instead of a self-embedding composite.
    let mut nested: NameTally = BTreeMap::new();
    vote(&mut nested, 0, SmolStr::new("ai/adapters"), 300);
    vote(&mut nested, 0, SmolStr::new("ai"), 250);
    vote(&mut nested, 0, SmolStr::new("workspace"), 200);

    assert_eq!(
        nested
            .get(&0)
            .map(|tally| elect(tally, &anchor))
            .unwrap_or_default(),
        "ai"
    );
}

#[test]
fn should_relocate_a_misplaced_file_despite_a_cyclic_folder_base() {
    // folders `a` and `b` cycle through each other on two disjoint file
    // pairs per direction (a1→b1, a2→b2 against b3→a3, b4→a4), so no
    // single relocation can dissolve the cycle — as tangles between
    // deliberately misplaced files behave on real repositories. A
    // whole-state acyclicity veto then prices every move at infinity and
    // freezes the polish pass wholesale; the veto must only bar moves that
    // grow the cyclicity, keeping the strictly-improving relocation of
    // `m.ts` (every edge pointing into `c`, far from the cycle) available.
    let snapshot = snapshot(
        vec![
            homed(0, "a1", 6, 10),
            homed(1, "a2", 7, 10),
            homed(2, "a3", 8, 10),
            homed(3, "a4", 9, 10),
            homed(4, "m", 10, 10),
            homed(5, "b1", 11, 10),
            homed(6, "b2", 12, 10),
            homed(7, "b3", 13, 10),
            homed(8, "b4", 14, 10),
            homed(9, "c1", 15, 10),
            homed(10, "c2", 16, 10),
        ],
        vec![
            edge(0, 5),
            edge(1, 6),
            edge(7, 2),
            edge(8, 3),
            edge(4, 9),
            edge(4, 10),
            edge(9, 10),
        ],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "app", ScopeLevel::Package, Some(0)),
            container(2, "app/core", ScopeLevel::Domain, Some(1)),
            container(3, "app/core/a", ScopeLevel::Folder, Some(2)),
            container(4, "app/core/b", ScopeLevel::Folder, Some(2)),
            container(5, "app/core/c", ScopeLevel::Folder, Some(2)),
            container(6, "src/core/a/a1.ts", ScopeLevel::File, Some(3)),
            container(7, "src/core/a/a2.ts", ScopeLevel::File, Some(3)),
            container(8, "src/core/a/a3.ts", ScopeLevel::File, Some(3)),
            container(9, "src/core/a/a4.ts", ScopeLevel::File, Some(3)),
            container(10, "src/core/a/m.ts", ScopeLevel::File, Some(3)),
            container(11, "src/core/b/b1.ts", ScopeLevel::File, Some(4)),
            container(12, "src/core/b/b2.ts", ScopeLevel::File, Some(4)),
            container(13, "src/core/b/b3.ts", ScopeLevel::File, Some(4)),
            container(14, "src/core/b/b4.ts", ScopeLevel::File, Some(4)),
            container(15, "src/core/c/c1.ts", ScopeLevel::File, Some(5)),
            container(16, "src/core/c/c2.ts", ScopeLevel::File, Some(5)),
        ],
    );

    let moves = analyze(&snapshot, &config_with_k(1))
        .ok()
        .and_then(|result| result.profiles.greenfield)
        .and_then(|mode| mode.candidates.into_iter().next())
        .map(|candidate| candidate.delta_narration)
        .unwrap_or_default();

    let destination = moves
        .iter()
        .find(|entry| {
            entry
                .files
                .iter()
                .any(|file| file.path == "app/src/core/a/m.ts")
        })
        .map(|entry| entry.to.clone());

    assert!(
        destination
            .as_deref()
            .is_some_and(|to| to.split('/').next_back() == Some("c")),
        "expected m.ts to land in folder c, got {destination:?} among {moves:?}"
    );
}

/// Builds the neutral file-placement shape used by rendered-leaf
/// uniqueness regressions. Each claimant is pulled toward `destination`;
/// callers choose whether that folder already owns the same rendered leaf.
fn rendered_leaf_collision_snapshot(
    include_second_claimant: bool,
    first_claimant_is_file_body: bool,
) -> Snapshot {
    let mut nodes = vec![
        homed(0, "first_left", 6, 10),
        homed(1, "second_left", 7, 10),
        homed(2, "first_return", 8, 10),
        homed(3, "second_return", 9, 10),
        homed(4, "compose_record", 10, 10),
        homed(5, "first_right", 11, 10),
        homed(6, "second_right", 12, 10),
        homed(7, "third_right", 13, 10),
        homed(8, "fourth_right", 14, 10),
        homed(9, "record_format", 15, 10),
        homed(10, "destination_helper", 16, 10),
    ];
    let mut edges = vec![
        edge(0, 5),
        edge(1, 6),
        edge(7, 2),
        edge(8, 3),
        edge(4, 9),
        edge(4, 10),
        edge(9, 10),
    ];
    if first_claimant_is_file_body && let Some(claimant) = nodes.get_mut(4) {
        claimant.kind = NodeKind::FileBody;
    }
    let mut containers = vec![
        container(0, "app", ScopeLevel::PackageGroup, None),
        container(1, "app", ScopeLevel::Package, Some(0)),
        container(2, "app/area", ScopeLevel::Domain, Some(1)),
        container(3, "app/area/origin", ScopeLevel::Folder, Some(2)),
        container(4, "app/area/return", ScopeLevel::Folder, Some(2)),
        container(5, "app/area/destination", ScopeLevel::Folder, Some(2)),
        container(6, "src/area/origin/first.ts", ScopeLevel::File, Some(3)),
        container(7, "src/area/origin/second.ts", ScopeLevel::File, Some(3)),
        container(8, "src/area/origin/third.ts", ScopeLevel::File, Some(3)),
        container(9, "src/area/origin/fourth.ts", ScopeLevel::File, Some(3)),
        container(10, "src/area/origin/record.ts", ScopeLevel::File, Some(3)),
        container(11, "src/area/return/first.ts", ScopeLevel::File, Some(4)),
        container(12, "src/area/return/second.ts", ScopeLevel::File, Some(4)),
        container(13, "src/area/return/third.ts", ScopeLevel::File, Some(4)),
        container(14, "src/area/return/fourth.ts", ScopeLevel::File, Some(4)),
        container(
            15,
            if include_second_claimant {
                "src/area/destination/format.ts"
            } else {
                "src/area/destination/record.ts"
            },
            ScopeLevel::File,
            Some(5),
        ),
        container(
            16,
            "src/area/destination/helper.ts",
            ScopeLevel::File,
            Some(5),
        ),
    ];
    if include_second_claimant {
        nodes.push(homed(11, "compose_second_record", 18, 10));
        nodes.push(homed(12, "second_origin_resident", 19, 10));
        edges.extend([edge(11, 9), edge(11, 10)]);
        containers.push(container(
            17,
            "app/area/second-origin",
            ScopeLevel::Folder,
            Some(2),
        ));
        containers.push(container(
            18,
            "src/area/second-origin/record.ts",
            ScopeLevel::File,
            Some(17),
        ));
        containers.push(container(
            19,
            "src/area/second-origin/resident.ts",
            ScopeLevel::File,
            Some(17),
        ));
    }
    snapshot(nodes, edges, containers)
}

/// Returns the destination narrated for `path` by the first greenfield
/// candidate, if that candidate relocates the file.
fn greenfield_file_destination(snapshot: &Snapshot, path: &str) -> Option<String> {
    analyze(snapshot, &config_with_k(1))
        .ok()
        .and_then(|result| result.profiles.greenfield)
        .and_then(|mode| mode.candidates.into_iter().next())
        .and_then(|candidate| {
            candidate.delta_narration.into_iter().find_map(|entry| {
                entry
                    .files
                    .iter()
                    .any(|file| file.path == path)
                    .then_some(entry.to)
            })
        })
}

/// Builds a neutral file-placement witness whose raw paths either share or
/// cross transparent source-root namespaces while their laminar homes omit
/// those roots exactly as a real adapter snapshot does.
fn transparent_namespace_file_snapshot(crosses_namespace: bool) -> Snapshot {
    let origin_namespace = "left";
    let destination_namespace = if crosses_namespace { "right" } else { "left" };
    let paths: Vec<SmolStr> = [
        format!("{origin_namespace}/area/origin/first.ts"),
        format!("{origin_namespace}/area/origin/second.ts"),
        format!("{origin_namespace}/area/origin/third.ts"),
        format!("{origin_namespace}/area/origin/fourth.ts"),
        format!("{origin_namespace}/area/origin/record.ts"),
        format!("{origin_namespace}/area/return/first.ts"),
        format!("{origin_namespace}/area/return/second.ts"),
        format!("{origin_namespace}/area/return/third.ts"),
        format!("{origin_namespace}/area/return/fourth.ts"),
        format!("{destination_namespace}/area/destination/format.ts"),
        format!("{destination_namespace}/area/destination/helper.ts"),
    ]
    .into_iter()
    .map(SmolStr::new)
    .collect();
    let layout = Layout {
        package_roots: Vec::new(),
        source_roots: vec![SmolStr::new("left"), SmolStr::new("right")],
    };
    let built = build_laminar_tree(&paths, "app", &layout);
    let nodes = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let id = u32::try_from(index).unwrap_or(u32::MAX);
            node(
                id,
                path.rsplit('/').next().unwrap_or(path.as_str()),
                built
                    .files
                    .get(path)
                    .map_or(u32::MAX, |container| container.0),
                Polarity::Production,
            )
        })
        .collect();
    snapshot(
        nodes,
        vec![
            edge(0, 5),
            edge(1, 6),
            edge(7, 2),
            edge(8, 3),
            edge(4, 9),
            edge(4, 10),
            edge(9, 10),
        ],
        built.tree.containers().to_vec(),
    )
}

#[test]
fn should_veto_a_file_move_across_transparent_namespaces() {
    let snapshot = transparent_namespace_file_snapshot(true);
    let destination = greenfield_file_destination(&snapshot, "app/left/area/origin/record.ts");

    assert!(
        destination
            .as_deref()
            .is_none_or(|to| !to.ends_with("destination")),
        "a file must remain inside its pass-start transparent namespace; destination \
             {destination:?}"
    );
}

#[test]
fn should_allow_a_unique_file_move_within_its_transparent_namespace() {
    let snapshot = transparent_namespace_file_snapshot(false);
    let destination = greenfield_file_destination(&snapshot, "app/left/area/origin/record.ts");

    assert!(
        destination
            .as_deref()
            .is_some_and(|to| to.ends_with("destination")),
        "a unique-basename file move inside one namespace must remain eligible; destination \
             {destination:?}"
    );
}

#[test]
fn should_veto_a_file_move_that_duplicates_an_existing_rendered_leaf() {
    let snapshot = rendered_leaf_collision_snapshot(false, false);
    let destination = greenfield_file_destination(&snapshot, "app/src/area/origin/record.ts");

    assert!(
        destination
            .as_deref()
            .is_none_or(|to| !to.ends_with("destination")),
        "a folder already containing record.ts must reject another rendered record.ts; \
             destination {destination:?}"
    );
}

#[test]
fn should_veto_a_sequential_sibling_move_after_the_first_claims_the_rendered_leaf() {
    let snapshot = rendered_leaf_collision_snapshot(true, false);
    let first = greenfield_file_destination(&snapshot, "app/src/area/origin/record.ts");
    let second = greenfield_file_destination(&snapshot, "app/src/area/second-origin/record.ts");
    let arrivals = [first.as_deref(), second.as_deref()]
        .into_iter()
        .flatten()
        .filter(|to| to.ends_with("destination"))
        .count();

    assert_eq!(
        arrivals, 1,
        "the first claimant may occupy a free rendered leaf, but every later claimant must \
             be rejected; \
             destinations {first:?} and {second:?}"
    );
}

#[test]
fn should_allow_same_rendered_leaf_names_in_distinct_folders() {
    let mut first = relief_file(0, "first");
    first.name = SmolStr::new("src/left/record.ts");
    let mut second = relief_file(1, "second");
    second.name = SmolStr::new("src/right/record.ts");
    let condensation = singleton_condensation(2);
    let partition = Partition::from_assignment(vec![ClusterId(0), ClusterId(1)], 2);
    let guard = RelocationIdentityGuard::new(
        &[first, second],
        &condensation,
        &partition,
        &partition,
        None,
    );

    assert!(guard.accepts(&partition));
}

#[test]
fn should_allow_same_rendered_leaf_names_across_transparent_namespaces() {
    let mut first = relief_file(0, "first");
    first.name = SmolStr::new("left/area/record.ts");
    first.namespace = SmolStr::new("left");
    let mut second = relief_file(1, "second");
    second.name = SmolStr::new("right/area/record.ts");
    second.namespace = SmolStr::new("right");
    let condensation = singleton_condensation(2);
    let partition = Partition::from_assignment(vec![ClusterId(0), ClusterId(0)], 1);
    let guard = RelocationIdentityGuard::new(
        &[first, second],
        &condensation,
        &partition,
        &partition,
        None,
    );

    assert!(guard.accepts(&partition));
}

#[test]
fn should_preserve_transparent_namespaces_for_synthetic_package_workspaces() {
    let paths = vec![
        SmolStr::new("packages/unit/left/entry.ts"),
        SmolStr::new("packages/unit/right/entry.ts"),
    ];
    let layout = Layout {
        package_roots: vec![SmolStr::new("packages/unit")],
        source_roots: vec![SmolStr::new("left"), SmolStr::new("right")],
    };
    let built = build_laminar_tree(&paths, "workspace", &layout);
    let snapshot = snapshot(
        paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                node(
                    u32::try_from(index).unwrap_or(u32::MAX),
                    "entry",
                    built
                        .files
                        .get(path)
                        .map_or(u32::MAX, |container| container.0),
                    Polarity::Production,
                )
            })
            .collect(),
        Vec::new(),
        built.tree.containers().to_vec(),
    );

    let (files, _) = file_inventory(snapshot.ir());
    let namespaces: BTreeSet<&str> = files.iter().map(|file| file.namespace.as_str()).collect();

    assert_eq!(namespaces, BTreeSet::from(["left", "right"]));
}

#[test]
fn should_treat_rendered_leaf_identity_as_case_sensitive() {
    let mut lower = relief_file(0, "lower");
    lower.name = SmolStr::new("left/area/record.ts");
    lower.namespace = SmolStr::new("left");
    let mut upper = relief_file(1, "upper");
    upper.name = SmolStr::new("left/area/Record.ts");
    upper.namespace = SmolStr::new("left");
    let condensation = singleton_condensation(2);
    let partition = Partition::from_assignment(vec![ClusterId(0), ClusterId(0)], 1);
    let guard =
        RelocationIdentityGuard::new(&[lower, upper], &condensation, &partition, &partition, None);

    assert!(guard.accepts(&partition));
}

#[test]
fn should_move_a_file_body_only_with_its_whole_file() {
    let snapshot = rendered_leaf_collision_snapshot(true, true);

    let destination = greenfield_file_destination(&snapshot, "app/src/area/origin/record.ts");

    assert!(
        destination
            .as_deref()
            .is_some_and(|to| to.ends_with("destination")),
        "file-body content must retain its file-grain mobility; destination {destination:?}"
    );
}

#[test]
fn should_build_candidates_with_real_names_and_five_levels() {
    // two files under a real directory path, each holding a multi-line
    // production symbol, plus a hard edge so clustering has a pair to group.
    let sized = |id: u32, name: &str, container: u32, size: u32| Node {
        id: NodeId(id),
        name: SmolStr::new(name),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container: ContainerId(container),
        visibility: ScopeLevel::File,
        effective_size: size,
    };
    let snapshot = snapshot(
        vec![sized(0, "alpha", 4, 10), sized(1, "beta", 5, 7)],
        vec![edge(0, 1)],
        vec![
            container(0, "strata", ScopeLevel::PackageGroup, None),
            container(1, "crates", ScopeLevel::Package, Some(0)),
            container(2, "crates/engine", ScopeLevel::Domain, Some(1)),
            container(3, "crates/engine/src", ScopeLevel::Folder, Some(2)),
            container(4, "crates/engine/src/alpha.rs", ScopeLevel::File, Some(3)),
            container(5, "crates/engine/src/beta.rs", ScopeLevel::File, Some(3)),
        ],
    );

    let rendered = analyze(&snapshot, &config_with_k(1)).ok().map(|result| {
        result
            .profiles
            .anchored
            .and_then(|profile| profile.candidates.into_iter().next())
            .map_or(result.current.tree, |candidate| candidate.tree)
    });

    let mut names = Vec::new();
    let mut sloc = Vec::new();
    let mut levels = Vec::new();
    if let Some(tree) = &rendered {
        collect_tree(tree, &mut names, &mut sloc, &mut levels);
    }
    assert!(!names.is_empty(), "expected a rendered analysis tree");

    // names are real directory-derived, never synthetic cluster labels.
    assert!(
        names.iter().all(|name| !name.contains("cluster-")),
        "expected real names, got {names:?}"
    );
    // interior names render incrementally: the domain "crates/engine" adds
    // "engine" over the package "crates", the folder adds "src"; files keep
    // their full path as their stable identity.
    assert!(names.iter().any(|name| name == "crates"));
    assert!(names.iter().any(|name| name == "engine"));
    assert!(names.iter().any(|name| name == "src"));
    assert!(
        names
            .iter()
            .any(|name| name == "crates/engine/src/alpha.rs")
    );

    // the full five-level laminar hierarchy is present.
    for expected in [
        Level::PackageGroup,
        Level::Package,
        Level::Domain,
        Level::Folder,
        Level::File,
    ] {
        assert!(
            levels.contains(&expected),
            "missing {expected:?} in {levels:?}"
        );
    }

    // production SLOC sums effective_size (10, 7), not the symbol count (1).
    assert!(
        sloc.contains(&10) && sloc.contains(&7),
        "expected summed effective_size, got {sloc:?}"
    );
}

/// A production symbol node with an explicit size, for the clustering tests.
fn homed(id: u32, name: &str, container: u32, size: u32) -> Node {
    Node {
        id: NodeId(id),
        name: SmolStr::new(name),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container: ContainerId(container),
        visibility: ScopeLevel::File,
        effective_size: size,
    }
}

/// Collects the `(level, name)` pairs of the best offered tree, or the
/// current tree when no strict improvement is offerable.
fn candidate_containers(snapshot: &Snapshot, config: &AnalyzeConfig) -> Vec<(Level, String)> {
    let rendered = analyze(snapshot, config).ok().map(|result| {
        result
            .profiles
            .greenfield
            .and_then(|profile| profile.candidates.into_iter().next())
            .map_or(result.current.tree, |candidate| candidate.tree)
    });
    let (mut names, mut sloc, mut levels) = (Vec::new(), Vec::new(), Vec::new());
    if let Some(tree) = &rendered {
        collect_tree(tree, &mut names, &mut sloc, &mut levels);
    }
    levels.into_iter().zip(names).collect()
}

#[test]
fn should_separate_domains_by_home_directory() {
    // two home directories (`adapters` with openai+google folders, `agent`
    // with loop+plan) with a weak cross edge: before the seed carried a home
    // affinity the upper level pooled all four folders by index order into one
    // cut-minimal cluster that `elect` could only call `mixed`; now each
    // domain packs its own home and elects a real name.
    let snapshot = snapshot(
        vec![
            homed(0, "a", 5, 10),
            homed(1, "b", 6, 10),
            homed(2, "c", 7, 10),
            homed(3, "d", 8, 10),
            homed(4, "e", 12, 10),
            homed(5, "f", 13, 10),
            homed(6, "g", 14, 10),
            homed(7, "h", 15, 10),
        ],
        vec![edge(0, 1), edge(2, 3), edge(4, 5), edge(6, 7), edge(0, 4)],
        vec![
            container(0, "ai", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
            container(3, "ai/adapters/openai", ScopeLevel::Folder, Some(2)),
            container(4, "ai/adapters/google", ScopeLevel::Folder, Some(2)),
            container(9, "ai/agent", ScopeLevel::Domain, Some(1)),
            container(10, "ai/agent/loop", ScopeLevel::Folder, Some(9)),
            container(11, "ai/agent/plan", ScopeLevel::Folder, Some(9)),
            container(5, "src/adapters/openai/a.ts", ScopeLevel::File, Some(3)),
            container(6, "src/adapters/openai/b.ts", ScopeLevel::File, Some(3)),
            container(7, "src/adapters/google/c.ts", ScopeLevel::File, Some(4)),
            container(8, "src/adapters/google/d.ts", ScopeLevel::File, Some(4)),
            container(12, "src/agent/loop/e.ts", ScopeLevel::File, Some(10)),
            container(13, "src/agent/loop/f.ts", ScopeLevel::File, Some(10)),
            container(14, "src/agent/plan/g.ts", ScopeLevel::File, Some(11)),
            container(15, "src/agent/plan/h.ts", ScopeLevel::File, Some(11)),
        ],
    );
    let mut config = config_with_k(1);
    config.profiles.anchored.capacity.folder = 2;
    config.profiles.anchored.capacity.domain = 2;
    config.profiles.anchored.capacity.package = 4;
    config.profiles.greenfield.capacity.folder = 2;
    config.profiles.greenfield.capacity.domain = 2;
    config.profiles.greenfield.capacity.package = 4;

    let containers = candidate_containers(&snapshot, &config);
    let domains: Vec<&String> = containers
        .iter()
        .filter(|(level, _)| *level == Level::Domain)
        .map(|(_, name)| name)
        .collect();

    assert!(
        domains.iter().all(|name| name.as_str() != "mixed"),
        "domains collapsed into a mixed grab-bag: {domains:?}"
    );
    assert!(
        domains.iter().any(|name| name.as_str() == "adapters")
            && domains.iter().any(|name| name.as_str() == "agent"),
        "expected the two home directories to elect distinct domains, got {domains:?}"
    );
}

#[test]
fn should_split_an_over_cap_directory_into_named_children_along_connectivity() {
    // one real directory (`openai`) holds two priced file pairs — more
    // than the folder cap of two — and no sibling directory exists to
    // relieve into. FIX03 prices that binding in the objective, so the
    // search grain itself relieves it: both connected pairs become named
    // children because keeping either two-file pair directly beside the
    // other child would exceed the parent budget.
    let snapshot = snapshot(
        vec![
            homed(0, "gpt", 4, 10),
            homed(1, "gpt", 5, 10),
            homed(2, "dalle", 6, 10),
            homed(3, "dalle", 7, 10),
        ],
        vec![edge(0, 1), edge(2, 3)],
        vec![
            container(0, "ai", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
            container(3, "ai/adapters/openai", ScopeLevel::Folder, Some(2)),
            container(
                4,
                "src/adapters/openai/gpt_one.ts",
                ScopeLevel::File,
                Some(3),
            ),
            container(
                5,
                "src/adapters/openai/gpt_two.ts",
                ScopeLevel::File,
                Some(3),
            ),
            container(
                6,
                "src/adapters/openai/dalle_one.ts",
                ScopeLevel::File,
                Some(3),
            ),
            container(
                7,
                "src/adapters/openai/dalle_two.ts",
                ScopeLevel::File,
                Some(3),
            ),
        ],
    );
    let mut config = config_with_k(1);
    config.profiles.anchored.capacity.folder = 2;
    config.profiles.greenfield.capacity.folder = 2;

    let containers = candidate_containers(&snapshot, &config);
    let folders: Vec<&String> = containers
        .iter()
        .filter(|(level, _)| *level == Level::Folder)
        .map(|(_, name)| name)
        .collect();

    assert_eq!(
        folders.len(),
        3,
        "the parent plus two named relief children must remain, got {folders:?}"
    );
    assert!(
        folders.iter().any(|name| name.as_str() == "openai"),
        "the original directory must keep its real name, got {folders:?}"
    );
    assert!(
        folders.iter().any(|name| name.as_str() == "gpt"),
        "the first connected group must nest below the base directory, got {folders:?}"
    );
    assert!(
        folders.iter().any(|name| name.as_str() == "dalle"),
        "the second group must nest its dominant stem under the base \
             directory's place, got {folders:?}"
    );
}

/// Collects each file-holding directory's folder-chain path and the file
/// names directly under it over a candidate tree, in pre-order.
///
/// Folder nodes render one path segment each, so a directory's identity is
/// the slash-joined chain of folder segments below its domain; interior
/// chain nodes holding no files directly are not directories of interest
/// and are skipped.
fn folder_files(node: &ContainerNode, folders: &mut Vec<(String, Vec<String>)>) {
    folder_files_under(node, "", folders);
}

/// Walks below [`folder_files`], threading the folder-chain `prefix`.
fn folder_files_under(
    node: &ContainerNode,
    prefix: &str,
    folders: &mut Vec<(String, Vec<String>)>,
) {
    let path = if node.level != Level::Folder {
        String::new()
    } else if prefix.is_empty() {
        node.name.clone()
    } else {
        format!("{prefix}/{}", node.name)
    };
    if node.level == Level::Folder {
        let files: Vec<String> = node
            .children
            .iter()
            .flatten()
            .filter(|child| child.level == Level::File)
            .map(|child| child.name.clone())
            .collect();
        if !files.is_empty() {
            folders.push((path.clone(), files));
        }
    }
    for child in node.children.iter().flatten() {
        folder_files_under(child, &path, folders);
    }
}

/// An over-cap real directory beside an under-cap one: `one` holds three
/// files against a folder cap of two, and its file `c` couples hard
/// (three priced edges) to `d` in `two`. The old folder clustering split
/// `one` into a suffixed synthetic sibling holding files from both real
/// directories; the real-directory partition must not.
fn over_cap_real_dir_snapshot() -> Snapshot {
    snapshot(
        vec![
            homed(0, "alpha", 5, 10),
            homed(1, "beta", 6, 10),
            homed(2, "c_zero", 7, 10),
            homed(3, "c_one", 7, 10),
            homed(4, "c_two", 7, 10),
            homed(5, "delta", 8, 10),
        ],
        vec![edge(2, 5), edge(3, 5), edge(4, 5)],
        vec![
            container(0, "ai", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/svc", ScopeLevel::Domain, Some(1)),
            container(3, "ai/svc/one", ScopeLevel::Folder, Some(2)),
            container(4, "ai/svc/two", ScopeLevel::Folder, Some(2)),
            container(5, "src/svc/one/a.ts", ScopeLevel::File, Some(3)),
            container(6, "src/svc/one/b.ts", ScopeLevel::File, Some(3)),
            container(7, "src/svc/one/c.ts", ScopeLevel::File, Some(3)),
            container(8, "src/svc/two/d.ts", ScopeLevel::File, Some(4)),
        ],
    )
}

/// The parent folder name of `file` in a candidate tree's folder listing.
fn folder_of<'a>(folders: &'a [(String, Vec<String>)], file: &str) -> Option<&'a str> {
    folders
        .iter()
        .find(|(_, files)| files.iter().any(|name| name == file))
        .map(|(name, _)| name.as_str())
}

/// True when a name carries a synthetic numeric dedup suffix like `-2`.
fn numeric_suffixed(name: &str) -> bool {
    name.rsplit_once('-')
        .is_some_and(|(_, tail)| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()))
}

/// Collects every container name rendered in a candidate tree, pre-order.
fn tree_names(node: &ContainerNode) -> Vec<String> {
    let (mut names, mut sloc, mut levels) = (Vec::new(), Vec::new(), Vec::new());
    collect_tree(node, &mut names, &mut sloc, &mut levels);
    names
}

#[test]
fn should_keep_fallback_folders_of_different_packages_apart() {
    // reality is the location, not the name: `a.ts` and `b.ts` both sit
    // directly in their domain directories (inheriting them as folder
    // keys), but they live in different real packages, so no candidate may
    // pool them into one folder cluster (which would drag them into one
    // shared domain and package).
    let snapshot = snapshot(
        vec![homed(0, "alpha", 3, 10), homed(1, "beta", 6, 10)],
        vec![],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/app", ScopeLevel::Domain, Some(1)),
            container(3, "src/a.ts", ScopeLevel::File, Some(2)),
            container(4, "bi", ScopeLevel::Package, Some(0)),
            container(5, "bi/app", ScopeLevel::Domain, Some(4)),
            container(6, "src/b.ts", ScopeLevel::File, Some(5)),
        ],
    );
    let config = config_with_k(1);

    let analyzed = analyze(&snapshot, &config).ok();
    let mut current_folders = Vec::new();
    if let Some(result) = &analyzed {
        folder_files(&result.current.tree, &mut current_folders);
    }
    assert!(
        current_folders.iter().all(|(_, files)| files.len() == 1),
        "files of different packages must stay in separate fallback folders, got \
             {current_folders:?}"
    );
    let modes = analyzed.map_or_else(Profiles::default, |result| result.profiles);

    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            let mut folders = Vec::new();
            folder_files(&candidate.tree, &mut folders);
            assert!(
                folders.iter().all(|(_, files)| files.len() == 1),
                "files of different packages must not pool into one \
                     fallback folder, got {folders:?}"
            );
        }
    }
}

#[test]
fn should_never_suffix_same_basename_folders_merged_into_one_domain() {
    // `ai/adapters/http` and `ai/core/http` share a basename but are
    // different real places. The satellite `nu` carries a sole inheritance
    // anchor into `adapters/http`, so polish genuinely consolidates it
    // there; the alpha/mu and gamma/delta spines pin every heavyweight,
    // and mu's weak type-reference to delta keeps both directories one
    // merged-domain suggestion. Each folder must still surface its own
    // real key — never a truncated twin deduped into a synthetic `http-2`.
    let snapshot = snapshot(
        vec![
            homed(0, "alpha", 4, 40),
            homed(1, "mu", 5, 30),
            homed(2, "gamma", 8, 20),
            homed(3, "delta", 9, 10),
            homed(4, "nu", 10, 5),
        ],
        vec![
            edge(0, 1),
            inherits(1, 0),
            edge(2, 3),
            inherits(3, 2),
            inherits(4, 0),
            type_ref(1, 3),
        ],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
            container(3, "ai/adapters/http", ScopeLevel::Folder, Some(2)),
            container(4, "src/adapters/http/client.ts", ScopeLevel::File, Some(3)),
            container(5, "src/adapters/http/codec.ts", ScopeLevel::File, Some(3)),
            container(6, "ai/core", ScopeLevel::Domain, Some(1)),
            container(7, "ai/core/http", ScopeLevel::Folder, Some(6)),
            container(8, "src/core/http/util.ts", ScopeLevel::File, Some(7)),
            container(9, "src/core/http/parse.ts", ScopeLevel::File, Some(7)),
            container(10, "src/core/http/net.ts", ScopeLevel::File, Some(7)),
        ],
    );
    let mut config = config_with_k(3);
    // headroom for the genuine consolidation: `adapters/http` absorbs the
    // sole-anchored satellite without ever exceeding the cap.
    config.profiles.anchored.capacity.folder = 3;
    config.profiles.greenfield.capacity.folder = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let names = tree_names(&candidate.tree);
            let suffixed: Vec<&String> = names
                .iter()
                .filter(|name| numeric_suffixed(name.as_str()))
                .collect();
            assert!(
                suffixed.is_empty(),
                "no container may carry a synthetic numeric suffix, got {suffixed:?}"
            );
            let mut folders = Vec::new();
            folder_files(&candidate.tree, &mut folders);
            assert_eq!(
                folder_of(&folders, "src/adapters/http/client.ts"),
                Some("http"),
                "the majority folder keeps its real basename, got {folders:?}"
            );
            let foreign = folder_of(&folders, "src/core/http/util.ts");
            assert!(
                foreign == Some("http") || foreign == Some("core/http"),
                "the minority folder must render its package-relative key, \
                     got {foreign:?} in {folders:?}"
            );
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
}

#[test]
fn should_inherit_real_domain_keys_for_domain_rooted_files() {
    // these files sit directly in two packages' domain directories, so
    // there is no deeper folder: each file's real folder IS its domain
    // directory. The satellite `a2` carries a sole inheritance anchor into
    // `bi/app`, so polish genuinely consolidates it there and coupling
    // merges the two domains into one suggestion — whose folders must keep
    // those inherited real keys, never collapse into `workspace` fallback
    // twins deduped as `workspace-2`.
    let snapshot = snapshot(
        vec![
            homed(0, "alpha", 3, 10),
            homed(1, "beta", 4, 10),
            homed(2, "gamma", 7, 10),
            homed(3, "delta", 8, 10),
        ],
        vec![
            type_ref(0, 1),
            inherits(1, 2),
            inherits(1, 3),
            inherits(2, 3),
            edge(3, 2),
            type_ref(0, 3),
        ],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/app", ScopeLevel::Domain, Some(1)),
            container(3, "src/a1.ts", ScopeLevel::File, Some(2)),
            container(4, "src/a2.ts", ScopeLevel::File, Some(2)),
            container(5, "bi", ScopeLevel::Package, Some(0)),
            container(6, "bi/app", ScopeLevel::Domain, Some(5)),
            container(7, "src/b1.ts", ScopeLevel::File, Some(6)),
            container(8, "src/b2.ts", ScopeLevel::File, Some(6)),
        ],
    );
    let mut config = config_with_k(2);
    // headroom for the genuine consolidation: `bi/app` absorbs the
    // sole-anchored satellite without ever exceeding the cap.
    config.profiles.anchored.capacity.folder = 3;
    config.profiles.greenfield.capacity.folder = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut merged_seen = false;
    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let names = tree_names(&candidate.tree);
            let suffixed: Vec<&String> = names
                .iter()
                .filter(|name| numeric_suffixed(name.as_str()))
                .collect();
            assert!(
                suffixed.is_empty(),
                "no container may carry a synthetic numeric suffix, got {suffixed:?}"
            );
            let mut folders = Vec::new();
            folder_files(&candidate.tree, &mut folders);
            assert!(
                folders
                    .iter()
                    .all(|(name, _)| ["app", "ai/app", "bi/app"].contains(&name.as_str())),
                "every folder must carry a real inherited key, got {folders:?}"
            );
            assert!(
                folders.iter().all(|(_, files)| !files.is_empty()),
                "every rendered folder holds its real members, got {folders:?}"
            );
            let a = folder_of(&folders, "src/a1.ts");
            let b = folder_of(&folders, "src/b1.ts");
            merged_seen |= a == Some("ai/app") && b == Some("bi/app");
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
    assert!(
        merged_seen,
        "expected a merged-domain candidate keeping both inherited real \
             keys `ai/app` and `bi/app` whole"
    );
}

#[test]
fn should_qualify_folder_keys_that_collide_across_nested_packages() {
    // source-root stripping can normalize two different real directories
    // to one folder key: `a/src/b/c` in package `a` and `a/b/src/c` in
    // nested package `a/b` both key as `a/b/c`. The satellite `f2` carries
    // a sole inheritance anchor into the nested package's `c`, so polish
    // genuinely consolidates it there and coupling merges the two domains
    // into one suggestion — whose twins must qualify by their real
    // package, never dedupe into a synthetic `c-2`.
    let snapshot = snapshot(
        vec![
            homed(0, "alpha", 4, 10),
            homed(1, "beta", 5, 10),
            homed(2, "gamma", 9, 10),
            homed(3, "delta", 10, 10),
        ],
        vec![
            type_ref(0, 1),
            inherits(1, 2),
            inherits(1, 3),
            inherits(2, 3),
            edge(3, 2),
            type_ref(0, 3),
        ],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "a", ScopeLevel::Package, Some(0)),
            container(2, "a/b", ScopeLevel::Domain, Some(1)),
            container(3, "a/b/c", ScopeLevel::Folder, Some(2)),
            container(4, "src/b/c/f1.ts", ScopeLevel::File, Some(3)),
            container(5, "src/b/c/f2.ts", ScopeLevel::File, Some(3)),
            container(6, "a/b", ScopeLevel::Package, Some(0)),
            container(7, "a/b/c", ScopeLevel::Domain, Some(6)),
            container(8, "a/b/c", ScopeLevel::Folder, Some(7)),
            container(9, "src/c/g1.ts", ScopeLevel::File, Some(8)),
            container(10, "src/c/g2.ts", ScopeLevel::File, Some(8)),
        ],
    );
    let mut config = config_with_k(2);
    // headroom for the genuine consolidation: the nested package's `c`
    // absorbs the sole-anchored satellite without exceeding the cap.
    config.profiles.anchored.capacity.folder = 3;
    config.profiles.greenfield.capacity.folder = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut merged_seen = false;
    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let names = tree_names(&candidate.tree);
            let suffixed: Vec<&String> = names
                .iter()
                .filter(|name| numeric_suffixed(name.as_str()))
                .collect();
            assert!(
                suffixed.is_empty(),
                "no container may carry a synthetic numeric suffix, got {suffixed:?}"
            );
            let mut folders = Vec::new();
            folder_files(&candidate.tree, &mut folders);
            assert!(
                folders.iter().all(|(name, _)| {
                    ["c", "c (a)", "c (a.b)", "a/b/c (a)", "a/b/c (a.b)"].contains(&name.as_str())
                }),
                "every folder must carry its real or qualified key, got {folders:?}"
            );
            assert!(
                folders.iter().all(|(_, files)| !files.is_empty()),
                "every rendered folder holds its real members, got {folders:?}"
            );
            let first = folder_of(&folders, "src/b/c/f1.ts");
            let second = folder_of(&folders, "src/c/g1.ts");
            merged_seen |= first == Some("c (a)") && second == Some("c (a.b)");
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
    assert!(
        merged_seen,
        "expected a merged-domain candidate qualifying the twins as \
             `c (a)` and `c (a.b)`"
    );
}

#[test]
fn should_qualify_folder_keys_that_collide_within_one_package() {
    // hand-built snapshots may reuse one bare folder key under two domains
    // of the same package. The satellite `f2` carries a sole inheritance
    // anchor into `d2`'s `shared`, so polish genuinely consolidates it
    // there and coupling merges the two domains into one suggestion —
    // whose twins must qualify by their real location, never dedupe into
    // a synthetic `shared-2`.
    let snapshot = snapshot(
        vec![
            homed(0, "alpha", 4, 10),
            homed(1, "beta", 5, 10),
            homed(2, "gamma", 8, 10),
            homed(3, "delta", 9, 10),
        ],
        vec![
            type_ref(0, 1),
            inherits(1, 2),
            inherits(1, 3),
            inherits(2, 3),
            edge(3, 2),
            type_ref(0, 3),
        ],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "pa", ScopeLevel::Package, Some(0)),
            container(2, "pa/d1", ScopeLevel::Domain, Some(1)),
            container(3, "shared", ScopeLevel::Folder, Some(2)),
            container(4, "src/shared/f1.ts", ScopeLevel::File, Some(3)),
            container(5, "src/shared/f2.ts", ScopeLevel::File, Some(3)),
            container(6, "pa/d2", ScopeLevel::Domain, Some(1)),
            container(7, "shared", ScopeLevel::Folder, Some(6)),
            container(8, "src/shared/g1.ts", ScopeLevel::File, Some(7)),
            container(9, "src/shared/g2.ts", ScopeLevel::File, Some(7)),
        ],
    );
    let mut config = config_with_k(2);
    // headroom for the genuine consolidation: `d2`'s `shared` absorbs the
    // sole-anchored satellite without exceeding the cap.
    config.profiles.anchored.capacity.folder = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut merged_seen = false;
    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let names = tree_names(&candidate.tree);
            let suffixed: Vec<&String> = names
                .iter()
                .filter(|name| numeric_suffixed(name.as_str()))
                .collect();
            assert!(
                suffixed.is_empty(),
                "no container may carry a synthetic numeric suffix, got {suffixed:?}"
            );
            let mut folders = Vec::new();
            folder_files(&candidate.tree, &mut folders);
            assert!(
                folders.iter().all(|(name, _)| {
                    ["shared", "shared (pa pa.d1)", "shared (pa pa.d2)"].contains(&name.as_str())
                }),
                "every folder must carry its real or qualified key, got {folders:?}"
            );
            let first = folder_of(&folders, "src/shared/f1.ts");
            let second = folder_of(&folders, "src/shared/g1.ts");
            merged_seen |=
                first == Some("shared (pa pa.d1)") && second == Some("shared (pa pa.d2)");
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
    assert!(
        merged_seen,
        "expected a merged-domain candidate qualifying the twins as \
             `shared (pa pa.d1)` and `shared (pa pa.d2)`"
    );
}

#[test]
fn should_keep_files_in_their_real_directories_as_folders() {
    // folders are reality: `b.ts` lives in the real `one` and `d.ts` in
    // the real `two`, so no candidate may pool them into one clustered
    // folder or invent a suffixed synthetic sibling for the overflow.
    let snapshot = over_cap_real_dir_snapshot();
    let mut config = config_with_k(3);
    config.profiles.anchored.capacity.folder = 2;
    config.profiles.greenfield.capacity.folder = 2;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let mut folders = Vec::new();
            folder_files(&candidate.tree, &mut folders);
            assert_eq!(
                folder_of(&folders, "src/svc/one/b.ts"),
                Some("one"),
                "b.ts must stay a member of its real directory, got {folders:?}"
            );
            assert_eq!(
                folder_of(&folders, "src/svc/two/d.ts"),
                Some("two"),
                "d.ts must stay a member of its real directory, got {folders:?}"
            );
            assert!(
                folders
                    .iter()
                    .all(|(name, _)| ["one", "two"].contains(&name.as_str())),
                "every folder must be one of the real directories, got {folders:?}"
            );
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
}

#[test]
fn should_relieve_an_over_cap_real_directory_through_polish() {
    // the real directory `one` holds three files against a cap of two; its
    // file `c` couples hard to `d` in under-cap `two`, so the polish pass
    // must relieve `one` by moving `c` into the real `two` — never by
    // splitting `one` into a suffixed synthetic sibling.
    let snapshot = over_cap_real_dir_snapshot();
    let mut config = config_with_k(2);
    config.profiles.anchored.capacity.folder = 2;
    config.profiles.greenfield.capacity.folder = 2;

    let candidate = analyze(&snapshot, &config)
        .ok()
        .and_then(|result| result.profiles.greenfield)
        .and_then(|mode| mode.candidates.into_iter().next());

    let mut folders = Vec::new();
    if let Some(candidate) = &candidate {
        folder_files(&candidate.tree, &mut folders);
    }
    let names: Vec<&str> = folders.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        ["one", "two"],
        "expected the two real directories as the only folders, got {folders:?}"
    );
    let by_name: BTreeMap<&str, &Vec<String>> = folders
        .iter()
        .map(|(name, files)| (name.as_str(), files))
        .collect();
    assert_eq!(
        by_name.get("one").map(|files| files.as_slice()),
        Some(&["src/svc/one/a.ts".to_owned(), "src/svc/one/b.ts".to_owned()][..]),
        "polish should relieve `one` down to its cap"
    );
    assert_eq!(
        by_name.get("two").map(|files| files.as_slice()),
        Some(&["src/svc/one/c.ts".to_owned(), "src/svc/two/d.ts".to_owned()][..]),
        "the relieving move must land `c` in the real `two`"
    );
    // the relieved layout clears every capacity breach.
    assert_eq!(
        candidate.as_ref().and_then(|candidate| {
            candidate
                .capacity_remainder
                .as_ref()
                .map(|remainder| remainder.remaining)
        }),
        Some(0),
        "the best greenfield candidate must fix the folder breach"
    );
}

#[test]
fn should_reject_file_polish_that_only_increases_cyclic_edges() {
    let snapshot = file_polish_edge_budget_snapshot();
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 4;
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let subject_vertex = solver.index_of.get(&5).copied().unwrap_or(u32::MAX);
    let subject_scc = solver
        .condensation
        .membership
        .get(subject_vertex as usize)
        .copied()
        .map_or(u32::MAX, |scc| scc.0);
    let mut parts = solver.real_partition.clone();
    let original = parts.cluster_of(subject_scc);

    solver.polish(&mut parts);

    assert_eq!(
        parts.cluster_of(subject_scc),
        original,
        "file polish must refuse an edge-only increase inside an existing quotient cycle"
    );
}

#[test]
fn file_polish_edge_budget_witness_is_invisible_to_vertex_only_comparison() {
    let snapshot = file_polish_edge_budget_snapshot();
    let tests = TestPolicy::defaults();
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 4;
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let subject_vertex = solver.index_of.get(&5).copied().unwrap_or(u32::MAX);
    let subject_scc = solver
        .condensation
        .membership
        .get(subject_vertex as usize)
        .copied()
        .map_or(u32::MAX, |scc| scc.0);
    let mut parts = solver.real_partition.clone();
    let source = parts.cluster_of(subject_scc);
    let before = parts.quotient(&solver.condensation.dag);
    let destination = solver
        .index_of
        .get(&8)
        .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
        .and_then(|scc| parts.cluster_of(scc.0));
    assert!(
        source.zip(destination).is_some_and(|(from, target)| {
            solver
                .pull_targets(&parts, subject_scc, from)
                .contains(&target)
                && !solver.absorbs_a_foreign_anchor(&parts, subject_scc, from, target)
                && !solver.strands_a_comparable_anchor(&parts, subject_scc, from, target)
                && !solver.flees_into_the_synthetic_bucket(&parts, subject_scc, from, target)
        }),
        "the control must reach file-polish evaluation"
    );
    let score_before = solver.evaluate(&parts);
    assert!(destination.is_some_and(|target| parts.move_node(subject_scc, target)));
    let after = parts.quotient(&solver.condensation.dag);
    let before_counts = CycleCounts::from_graph(&before);
    let after_counts = CycleCounts::from_graph(&after);

    assert_eq!(after_counts.vertices, before_counts.vertices);
    assert!(
        after_counts.edges > before_counts.edges,
        "the control must add only an internal edge to the existing cyclic component"
    );
    assert!(
        solver.evaluate(&parts) < score_before,
        "a vertex-only polish must accept the witness move"
    );
}

fn file_polish_edge_budget_snapshot() -> Snapshot {
    snapshot(
        vec![
            node(0, "subject", 5, Polarity::Production),
            node(1, "a_to_b", 6, Polarity::Production),
            node(2, "a_claimant", 7, Polarity::Production),
            node(3, "b_first", 8, Polarity::Production),
            node(4, "b_second", 9, Polarity::Production),
            node(5, "b_to_c", 10, Polarity::Production),
            node(6, "c_target", 11, Polarity::Production),
            node(7, "c_to_a", 12, Polarity::Production),
        ],
        vec![
            edge(3, 1),
            edge(2, 6),
            edge(7, 4),
            edge(3, 0),
            edge(4, 0),
            edge(1, 0),
        ],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "lib", ScopeLevel::Domain, Some(0)),
            container(2, "a", ScopeLevel::Folder, Some(1)),
            container(3, "b", ScopeLevel::Folder, Some(1)),
            container(4, "c", ScopeLevel::Folder, Some(1)),
            container(5, "lib/a/subject.ts", ScopeLevel::File, Some(2)),
            container(6, "lib/a/to-b.ts", ScopeLevel::File, Some(2)),
            container(7, "lib/a/claimant.ts", ScopeLevel::File, Some(2)),
            container(8, "lib/b/first.ts", ScopeLevel::File, Some(3)),
            container(9, "lib/b/second.ts", ScopeLevel::File, Some(3)),
            container(10, "lib/b/to-c.ts", ScopeLevel::File, Some(3)),
            container(11, "lib/c/target.ts", ScopeLevel::File, Some(4)),
            container(12, "lib/c/to-a.ts", ScopeLevel::File, Some(4)),
        ],
    )
}

#[test]
fn should_read_laminar_home_keys_from_the_container_chain() {
    // the laminar tree keeps source-root-stripped, package-root-resolved
    // name keys; a file under `src/` still keys to the `ai` package and the
    // `ai/adapters` domain/folder, never to a bare `src`.
    let containers = [
        container(0, "ai", ScopeLevel::PackageGroup, None),
        container(1, "ai", ScopeLevel::Package, Some(0)),
        container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
        container(3, "ai/adapters", ScopeLevel::Folder, Some(2)),
        container(4, "src/adapters/openai.ts", ScopeLevel::File, Some(3)),
    ];
    let by_id: BTreeMap<u32, &Container> = containers.iter().map(|c| (c.id.0, c)).collect();

    let home = laminar_home(&by_id, 4);
    assert_eq!(home.package, "ai");
    assert_eq!(home.domain, "ai/adapters");
    assert_eq!(home.folder, "ai/adapters");
}

#[test]
fn should_key_the_home_to_the_nearest_folder_and_package() {
    // laminar folders and package roots nest; a file's real place is the
    // NEAREST ancestor at each level (folder `…/deeper`, package `a/b`),
    // never the outermost one.
    let containers = [
        container(0, "g", ScopeLevel::PackageGroup, None),
        container(1, "a", ScopeLevel::Package, Some(0)),
        container(2, "a/b", ScopeLevel::Package, Some(1)),
        container(3, "a/b/x", ScopeLevel::Domain, Some(2)),
        container(4, "a/b/x/deep", ScopeLevel::Folder, Some(3)),
        container(5, "a/b/x/deep/deeper", ScopeLevel::Folder, Some(4)),
        container(6, "src/x/deep/deeper/f.ts", ScopeLevel::File, Some(5)),
    ];
    let by_id: BTreeMap<u32, &Container> = containers.iter().map(|c| (c.id.0, c)).collect();

    let home = laminar_home(&by_id, 6);
    assert_eq!(home.package, "a/b", "the nearest package root wins");
    assert_eq!(home.domain, "a/b/x");
    assert_eq!(home.folder, "a/b/x/deep/deeper", "the nearest folder wins");
}

#[test]
fn should_inherit_missing_levels_from_the_nearest_broader_key() {
    // a file directly in its domain directory has no deeper folder: its
    // real folder IS that directory, so the folder key inherits the domain
    // key instead of collapsing to a synthetic `workspace` twin.
    let containers = [
        container(0, "g", ScopeLevel::PackageGroup, None),
        container(1, "p", ScopeLevel::Package, Some(0)),
        container(2, "p/d", ScopeLevel::Domain, Some(1)),
        container(3, "src/f.ts", ScopeLevel::File, Some(2)),
    ];
    let by_id: BTreeMap<u32, &Container> = containers.iter().map(|c| (c.id.0, c)).collect();

    let home = laminar_home(&by_id, 3);
    assert_eq!(home.package, "p");
    assert_eq!(home.domain, "p/d");
    assert_eq!(home.folder, "p/d", "the folder inherits the domain key");
}

#[test]
fn should_not_over_strip_a_user_named_src_module() {
    // a `src` that is a real module name (not a transparent source root) is
    // preserved by the laminar tree as `ai/src`; the engine honors it rather
    // than blindly dropping every `src` segment.
    let containers = [
        container(0, "ai", ScopeLevel::PackageGroup, None),
        container(1, "ai", ScopeLevel::Package, Some(0)),
        container(2, "ai/src", ScopeLevel::Domain, Some(1)),
        container(3, "ai/src", ScopeLevel::Folder, Some(2)),
        container(4, "src/src/deep.ts", ScopeLevel::File, Some(3)),
    ];
    let by_id: BTreeMap<u32, &Container> = containers.iter().map(|c| (c.id.0, c)).collect();

    let home = laminar_home(&by_id, 4);
    assert_eq!(home.package, "ai");
    assert_eq!(home.domain, "ai/src");
    assert_eq!(home.folder, "ai/src");
}

#[test]
fn should_name_the_package_from_the_manifest_root_not_the_source_root() {
    // sources live under `src/`, but the laminar tree already resolved the
    // package to the manifest root `ai`; candidate naming must reuse that,
    // so `ai` names the package and `src` never surfaces as a container.
    let sized = |id: u32, name: &str, container: u32, size: u32| Node {
        id: NodeId(id),
        name: SmolStr::new(name),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container: ContainerId(container),
        visibility: ScopeLevel::File,
        effective_size: size,
    };
    let snapshot = snapshot(
        vec![sized(0, "openai", 4, 10), sized(1, "anthropic", 5, 7)],
        vec![edge(0, 1)],
        vec![
            container(0, "ai", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/adapters", ScopeLevel::Domain, Some(1)),
            container(3, "ai/adapters", ScopeLevel::Folder, Some(2)),
            container(4, "src/adapters/openai.ts", ScopeLevel::File, Some(3)),
            container(5, "src/adapters/anthropic.ts", ScopeLevel::File, Some(3)),
        ],
    );

    let rendered = analyze(&snapshot, &config_with_k(1)).ok().map(|result| {
        result
            .profiles
            .anchored
            .and_then(|profile| profile.candidates.into_iter().next())
            .map_or(result.current.tree, |candidate| candidate.tree)
    });

    let mut names = Vec::new();
    let mut sloc = Vec::new();
    let mut levels = Vec::new();
    if let Some(tree) = &rendered {
        collect_tree(tree, &mut names, &mut sloc, &mut levels);
    }
    assert!(!names.is_empty(), "expected a rendered analysis tree");

    let named: Vec<(&Level, &String)> = levels.iter().zip(names.iter()).collect();
    // the package elects the manifest root, not the transparent source root.
    assert!(
        named
            .iter()
            .any(|(level, name)| **level == Level::Package && name.as_str() == "ai"),
        "expected an `ai` package, got {names:?}"
    );
    // `src` leaks nowhere as an interior container (files keep their paths).
    assert!(
        named
            .iter()
            .all(|(level, name)| **level == Level::File || name.as_str() != "src"),
        "src leaked as an interior container: {names:?}"
    );
}

/// True when a rendered name is a bare number — every byte a digit.
fn bare_numeric(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

/// Collects every `(level, rendered name)` pair of a candidate tree,
/// pre-order.
fn level_names(tree: &ContainerNode) -> Vec<(Level, String)> {
    let (mut names, mut sloc, mut levels) = (Vec::new(), Vec::new(), Vec::new());
    collect_tree(tree, &mut names, &mut sloc, &mut levels);
    levels.into_iter().zip(names).collect()
}

/// Returns the rendered Domain-level names of a candidate tree.
fn domain_names(tree: &ContainerNode) -> Vec<String> {
    level_names(tree)
        .into_iter()
        .filter(|(level, _)| *level == Level::Domain)
        .map(|(_, name)| name)
        .collect()
}

#[test]
fn should_never_elect_the_mixed_label() {
    // three homes with no strict majority and no shared prefix. The
    // satellite `s` carries a sole inheritance anchor into `d2`, so polish
    // genuinely consolidates it there, and the weak type-reference triangle
    // (kept acyclic so the file graph never welds the folders into one
    // immovable atom) couples all three folders into one grab-bag domain.
    // Votes follow real homes — {d1:48, d2:40, d3:38}, no strict majority —
    // so the election must reach the top-two join and produce a real
    // composite name — never the synthetic `mixed` label.
    let snapshot = snapshot(
        vec![
            homed(0, "a", 2, 24),
            homed(1, "b", 3, 24),
            homed(2, "c", 5, 22),
            homed(3, "d", 6, 18),
            homed(4, "e", 8, 15),
            homed(5, "f", 9, 14),
            homed(6, "s", 10, 9),
        ],
        vec![
            edge(0, 1),
            edge(2, 3),
            inherits(3, 2),
            edge(4, 5),
            type_ref(0, 2),
            type_ref(1, 4),
            type_ref(5, 2),
            inherits(6, 2),
        ],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "d1", ScopeLevel::Domain, Some(0)),
            container(2, "src/d1/a.ts", ScopeLevel::File, Some(1)),
            container(3, "src/d1/b.ts", ScopeLevel::File, Some(1)),
            container(4, "d2", ScopeLevel::Domain, Some(0)),
            container(5, "src/d2/c.ts", ScopeLevel::File, Some(4)),
            container(6, "src/d2/d.ts", ScopeLevel::File, Some(4)),
            container(7, "d3", ScopeLevel::Domain, Some(0)),
            container(8, "src/d3/e.ts", ScopeLevel::File, Some(7)),
            container(9, "src/d3/f.ts", ScopeLevel::File, Some(7)),
            container(10, "src/d3/s.ts", ScopeLevel::File, Some(7)),
        ],
    );
    let mut config = config_with_k(2);
    // headroom for the genuine consolidation, and a domain cap that
    // admits all three coupled folders into one cluster.
    config.profiles.anchored.capacity.folder = 3;
    config.profiles.anchored.capacity.domain = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut merged_seen = false;
    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let names = tree_names(&candidate.tree);
            assert!(
                names
                    .iter()
                    .all(|name| name != "mixed" && !name.ends_with("/mixed")),
                "no container may carry the synthetic mixed label, got {names:?}"
            );
            let domains = domain_names(&candidate.tree);
            merged_seen |= domains == ["d1/d2"];
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
    assert!(
        merged_seen,
        "expected the merged grab-bag domain to elect the top-two join \
             `d1/d2`"
    );
}

/// Builds the all-numeric fixture: real package `2024` holding real
/// directories `2024/x` and `2024/y`, two cross-coupled files each.
fn numeric_home_snapshot() -> Snapshot {
    snapshot(
        vec![
            homed(0, "a", 3, 20),
            homed(1, "b", 4, 20),
            homed(2, "c", 6, 20),
            homed(3, "d", 7, 20),
        ],
        vec![edge(0, 2), edge(1, 3)],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "2024", ScopeLevel::Package, Some(0)),
            container(2, "2024/x", ScopeLevel::Folder, Some(1)),
            container(3, "src/x/a.ts", ScopeLevel::File, Some(2)),
            container(4, "src/x/b.ts", ScopeLevel::File, Some(2)),
            container(5, "2024/y", ScopeLevel::Folder, Some(1)),
            container(6, "src/y/c.ts", ScopeLevel::File, Some(5)),
            container(7, "src/y/d.ts", ScopeLevel::File, Some(5)),
        ],
    )
}

#[test]
fn should_never_elect_a_bare_numeric_name() {
    // a real package directory named `2024` holds every vote: the elected
    // package and domain names of every emitted candidate must still never
    // surface as a bare number. Identity clones are exempt — they carry
    // the real tree verbatim, and folders always keep their real names.
    let snapshot = numeric_home_snapshot();
    let mut config = config_with_k(2);
    // headroom of one over the two-file directories, so a moved-file
    // partition exists and at least one candidate is emitted (elected)
    // rather than cloned.
    config.profiles.anchored.capacity.folder = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut emitted = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            if candidate.delta_narration.is_empty() {
                continue;
            }
            emitted += 1;
            let elected: Vec<(Level, String)> = level_names(&candidate.tree)
                .into_iter()
                .filter(|(level, _)| {
                    matches!(level, Level::Domain | Level::Package | Level::PackageGroup)
                })
                .collect();
            assert!(
                elected
                    .iter()
                    .all(|(_, name)| !bare_numeric(name) && !numeric_suffixed(name)),
                "no elected container may render as a bare number or a \
                     numeric-suffixed twin, got {elected:?}"
            );
        }
    }
    assert!(
        emitted > 0,
        "expected at least one emitted (elected) candidate"
    );
}

#[test]
fn should_elect_the_strict_majority_home_verbatim() {
    // rung R1: `apps/d1` holds 40 of 50 SLOC — a strict majority — so the
    // merged domain takes that home key whole. The old cumulative rewrite
    // truncated a key foreign to its package to `workspace/d1`, rendering
    // `d1` and hiding the real `apps` prefix.
    let snapshot = snapshot(
        vec![
            homed(0, "a", 2, 20),
            homed(1, "b", 3, 20),
            homed(2, "c", 5, 5),
            homed(3, "d", 6, 5),
        ],
        vec![edge(0, 2), edge(1, 3)],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "apps/d1", ScopeLevel::Domain, Some(0)),
            container(2, "src/d1/a.ts", ScopeLevel::File, Some(1)),
            container(3, "src/d1/b.ts", ScopeLevel::File, Some(1)),
            container(4, "beta/d2", ScopeLevel::Domain, Some(0)),
            container(5, "src/d2/c.ts", ScopeLevel::File, Some(4)),
            container(6, "src/d2/d.ts", ScopeLevel::File, Some(4)),
        ],
    );
    let mut config = config_with_k(2);
    config.profiles.anchored.capacity.folder = 2;

    let analyzed = analyze(&snapshot, &config).ok();
    let mut majority_seen = analyzed.as_ref().is_some_and(|result| {
        domain_names(&result.current.tree)
            .iter()
            .any(|name| name == "apps/d1")
    });
    let modes = analyzed.map_or_else(Profiles::default, |result| result.profiles);
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            let domains = domain_names(&candidate.tree);
            assert!(
                domains.iter().all(|name| name != "d1"),
                "a majority home must render whole, not truncated to its \
                     last segment, got {domains:?}"
            );
            majority_seen |= domains.iter().any(|name| name == "apps/d1");
        }
    }
    assert!(
        majority_seen,
        "expected the strict-majority home to render by its full real \
             key `apps/d1`"
    );
}

#[test]
fn should_name_a_balanced_cluster_by_the_shared_home_prefix() {
    // rung R2: `ai/app` and `ai/core` tie at 40 SLOC of real homes — no
    // strict majority — but share the `ai` prefix, so the merged domain
    // elects `ai` rather than misnaming the whole after one tied side.
    // The satellite `b` carries a sole call anchor into `ai/core`, so
    // polish genuinely consolidates it there while every vote keeps its
    // real home. The elected name repeats the package's key, so the
    // redundant domain level is suppressed at the render boundary: the
    // merged candidate shows no domain node at all, its folders hanging
    // directly under the `ai` package.
    let snapshot = snapshot(
        vec![
            homed(0, "a", 3, 20),
            homed(1, "b", 4, 20),
            homed(2, "c", 6, 20),
            homed(3, "d", 7, 20),
        ],
        vec![type_ref(0, 1), type_ref(0, 2), edge(1, 2), inherits(2, 3)],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/app", ScopeLevel::Domain, Some(1)),
            container(3, "src/app/a.ts", ScopeLevel::File, Some(2)),
            container(4, "src/app/b.ts", ScopeLevel::File, Some(2)),
            container(5, "ai/core", ScopeLevel::Domain, Some(1)),
            container(6, "src/core/c.ts", ScopeLevel::File, Some(5)),
            container(7, "src/core/d.ts", ScopeLevel::File, Some(5)),
        ],
    );
    let mut config = config_with_k(2);
    // headroom for the genuine consolidation: `ai/core` absorbs the
    // sole-anchored satellite without exceeding the cap. Three is one file
    // short of that promise — the merged candidate it accepted carried
    // `capacity: 1.3333` and a total of `1.5075`, four times worse than the
    // split it was supposed to beat. Five is the smallest budget under
    // which the merge is genuinely legal, and it then scores `0.125`.
    config.profiles.anchored.capacity.folder = 5;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut prefix_seen = false;
    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let domains = domain_names(&candidate.tree);
            prefix_seen |= domains.is_empty();
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
    assert!(
        prefix_seen,
        "expected the balanced merged domain to elect the shared home \
             prefix `ai` and be suppressed as a redundant echo of the package"
    );
}

#[test]
fn should_join_the_top_two_homes_when_no_prefix_is_shared() {
    // rung R3: `ai/app` and `bi/app` tie at 30 SLOC of real homes with no
    // shared prefix, so the merged domain joins the two homes — ranked by
    // production SLOC then file count, so the exact SLOC tie falls to file
    // count and `bi/app` leads despite `ai/app` sorting first. The
    // satellite `d` carries a sole call anchor into `ai/app`, so polish
    // genuinely consolidates it there while every vote keeps its real
    // home, and the weak `a`-to-`c` reference keeps the packages coupled
    // into one suggestion.
    let snapshot = snapshot(
        vec![
            homed(0, "a", 3, 15),
            homed(1, "b", 4, 15),
            homed(2, "c", 7, 10),
            homed(3, "d", 8, 10),
            homed(4, "e", 9, 10),
        ],
        vec![
            inherits(0, 1),
            type_ref(0, 2),
            edge(3, 1),
            inherits(2, 4),
            edge(4, 2),
            type_ref(4, 3),
        ],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "ai", ScopeLevel::Package, Some(0)),
            container(2, "ai/app", ScopeLevel::Domain, Some(1)),
            container(3, "src/a.ts", ScopeLevel::File, Some(2)),
            container(4, "src/b.ts", ScopeLevel::File, Some(2)),
            container(5, "bi", ScopeLevel::Package, Some(0)),
            container(6, "bi/app", ScopeLevel::Domain, Some(5)),
            container(7, "src/c.ts", ScopeLevel::File, Some(6)),
            container(8, "src/d.ts", ScopeLevel::File, Some(6)),
            container(9, "src/e.ts", ScopeLevel::File, Some(6)),
        ],
    );
    let mut config = config_with_k(2);
    // headroom for the genuine consolidation: `ai/app` absorbs the
    // sole-anchored satellite without exceeding the cap.
    config.profiles.anchored.capacity.folder = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut joined_seen = false;
    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let containers = level_names(&candidate.tree);
            let joined_domain = containers
                .iter()
                .any(|(level, name)| *level == Level::Domain && name == "bi/app/ai/app");
            let joined_package = containers
                .iter()
                .any(|(level, name)| *level == Level::Package && name == "bi/ai");
            joined_seen |= joined_domain && joined_package;
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
    assert!(
        joined_seen,
        "expected the merged levels to join their top-two homes as \
             `bi/app/ai/app` under `bi/ai`"
    );
}

#[test]
fn should_fall_to_the_dominant_token_when_the_join_is_numeric() {
    // rung R4: year directories `2024/2025` and `2024/2026` outweigh
    // `2024/shared`, but their top-two join is all-digit segments — unfit
    // — so the election falls to the dominant non-numeric token `shared`
    // rather than a bare-number composite or the old `mixed` label. The
    // satellite `e` carries a sole inheritance anchor into `2024/2026`,
    // so polish genuinely consolidates it there while the `a`-to-`c` and
    // `f`-to-`c` references keep all three folders one coupled suggestion.
    let snapshot = snapshot(
        vec![
            homed(0, "a", 3, 20),
            homed(1, "b", 4, 20),
            homed(2, "c", 6, 20),
            homed(3, "d", 7, 20),
            homed(4, "e", 9, 10),
            homed(5, "f", 10, 10),
            homed(6, "g", 11, 10),
        ],
        vec![
            edge(0, 1),
            type_ref(0, 2),
            inherits(2, 3),
            edge(3, 2),
            inherits(4, 3),
            edge(5, 6),
            inherits(6, 5),
            type_ref(5, 2),
        ],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "2024", ScopeLevel::Package, Some(0)),
            container(2, "2024/2025", ScopeLevel::Domain, Some(1)),
            container(3, "src/2025/a.ts", ScopeLevel::File, Some(2)),
            container(4, "src/2025/b.ts", ScopeLevel::File, Some(2)),
            container(5, "2024/2026", ScopeLevel::Domain, Some(1)),
            container(6, "src/2026/c.ts", ScopeLevel::File, Some(5)),
            container(7, "src/2026/d.ts", ScopeLevel::File, Some(5)),
            container(8, "2024/shared", ScopeLevel::Domain, Some(1)),
            container(9, "src/shared/e.ts", ScopeLevel::File, Some(8)),
            container(10, "src/shared/f.ts", ScopeLevel::File, Some(8)),
            container(11, "src/shared/g.ts", ScopeLevel::File, Some(8)),
        ],
    );
    let mut config = config_with_k(2);
    // headroom for the genuine consolidation, and a domain cap that
    // admits all three coupled folders into one cluster.
    config.profiles.anchored.capacity.folder = 3;
    config.profiles.anchored.capacity.domain = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut token_seen = false;
    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let names = tree_names(&candidate.tree);
            assert!(
                names
                    .iter()
                    .all(|name| name != "mixed" && !name.ends_with("/mixed")),
                "no container may carry the synthetic mixed label, got {names:?}"
            );
            token_seen |= domain_names(&candidate.tree) == ["shared"];
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
    assert!(
        token_seen,
        "expected the numeric grab-bag domain to elect the dominant \
             non-numeric token `shared`"
    );
}

#[test]
fn should_wrap_a_bare_numeric_last_resort_with_its_anchor() {
    // rung R5: every home key is the all-digit `2024`, so no rung can
    // yield a fit name and the last resort wraps the key with its anchor
    // folder — `2024 (2024.x)` — never a bare number. The real `2024/x`
    // and `2024/y` folders keep their real keys. The domain elects the
    // same wrap as its package, so the redundant domain level is
    // suppressed at the render boundary; the wrap survives at the package.
    let snapshot = numeric_home_snapshot();
    let mut config = config_with_k(2);
    // headroom of one so a moved-file partition exists and at least one
    // candidate is emitted (elected) rather than cloned.
    config.profiles.anchored.capacity.folder = 3;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut wrapped_seen = false;
    let mut emitted = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            if candidate.delta_narration.is_empty() {
                continue;
            }
            emitted += 1;
            let containers = level_names(&candidate.tree);
            let package_wrapped = containers
                .iter()
                .any(|(level, name)| *level == Level::Package && name == "2024 (2024.x)");
            let domain_suppressed = !containers.iter().any(|(level, _)| *level == Level::Domain);
            let mut folders = Vec::new();
            folder_files(&candidate.tree, &mut folders);
            let folder_real = folders.iter().any(|(name, _)| name.starts_with("2024/"));
            wrapped_seen |= package_wrapped && domain_suppressed && folder_real;
        }
    }
    assert!(
        emitted > 0,
        "expected at least one emitted (elected) candidate"
    );
    assert!(
        wrapped_seen,
        "expected the all-numeric home to elect the anchored wrap \
             `2024 (2024.x)` at the package, suppress the echoing domain, and \
             keep the real folders"
    );
}

#[test]
fn should_disambiguate_name_collisions_with_a_home_qualifier_not_an_integer() {
    // a domain cap of one splits the real `pa/app` home into two sibling
    // domain clusters that elect the same name. The satellite `b` carries
    // a sole call anchor into `y`, so polish genuinely consolidates it
    // there while the `a`-`e` spine pins `x`'s stayers — and the twins
    // must still qualify by their anchor folders, never dedupe into a
    // synthetic `app-2`.
    let snapshot = snapshot(
        vec![
            homed(0, "a", 4, 10),
            homed(1, "b", 5, 10),
            homed(2, "e", 6, 10),
            homed(3, "c", 8, 10),
            homed(4, "d", 9, 10),
        ],
        vec![
            edge(0, 2),
            inherits(2, 0),
            type_ref(0, 1),
            edge(1, 3),
            inherits(3, 4),
        ],
        vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "pa", ScopeLevel::Package, Some(0)),
            container(2, "pa/app", ScopeLevel::Domain, Some(1)),
            container(3, "pa/app/x", ScopeLevel::Folder, Some(2)),
            container(4, "src/app/x/a.ts", ScopeLevel::File, Some(3)),
            container(5, "src/app/x/b.ts", ScopeLevel::File, Some(3)),
            container(6, "src/app/x/e.ts", ScopeLevel::File, Some(3)),
            container(7, "pa/app/y", ScopeLevel::Folder, Some(2)),
            container(8, "src/app/y/c.ts", ScopeLevel::File, Some(7)),
            container(9, "src/app/y/d.ts", ScopeLevel::File, Some(7)),
        ],
    );
    let mut config = config_with_k(1);
    // headroom for the genuine consolidation, and a domain cap that
    // forces the two sibling clusters apart.
    config.profiles.anchored.capacity.folder = 3;
    config.profiles.anchored.capacity.domain = 1;
    config.profiles.greenfield.capacity.folder = 3;
    config.profiles.greenfield.capacity.domain = 1;

    let modes = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();

    let mut qualified_seen = false;
    let mut seen = 0;
    for mode in [modes.anchored, modes.greenfield].into_iter().flatten() {
        for candidate in &mode.candidates {
            seen += 1;
            let names = tree_names(&candidate.tree);
            let suffixed: Vec<&String> = names
                .iter()
                .filter(|name| numeric_suffixed(name.as_str()))
                .collect();
            assert!(
                suffixed.is_empty(),
                "colliding elected names must qualify by their homes, \
                     never a numeric twin, got {suffixed:?}"
            );
            let domains = domain_names(&candidate.tree);
            qualified_seen |= domains == ["app (pa.app.x)".to_owned(), "app (pa.app.y)".to_owned()];
        }
    }
    assert!(seen > 0, "expected at least one candidate across the modes");
    assert!(
        qualified_seen,
        "expected the tied sibling domains to qualify by their anchor \
             folders `app (pa.app.x)` and `app (pa.app.y)`"
    );
}

/// Builds an over-capacity fixture file living in the synthetic `hub`
/// folder with a `{stem}_{index}.py` basename.
fn relief_file(index: usize, stem: &str) -> FileInfo {
    FileInfo {
        container: u32::try_from(index).unwrap_or(u32::MAX),
        name: SmolStr::new(format!("{stem}_{index:02}.py")),
        production_sloc: 10,
        namespace: SmolStr::new(""),
        home: LaminarHome {
            folder: SmolStr::new("hub"),
            domain: SmolStr::new("hub"),
            package: SmolStr::new("app"),
            synthetic: false,
        },
    }
}

#[test]
fn should_accumulate_transitive_binding_over_the_flat_candidate_tree() {
    let tree = ContainerTree::new(vec![
        container(0, "app", ScopeLevel::PackageGroup, None),
        container(1, "hub", ScopeLevel::Domain, Some(0)),
        container(2, "hub/ingest", ScopeLevel::Folder, Some(1)),
        container(3, "hub/ingest/a.py", ScopeLevel::File, Some(2)),
        container(4, "hub/ingest/b.py", ScopeLevel::File, Some(2)),
        container(5, "hub/ingest/c.py", ScopeLevel::File, Some(2)),
        container(6, "hub/emit", ScopeLevel::Folder, Some(1)),
        container(7, "hub/emit/d.py", ScopeLevel::File, Some(6)),
    ]);

    // `ingest` binds 3 files → one over-budget share against budget 2;
    // `emit` stays within budget; the domain binds all 4 → two shares.
    // Ancestor binding is the point: nesting cannot dodge the budget.
    assert!((binding_pressure(&tree, 2) - 1.5).abs() < 1e-9);
    // everything within budget charges nothing.
    assert!(binding_pressure(&tree, 4).abs() < 1e-9);
    // a zero budget disables the term entirely.
    assert!(binding_pressure(&tree, 0).abs() < 1e-9);
}

#[test]
fn should_score_folder_pressure_from_immediate_entries_only() {
    let tree = ContainerTree::new(vec![
        container(0, "workspace", ScopeLevel::PackageGroup, None),
        container(1, "section", ScopeLevel::Folder, Some(0)),
        container(2, "section/entry.ts", ScopeLevel::File, Some(1)),
        container(3, "section/nested", ScopeLevel::Folder, Some(1)),
        container(4, "section/nested/a.ts", ScopeLevel::File, Some(3)),
        container(5, "section/nested/b.ts", ScopeLevel::File, Some(3)),
        container(6, "section/nested/c.ts", ScopeLevel::File, Some(3)),
    ]);

    assert!(
        (binding_pressure(&tree, 2) - 0.5).abs() < 1e-9,
        "only the nested folder exceeds the two-entry budget"
    );
}

#[test]
fn should_count_binding_folders_through_an_intermediate_domain() {
    // QUAL-P3-2 witness: `dom` directly holds only one child (the nested
    // `sub`), but three file-binding folders live beneath it. Direct-child
    // counting stayed silent at cap 2; deep counting fires on both levels.
    let tree = interior(
        "root",
        Level::PackageGroup,
        vec![interior(
            "pkg",
            Level::Package,
            vec![interior(
                "dom",
                Level::Domain,
                vec![interior(
                    "sub",
                    Level::Domain,
                    vec![
                        folder("x", vec![file("f1", 10)]),
                        folder("y", vec![file("f2", 10)]),
                        folder("z", vec![file("f3", 10)]),
                    ],
                )],
            )],
        )],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.domain = 2;

    let findings = walk_all_capacity(&tree, &config);

    let domains: Vec<Vec<String>> = findings
        .iter()
        .filter(|(level, _)| *level == Level::Domain)
        .map(|(_, violation)| violation.location.clone())
        .collect();
    assert_eq!(
        domains,
        vec![
            vec!["root".to_owned(), "pkg".to_owned(), "dom".to_owned()],
            vec![
                "root".to_owned(),
                "pkg".to_owned(),
                "dom".to_owned(),
                "sub".to_owned()
            ],
        ]
    );
}

#[test]
fn should_split_an_over_capacity_folder_along_priced_connectivity() {
    // 24 files in one real-folder cluster against budget 20: two priced
    // 12-file chains (`ingest`, `emit`) with no cross-chain edges, so
    // connectivity elects exactly the two halves a human would cut.
    let file_count = 24;
    let files: Vec<FileInfo> = (0..file_count)
        .map(|index| {
            if index < 12 {
                relief_file(index, "ingest")
            } else {
                relief_file(index, "emit")
            }
        })
        .collect();
    let condensation = singleton_condensation(file_count);
    let chain = |start: u32| {
        (start..start + 11)
            .map(|vertex| (vertex, vertex + 1, 1.0_f32))
            .collect::<Vec<_>>()
    };
    let mut edges = chain(0);
    edges.extend(chain(12));
    let graph = Csr::from_weighted_edges(file_count, &edges);
    let base = Partition::from_assignment(vec![ClusterId(0); file_count], 1);

    let (relieved, partition, names, synthetic) = relieve_over_capacity(
        files,
        &condensation,
        &graph,
        &base,
        &[SmolStr::new("hub")],
        &[false],
        20,
    );

    assert_eq!(partition.cluster_count(), 2);
    for index in 0..12usize {
        let scc = u32::try_from(index).unwrap_or(u32::MAX);
        assert_eq!(
            partition.cluster_of(scc),
            Some(ClusterId(0)),
            "pile 0 keeps the original cluster id"
        );
        let home = relieved.get(index).map(|file| file.home.domain.as_str());
        assert_eq!(home, Some("hub"));
    }
    for index in 12..24usize {
        let scc = u32::try_from(index).unwrap_or(u32::MAX);
        assert_eq!(
            partition.cluster_of(scc),
            Some(ClusterId(1)),
            "the later pile gets the fresh cluster id"
        );
        let home = relieved.get(index).map(|file| file.home.domain.as_str());
        assert_eq!(
            home,
            Some("hub"),
            "split halves never rewrite member homes — the '/' label alone \
                 nests the half under its base folder"
        );
    }
    assert_eq!(names, vec![SmolStr::new("hub"), SmolStr::new("hub/emit")]);
    assert_eq!(synthetic, vec![false, false]);
}

#[test]
fn should_project_a_mixed_namespace_scc_into_every_occupied_root() {
    let files: Vec<FileInfo> = (0..4)
        .map(|index| {
            let mut file = relief_file(index, "joined");
            file.namespace = SmolStr::new(if index < 2 { "left" } else { "right" });
            file
        })
        .collect();
    let condensation = Condensation {
        dag: Csr::from_sorted_edges(1, &[]),
        membership: vec![SccId(0); 4],
        members: vec![vec![NodeId(0), NodeId(1), NodeId(2), NodeId(3)]],
    };
    let graph = Csr::from_sorted_edges(4, &[]);
    let base = Partition::from_assignment(vec![ClusterId(0)], 1);

    let by_namespace = relief_sccs_by_namespace(&[0], &condensation, &files);

    assert_eq!(by_namespace.get("left"), Some(&BTreeMap::from([(0, 2)])));
    assert_eq!(by_namespace.get("right"), Some(&BTreeMap::from([(0, 2)])));

    let (files, partition, names, _) = relieve_over_capacity(
        files,
        &condensation,
        &graph,
        &base,
        &[SmolStr::new("hub")],
        &[false],
        1,
    );

    assert_eq!(partition.cluster_count(), 2);
    assert_eq!(partition.cluster_of(0), Some(ClusterId(1)));
    let child_entries = relief_child_entries(&partition, &names, &condensation, &files);
    assert_eq!(child_entries.get(&(0, SmolStr::new("left"))), Some(&1));
    assert_eq!(child_entries.get(&(0, SmolStr::new("right"))), Some(&1));
}

#[test]
fn should_relieve_one_connected_pile_beside_stationary_files() {
    let file_count = 21;
    let files: Vec<FileInfo> = (0..file_count)
        .map(|index| relief_file(index, if index < 11 { "loose" } else { "joined" }))
        .collect();
    let condensation = singleton_condensation(file_count);
    let edges: Vec<(u32, u32, f32)> = (11..20)
        .map(|vertex| (vertex, vertex + 1, 1.0_f32))
        .collect();
    let graph = Csr::from_weighted_edges(file_count, &edges);
    let base = Partition::from_assignment(vec![ClusterId(0); file_count], 1);

    let (_, partition, names, _) = relieve_over_capacity(
        files,
        &condensation,
        &graph,
        &base,
        &[SmolStr::new("hub")],
        &[false],
        20,
    );

    assert_eq!(partition.cluster_count(), 2);
    assert!(
        (0..11).all(|scc| partition.cluster_of(scc) == Some(ClusterId(0))),
        "unconnected stationary files remain in the parent"
    );
    assert!(
        (11..21).all(|scc| partition.cluster_of(scc) == Some(ClusterId(1))),
        "the sole evidence-backed pile moves beneath one child"
    );
    assert_eq!(names, vec![SmolStr::new("hub"), SmolStr::new("hub/joined")]);
}

#[test]
fn should_relieve_both_connected_piles_when_an_existing_child_consumes_budget() {
    let file_count = 13;
    let files: Vec<FileInfo> = (0..file_count)
        .map(|index| {
            let stem = match index {
                0..=5 => "amber",
                6..=9 => "brisk",
                10..=11 => "loose",
                _ => "existing",
            };
            relief_file(index, stem)
        })
        .collect();
    let condensation = singleton_condensation(file_count);
    let mut edges: Vec<(u32, u32, f32)> =
        (0..5).map(|vertex| (vertex, vertex + 1, 1.0_f32)).collect();
    edges.extend((6..9).map(|vertex| (vertex, vertex + 1, 1.0_f32)));
    let graph = Csr::from_weighted_edges(file_count, &edges);
    let mut assignment = vec![ClusterId(0); file_count];
    if let Some(existing) = assignment.get_mut(12) {
        *existing = ClusterId(1);
    }
    let base = Partition::from_assignment(assignment, 2);

    let (_, partition, names, _) = relieve_over_capacity(
        files,
        &condensation,
        &graph,
        &base,
        &[SmolStr::new("hub"), SmolStr::new("hub/existing")],
        &[false, false],
        9,
    );

    assert_eq!(partition.cluster_count(), 4);
    assert!(
        (0..10).all(|scc| partition.cluster_of(scc) != Some(ClusterId(0))),
        "both connected piles move because keeping either would exceed the parent budget"
    );
    assert!(
        (10..12).all(|scc| partition.cluster_of(scc) == Some(ClusterId(0))),
        "unconnected stationary files remain in the parent"
    );
    assert_eq!(
        names,
        vec![
            SmolStr::new("hub"),
            SmolStr::new("hub/existing"),
            SmolStr::new("hub/amber"),
            SmolStr::new("hub/brisk"),
        ]
    );
}

#[test]
fn should_not_relieve_a_folder_into_more_immediate_entries_than_its_cap() {
    let file_count = 6;
    let labels = ["amber", "brisk", "calm"];
    let files: Vec<FileInfo> = (0..file_count)
        .map(|index| relief_file(index, labels.get(index / 2).copied().unwrap_or_default()))
        .collect();
    let condensation = singleton_condensation(file_count);
    let graph = Csr::from_weighted_edges(file_count, &[(0, 1, 1.0), (2, 3, 1.0), (4, 5, 1.0)]);
    let base = Partition::from_assignment(vec![ClusterId(0); file_count], 1);

    let (_, partition, names, _) = relieve_over_capacity(
        files,
        &condensation,
        &graph,
        &base,
        &[SmolStr::new("hub")],
        &[false],
        2,
    );

    let direct_files = (0..file_count)
        .filter(|index| {
            partition.cluster_of(u32::try_from(*index).unwrap_or(u32::MAX)) == Some(ClusterId(0))
        })
        .count();
    let child_folders = names.iter().filter(|name| name.contains('/')).count();
    assert!(
        direct_files + child_folders <= 2,
        "new relief folders consume parent entries: {direct_files} files + {child_folders} folders"
    );
}

#[test]
fn should_move_all_oversized_chunks_when_the_parent_cannot_keep_the_first() {
    // one 22-file priced chain against budget 20 is a single oversized
    // component: BFS chunks it into 20 + 2, and the numeric-only basenames
    // yield no alphabetic stem. Keeping the 20-file chunk beside one relief
    // child would consume 21 parent entries, so both connected chunks move
    // into numeric children and the parent is left with two entries.
    let file_count = 22;
    let files: Vec<FileInfo> = (0..file_count)
        .map(|index| relief_file(index, "123"))
        .collect();
    let condensation = singleton_condensation(file_count);
    let edges: Vec<(u32, u32, f32)> = (0..21).map(|v| (v, v + 1, 1.0_f32)).collect();
    let graph = Csr::from_weighted_edges(file_count, &edges);
    let base = Partition::from_assignment(vec![ClusterId(0); file_count], 1);

    let (relieved, partition, names, _) = relieve_over_capacity(
        files,
        &condensation,
        &graph,
        &base,
        &[SmolStr::new("hub")],
        &[false],
        20,
    );

    assert_eq!(partition.cluster_count(), 3);
    let moved: Vec<usize> = (0..u32::try_from(file_count).unwrap_or(u32::MAX))
        .filter(|scc| partition.cluster_of(*scc) != Some(ClusterId(0)))
        .map(|scc| usize::try_from(scc).unwrap_or(usize::MAX))
        .collect();
    assert_eq!(moved, (0..file_count).collect::<Vec<_>>());
    assert!(
        moved.iter().all(|&index| relieved
            .get(index)
            .is_some_and(|file| file.home.domain == "hub")),
        "numeric relief groups keep their base domain"
    );
    assert_eq!(names.get(1), Some(&SmolStr::new("hub/1")));
    assert_eq!(names.get(2), Some(&SmolStr::new("hub/2")));
}

#[test]
fn should_not_invent_halves_for_unconnected_members() {
    // 22 mutually unconnected files against budget 20: no pile is joined
    // by priced evidence, so the folder stays whole — the objective prices
    // the binding and the search may still relieve it by moving files into
    // folders that actually pull them.
    let file_count = 22;
    let files: Vec<FileInfo> = (0..file_count)
        .map(|index| relief_file(index, "solo"))
        .collect();
    let condensation = singleton_condensation(file_count);
    let graph = Csr::from_weighted_edges(file_count, &[]);
    let base = Partition::from_assignment(vec![ClusterId(0); file_count], 1);

    let (relieved, partition, names, _) = relieve_over_capacity(
        files,
        &condensation,
        &graph,
        &base,
        &[SmolStr::new("hub")],
        &[false],
        20,
    );

    assert_eq!(partition.cluster_count(), 1);
    assert!(relieved.iter().all(|file| file.home.domain == "hub"));
    assert_eq!(names, vec![SmolStr::new("hub")]);
}

#[test]
fn should_never_nominate_a_folder_connected_only_by_zero_priced_edges() {
    // the welding shape: `barrel/index.ts` re-exports two unrelated
    // directories while one priced import ties render to auth. A free edge
    // carries no evidence two folders belong together (the FIX04 doctrine),
    // so it must nominate nothing: polish is never offered a move that
    // welds files from different real directories into one folder.
    let snapshot = snapshot(
        vec![
            node(0, "login", 3, Polarity::Production),
            node(1, "session", 4, Polarity::Production),
            node(2, "canvas", 5, Polarity::Production),
            node(3, "index", 6, Polarity::Production),
        ],
        vec![
            edge(0, 1),     // login -> session: priced, inside auth.
            edge(2, 0),     // canvas -> login: priced, render pulls on auth.
            reexport(3, 0), // barrel -> login: free.
            reexport(3, 1), // barrel -> session: free.
            reexport(3, 2), // barrel -> canvas: free.
        ],
        vec![
            container(0, "auth", ScopeLevel::Folder, None),
            container(1, "render", ScopeLevel::Folder, None),
            container(2, "barrel", ScopeLevel::Folder, None),
            container(3, "auth/login.ts", ScopeLevel::File, Some(0)),
            container(4, "auth/session.ts", ScopeLevel::File, Some(0)),
            container(5, "render/canvas.ts", ScopeLevel::File, Some(1)),
            container(6, "barrel/index.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(
        &snapshot,
        &AnalyzeConfig::default(),
        Coefficients::anchored(),
        false,
        &tests,
    );
    let parts = &solver.real_partition;
    let scc_of = |file_container: u32| -> u32 {
        let vertex = solver
            .index_of
            .get(&file_container)
            .copied()
            .unwrap_or(u32::MAX);
        solver
            .condensation
            .membership
            .get(vertex as usize)
            .map_or(0, |scc| scc.0)
    };

    // three real directories start in three distinct clusters.
    let auth = parts.cluster_of(scc_of(3)).unwrap_or(ClusterId(0));
    let render = parts.cluster_of(scc_of(5)).unwrap_or(ClusterId(0));
    let barrel = parts.cluster_of(scc_of(6)).unwrap_or(ClusterId(0));
    assert_eq!(parts.cluster_count(), 3);
    assert_ne!(auth, render);
    assert_ne!(auth, barrel);
    assert_ne!(render, barrel);

    // login's cross-folder pull comes only from the priced import: render
    // is nominated, but the free barrel edge nominates nothing.
    let targets = solver.pull_targets(parts, scc_of(3), auth);
    assert_eq!(targets, vec![render]);

    // the barrel's own folder connects only through free re-exports: no
    // move target exists for it at all.
    let barrel_targets = solver.pull_targets(parts, scc_of(6), barrel);
    assert!(
        barrel_targets.is_empty(),
        "zero-priced edges must not nominate any move target"
    );
}

#[test]
fn should_never_absorb_a_multi_folder_bridge_into_one_side() {
    // the inversion shape under greenfield coefficients (the mode where the
    // FIX05 defect churns): `main.ts` calls into two sibling features while
    // each feature is internally cohesive. Absorbing the facade into one
    // feature strands its edges to the other at package height, yet every
    // locally-scored statistic of the absorber improves — so the unguarded
    // objective ratifies the fold and greenfield out-churns anchored,
    // inverting the product promise. Bridge integrity vetoes the fold: a
    // folder that does not already contain an SCC's whole priced
    // neighborhood may not absorb it.
    let snapshot = snapshot(
        vec![
            node(0, "Shape", 4, Polarity::Production),
            node(1, "area_of", 5, Polarity::Production),
            node(2, "scale", 6, Polarity::Production),
            node(3, "Counter", 7, Polarity::Production),
            node(4, "run", 8, Polarity::Production),
            node(5, "per_second", 9, Polarity::Production),
        ],
        vec![
            edge(1, 0), // area_of -> Shape: priced, inside geometry.
            edge(1, 2), // area_of -> scale: priced, geometry coheres.
            edge(2, 0), // scale -> Shape: priced, geometry coheres.
            edge(5, 3), // per_second -> Counter: priced, metrics coheres.
            edge(4, 1), // run -> area_of: priced, facade reaches geometry.
            edge(4, 5), // run -> per_second: priced, facade reaches metrics.
        ],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "geometry", ScopeLevel::Folder, Some(0)),
            container(2, "metrics", ScopeLevel::Folder, Some(0)),
            container(3, "workspace", ScopeLevel::Folder, Some(0)),
            container(4, "shape.ts", ScopeLevel::File, Some(1)),
            container(5, "area.ts", ScopeLevel::File, Some(1)),
            container(6, "units.ts", ScopeLevel::File, Some(1)),
            container(7, "counter.ts", ScopeLevel::File, Some(2)),
            container(8, "main.ts", ScopeLevel::File, Some(3)),
            container(9, "rate.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(
        &snapshot,
        &AnalyzeConfig::default(),
        AnalyzeConfig::default()
            .profiles
            .greenfield
            .objective
            .coefficients(),
        false,
        &tests,
    );
    let scc_of = |file_container: u32| -> u32 {
        let vertex = solver
            .index_of
            .get(&file_container)
            .copied()
            .unwrap_or(u32::MAX);
        solver
            .condensation
            .membership
            .get(vertex as usize)
            .map_or(0, |scc| scc.0)
    };

    let mut polished = solver.real_partition.clone();
    let before = polished.clone();
    solver.polish(&mut polished);

    assert_eq!(
        polished.cluster_of(scc_of(8)),
        before.cluster_of(scc_of(8)),
        "the facade bridges geometry and metrics; absorbing it into either \
             side makes the bridge a member of the thing it bridges"
    );
    assert_eq!(
        polished, before,
        "every move this layout offers is a bridge fold, so polish must hold"
    );
}
