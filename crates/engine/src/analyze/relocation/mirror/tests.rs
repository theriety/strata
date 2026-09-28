#![allow(clippy::assertions_on_constants)]

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};
use strata_core::score::Coefficients;
use strata_ir::{Container, Node, Polarity, ScopeLevel};

use crate::config::{AnalyzeConfig, TestMirrorRule};
#[cfg(test)]
use crate::narrate::physical_file_folders;
use crate::result::BlockedMirrorReason;

use super::*;
use crate::analyze::relocation::*;
use crate::analyze::test_support::*;

#[test]
#[allow(clippy::too_many_lines)]
fn should_price_one_exact_mirror_before_finish_and_replay_idempotently() {
    let snap = snapshot(
        vec![
            node(0, "perform_task", 4, Polarity::Production),
            node(1, "own_task", 5, Polarity::Production),
            node(2, "task_spec", 6, Polarity::TestCase),
        ],
        vec![edge(0, 1), edge(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "source/module", ScopeLevel::Folder, Some(0)),
            container(2, "source/target", ScopeLevel::Folder, Some(0)),
            container(3, "spec/module", ScopeLevel::Folder, Some(0)),
            container(4, "source/module/task.ts", ScopeLevel::File, Some(1)),
            container(5, "source/target/owner.ts", ScopeLevel::File, Some(2)),
            container(6, "spec/module/task.spec.ts", ScopeLevel::File, Some(3)),
        ],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.relocation.test_mirroring.builtins = false;
    config.profiles.anchored.relocation.test_mirroring.rules = vec![TestMirrorRule {
        source: "source/{dir}/{stem}.ts".to_owned(),
        tests: vec!["spec/{dir}/{stem}.spec.ts".to_owned()],
    }];
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);
    let scc_of = |file: u32| {
        solver
            .index_of
            .get(&file)
            .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
            .map_or(u32::MAX, |scc| scc.0)
    };
    let mut before = solver.real_partition.clone();
    let source = scc_of(4);
    let owner = scc_of(5);
    let mirror = scc_of(6);
    let owner_cluster = before.cluster_of(owner);
    assert!(owner_cluster.is_some(), "owner is placed");
    let Some(owner_cluster) = owner_cluster else {
        return;
    };
    assert!(before.move_node(source, owner_cluster));

    let score_before = solver.evaluate(&before);
    let mut after = before.clone();
    let evidence = solver.shadow_tests(&mut after);
    assert_eq!(evidence.outcomes.len(), 1);
    assert_eq!(
        evidence.outcomes.first().map(|outcome| outcome.disposition),
        Some(MirrorDisposition::Applied)
    );
    let changed: Vec<usize> = before
        .assignment()
        .iter()
        .zip(after.assignment())
        .enumerate()
        .filter_map(|(index, (left, right))| (left != right).then_some(index))
        .collect();
    assert_eq!(changed, vec![mirror as usize]);

    let recompute = |parts: &Partition, replay: &MirrorEvidence| {
        let assembled = solver.assemble_with_mirror_evidence(parts, replay);
        let placement = |node: &Node| assembled.placement.get(&node.id.0).copied();
        render_tree(
            &assembled.tree,
            &snap.ir().nodes,
            &placement,
            &assembled.key_by_id,
        )
        .ok()
        .map(|rendered| {
            (
                solver.evaluate_with_mirror_evidence(parts, replay),
                solver.capacity_remainder_with_mirror_evidence(
                    parts,
                    replay,
                    &rendered,
                    &config.profiles.anchored.capacity,
                ),
            )
        })
    };
    let projected =
        physical_file_folders(&solver.assemble_with_mirror_evidence(&after, &evidence).tree);
    let follower_folder = projected
        .iter()
        .find(|(path, _)| path.ends_with("spec/module/task.spec.ts"))
        .map(|(_, folder)| folder);
    assert!(
        follower_folder
            .is_some_and(|folder| { folder.ends_with(&["spec".to_owned(), "target".to_owned()]) }),
        "an applied exact follower is projected under its test root: {projected:?}"
    );
    let post = recompute(&after, &evidence);
    assert!(post.is_some(), "candidate tree renders");
    let Some(post) = post else {
        return;
    };
    assert!(
        (post.0 - solver.evaluate_with_mirror_evidence(&after, &evidence)).abs() < f64::EPSILON
    );
    assert!(
        (score_before - post.0).abs() >= f64::EPSILON,
        "the follower must enter final pricing"
    );

    let mut replayed = after.clone();
    let replay_evidence = solver.shadow_tests(&mut replayed);
    assert_eq!(replayed.assignment(), after.assignment());
    let replayed_post = recompute(&replayed, &replay_evidence);
    assert!(replayed_post.is_some(), "replayed candidate tree renders");
    let Some(replayed_post) = replayed_post else {
        return;
    };
    assert!((replayed_post.0 - post.0).abs() < f64::EPSILON);
    assert_eq!(replayed_post.1, post.1);
    assert_eq!(replay_evidence, evidence);
}

