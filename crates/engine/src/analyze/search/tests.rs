#![allow(clippy::assertions_on_constants)]

use std::collections::BTreeMap;

use strata_core::cluster::ClusterId;
use strata_core::diversify::Solver;
use strata_core::score::{Coefficients, KindWeights};
use strata_ir::{Edge, EdgeKind, Node, NodeId, Polarity, ScopeLevel, Snapshot};

use crate::config::{AnalyzeConfig, CapacityConfig};
use crate::error::StrataError;
use crate::result::{CurrentStanding, Level};

use super::*;
use crate::analyze::test_support::*;
use crate::analyze::*;
use crate::analyze::{findings::*, relocation::*, rendering::*, scoring::*};

#[test]
fn should_produce_both_modes_when_requested() {
    let snapshot = snapshot(
        vec![node(0, "a", 0, Polarity::Production)],
        vec![],
        vec![container(0, "file", ScopeLevel::File, None)],
    );

    let result = analyze(&snapshot, &AnalyzeConfig::default());
    let modes = result.map(|result| result.profiles).unwrap_or_default();

    assert!(modes.anchored.is_some());
    assert!(modes.greenfield.is_some());
}

#[test]
fn should_return_up_to_k_candidates_per_mode() {
    // a graph with several independent files gives the clusterer room to find
    // more than one distinct grouping, so diversification returns multiple.
    let nodes = (0..8)
        .map(|i| node(i, &format!("sym{i}"), i, Polarity::Production))
        .collect::<Vec<_>>();
    let containers = (0..8)
        .map(|i| container(i, &format!("file{i}"), ScopeLevel::File, None))
        .collect::<Vec<_>>();
    let snapshot = snapshot(nodes, vec![edge(0, 1), edge(2, 3), edge(4, 5)], containers);

    let anchored = analyze(&snapshot, &config_with_k(3))
        .ok()
        .and_then(|result| result.profiles.anchored)
        .unwrap_or_else(empty_profile_result);

    // never more than k; an already-best baseline can legitimately yield
    // no offer under strict improvement admission.
    assert!(anchored.candidates.len() <= 3);
    assert!(
        anchored
            .candidates
            .iter()
            .all(|candidate| candidate.improvement > 0.0)
    );
    // candidates are ranked, best (lowest) score first.
    for window in anchored.candidates.windows(2) {
        let (Some(first), Some(second)) = (window.first(), window.get(1)) else {
            continue;
        };
        assert!(first.score <= second.score);
    }
    // the pairwise distance matrix is square over the returned candidates.
    assert_eq!(anchored.pairwise_distance.len(), anchored.candidates.len());
}

#[test]
fn should_populate_capacity_remainder_on_every_candidate_when_infeasible() {
    let make = || {
        snapshot(
            vec![
                node(0, "a", 0, Polarity::Production),
                node(1, "b", 1, Polarity::Production),
            ],
            vec![edge(0, 1)],
            vec![
                container(0, "src/a.ts", ScopeLevel::File, Some(2)),
                container(1, "src/b.ts", ScopeLevel::File, Some(2)),
                container(2, "src", ScopeLevel::Folder, None),
            ],
        )
    };

    // a zero file cap makes every file a hard breach: infeasible standing,
    // so every candidate must carry its own remainder.
    let mut infeasible_config = config_with_k(2);
    infeasible_config.profiles.anchored.capacity.file = 0;
    let infeasible = analyze(&make(), &infeasible_config)
        .ok()
        .and_then(|result| result.profiles.anchored);
    let candidates = infeasible
        .as_ref()
        .map_or(&[] as &[_], |mode| &mode.candidates);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.capacity_remainder.is_some())
    );

    // a clean tree carries no remainder on any candidate.
    let clean = analyze(&make(), &config_with_k(2))
        .ok()
        .and_then(|result| result.profiles.anchored);
    let clean_candidates = clean.as_ref().map_or(&[] as &[_], |mode| &mode.candidates);
    assert!(
        clean_candidates
            .iter()
            .all(|candidate| candidate.capacity_remainder.is_none())
    );
}

