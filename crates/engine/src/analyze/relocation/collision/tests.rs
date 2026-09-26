use strata_core::cluster::{ClusterId, Partition};
use strata_ir::{Polarity, ScopeLevel, Snapshot};

use super::*;
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::{PipelineSolver, TestPolicy};
use crate::analyze::test_support::*;
use crate::config::AnalyzeConfig;

/// One production symbol per file, no edges: every file is its own SCC and
/// every collision below is placed by hand.
fn flat_snapshot(files: &[(&str, &str)], packages: &[&str]) -> Snapshot {
    let mut containers = vec![container(0, "ws", ScopeLevel::PackageGroup, None)];
    let mut next = 1;
    let mut parent_of = std::collections::BTreeMap::new();
    for package in packages {
        containers.push(container(next, package, ScopeLevel::Package, Some(0)));
        parent_of.insert((*package).to_owned(), next);
        next += 1;
    }
    let mut nodes = Vec::new();
    for (folder, file) in files {
        let folder_id = if let Some(&id) = parent_of.get(*folder) {
            id
        } else {
            let package = packages
                .iter()
                .find(|package| folder.starts_with(&format!("{package}/")))
                .and_then(|package| parent_of.get(*package).copied())
                .unwrap_or(0);
            containers.push(container(next, folder, ScopeLevel::Folder, Some(package)));
            parent_of.insert((*folder).to_owned(), next);
            next += 1;
            next - 1
        };
        containers.push(container(next, file, ScopeLevel::File, Some(folder_id)));
        nodes.push(node(
            u32::try_from(nodes.len()).unwrap_or(u32::MAX),
            file,
            next,
            Polarity::Production,
        ));
        next += 1;
    }
    snapshot(nodes, Vec::new(), containers)
}

fn solver<'a>(
    snapshot: &'a Snapshot,
    config: &'a AnalyzeConfig,
    tests: &'a TestPolicy,
) -> PipelineSolver<'a> {
    let profile = &config.profiles.greenfield;
    PipelineSolver::new(
        snapshot,
        profile,
        profile.objective.coefficients(),
        false,
        tests,
    )
}

fn vertex(solver: &PipelineSolver<'_>, name: &str) -> usize {
    solver
        .files
        .iter()
        .position(|file| file.name == name)
        .unwrap_or(usize::MAX)
}

fn scc(solver: &PipelineSolver<'_>, name: &str) -> u32 {
    solver
        .condensation
        .membership
        .get(vertex(solver, name))
        .map_or(u32::MAX, |scc| scc.0)
}

fn home(solver: &PipelineSolver<'_>, name: &str) -> ClusterId {
    solver
        .pass_start_partition
        .cluster_of(scc(solver, name))
        .unwrap_or(ClusterId(u32::MAX))
}

fn moved(solver: &PipelineSolver<'_>, parts: &Partition, name: &str, to: ClusterId) -> Partition {
    let mut moved = parts.clone();
    let _moved = moved.move_node(scc(solver, name), to);
    moved
}

/// F2: a newcomer the withdrawal evicts toward the occupant's folder must pass
/// the identity guard like any polish move; refused, it returns to its own
/// pass-start home instead of landing beside a same-named file.
#[test]
fn should_return_an_evicted_newcomer_the_landing_refuses_to_its_own_home() {
    let snapshot = flat_snapshot(
        &[
            ("h", "h/mover.ts"),
            ("l", "l/mover.ts"),
            ("l", "l/util.ts"),
            ("n", "n/util.ts"),
            ("n", "n/extra.ts"),
        ],
        &[],
    );
    let config = AnalyzeConfig::default();
    let tests = TestPolicy::defaults();
    let solver = solver(&snapshot, &config, &tests);
    let hub = home(&solver, "h/mover.ts");
    let landing = home(&solver, "l/mover.ts");
    let own = home(&solver, "n/util.ts");
    let joined = moved(&solver, &solver.real_partition, "n/util.ts", hub);
    let before = moved(&solver, &joined, "n/extra.ts", hub);
    let mut parts = before.clone();

    let evicted = solver.evict_newcomers(&before, &mut parts, hub, landing);

    assert_eq!(
        parts.cluster_of(scc(&solver, "n/util.ts")),
        Some(own),
        "n/util.ts would collide with l/util.ts, so it goes back to n"
    );
    assert_eq!(
        parts.cluster_of(scc(&solver, "n/extra.ts")),
        Some(landing),
        "n/extra.ts passes every guard, so it joins the occupant's folder"
    );
    assert_eq!(
        parts.cluster_of(scc(&solver, "h/mover.ts")),
        Some(hub),
        "the colliding file itself stays where it is"
    );
    assert_eq!(evicted.len(), 2, "both evictions are logged for undo");
}