#[test]
fn should_attempt_mirrors_for_every_physical_source_move() {
    let snap = snapshot(
        vec![
            node(0, "move_by_assignment", 6, Polarity::Production),
            node(1, "move_by_projection", 7, Polarity::Production),
            node(2, "receive_source", 8, Polarity::Production),
            node(3, "assignment_spec", 9, Polarity::TestCase),
            node(4, "projection_spec", 10, Polarity::TestCase),
        ],
        vec![edge(0, 2)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "source/assignment", ScopeLevel::Folder, Some(0)),
            container(2, "source/projection", ScopeLevel::Folder, Some(0)),
            container(3, "source/target", ScopeLevel::Folder, Some(0)),
            container(4, "spec/assignment", ScopeLevel::Folder, Some(0)),
            container(5, "spec/projection", ScopeLevel::Folder, Some(0)),
            container(
                6,
                "source/assignment/assignment.ts",
                ScopeLevel::File,
                Some(1),
            ),
            container(
                7,
                "source/projection/projection.ts",
                ScopeLevel::File,
                Some(2),
            ),
            container(8, "source/target/owner.ts", ScopeLevel::File, Some(3)),
            container(
                9,
                "spec/assignment/assignment.spec.ts",
                ScopeLevel::File,
                Some(4),
            ),
            container(
                10,
                "spec/projection/projection.spec.ts",
                ScopeLevel::File,
                Some(5),
            ),
        ],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.relocation.test_mirroring.builtins = false;
    config.profiles.anchored.relocation.test_mirroring.rules = vec![TestMirrorRule {
        source: "source/{dir}/{stem}.ts".to_owned(),
        tests: vec!["spec/{dir}/{stem}.spec.ts".to_owned()],
    }];
    let tests = TestPolicy::defaults();
    let mut solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);
    let scc_of = |file: u32| {
        solver
            .index_of
            .get(&file)
            .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
            .map_or(u32::MAX, |scc| scc.0)
    };
    let mut parts = solver.real_partition.clone();
    let target = parts.cluster_of(scc_of(8));
    assert!(target.is_some(), "target is placed");
    let Some(target) = target else {
        return;
    };
    assert!(parts.move_node(scc_of(6), target));

    let projected = parts.cluster_of(scc_of(7));
    assert!(projected.is_some(), "projected source is placed");
    let Some(projected) = projected else {
        return;
    };
    let Some(name) = solver.real_folder_names.get_mut(projected.0 as usize) else {
        return;
    };
    *name = SmolStr::new("source/projected");

    let evidence = solver.shadow_tests(&mut parts);
    let applied_sources: BTreeSet<&str> = evidence
        .outcomes
        .iter()
        .filter(|outcome| outcome.disposition == MirrorDisposition::Applied)
        .map(|outcome| outcome.source_path.as_str())
        .collect();

    assert_eq!(
        applied_sources,
        BTreeSet::from([
            "workspace/source/assignment/assignment.ts",
            "workspace/source/projection/projection.ts",
        ]),
        "every physical source move attempts its exact mirror"
    );
}