/// Builds the gate-probe snapshot: two edgeless production files. Polish
/// nominates moves only along priced pulls (D-46), so with no edges every
/// seed converges back to reality under either objective and the identity
/// layout wins whichever pool is allowed to carry it.
fn gate_fixture() -> Snapshot {
    snapshot(
        vec![
            node(0, "alpha", 0, Polarity::Production),
            node(1, "beta", 1, Polarity::Production),
        ],
        vec![],
        vec![
            container(0, "alpha.py", ScopeLevel::File, None),
            container(1, "beta.py", ScopeLevel::File, None),
        ],
    )
}

#[test]
fn should_give_identical_profiles_identical_search_results() {
    let mut config = config_with_k(2);
    config.profiles.greenfield = config.profiles.anchored.clone();

    let profiles = analyze(&gate_fixture(), &config)
        .map(|result| result.profiles)
        .unwrap_or_default();
    let anchored = profiles.anchored;
    let greenfield = profiles.greenfield;

    assert_eq!(anchored, greenfield);
}

#[test]
fn should_gate_identity_seeding_on_the_solver_flag() {
    // the mechanism behind the wiring: `PipelineSolver` constructs the
    // identity entry only when asked, and only then does the offset-0 seed
    // short-circuit to it — re-priced at the true current tree rather than
    // the assembled search view.
    let snapshot = gate_fixture();
    let config = AnalyzeConfig::default();
    let weights = config.profiles.anchored.weights.kind_weights();

    let anchored_tests = TestPolicy::defaults();
    let anchored = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.anchored.objective.coefficients(),
        true,
        &anchored_tests,
    );
    let seeded = anchored.solve(config.profiles.anchored.seed);
    let current_total = score_current(
        &snapshot,
        &config.profiles.anchored.objective.coefficients(),
        &weights,
        level_caps(&config).folder,
    )
    .total;
    let identity_matched = anchored.identity.as_ref().is_some_and(|identity| {
        seeded.partition == *identity && (seeded.score - current_total).abs() < f64::EPSILON
    });
    assert!(
        identity_matched,
        "seed_identity=true carries the identity partition and solve(base) \
             returns it at the true current-tree score"
    );

    let greenfield_tests = TestPolicy::defaults();
    let greenfield = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &greenfield_tests,
    );
    let searched = greenfield.solve(config.profiles.anchored.seed);
    let covered = (0..greenfield.condensation.members.len()).all(|scc| {
        searched
            .partition
            .cluster_of(u32::try_from(scc).unwrap_or(u32::MAX))
            .is_some()
    });
    assert!(
        greenfield.identity.is_none(),
        "seed_identity=false constructs no identity entry at all"
    );
    assert!(covered, "closing the gate must not break the search");
}

#[test]
fn should_not_offer_non_improving_candidates_when_capacity_breaches() {
    // the cap-clean arm of the gate: a hard breach makes the current layout
    // an illegal candidate, so neither mode may report optimal even though
    // both searches still run and still emit candidates measured against
    // the (illegal) baseline. Borderline observations never reach this arm:
    // the same hard-breaks predicate feeds both the DTO count and the gate.
    let mut dirty = config_with_k(2);
    dirty.profiles.anchored.capacity.file = 0;
    dirty.profiles.greenfield.capacity.file = 0;

    let analyzed = analyze(&gate_fixture(), &dirty);
    let breaks = analyzed.as_ref().map_or(0, |result| {
        result
            .profiles
            .anchored
            .as_ref()
            .map_or(0, |profile| profile.current.capacity_breaks)
    });
    assert!(breaks > 0, "a zero file cap counts as a hard break");

    let modes = analyzed.map(|result| result.profiles).unwrap_or_default();
    let anchored_ok = modes.anchored.as_ref().is_some_and(|profile| {
        profile.current.standing == CurrentStanding::Infeasible && profile.candidates.is_empty()
    });
    let greenfield_ok = modes.greenfield.as_ref().is_some_and(|profile| {
        profile.current.standing == CurrentStanding::Infeasible && profile.candidates.is_empty()
    });
    assert!(
        anchored_ok,
        "an infeasible current layout does not authorize an anchored candidate \
             that fails to improve its profile score; got {:?}",
        modes.anchored
    );
    assert!(
        greenfield_ok,
        "an infeasible current layout does not authorize a greenfield candidate \
             that fails to improve its profile score; got {:?}",
        modes.greenfield
    );
}