/// F7: a module root is never folded. Its colliding move is withdrawn and it
/// is pinned, but the symbol pass is never offered its declarations; an
/// ordinary file in the same situation is offered.
#[test]
fn should_withdraw_a_colliding_module_root_without_offering_it() {
    let snapshot = flat_snapshot(
        &[
            ("h", "h/index.ts"),
            ("h", "h/helper.ts"),
            ("l", "l/index.ts"),
            ("l", "l/helper.ts"),
        ],
        &[],
    );
    let config = AnalyzeConfig::default();
    let tests = TestPolicy::defaults();
    let solver = solver(&snapshot, &config, &tests);
    let landing = home(&solver, "l/index.ts");
    let carried = moved(&solver, &solver.real_partition, "h/index.ts", landing);
    let mut parts = moved(&solver, &carried, "h/helper.ts", landing);
    let mut folds = Vec::new();
    let mut withdrawals = Withdrawals::default();

    let withdrew = solver.fold_path_collisions(
        &mut parts,
        &PolishEvidence::default(),
        &mut folds,
        &mut withdrawals,
    );

    let container = |name: &str| {
        solver
            .files
            .get(vertex(&solver, name))
            .map_or(u32::MAX, |file| file.container)
    };
    let offered = |name: &str| {
        folds
            .iter()
            .find(|fold| fold.file == container(name))
            .map(|fold| fold.offered)
    };
    assert!(withdrew, "both colliding moves are withdrawn");
    assert_eq!(
        offered("h/index.ts"),
        Some(false),
        "a module root is pinned only"
    );
    assert_eq!(
        offered("h/helper.ts"),
        Some(true),
        "an ordinary file is offered"
    );
    assert_eq!(
        parts.cluster_of(scc(&solver, "h/index.ts")),
        Some(home(&solver, "h/index.ts")),
        "the withdrawn module root returns home"
    );
    assert!(
        solver
            .path_collisions(
                &parts,
                &PolishEvidence {
                    folds: folds.clone(),
                    ..PolishEvidence::default()
                }
            )
            .is_empty(),
        "no collision survives the withdrawal"
    );
}

/// F7: a fold the symbol pass refuses leaves its file where it is and undoes
/// the evictions its withdrawal made; the fold is no longer offered.
#[test]
fn should_undo_the_evictions_of_a_refused_fold() {
    let snapshot = flat_snapshot(
        &[
            ("h", "h/mover.ts"),
            ("l", "l/mover.ts"),
            ("n", "n/extra.ts"),
        ],
        &[],
    );
    let config = AnalyzeConfig::default();
    let tests = TestPolicy::defaults();
    let solver = solver(&snapshot, &config, &tests);
    let hub = home(&solver, "h/mover.ts");
    let landing = home(&solver, "l/mover.ts");
    let polished = moved(&solver, &solver.real_partition, "n/extra.ts", hub);
    let mut parts = polished.clone();
    let mover = solver
        .files
        .get(vertex(&solver, "h/mover.ts"))
        .map_or(u32::MAX, |file| file.container);
    let occupant = solver
        .files
        .get(vertex(&solver, "l/mover.ts"))
        .map_or(u32::MAX, |file| file.container);
    let evicted = solver.evict_newcomers(&polished, &mut parts, hub, landing);
    let mut withdrawals = Withdrawals::default();
    withdrawals.evicted.insert(mover, evicted);
    let mut folds = vec![CollisionFold {
        file: mover,
        into: occupant,
        offered: true,
    }];
    assert_eq!(
        parts.cluster_of(scc(&solver, "n/extra.ts")),
        Some(landing),
        "precondition: the withdrawal evicted n/extra.ts toward the occupant"
    );
    // a file with no pull toward its occupant: the symbol pass refuses the fold.
    let symbols = solver.symbol_polish_with_polish_evidence(
        &parts,
        &PolishEvidence {
            folds: folds.clone(),
            ..PolishEvidence::default()
        },
    );

    let undone = undo_refused_folds(&mut parts, &mut folds, &mut withdrawals, &symbols);

    assert!(undone, "the refused fold is undone");
    assert_eq!(parts, polished, "the eviction is reverted");
    assert!(
        folds.iter().all(|fold| !fold.offered),
        "a refused fold is not offered again"
    );
}

/// Every round of the refusal loop can offer a new fold, so the chain runs
/// past the number of folds the loop started with: folds 1, 2 and 3 each
/// produce the next when refused, and the loop only ends once none is offered.
#[test]
fn should_keep_retiring_refused_folds_along_a_chain_of_three() {
    let snapshot = flat_snapshot(&[("h", "h/a.ts"), ("l", "l/a.ts")], &[]);
    let config = AnalyzeConfig::default();
    let tests = TestPolicy::defaults();
    let solver = solver(&snapshot, &config, &tests);
    let mut parts = solver.real_partition.clone();
    let mut folds = vec![CollisionFold {
        file: 1,
        into: 9,
        offered: true,
    }];
    let mut withdrawals = Withdrawals::default();
    let mut settling = Settling {
        parts: &mut parts,
        folds: &mut folds,
        withdrawals: &mut withdrawals,
    };
    let refuse_all =
        solver.symbol_polish_with_polish_evidence(settling.parts, &PolishEvidence::default());
    let mut rounds = 0;

    let (latest, _symbols) =
        retire_refused_folds(solver.files.len(), &mut settling, refuse_all, |settling| {
            rounds += 1;
            // settling after a refusal offers the next fold of the chain.
            if rounds < 3 {
                settling.folds.push(CollisionFold {
                    file: 1 + rounds,
                    into: 9,
                    offered: true,
                });
            }
            (
                PolishEvidence::default(),
                solver
                    .symbol_polish_with_polish_evidence(settling.parts, &PolishEvidence::default()),
            )
        });

    assert_eq!(rounds, 3, "one round per fold of the chain");
    assert!(latest.is_some(), "the loop settled again after a refusal");
    assert!(
        folds.iter().all(|fold| !fold.offered),
        "no offered fold stays refused: {folds:?}"
    );
    assert_eq!(folds.len(), 3, "the chain offered three folds");
}