#[test]
fn should_attempt_a_mirror_when_the_source_moves_to_its_namespace_root() {
    let snap = snapshot(
        vec![
            node(0, "nested_source", 3, Polarity::Production),
            node(1, "root_owner", 4, Polarity::Production),
            node(2, "nested_spec", 5, Polarity::TestCase),
        ],
        vec![edge(0, 1)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "source/nested", ScopeLevel::Folder, Some(0)),
            container(2, "spec/nested", ScopeLevel::Folder, Some(0)),
            container(3, "source/nested/item.ts", ScopeLevel::File, Some(1)),
            container(4, "owner.ts", ScopeLevel::File, Some(0)),
            container(5, "spec/nested/item.spec.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.relocation.test_mirroring.builtins = false;
    config.profiles.anchored.relocation.test_mirroring.rules = vec![TestMirrorRule {
        source: "source/{dir}/{stem}.ts".to_owned(),
        tests: vec!["spec/{dir}/{stem}.spec.ts".to_owned()],
    }];
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);
    let scc_of = |file: u32| {
        solver
            .index_of
            .get(&file)
            .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
            .map_or(u32::MAX, |scc| scc.0)
    };
    let mut parts = solver.real_partition.clone();
    let target = parts.cluster_of(scc_of(4));
    assert!(target.is_some(), "root owner is placed");
    let Some(target) = target else {
        return;
    };
    assert!(parts.move_node(scc_of(3), target));

    let evidence = solver.shadow_tests(&mut parts);

    assert_eq!(
        evidence
            .outcomes
            .iter()
            .map(|outcome| outcome.source_path.as_str())
            .collect::<Vec<_>>(),
        vec!["workspace/source/nested/item.ts"],
        "a source moved to its namespace root still attempts its exact mirror"
    );
    assert_eq!(
        evidence
            .outcomes
            .first()
            .map(|outcome| outcome.intended_to.as_str()),
        Some("workspace/spec"),
        "a synthetic source-root cluster projects to the test root, not a workspace folder"
    );
}

fn repeated_root_mirror_evidence(with_collision: bool) -> MirrorEvidence {
    let mut containers = vec![
        container(0, "app", ScopeLevel::PackageGroup, None),
        container(1, "app/source/module", ScopeLevel::Folder, Some(0)),
        container(2, "app/source/target", ScopeLevel::Folder, Some(0)),
        container(3, "app/spec/module", ScopeLevel::Folder, Some(0)),
        container(4, "app/source/module/task.ts", ScopeLevel::File, Some(1)),
        container(5, "app/source/target/owner.ts", ScopeLevel::File, Some(2)),
        container(6, "app/spec/module/task.spec.ts", ScopeLevel::File, Some(3)),
    ];
    let mut nodes = vec![
        node(0, "perform_task", 4, Polarity::Production),
        node(1, "own_task", 5, Polarity::Production),
        node(2, "task_spec", 6, Polarity::TestCase),
    ];
    if with_collision {
        containers.extend([
            container(7, "app/spec/target", ScopeLevel::Folder, Some(0)),
            container(8, "app/spec/target/task.spec.ts", ScopeLevel::File, Some(7)),
        ]);
        nodes.push(node(3, "existing_spec", 8, Polarity::TestCase));
    }
    let snap = snapshot(nodes, vec![edge(0, 1)], containers);
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.relocation.test_mirroring.builtins = false;
    config.profiles.anchored.relocation.test_mirroring.rules = vec![TestMirrorRule {
        source: "app/source/{dir}/{stem}.ts".to_owned(),
        tests: vec!["app/spec/{dir}/{stem}.spec.ts".to_owned()],
    }];
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);
    let scc = |file: u32| {
        solver
            .index_of
            .get(&file)
            .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
            .map_or(u32::MAX, |scc| scc.0)
    };
    let mut parts = solver.pass_start_partition.clone();
    let target = parts.cluster_of(scc(5)).unwrap_or(ClusterId(0));
    assert!(parts.move_node(scc(4), target));
    solver.shadow_tests(&mut parts)
}