#[test]
fn should_score_file_capacity_with_each_profiles_file_cap() {
    let snapshot = snapshot(
        vec![sized_node(0, "entry", 3, 5)],
        vec![],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "workspace", ScopeLevel::Package, Some(0)),
            container(2, "area", ScopeLevel::Domain, Some(1)),
            container(3, "src/area/entry.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let mut config = config_with_k(1);
    config.profiles.greenfield = config.profiles.anchored.clone();
    config.profiles.anchored.capacity.file = 1;
    config.profiles.greenfield.capacity.file = 10;

    let profiles = analyze(&snapshot, &config)
        .map(|result| result.profiles)
        .unwrap_or_default();
    let anchored_capacity = profiles
        .anchored
        .map_or(0.0, |profile| profile.current.score_breakdown.capacity);
    let greenfield_capacity = profiles
        .greenfield
        .map_or(0.0, |profile| profile.current.score_breakdown.capacity);

    assert!(
        anchored_capacity > greenfield_capacity,
        "a five-line file must exert more capacity pressure under cap 1 than cap 10; \
             anchored {anchored_capacity}, greenfield {greenfield_capacity}"
    );
}

#[test]
fn should_keep_structural_capacity_findings_and_scores_in_lockstep_at_every_level()
-> Result<(), StrataError> {
    let snapshot = snapshot(
        vec![
            node(0, "first", 10, Polarity::Production),
            node(1, "second", 11, Polarity::Production),
            node(2, "third", 12, Polarity::Production),
            node(3, "fourth", 13, Polarity::Production),
            node(4, "fifth", 14, Polarity::Production),
        ],
        vec![],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "first-package", ScopeLevel::Package, Some(0)),
            container(2, "second-package", ScopeLevel::Package, Some(0)),
            container(3, "first-domain", ScopeLevel::Domain, Some(1)),
            container(4, "second-domain", ScopeLevel::Domain, Some(1)),
            container(5, "third-domain", ScopeLevel::Domain, Some(2)),
            container(6, "first-folder", ScopeLevel::Folder, Some(3)),
            container(7, "second-folder", ScopeLevel::Folder, Some(3)),
            container(8, "third-folder", ScopeLevel::Folder, Some(4)),
            container(9, "fourth-folder", ScopeLevel::Folder, Some(5)),
            container(10, "first-folder/first.ts", ScopeLevel::File, Some(6)),
            container(11, "first-folder/second.ts", ScopeLevel::File, Some(6)),
            container(12, "second-folder/third.ts", ScopeLevel::File, Some(7)),
            container(13, "third-folder/fourth.ts", ScopeLevel::File, Some(8)),
            container(14, "fourth-folder/fifth.ts", ScopeLevel::File, Some(9)),
        ],
    );
    let placement = |node: &Node| Some(node.container);
    let rendered = render_tree(
        &snapshot.ir().containers,
        &snapshot.ir().nodes,
        &placement,
        &BTreeMap::new(),
    )?;
    let roomy = CapacityConfig {
        file: 100,
        folder: 100,
        domain: 100,
        package: 100,
        package_group: 100,
    };
    let baseline = score_current_with_affinity(
        &snapshot,
        &Coefficients::anchored(),
        &KindWeights::default(),
        &roomy,
        1.0,
        3.0,
    )
    .capacity;

    for level in [
        Level::Folder,
        Level::Domain,
        Level::Package,
        Level::PackageGroup,
    ] {
        let mut capacity = roomy;
        match level {
            Level::Folder => capacity.folder = 1,
            Level::Domain => capacity.domain = 1,
            Level::Package => capacity.package = 1,
            Level::PackageGroup => capacity.package_group = 1,
            Level::File => unreachable!("file capacity has its own parity regression"),
        }
        let finding_levels: Vec<_> = walk_all_capacity(&rendered, &capacity)
            .into_iter()
            .map(|(finding_level, _)| finding_level)
            .collect();
        let score = score_current_with_affinity(
            &snapshot,
            &Coefficients::anchored(),
            &KindWeights::default(),
            &capacity,
            1.0,
            3.0,
        )
        .capacity;

        assert_eq!(
            finding_levels,
            [level],
            "only the configured {level:?} cap should produce a finding"
        );
        assert!(
            score > baseline,
            "the {level:?} finding must contribute matching capacity pressure"
        );
    }
    Ok(())
}