/// F8: with the wall up the identity guard keeps a same-named file out of an
/// occupied folder and package levels mirror the manifests, so a collision
/// practically never arises. When one is forced by hand, the fold it yields
/// pairs two files of one package.
#[test]
fn should_fold_a_wall_up_collision_within_one_package() {
    let snapshot = flat_snapshot(
        &[
            ("atlas/a", "atlas/a/util.ts"),
            ("atlas/b", "atlas/b/util.ts"),
            ("beta/c", "beta/c/util.ts"),
        ],
        &["atlas", "beta"],
    );
    let config = AnalyzeConfig::default();
    let tests = TestPolicy::defaults();
    let solver = solver(&snapshot, &config, &tests);
    let landing = home(&solver, "atlas/b/util.ts");
    assert!(
        !solver.relocation_identity.permits_join(
            &solver.real_partition,
            scc(&solver, "atlas/a/util.ts"),
            landing
        ),
        "the identity guard alone refuses the same-named join"
    );
    let mut parts = moved(&solver, &solver.real_partition, "atlas/a/util.ts", landing);
    let mut folds = Vec::new();
    let mut withdrawals = Withdrawals::default();

    let _withdrew = solver.fold_path_collisions(
        &mut parts,
        &PolishEvidence::default(),
        &mut folds,
        &mut withdrawals,
    );

    let package_of = |container: u32| {
        solver
            .files
            .iter()
            .find(|file| file.container == container)
            .map(|file| file.home.package.clone())
    };
    assert_eq!(
        folds.len(),
        1,
        "the forced collision yields one fold; got {folds:?}"
    );
    assert!(
        folds.iter().all(|fold| package_of(fold.file).is_some()
            && package_of(fold.file) == package_of(fold.into)),
        "a wall-up fold pairs files of one package; got {folds:?}"
    );
    assert_eq!(
        parts.cluster_of(scc(&solver, "atlas/a/util.ts")),
        Some(home(&solver, "atlas/a/util.ts")),
        "the colliding file returns home"
    );
}

/// N2: a layout that settles faithful renders as the current tree, which moves
/// no file and so prints no collision; it keeps no fold, so the symbol pass is
/// never offered a group move justified by a collision the report never shows.
#[test]
fn should_keep_no_fold_when_the_layout_settles_faithful() {
    let snapshot = flat_snapshot(&[("h", "h/mover.ts"), ("l", "l/mover.ts")], &[]);
    let config = AnalyzeConfig::default();
    let tests = TestPolicy::defaults();
    let solver = solver(&snapshot, &config, &tests);
    let landing = home(&solver, "l/mover.ts");
    let mut parts = moved(&solver, &solver.real_partition, "h/mover.ts", landing);
    let mut folds = Vec::new();
    let mut withdrawals = Withdrawals::default();
    assert!(
        !solver
            .path_collisions(&parts, &PolishEvidence::default())
            .is_empty(),
        "precondition: h/mover.ts lands on l/mover.ts"
    );

    let evidence = solver.settle_collisions(&mut parts, &mut folds, &mut withdrawals);

    assert_eq!(
        parts, solver.real_partition,
        "the withdrawn move returns the layout to reality"
    );
    assert!(solver.is_faithful(&parts), "the settled layout is faithful");
    assert!(
        folds.is_empty(),
        "a faithful layout keeps no fold; got {folds:?}"
    );
    assert!(
        evidence.folds.is_empty(),
        "the symbol pass is offered no fold; got {:?}",
        evidence.folds
    );
}

/// N3: a file whose fold the symbol pass refused is never offered again when
/// its collision comes back, so a second withdrawal evicts nothing that no
/// refusal would undo; a module root is never offered, an ordinary file is.
#[test]
fn should_not_offer_a_refused_fold_again() {
    let refused = CollisionFold {
        file: 7,
        into: 9,
        offered: false,
    };

    assert!(
        !offers_fold(7, "h/mover.ts", &[refused]),
        "a refused fold stays refused"
    );
    assert!(
        offers_fold(8, "h/other.ts", &[refused]),
        "another file's refusal does not bind this one"
    );
    assert!(
        !offers_fold(8, "h/index.ts", &[]),
        "a module root is never offered"
    );
    assert!(
        offers_fold(8, "h/other.ts", &[]),
        "an ordinary file is offered"
    );
}