#[test]
fn should_link_an_exact_mirror_when_a_real_directory_repeats_the_repository_root() {
    let evidence = repeated_root_mirror_evidence(false);
    let outcome = evidence.outcomes.first();

    assert_eq!(evidence.outcomes.len(), 1);
    assert_eq!(
        outcome.map(|outcome| outcome.disposition),
        Some(MirrorDisposition::Applied)
    );
    assert_eq!(
        outcome.map(|outcome| outcome.source_path.as_str()),
        Some("app/app/source/module/task.ts")
    );
}

#[test]
fn should_detect_a_mirror_collision_beneath_a_repeated_repository_root() {
    let evidence = repeated_root_mirror_evidence(true);
    let outcome = evidence.outcomes.first();

    assert_eq!(evidence.outcomes.len(), 1);
    assert_eq!(
        outcome.map(|outcome| outcome.disposition),
        Some(MirrorDisposition::Blocked(
            BlockedMirrorReason::PathCollision
        ))
    );
}

#[test]
fn should_assemble_a_root_mirror_at_the_repository_root_exactly_once() {
    let snap = snapshot(
        vec![
            node(0, "nested_source", 3, Polarity::Production),
            node(1, "root_owner", 4, Polarity::Production),
            node(2, "nested_spec", 5, Polarity::TestCase),
        ],
        vec![edge(0, 1)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "source/nested", ScopeLevel::Folder, Some(0)),
            container(2, "nested", ScopeLevel::Folder, Some(0)),
            container(3, "source/nested/item.ts", ScopeLevel::File, Some(1)),
            container(4, "owner.ts", ScopeLevel::File, Some(0)),
            container(5, "nested/item.spec.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.relocation.test_mirroring.builtins = false;
    config.profiles.anchored.relocation.test_mirroring.rules = vec![TestMirrorRule {
        source: "source/{dir}/{stem}.ts".to_owned(),
        tests: vec!["{dir}/{stem}.spec.ts".to_owned()],
    }];
    let tests = TestPolicy::defaults();
    let mut solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);
    let scc = |file: u32| {
        solver
            .index_of
            .get(&file)
            .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
            .map_or(u32::MAX, |scc| scc.0)
    };
    let mut parts = solver.real_partition.clone();
    let target = parts.cluster_of(scc(4)).unwrap_or(ClusterId(0));
    if let Some(folder) = solver.real_folder_names.get_mut(target.0 as usize) {
        *folder = SmolStr::new("");
    }
    assert!(parts.move_node(scc(3), target));

    let evidence = solver.shadow_tests(&mut parts);
    assert_eq!(
        evidence.outcomes.first().map(|outcome| outcome.disposition),
        Some(MirrorDisposition::Applied)
    );
    assert_eq!(
        evidence
            .outcomes
            .first()
            .map(|outcome| outcome.intended_to.as_str()),
        Some("app")
    );
    let assembled = solver.assemble_with_mirror_evidence(&parts, &evidence);
    let candidate_file = assembled
        .pass_start_file_by_candidate
        .iter()
        .find_map(|(candidate, original)| (original.0 == 5).then_some(*candidate));
    let by_id: BTreeMap<u32, &Container> = assembled
        .tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let folder_name = candidate_file
        .and_then(|file| by_id.get(&file.0))
        .and_then(|file| file.parent)
        .and_then(|folder| by_id.get(&folder.0))
        .map(|folder| folder.name.as_str());
    assert_eq!(folder_name, Some(""));
}

#[derive(Clone, Copy)]
enum MirrorBlockFixture {
    Ambiguous,
    Namespace,
    Capacity,
    Collision,
}

fn assert_actual_mirror_block(fixture: MirrorBlockFixture, expected: BlockedMirrorReason) {
    let mut containers = vec![
        container(0, "workspace", ScopeLevel::PackageGroup, None),
        container(1, "source/a", ScopeLevel::Folder, Some(0)),
        container(2, "source/b", ScopeLevel::Folder, Some(0)),
        container(3, "source/target", ScopeLevel::Folder, Some(0)),
        container(4, "spec/a", ScopeLevel::Folder, Some(0)),
        container(5, "spec/target", ScopeLevel::Folder, Some(0)),
        container(6, "source/a/task.ts", ScopeLevel::File, Some(1)),
        container(7, "source/b/task.ts", ScopeLevel::File, Some(2)),
        container(8, "source/target/owner.ts", ScopeLevel::File, Some(3)),
        container(9, "spec/a/task.spec.ts", ScopeLevel::File, Some(4)),
    ];
    let mut nodes = vec![
        node(0, "perform_task", 6, Polarity::Production),
        node(1, "alternate_task", 7, Polarity::Production),
        node(2, "own_task", 8, Polarity::Production),
        node(3, "task_spec", 9, Polarity::TestCase),
    ];
    let mut edges = vec![edge(0, 2)];
    match fixture {
        MirrorBlockFixture::Namespace => {
            containers.push(container(10, "support", ScopeLevel::Folder, Some(0)));
            containers.push(container(
                11,
                "support/runtime.ts",
                ScopeLevel::File,
                Some(10),
            ));
            nodes.push(node(4, "support", 11, Polarity::Production));
            edges.extend([edge(3, 4), edge(4, 3)]);
        }
        MirrorBlockFixture::Capacity => {
            containers.push(container(
                10,
                "spec/a/support.spec.ts",
                ScopeLevel::File,
                Some(4),
            ));
            nodes.push(node(4, "support_spec", 10, Polarity::TestCase));
            edges.extend([edge(3, 4), edge(4, 3)]);
        }
        MirrorBlockFixture::Collision => {
            containers.push(container(
                10,
                "spec/target/task.spec.ts",
                ScopeLevel::File,
                Some(5),
            ));
            nodes.push(node(4, "existing_spec", 10, Polarity::TestCase));
        }
        MirrorBlockFixture::Ambiguous => {}
    }
    let snap = snapshot(nodes, edges, containers);
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.relocation.test_mirroring.builtins = false;
    config.profiles.anchored.relocation.test_mirroring.rules =
        if matches!(fixture, MirrorBlockFixture::Ambiguous) {
            ["a", "b"]
                .into_iter()
                .map(|branch| TestMirrorRule {
                    source: format!("source/{branch}/{{dir}}/{{stem}}.ts"),
                    tests: vec!["spec/a/{dir}/{stem}.spec.ts".to_owned()],
                })
                .collect()
        } else {
            vec![TestMirrorRule {
                source: "source/{dir}/{stem}.ts".to_owned(),
                tests: vec!["spec/{dir}/{stem}.spec.ts".to_owned()],
            }]
        };
    if matches!(fixture, MirrorBlockFixture::Capacity) {
        config.profiles.anchored.capacity.folder = 1;
    }
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);
    let scc = |file: u32| {
        solver
            .index_of
            .get(&file)
            .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
            .map_or(u32::MAX, |scc| scc.0)
    };
    let mut parts = solver.pass_start_partition.clone();
    let target = parts.cluster_of(scc(8));
    assert!(target.is_some(), "owner is placed");
    let target = target.unwrap_or(ClusterId(0));
    assert!(parts.move_node(scc(6), target));

    let evidence = solver.shadow_tests(&mut parts);

    assert_eq!(evidence.outcomes.len(), 1, "one follower was attempted");
    assert_eq!(
        evidence.outcomes.first().map(|outcome| outcome.disposition),
        Some(MirrorDisposition::Blocked(expected))
    );
}