#[test]
fn should_exclude_test_only_sloc_from_file_findings_and_pressure() -> Result<(), StrataError> {
    let mut test_case = node(0, "exercise", 3, Polarity::TestCase);
    test_case.effective_size = 5;
    let snapshot = snapshot(
        vec![test_case],
        vec![],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "workspace", ScopeLevel::Package, Some(0)),
            container(2, "checks", ScopeLevel::Domain, Some(1)),
            container(3, "checks/exercise.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let capacity = CapacityConfig {
        file: 1,
        ..CapacityConfig::default()
    };
    let placement = |node: &Node| Some(node.container);
    let rendered = render_tree(
        &snapshot.ir().containers,
        &snapshot.ir().nodes,
        &placement,
        &BTreeMap::new(),
    )?;
    let findings = snapshot_capacity_violations(&snapshot, &rendered, &capacity);
    let score = score_current_with_affinity(
        &snapshot,
        &Coefficients::anchored(),
        &KindWeights::default(),
        &capacity,
        1.0,
        3.0,
    );

    assert!(findings.iter().all(|finding| finding.capacity.is_none()));
    assert!(score.capacity.abs() < f64::EPSILON);
    Ok(())
}

#[test]
fn should_include_production_sloc_in_file_findings_and_pressure() -> Result<(), StrataError> {
    let snapshot = snapshot(
        vec![sized_node(0, "execute", 3, 5)],
        vec![],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "workspace", ScopeLevel::Package, Some(0)),
            container(2, "feature", ScopeLevel::Domain, Some(1)),
            container(3, "feature/execute.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let capacity = CapacityConfig {
        file: 1,
        ..CapacityConfig::default()
    };
    let placement = |node: &Node| Some(node.container);
    let rendered = render_tree(
        &snapshot.ir().containers,
        &snapshot.ir().nodes,
        &placement,
        &BTreeMap::new(),
    )?;
    let findings = snapshot_capacity_violations(&snapshot, &rendered, &capacity);
    let score = score_current_with_affinity(
        &snapshot,
        &Coefficients::anchored(),
        &KindWeights::default(),
        &capacity,
        1.0,
        3.0,
    );

    assert!(findings.iter().any(|finding| {
        finding
            .capacity
            .as_ref()
            .is_some_and(|breach| breach.measured == 5 && breach.cap == 1)
    }));
    assert!(score.capacity > 0.0);
    Ok(())
}

#[test]
fn should_index_candidates_from_one() {
    let snapshot = snapshot(
        vec![
            node(0, "a", 0, Polarity::Production),
            node(1, "b", 1, Polarity::Production),
        ],
        vec![],
        vec![
            container(0, "file_a", ScopeLevel::File, None),
            container(1, "file_b", ScopeLevel::File, None),
        ],
    );

    let first_index = analyze(&snapshot, &config_with_k(2))
        .ok()
        .and_then(|result| result.profiles.anchored)
        .and_then(|anchored| anchored.candidates.first().map(|candidate| candidate.index));

    assert!(first_index.is_none_or(|index| index == 1));
}