#[test]
fn should_report_actual_ambiguous_mirror_attempt() {
    assert_actual_mirror_block(
        MirrorBlockFixture::Ambiguous,
        BlockedMirrorReason::AmbiguousMapping,
    );
}

#[test]
fn should_report_actual_namespace_mirror_attempt() {
    assert_actual_mirror_block(
        MirrorBlockFixture::Namespace,
        BlockedMirrorReason::NamespaceBoundary,
    );
}

#[test]
fn should_report_actual_capacity_mirror_attempt() {
    assert_actual_mirror_block(MirrorBlockFixture::Capacity, BlockedMirrorReason::Capacity);
}

#[test]
fn should_report_actual_path_collision_mirror_attempt() {
    assert_actual_mirror_block(
        MirrorBlockFixture::Collision,
        BlockedMirrorReason::PathCollision,
    );
}

#[test]
fn should_not_shadow_by_basename_when_exact_mirroring_is_disabled() {
    let snap = snapshot(
        vec![
            node(0, "perform_task", 4, Polarity::Production),
            node(1, "own_task", 5, Polarity::Production),
            node(2, "task_spec", 6, Polarity::TestCase),
        ],
        vec![edge(0, 1), edge(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "source/module", ScopeLevel::Folder, Some(0)),
            container(2, "source/target", ScopeLevel::Folder, Some(0)),
            container(3, "spec/module", ScopeLevel::Folder, Some(0)),
            container(4, "source/module/task.ts", ScopeLevel::File, Some(1)),
            container(5, "source/target/owner.ts", ScopeLevel::File, Some(2)),
            container(6, "spec/module/task.spec.ts", ScopeLevel::File, Some(3)),
        ],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.relocation.test_mirroring.enabled = false;
    config.profiles.anchored.relocation.pin_detected_test_files = false;
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);
    let scc_of = |file: u32| {
        solver
            .index_of
            .get(&file)
            .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
            .map_or(u32::MAX, |scc| scc.0)
    };
    let mut parts = solver.real_partition.clone();
    let source = scc_of(4);
    let owner = scc_of(5);
    let mirror = scc_of(6);
    let owner_cluster = parts.cluster_of(owner);
    assert!(owner_cluster.is_some(), "owner is placed");
    let Some(owner_cluster) = owner_cluster else {
        return;
    };
    assert!(parts.move_node(source, owner_cluster));
    let before = parts.cluster_of(mirror);

    solver.shadow_tests(&mut parts);

    assert_eq!(parts.cluster_of(mirror), before);
}

#[test]
fn should_not_duplicate_an_unfollowed_namespace_during_assembly() {
    let snap = snapshot(
        vec![
            node(0, "subject", 3, Polarity::Production),
            node(1, "subject_spec", 4, Polarity::TestCase),
        ],
        vec![],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "spec", ScopeLevel::Folder, Some(0)),
            container(3, "src/subject.ts", ScopeLevel::File, Some(1)),
            container(4, "spec/subject.spec.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.relocation.test_mirroring.enabled = false;
    config.profiles.anchored.relocation.pin_detected_test_files = false;
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);
    let scc_of = |file: u32| {
        solver
            .index_of
            .get(&file)
            .and_then(|vertex| solver.condensation.membership.get(*vertex as usize))
            .map_or(u32::MAX, |scc| scc.0)
    };
    let mut parts = solver.real_partition.clone();
    let spec_cluster = parts.cluster_of(scc_of(4));
    assert!(spec_cluster.is_some(), "spec folder is placed");
    let Some(spec_cluster) = spec_cluster else {
        return;
    };
    assert!(parts.move_node(scc_of(3), spec_cluster));

    let assembled = solver.assemble(&parts);
    let folders: Vec<&str> = assembled
        .tree
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::Folder)
        .map(|container| container.name.as_str())
        .collect();

    assert!(
        !folders.iter().any(|folder| folder.contains("spec/spec")),
        "an ordinary mixed cluster must not manufacture a duplicate namespace: {folders:?}"
    );
}