#[test]
fn should_be_deterministic_across_runs() {
    let make = || {
        let nodes = (0..6)
            .map(|i| node(i, &format!("sym{i}"), i, Polarity::Production))
            .collect::<Vec<_>>();
        let containers = (0..6)
            .map(|i| container(i, &format!("file{i}"), ScopeLevel::File, None))
            .collect::<Vec<_>>();
        snapshot(nodes, vec![edge(0, 1), edge(2, 3)], containers)
    };

    let scores = |snapshot: &Snapshot| {
        analyze(snapshot, &config_with_k(3))
            .ok()
            .and_then(|result| result.profiles.anchored)
            .map(|mode| mode.candidates.iter().map(|c| c.score).collect::<Vec<_>>())
            .unwrap_or_default()
    };

    assert_eq!(scores(&make()), scores(&make()));
}

/// Builds a production symbol node with an explicit effective size.
fn sized_node(id: u32, name: &str, container: u32, size: u32) -> Node {
    Node {
        effective_size: size,
        re_export: false,
        ..node(id, name, container, Polarity::Production)
    }
}

#[test]
fn should_solve_a_cycle_with_priced_weights_and_a_minimal_break_set() {
    // a -> b -> c -> a; the c -> a edge is inheritance (1.5), the rest
    // calls (1.0), so the optimal break is a 1.0 call edge.
    let snapshot = snapshot(
        vec![
            node(0, "a", 0, Polarity::Production),
            node(1, "b", 1, Polarity::Production),
            node(2, "c", 2, Polarity::Production),
        ],
        vec![
            edge(0, 1),
            edge(1, 2),
            Edge {
                kind: EdgeKind::Inheritance,
                ..edge(2, 0)
            },
        ],
        vec![
            container(0, "a.ts", ScopeLevel::File, None),
            container(1, "b.ts", ScopeLevel::File, None),
            container(2, "c.ts", ScopeLevel::File, None),
        ],
    );

    let solutions = solve_cycles(
        &snapshot,
        &AnalyzeConfig::default(),
        &KindWeights::default(),
    );

    assert_eq!(solutions.len(), 1);
    let solution = solutions.first();
    assert_eq!(
        solution.map(|s| s.members.clone()),
        Some(vec![NodeId(0), NodeId(1), NodeId(2)])
    );
    let weight_of = |pair: (u32, u32)| {
        solution
            .and_then(|s| s.pair_weights.get(&pair))
            .copied()
            .unwrap_or(0.0)
    };
    assert!((weight_of((0, 1)) - 1.0).abs() < f64::EPSILON);
    assert!((weight_of((1, 2)) - 1.0).abs() < f64::EPSILON);
    assert!((weight_of((2, 0)) - 1.5).abs() < f64::EPSILON);
    assert_eq!(solution.map(|s| s.break_set.exact), Some(true));
    assert_eq!(solution.map(|s| s.break_set.edges.len()), Some(1));
    // the pricier inheritance edge must survive.
    assert!(solution.is_some_and(|s| {
        s.break_set
            .edges
            .iter()
            .all(|e| !(e.source == 2 && e.target == 0))
    }));
    assert_eq!(solution.map(|s| s.production_sloc), Some(3));
}

#[test]
fn should_solve_no_cycles_on_an_acyclic_graph() {
    let snapshot = snapshot(
        vec![
            node(0, "a", 0, Polarity::Production),
            node(1, "b", 1, Polarity::Production),
        ],
        vec![edge(0, 1)],
        vec![
            container(0, "a.ts", ScopeLevel::File, None),
            container(1, "b.ts", ScopeLevel::File, None),
        ],
    );

    let solutions = solve_cycles(
        &snapshot,
        &AnalyzeConfig::default(),
        &KindWeights::default(),
    );

    assert!(solutions.is_empty());
}