#[test]
fn should_keep_an_ambiguously_twinned_spec_where_it_is() {
    // Two production files reduce to the same stem in the same package —
    // no unique twin, so the shadow pass must leave the spec alone (ADR-16:
    // unchanged placements are never presented as suggested actions).
    let nodes = vec![
        node(0, "one", 4, Polarity::Production),
        node(1, "two", 5, Polarity::Production),
        node(2, "spec", 3, Polarity::TestCase),
    ];
    let edges = vec![edge(2, 0)];
    let containers = vec![
        container(0, "workspace", ScopeLevel::PackageGroup, None),
        container(1, "spec", ScopeLevel::Folder, Some(0)),
        container(2, "alt", ScopeLevel::Folder, Some(0)),
        container(3, "spec/openai.spec.ts", ScopeLevel::File, Some(1)),
        container(4, "alt/openai.ts", ScopeLevel::File, Some(2)),
        container(5, "alt/openai.tsx", ScopeLevel::File, Some(2)),
    ];
    let snap = snapshot(nodes, edges, containers);

    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(
        &snap,
        &AnalyzeConfig::default(),
        Coefficients::anchored(),
        true,
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
            .map_or(u32::MAX, |scc| scc.0)
    };

    let mut parts = solver.real_partition.clone();
    let unit = scc_of(3);
    let before = parts.cluster_of(unit);
    assert!(before.is_some(), "setup: the spec starts placed");
    assert_ne!(
        before,
        parts.cluster_of(scc_of(4)),
        "setup: the spec starts apart from its would-be twins"
    );

    solver.shadow_tests(&mut parts);

    assert_eq!(parts.cluster_of(unit), before);
}

#[test]
fn should_veto_a_shadow_move_that_would_overflow_the_folder_cap() {
    // A folder cap of one leaves no room next to the twin; the cap veto
    // binds exactly like it does for polish moves.
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 1;
    let nodes = vec![
        node(0, "openai", 3, Polarity::Production),
        node(1, "openai_spec", 4, Polarity::TestCase),
    ];
    let edges = vec![edge(1, 0)];
    let containers = vec![
        container(0, "workspace", ScopeLevel::PackageGroup, None),
        container(1, "src", ScopeLevel::Folder, Some(0)),
        container(2, "spec", ScopeLevel::Folder, Some(0)),
        container(3, "src/openai.ts", ScopeLevel::File, Some(1)),
        container(4, "spec/openai.spec.ts", ScopeLevel::File, Some(2)),
    ];
    let snap = snapshot(nodes, edges, containers);

    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), true, &tests);

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
            .map_or(u32::MAX, |scc| scc.0)
    };

    let mut parts = solver.real_partition.clone();
    let unit = scc_of(4);
    let subject = scc_of(3);
    assert_ne!(
        parts.cluster_of(unit),
        parts.cluster_of(subject),
        "setup: real dirs must start the pair apart"
    );

    solver.shadow_tests(&mut parts);

    assert_ne!(
        parts.cluster_of(unit),
        parts.cluster_of(subject),
        "the cap veto binds even against a unique twin"
    );
}

#[test]
fn should_allow_a_childs_only_file_to_replace_its_folder_entry_at_the_parent_cap() {
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.capacity.folder = 2;
    let snap = snapshot(
        vec![
            node(0, "anchor", 3, Polarity::Production),
            node(1, "subject", 4, Polarity::Production),
        ],
        vec![edge(1, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Domain, Some(0)),
            container(2, "area/child", ScopeLevel::Folder, Some(1)),
            container(3, "area/anchor.ts", ScopeLevel::File, Some(1)),
            container(4, "area/child/subject.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(&snap, &config, Coefficients::anchored(), false, &tests);
    let unit_vertex = solver.index_of.get(&4).copied().unwrap_or(u32::MAX);
    let unit = solver
        .condensation
        .membership
        .get(unit_vertex as usize)
        .map_or(u32::MAX, |scc| scc.0);
    let target_vertex = solver.index_of.get(&3).copied().unwrap_or(u32::MAX);
    let target = solver
        .condensation
        .membership
        .get(target_vertex as usize)
        .and_then(|scc| solver.real_partition.cluster_of(scc.0));

    assert!(
        target.is_some_and(|target| {
            solver.permits_physical_capacity(&solver.real_partition, unit, target)
        }),
        "the child-folder entry is replaced by its only file, so the parent remains at cap"
    );
}