/// Folder clusters of the identity partition, keyed by file container id.
fn identity_clusters(snapshot: &Snapshot, file_containers: &[u32]) -> Vec<Option<ClusterId>> {
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(
        snapshot,
        &AnalyzeConfig::default(),
        Coefficients::anchored(),
        true,
        &tests,
    );
    file_containers
        .iter()
        .map(|container| {
            let vertex = solver.index_of.get(container).copied()?;
            let scc = solver.condensation.membership.get(vertex as usize)?;
            solver
                .identity
                .as_ref()
                .and_then(|identity| identity.cluster_of(scc.0))
        })
        .collect()
}

#[test]
fn should_map_singleton_file_sccs_onto_their_current_folders() {
    // two folders, two files each; identity must reproduce the folders.
    let snapshot = snapshot(
        vec![
            node(0, "a", 2, Polarity::Production),
            node(1, "b", 3, Polarity::Production),
            node(2, "c", 4, Polarity::Production),
            node(3, "d", 5, Polarity::Production),
        ],
        vec![],
        vec![
            container(0, "left", ScopeLevel::Folder, None),
            container(1, "right", ScopeLevel::Folder, None),
            container(2, "left/a.ts", ScopeLevel::File, Some(0)),
            container(3, "left/b.ts", ScopeLevel::File, Some(0)),
            container(4, "right/c.ts", ScopeLevel::File, Some(1)),
            container(5, "right/d.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let clusters = identity_clusters(&snapshot, &[2, 3, 4, 5]);

    assert!(clusters.iter().all(Option::is_some));
    assert_eq!(clusters.first(), clusters.get(1));
    assert_eq!(clusters.get(2), clusters.get(3));
    assert_ne!(clusters.first(), clusters.get(2));
}

#[test]
fn should_place_a_cross_folder_cycle_by_its_dominant_file() {
    // the symbol cycle 0 <-> 1 fuses files 2 (left, sloc 5) and 4 (right,
    // sloc 1) into one file SCC; it must land in left's cluster, beside
    // left resident file 3.
    let snapshot = snapshot(
        vec![
            sized_node(0, "a", 2, 5),
            sized_node(1, "b", 4, 1),
            sized_node(2, "c", 3, 1),
            sized_node(3, "d", 5, 1),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![
            container(0, "left", ScopeLevel::Folder, None),
            container(1, "right", ScopeLevel::Folder, None),
            container(2, "left/a.ts", ScopeLevel::File, Some(0)),
            container(3, "left/c.ts", ScopeLevel::File, Some(0)),
            container(4, "right/b.ts", ScopeLevel::File, Some(1)),
            container(5, "right/d.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let clusters = identity_clusters(&snapshot, &[2, 4, 3, 5]);

    assert!(clusters.iter().all(Option::is_some));
    assert_eq!(
        clusters.first(),
        clusters.get(1),
        "the fused SCC is one cluster"
    );
    assert_eq!(
        clusters.first(),
        clusters.get(2),
        "cycle follows dominant left file"
    );
    assert_ne!(clusters.first(), clusters.get(3));
}

#[test]
fn should_break_a_dominance_tie_by_the_smaller_file_name() {
    // equal SLOC on both sides of the cycle; "left/a.ts" < "right/b.ts",
    // so the fused file SCC lands in left, beside left resident file 3.
    let snapshot = snapshot(
        vec![
            sized_node(0, "a", 2, 1),
            sized_node(1, "b", 4, 1),
            sized_node(2, "c", 3, 1),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![
            container(0, "left", ScopeLevel::Folder, None),
            container(1, "right", ScopeLevel::Folder, None),
            container(2, "left/a.ts", ScopeLevel::File, Some(0)),
            container(3, "left/c.ts", ScopeLevel::File, Some(0)),
            container(4, "right/b.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let clusters = identity_clusters(&snapshot, &[2, 3]);

    assert!(clusters.iter().all(Option::is_some));
    assert_eq!(clusters.first(), clusters.get(1));
}
