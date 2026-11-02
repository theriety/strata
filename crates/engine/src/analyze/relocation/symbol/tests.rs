#![allow(clippy::assertions_on_constants)]

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};
use strata_core::graph::csr::Csr;
use strata_core::score::Coefficients;
use strata_ir::{
    Affinity, AffinityKind, Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness,
    IntermediateRepresentation, Layout, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot,
    build_laminar_tree,
};

use crate::config::{AnalyzeConfig, ProfileConfig, TestsConfig};
use crate::result::{ContainerNode, Level, SymbolKind, SymbolMove};

use super::*;
use crate::analyze::relocation::solver::{build_file_graph, test_zone_marks};
use crate::analyze::test_support::*;
use crate::analyze::*;
use crate::analyze::{layout::*, relocation::*, scoring::*};

/// Symbol-relocation cases use greenfield where the anchored move-distance
/// price intentionally outweighs the fixture's cut improvement.
#[test]
fn should_relocate_a_misfiled_symbol_between_existing_files() {
    let snapshot = snapshot(
        vec![
            node(0, "s", 3, Polarity::Production),
            node(1, "mate", 3, Polarity::Production),
            node(2, "c1", 4, Polarity::Production),
            node(3, "c2", 4, Polarity::Production),
            node(4, "base", 4, Polarity::Production),
        ],
        vec![
            inherits(2, 0), // c1 extends s: strong pull toward b.ts …
            inherits(3, 0), // … twice over.
            edge(4, 2),     // base calls c1/c2: migrating them would
            edge(4, 3),     // re-sever more than following s gains.
        ],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "keep", ScopeLevel::Folder, Some(0)),
            container(2, "sink", ScopeLevel::Folder, Some(0)),
            container(3, "a.ts", ScopeLevel::File, Some(1)),
            container(4, "b.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.candidates = 1;
    let moves = analyze(&snapshot, &config)
        .ok()
        .and_then(|result| result.profiles.greenfield)
        .and_then(|mode| mode.candidates.into_iter().next())
        .map(|candidate| candidate.symbol_moves)
        .unwrap_or_default();

    // The symbol itself relocates between the two existing files, with an
    // improvement past the float-dust floor and no severed imports.
    let s_move = moves.iter().find(|entry| entry.symbol == "s");
    assert!(
        s_move.is_some_and(|entry| {
            entry.from_path == "a.ts"
                && entry.to_path == "b.ts"
                && entry.kind == SymbolKind::Symbol
                && entry.delta < -SYMBOL_MIN_IMPROVEMENT
                && entry.broken_imports == 0
        }),
        "the symbol pass must relocate s into its consumers' file with real \
             improvement and nothing severed; got {moves:?}"
    );
    // Nothing else relocates: migrating the consumers would re-sever their
    // calls to base, and a.ts keeps its co-resident either way.
    assert!(
        moves.iter().all(|entry| entry.symbol == "s"),
        "only s has pull justifying relocation; got {moves:?}"
    );
}

#[test]
fn should_count_a_surviving_source_consumer_as_an_import_to_repoint() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_step", 2, Polarity::Production),
            node(1, "source_consumer", 2, Polarity::Production),
            node(2, "destination_dependency", 3, Polarity::Production),
        ],
        vec![edge(1, 0), edge(0, 2)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "area/source.ts", ScopeLevel::File, Some(1)),
            container(3, "area/destination.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let moves = relocates_and_narrates_first_symbol(&snapshot);
    let broken_imports = moves
        .iter()
        .find(|entry| entry.symbol == "shared_step")
        .map(|entry| entry.broken_imports);

    assert_eq!(
        broken_imports,
        Some(1),
        "the consumer left in source.ts needs one new import after relocation; moves {moves:?}"
    );
}

fn relocates_and_narrates_first_symbol(snapshot: &Snapshot) -> Vec<SymbolMove> {
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let profile = &config.profiles.greenfield;
    let solver = PipelineSolver::new(
        snapshot,
        profile,
        profile.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let assembled = solver.assemble(&solver.real_partition);
    let from_file = ir
        .nodes
        .first()
        .and_then(|subject| assembled.placement.get(&subject.id.0))
        .copied();
    let to_file = ir
        .nodes
        .get(2)
        .and_then(|dependency| assembled.placement.get(&dependency.id.0))
        .copied();
    let (Some(from_file), Some(to_file)) = (from_file, to_file) else {
        return Vec::new();
    };
    let outcome = SymbolOutcome {
        overlay: [(0, to_file)].into_iter().collect(),
        relocations: vec![SymbolRelocation {
            node: 0,
            from_file,
            to_file,
            delta: 1.0,
        }],
        total: 0.0,
    };

    solver.symbol_narrate(&assembled, &outcome)
}

/// Builds the symbol-relocation shape with both files in either one or two
/// transparent source-root namespaces. Both roots normalize to the same
/// laminar homes, leaving namespace identity as the only changed premise.
fn transparent_namespace_symbol_snapshot(crosses_namespace: bool) -> Snapshot {
    let origin_namespace = "left";
    let destination_namespace = if crosses_namespace { "right" } else { "left" };
    let origin_path = SmolStr::new(format!("{origin_namespace}/area/keep/a.ts"));
    let destination_path = SmolStr::new(format!("{destination_namespace}/area/sink/b.ts"));
    let paths = vec![origin_path.clone(), destination_path.clone()];
    let layout = Layout {
        package_roots: Vec::new(),
        source_roots: vec![SmolStr::new("left"), SmolStr::new("right")],
    };
    let built = build_laminar_tree(&paths, "app", &layout);
    let origin_container = built
        .files
        .get(&origin_path)
        .map_or(u32::MAX, |container| container.0);
    let destination_container = built
        .files
        .get(&destination_path)
        .map_or(u32::MAX, |container| container.0);
    snapshot(
        vec![
            node(0, "subject", origin_container, Polarity::Production),
            node(1, "mate", origin_container, Polarity::Production),
            node(
                2,
                "first_consumer",
                destination_container,
                Polarity::Production,
            ),
            node(
                3,
                "second_consumer",
                destination_container,
                Polarity::Production,
            ),
            node(4, "base", destination_container, Polarity::Production),
        ],
        vec![inherits(2, 0), inherits(3, 0), edge(4, 2), edge(4, 3)],
        built.tree.containers().to_vec(),
    )
}

fn greenfield_symbol_moves(snapshot: &Snapshot) -> Vec<SymbolMove> {
    analyze(snapshot, &config_with_k(1))
        .ok()
        .and_then(|result| result.profiles.greenfield)
        .and_then(|mode| mode.candidates.into_iter().next())
        .map(|candidate| candidate.symbol_moves)
        .unwrap_or_default()
}

#[test]
fn should_veto_a_symbol_move_across_transparent_namespaces() {
    let snapshot = transparent_namespace_symbol_snapshot(true);
    let moves = greenfield_symbol_moves(&snapshot);

    assert!(
        moves.iter().all(|entry| entry.symbol != "subject"),
        "a symbol must remain inside its pass-start transparent namespace; got {moves:?}"
    );
}

#[test]
fn should_allow_a_symbol_move_within_its_transparent_namespace() {
    let snapshot = transparent_namespace_symbol_snapshot(false);
    let moves = greenfield_symbol_moves(&snapshot);

    assert!(
        moves.iter().any(|entry| {
            entry.symbol == "subject"
                && entry.from_path == "left/area/keep/a.ts"
                && entry.to_path == "left/area/sink/b.ts"
        }),
        "an ordinary symbol move inside one namespace must remain eligible; got {moves:?}"
    );
}

#[test]
fn should_reject_a_cross_namespace_symbol_in_the_final_overlay() {
    let snapshot = transparent_namespace_symbol_snapshot(true);
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    let source = assembled
        .placement
        .get(&0)
        .copied()
        .unwrap_or(ContainerId(u32::MAX));
    let destination = assembled
        .placement
        .get(&2)
        .copied()
        .unwrap_or(ContainerId(u32::MAX));
    let outcome = SymbolOutcome {
        overlay: BTreeMap::from([(0, destination)]),
        relocations: vec![SymbolRelocation {
            node: 0,
            from_file: source,
            to_file: destination,
            delta: 1.0,
        }],
        total: 0.0,
    };

    assert!(!outcome.preserves_namespaces(&assembled));
}

/// The FIX04 doctrine at symbol grain: zero-priced edges nominate nothing,
/// so a symbol connected only through re-exports is never relocated, no
/// matter how many of them point across folders.
#[test]
fn should_let_zero_priced_edges_nominate_no_symbol_target() {
    let snapshot = snapshot(
        vec![
            node(0, "s", 3, Polarity::Production),
            node(1, "fill", 3, Polarity::Production),
            node(2, "c1", 4, Polarity::Production),
            node(3, "c2", 4, Polarity::Production),
        ],
        vec![reexport(2, 0), reexport(3, 0)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "keep", ScopeLevel::Folder, Some(0)),
            container(2, "sink", ScopeLevel::Folder, Some(0)),
            container(3, "a.ts", ScopeLevel::File, Some(1)),
            container(4, "b.ts", ScopeLevel::File, Some(2)),
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
    let outcome = solver.symbol_polish(&solver.real_partition.clone());

    assert!(
        outcome.relocations.is_empty() && outcome.overlay.is_empty(),
        "re-export edges are priced 0.0 and never bind placement"
    );
}

/// The no-empty-shells veto: a file's last production resident stays home
/// even when an out-of-file pull exists — draining the file would be a
/// file move wearing a symbol costume, which v1 does not propose.
#[test]
fn should_not_drain_a_file_of_its_last_resident() {
    let snapshot = snapshot(
        vec![
            node(0, "s", 3, Polarity::Production),
            node(1, "c1", 4, Polarity::Production),
            node(2, "c2", 4, Polarity::Production),
            node(3, "base", 4, Polarity::Production),
        ],
        vec![
            type_ref(1, 0), // the consumers do pull s across …
            type_ref(2, 0), //
            edge(3, 1),     // … but base binds them home, so the only
            edge(3, 2),     // candidate move is s's, which must be vetoed.
        ],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "keep", ScopeLevel::Folder, Some(0)),
            container(2, "sink", ScopeLevel::Folder, Some(0)),
            container(3, "a.ts", ScopeLevel::File, Some(1)),
            container(4, "b.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.candidates = 1;
    let undrained = analyze(&snapshot, &config).is_ok_and(|result| {
        [&result.profiles.anchored, &result.profiles.greenfield]
            .into_iter()
            .flatten()
            .flat_map(|mode| &mode.candidates)
            .all(|candidate| {
                candidate
                    .symbol_moves
                    .iter()
                    .all(|mv| mv.from_path != "a.ts")
            })
    });
    assert!(
        undrained,
        "a.ts holds s alone; relocating it empties the file, so the veto \
             must hold"
    );
}

/// The placement-aware move_distance extension (FIX08): a node whose
/// effective placement lands in a different FILE counts as moved even when
/// both files keep their folder keys, while the identical layout with
/// home-file placements measures zero.
#[test]
fn should_count_a_file_grain_identity_placement_as_unmoved() {
    let ir = IntermediateRepresentation::new(
        vec![
            node(0, "s", 3, Polarity::Production),
            node(1, "t", 4, Polarity::Production),
        ],
        vec![],
        ContainerTree::new(vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "keep", ScopeLevel::Folder, Some(0)),
            container(2, "sink", ScopeLevel::Folder, Some(0)),
            container(3, "a.ts", ScopeLevel::File, Some(1)),
            container(4, "b.ts", ScopeLevel::File, Some(2)),
        ]),
    );
    // The fixture layout is static, so a failed assemble means the fixture
    // itself rotted — surface that loudly rather than testing a fallback.
    let assembled = Snapshot::assemble(ir);
    assert!(assembled.is_ok(), "fixture layout must assemble");
    let Ok(snap) = assembled else {
        return;
    };

    // Candidate tree mirrors the current folders; placement sends s from
    // a.ts (container 3) into b.ts (container 4).
    let candidate = ContainerTree::new(snap.ir().containers.containers().to_vec());
    let distance = move_distance(&snap, &candidate, &|id| (id == 0).then_some(ContainerId(4)));
    assert!(
        distance > 0.0,
        "s left its home file, so μ must see the move: measured {distance}"
    );

    let home = move_distance(&snap, &candidate, &|_| None);
    assert!(
        home.abs() < f64::EPSILON,
        "unplaced nodes fall back to folder keys, which did not change: \
             measured {home}"
    );
}

/// Builds a minimal [`FileInfo`] owning `container`, whose naming evidence
/// is just its path; laminar home keys never feed label formation, so they
/// stay blank.
fn file_info(container: u32, path: &str, sloc: u32) -> FileInfo {
    FileInfo {
        container,
        name: SmolStr::new(path),
        production_sloc: sloc,
        namespace: SmolStr::new(""),
        home: LaminarHome {
            folder: SmolStr::new(""),
            domain: SmolStr::new(""),
            package: SmolStr::new(""),
            synthetic: false,
        },
    }
}

/// An all-digit shared token must never name a rebuilt place (FIX09 label
/// honesty): `helpers/2024` is indistinguishable from the numeric fallback
/// and can never align under the contract tokenizer, which drops digit
/// tokens. The stem path names the place instead.
#[test]
fn should_skip_a_digit_token_when_naming_a_rebuilt_place() {
    let condensation = singleton_condensation(2);
    let files = vec![
        file_info(0, "report_2024.py", 10),
        file_info(0, "audit_2024.py", 10),
    ];
    let mut used = BTreeSet::new();

    let label = rebuild_label("helpers", &[0, 1], &condensation, &files, &mut used, 9);

    assert_eq!(
        label, "helpers/audit-report",
        "the only shared token is the digit run '2024'; the joined stems \
             must name the place instead"
    );
}

/// Token grouping joins two SCCs only on a genuinely shared basename
/// token; unrelated files stay in their own groups.
#[test]
fn should_group_only_sccs_sharing_a_basename_token() {
    let condensation = singleton_condensation(3);
    let files = vec![
        file_info(0, "alpha.py", 1),
        file_info(0, "alpha_beta.py", 1),
        file_info(0, "gamma.py", 1),
    ];

    let groups = token_groups(&[0, 1, 2], &condensation, &files);

    assert_eq!(
        groups,
        vec![vec![0, 1], vec![2]],
        "'alpha' joins the first pair; 'gamma' shares nothing and stays alone"
    );
}

/// A file with any priced incident edge is bonded, and its whole SCC stays
/// glued: bonded company never enters the stranger population, so a folder
/// holding only bonded files and ungroupable strays fires nothing.
#[test]
fn should_keep_a_priced_bond_out_of_the_stranger_population() {
    // beta carries the only priced edge, so alpha and gamma are strangers —
    // but they share no token, so no group of two forms and nothing fires.
    let graph = Csr::from_weighted_edges(3, &[(0_u32, 1_u32, 1.0_f32)]);
    let condensation = singleton_condensation(3);
    let base = Partition::from_assignment(vec![ClusterId(0), ClusterId(0), ClusterId(0)], 1);
    let files = vec![
        file_info(0, "alpha.py", 1),
        file_info(0, "beta.py", 1),
        file_info(0, "gamma.py", 1),
    ];
    let mut names = vec![SmolStr::new("helpers")];
    let mut synthetic = vec![false];

    let rebuilt = synthesize_roof_rebuild(
        &files,
        &condensation,
        &graph,
        &[false, false, false],
        &[false, false, false],
        &base,
        &mut names,
        &mut synthetic,
    );

    assert!(
        rebuilt.is_none(),
        "no token group of two forms among the strangers, and the bonded \
             residual is a lone file, so the folder must stay put"
    );
}

#[test]
fn should_keep_a_collision_free_roof_subgroup_when_one_leaf_is_duplicated() {
    let graph = Csr::from_weighted_edges(5, &[(3_u32, 4_u32, 1.0_f32)]);
    let condensation = singleton_condensation(5);
    let base = Partition::from_assignment(vec![ClusterId(0); 5], 1);
    let files = vec![
        file_info(0, "left/record.ts", 1),
        file_info(1, "right/record.ts", 1),
        file_info(2, "record-helper.ts", 1),
        file_info(3, "anchor.ts", 1),
        file_info(4, "resident.ts", 1),
    ];
    let mut names = vec![SmolStr::new("misc")];
    let mut synthetic = vec![false];

    let rebuilt = synthesize_roof_rebuild(
        &files,
        &condensation,
        &graph,
        &[false; 5],
        &[false; 5],
        &base,
        &mut names,
        &mut synthetic,
    );
    let guard = RelocationIdentityGuard::new(&files, &condensation, &base, &base, rebuilt.as_ref());

    let retained = rebuilt.as_ref().is_some_and(|partition| {
        partition.cluster_of(0) == partition.cluster_of(2)
            && partition.cluster_of(0) != partition.cluster_of(1)
            && guard.accepts(partition)
    });
    assert!(
        retained,
        "roof synthesis must retain the unique record-helper subgroup while separating the \
             duplicate record leaf; partition {rebuilt:?}"
    );
}

/// The end-to-end FIX09 win: zero-priced strangers sharing a basename token
/// under a misnamed roof leave for a place named after their own shared
/// word, so the greenfield pool carries a genuinely different shape.
#[test]
fn should_synthesize_a_place_named_after_its_strangers() {
    // checkout calls into helpers/charge; every other helpers file prices
    // zero anywhere, so refund/string/date are the unanchored population.
    let snapshot = snapshot(
        vec![
            node(0, "pay", 1, Polarity::Production),
            node(1, "charge_card", 3, Polarity::Production),
            node(2, "refund_card", 4, Polarity::Production),
            node(3, "slugify", 5, Polarity::Production),
            node(4, "parse_iso", 6, Polarity::Production),
        ],
        vec![edge(0, 1)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "checkout.py", ScopeLevel::File, Some(0)),
            container(2, "helpers", ScopeLevel::Folder, Some(0)),
            container(3, "charge.py", ScopeLevel::File, Some(2)),
            container(4, "refund.py", ScopeLevel::File, Some(2)),
            container(5, "string_utils.py", ScopeLevel::File, Some(2)),
            container(6, "date_utils.py", ScopeLevel::File, Some(2)),
        ],
    );
    let config = config_with_k(3);

    // analysis failures surface as an empty candidate list, so the
    // carries_rebuild assertion below fails informatively (house style:
    // no panic!/expect in tests).
    let greenfield = analyze(&snapshot, &config)
        .ok()
        .and_then(|result| result.profiles.greenfield)
        .unwrap_or_else(empty_profile_result);

    let carries_rebuild = greenfield.candidates.iter().any(|candidate| {
        // the rebuilt label path-extends its base folder, so the rendered
        // tree nests `utils` under `helpers` rather than naming one node
        // with the slash — match the pair structurally.
        find_named(&candidate.tree, "helpers")
            .and_then(|helpers| {
                helpers
                    .children
                    .as_ref()
                    .and_then(|children| children.iter().find(|child| child.name == "utils"))
            })
            .is_some_and(|place| {
                let paths = descendant_files(place);
                paths.iter().any(|path| path.ends_with("string_utils.py"))
                    && paths.iter().any(|path| path.ends_with("date_utils.py"))
                    && paths.len() == 2
            })
    });
    assert!(
        carries_rebuild,
        "string_utils and date_utils share only the roof's misnomer; the \
             synthesized helpers/utils place must reach the greenfield pool \
             nested under helpers, holding exactly those two files"
    );
}

/// Depth-first search for a non-file container whose name matches exactly.
fn find_named<'a>(node: &'a ContainerNode, name: &str) -> Option<&'a ContainerNode> {
    if node.level != Level::File && node.name == name {
        return Some(node);
    }
    node.children
        .iter()
        .flatten()
        .find_map(|child| find_named(child, name))
}

/// Collects every descendant file name beneath one rendered container.
fn descendant_files(node: &ContainerNode) -> Vec<String> {
    let mut paths = Vec::new();
    accumulate_files(node, &mut paths);
    paths
}

/// Depth-first accumulation of descendant file names.
fn accumulate_files(node: &ContainerNode, out: &mut Vec<String>) {
    if node.level == Level::File {
        out.push(node.name.clone());
        return;
    }
    for child in node.children.iter().flatten() {
        accumulate_files(child, out);
    }
}

/// Compiles a `[tests]` section into a policy, failing loudly on an
/// invalid fixture pattern — a broken test must surface, never hide.
#[allow(clippy::panic)] // loud failure is the point of this test helper
fn policy(tests: &TestsConfig) -> TestPolicy {
    match TestPolicy::new(tests) {
        Ok(compiled) => compiled,
        Err(error) => panic!("test policy failed to compile: {error}"),
    }
}

/// Returns the weight of the `from -> to` edge in a CSR, or `None` when no
/// such edge exists.
fn edge_weight(graph: &Csr, from: u32, to: u32) -> Option<f32> {
    let slot = graph
        .neighbors(from)
        .iter()
        .position(|&target| target == to)?;
    graph.weights(from).get(slot).copied()
}

#[test]
fn should_mark_a_polarity_detected_spec_as_the_test_zone() {
    // the spec holds one test-case symbol; its subject is production.
    let nodes = vec![
        node(0, "openai", 1, Polarity::Production),
        node(1, "openai_spec", 2, Polarity::TestCase),
    ];
    let files = [
        file_info(1, "src/openai.ts", 1),
        file_info(2, "src/openai.spec.ts", 0),
    ];

    assert_eq!(
        test_zone_marks(&TestPolicy::defaults(), &files, &nodes),
        vec![false, true]
    );
}

#[test]
fn should_not_mark_a_file_holding_any_production_symbol() {
    let nodes = vec![
        node(0, "helper", 1, Polarity::TestCase),
        node(1, "real", 1, Polarity::Production),
    ];
    let files = [file_info(1, "src/mixed.ts", 1)];

    assert_eq!(
        test_zone_marks(&TestPolicy::defaults(), &files, &nodes),
        vec![false]
    );
}

#[test]
fn should_mark_a_pattern_matched_file_even_when_production_polarity() {
    let tests = TestsConfig {
        patterns: vec!["*.custom-test.*".to_owned()],
        ..TestsConfig::default()
    };
    let nodes = vec![node(0, "weird", 1, Polarity::Production)];
    let files = [
        file_info(1, "src/plain.ts", 1),
        file_info(2, "src/weird.custom-test.ts", 1),
    ];

    assert_eq!(
        test_zone_marks(&policy(&tests), &files, &nodes),
        vec![false, true]
    );
}

#[test]
fn should_mark_production_polarity_support_under_a_builtin_test_root() {
    let nodes = vec![node(0, "shared_fixture", 1, Polarity::Production)];
    let files = [file_info(1, "tests/support/fixtures.ts", 1)];

    assert_eq!(
        test_zone_marks(&TestPolicy::defaults(), &files, &nodes),
        vec![true],
        "test-support declarations inherit the conventional test root"
    );
}

#[test]
fn should_pin_symbols_in_production_polarity_test_support_by_default() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_fixture", 3, Polarity::Production),
            node(1, "fixture_resident", 3, Polarity::Production),
            node(2, "consumer", 4, Polarity::Production),
        ],
        vec![edge(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "tests/support", ScopeLevel::Folder, Some(0)),
            container(2, "src", ScopeLevel::Folder, Some(0)),
            container(3, "tests/support/fixtures.ts", ScopeLevel::File, Some(1)),
            container(4, "src/consumer.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.anchored.objective.coefficients(),
        true,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    let support_file = assembled.placement.get(&0).copied();

    assert!(support_file.is_some(), "support symbol must have a file");
    assert!(
        support_file.is_some_and(|file| solver.symbol_source_blocks(&assembled).contains(&file)),
        "default test-symbol policy must block support declarations before nomination"
    );
}

/// Bundles a solver's pricing inputs for a directly constructed pass.
fn pass_inputs<'a>(
    snapshot: &'a Snapshot,
    solver: &'a PipelineSolver<'_>,
    assembled: &'a CandidateTree,
) -> PassInputs<'a> {
    let ir = snapshot.ir();
    PassInputs {
        snapshot,
        coefficients: &solver.coefficients,
        weights: &solver.weights,
        same_file_symbol: solver.same_file_symbol,
        same_file_type: solver.same_file_type,
        capacity: solver.capacity,
        assembled,
        nodes: &ir.nodes,
        edges: &ir.edges,
    }
}

/// Fixture where symbol `s`, carrying `polarity`, sits in `a.ts` and is pulled
/// toward `b.ts` by two consumers that also feed `base`.
fn misfiled_symbol_snapshot(polarity: Polarity) -> Snapshot {
    snapshot(
        vec![
            node(0, "s", 3, polarity),
            node(1, "mate", 3, Polarity::Production),
            node(2, "c1", 4, Polarity::Production),
            node(3, "c2", 4, Polarity::Production),
            node(4, "base", 4, Polarity::Production),
            // a second production resident keeps a.ts above the no-empty-shell
            // floor, which counts production declarations only.
            node(5, "mate2", 3, Polarity::Production),
        ],
        vec![inherits(2, 0), inherits(3, 0), edge(4, 2), edge(4, 3)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "keep", ScopeLevel::Folder, Some(0)),
            container(2, "sink", ScopeLevel::Folder, Some(0)),
            container(3, "a.ts", ScopeLevel::File, Some(1)),
            container(4, "b.ts", ScopeLevel::File, Some(2)),
        ],
    )
}

/// Greenfield symbol moves for the misfiled-symbol fixture with `s` carrying
/// `polarity`, under the given `pin-detected-test-symbols` policy.
fn misfiled_symbol_moves(polarity: Polarity, pin_test_symbols: bool) -> Vec<SymbolMove> {
    let snapshot = misfiled_symbol_snapshot(polarity);
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.candidates = 1;
    config
        .profiles
        .greenfield
        .relocation
        .pin_detected_test_symbols = pin_test_symbols;
    let result = analyze(&snapshot, &config);
    assert!(result.is_ok(), "analysis must succeed; got {result:?}");
    let candidate = result
        .ok()
        .and_then(|result| result.profiles.greenfield)
        .and_then(|mode| mode.candidates.into_iter().next());
    // a.ts still misfiles its production resident's consumers, so a
    // greenfield candidate exists either way; its absence would make the
    // pin assertion vacuous.
    assert!(
        candidate.is_some(),
        "the misfiled fixture must yield a greenfield candidate"
    );
    candidate
        .map(|candidate| candidate.symbol_moves)
        .unwrap_or_default()
}

#[test]
fn should_pin_a_test_polarity_symbol_living_in_a_production_file() {
    for polarity in [Polarity::TestSupport, Polarity::TestCase] {
        let moves = misfiled_symbol_moves(polarity, true);

        assert!(
            moves.iter().all(|entry| entry.symbol != "s"),
            "a {polarity:?} declaration in a production file stays pinned by its \
             own polarity; got {moves:?}"
        );
    }
}

#[test]
fn should_release_a_test_polarity_symbol_when_symbol_pinning_is_off() {
    for polarity in [Polarity::TestSupport, Polarity::TestCase] {
        let moves = misfiled_symbol_moves(polarity, false);

        assert!(
            moves.iter().any(|entry| entry.symbol == "s"),
            "without pin-detected-test-symbols the same pull relocates the \
             {polarity:?} declaration, proving the pin is what held it; got {moves:?}"
        );
    }
}

/// Drives a bare pass over the misfiled fixture so the polarity pin is the
/// only thing that can hold `s`, independent of the config plumbing.
fn relocates_test_polarity_symbol(pin_test_polarity: bool) -> bool {
    let snapshot = misfiled_symbol_snapshot(Polarity::TestSupport);
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new_with_policy(
        pass_inputs(&snapshot, &solver, &assembled),
        RelocationPolicy {
            pin_test_polarity,
            ..RelocationPolicy::default()
        },
    );
    let ir = snapshot.ir();
    let subject = ir.nodes.first();
    assert!(subject.is_some(), "the fixture declares symbol s first");
    subject.is_some_and(|subject| pass.try_relocate(subject))
}

#[test]
fn should_refuse_a_test_polarity_symbol_only_when_the_pass_pins_polarity() {
    assert!(
        relocates_test_polarity_symbol(false),
        "without the pin the fixture's pull must relocate s, else the pin case is vacuous"
    );
    assert!(
        !relocates_test_polarity_symbol(true),
        "the polarity pin alone must hold a test-polarity declaration in place"
    );
}

#[test]
fn should_leave_the_zone_inert_when_builtins_are_off_and_no_patterns_given() {
    let nodes = vec![node(0, "spec", 1, Polarity::TestCase)];
    let files = [file_info(1, "src/x.spec.ts", 0)];

    assert_eq!(
        test_zone_marks(&TestPolicy::disabled(), &files, &nodes),
        vec![false]
    );
}

#[test]
fn should_zero_price_every_edge_touching_the_test_zone() {
    // spec imports both production files; the two production files bond
    // with each other. Only edges touching vertex 1 (the spec) may cut.
    let nodes = vec![
        node(0, "alpha", 1, Polarity::Production),
        node(1, "beta", 2, Polarity::Production),
        node(2, "alpha_spec", 3, Polarity::TestCase),
    ];
    let edges = vec![edge(0, 1), edge(2, 0)];
    let index_of = BTreeMap::from([(1_u32, 0_u32), (2, 1), (3, 2)]);
    let weights = AnalyzeConfig::default()
        .profiles
        .anchored
        .weights
        .kind_weights();
    let test_zone = vec![false, false, true];

    let graph = build_file_graph(&edges, &nodes, &index_of, 3, &weights, &test_zone);

    // FIX04 doctrine: cut edges stay in the graph at price zero — the CSR
    // shape must not change, only the binding.
    #[allow(clippy::cast_possible_truncation)] // mirrors the production narrowing
    let call = weights.call as f32;
    assert_eq!(graph.edge_count(), 2);
    assert_eq!(edge_weight(&graph, 0, 1), Some(call));
    assert_eq!(edge_weight(&graph, 1, 0), None); // never priced in reverse
    assert_eq!(edge_weight(&graph, 2, 0), Some(0.0)); // spec -> subject: cut
}

#[test]
fn should_zero_price_test_to_test_edges_in_both_directions() {
    let nodes = vec![
        node(0, "one", 1, Polarity::TestCase),
        node(1, "two", 2, Polarity::TestCase),
    ];
    let edges = vec![edge(0, 1), edge(1, 0)];
    let index_of = BTreeMap::from([(1_u32, 0_u32), (2, 1_u32)]);
    let weights = AnalyzeConfig::default()
        .profiles
        .anchored
        .weights
        .kind_weights();
    let test_zone = vec![true, true];

    let graph = build_file_graph(&edges, &nodes, &index_of, 2, &weights, &test_zone);

    assert_eq!(graph.edge_count(), 2);
    assert_eq!(edge_weight(&graph, 0, 1), Some(0.0));
    assert_eq!(edge_weight(&graph, 1, 0), Some(0.0));
}

#[test]
fn should_give_a_spec_no_priced_pull_toward_its_subject() {
    // The exact openai.spec.ts shape: a mirrored spec tree importing its
    // production twin. Condensation stays structural (Tarjan folds real
    // cycles whatever they cost), but every stage that moves files —
    // relief piles, polish pulls, heavy-edge matching — reads prices and
    // skips zero, so the cut must leave no priced edge anywhere between
    // the pair's components.
    let nodes = vec![
        node(0, "openai", 1, Polarity::Production),
        node(1, "openai_spec", 2, Polarity::TestCase),
    ];
    let edges = vec![edge(1, 0)];
    let containers = vec![
        container(0, "workspace", ScopeLevel::PackageGroup, None),
        container(1, "src/openai.ts", ScopeLevel::File, Some(0)),
        container(2, "spec/openai.spec.ts", ScopeLevel::File, Some(0)),
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

    let mut priced: Vec<(u32, u32)> = Vec::new();
    for vertex in 0..solver.condensation.dag.vertex_count() {
        let from = u32::try_from(vertex).unwrap_or(u32::MAX);
        let weights = solver.condensation.dag.weights(from);
        for (slot, &to) in solver.condensation.dag.neighbors(from).iter().enumerate() {
            if weights.get(slot).copied().unwrap_or(0.0) > 0.0 {
                priced.push((from, to));
            }
        }
    }

    assert!(
        priced.is_empty(),
        "the tie-cut left a priced edge between spec and subject: {priced:?}"
    );
}

/// The FIX11 repro, mirrored on the `ai` codec.ts ↔ codec.spec.ts shape: a
/// production subject whose spec twin pulls hardest (two inherited
/// extensions) must never be relocated across the source/test boundary —
/// the file-graph tie-cut prices that bond at zero, so the symbol pass
/// must read the same price and nominate nothing across it, in either
/// direction.
#[test]
fn should_not_relocate_a_production_symbol_into_its_spec_twin() {
    let snapshot = snapshot(
        vec![
            node(0, "to_gemini_image_response", 3, Polarity::Production),
            node(1, "codec_helper", 3, Polarity::Production),
            node(2, "google_codec", 4, Polarity::Production),
            node(3, "batch_codec", 4, Polarity::Production),
            node(4, "codec_spec_case", 5, Polarity::TestCase),
        ],
        vec![
            edge(2, 0),     // consumers import the subject, but the
            edge(3, 0),     // spec twin pulls harder: the mirrored
            inherits(4, 0), // spec extends it twice over, the heaviest
            inherits(4, 0), // priced pull this graph can carry.
        ],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "spec", ScopeLevel::Folder, Some(0)),
            container(3, "src/codec.ts", ScopeLevel::File, Some(1)),
            container(4, "src/consumers.ts", ScopeLevel::File, Some(1)),
            container(5, "spec/codec.spec.ts", ScopeLevel::File, Some(2)),
        ],
    );

    // The pass driven straight on the real layout: no relocation may
    // touch the spec twin, in either direction.
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
    let assembled = solver.assemble(&solver.real_partition);
    let spec_file = assembled
        .tree
        .containers()
        .iter()
        .find(|file| file.level == ScopeLevel::File && file.name.as_str() == "spec/codec.spec.ts")
        .map(|file| file.id);
    let outcome = solver.symbol_polish(&solver.real_partition);
    let crossings: Vec<String> = outcome
        .relocations
        .iter()
        .filter(|relocation| {
            Some(relocation.to_file) == spec_file || Some(relocation.from_file) == spec_file
        })
        .map(|relocation| {
            format!(
                "node {} across file {}",
                relocation.node, relocation.to_file.0
            )
        })
        .collect();
    assert!(
        crossings.is_empty(),
        "the symbol pass must never relocate across the source/test \
             boundary in either direction; crossed {crossings:?}"
    );

    // End to end: no candidate of either mode narrates a move whose
    // destination is the spec twin.
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.candidates = 1;
    let offenders: Vec<String> = analyze(&snapshot, &config)
        .ok()
        .map(|result| {
            [&result.profiles.anchored, &result.profiles.greenfield]
                .into_iter()
                .flatten()
                .flat_map(|mode| &mode.candidates)
                .flat_map(|candidate| &candidate.symbol_moves)
                .filter(|move_entry| move_entry.to_path.ends_with(".spec.ts"))
                .map(|move_entry| {
                    format!(
                        "{}: {} -> {}",
                        move_entry.symbol, move_entry.from_path, move_entry.to_path
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        offenders.is_empty(),
        "no candidate may suggest moving a production symbol into its \
             spec twin; suggested {offenders:?}"
    );
}

/// The FIX11 incidence tie-cut at symbol grain: an edge with either
/// endpoint resident in a test-zone file prices zero in the pass's
/// incident map, exactly as `build_file_graph` prices it — while priced
/// production bonds survive untouched.
#[test]
fn should_zero_price_incident_edges_touching_the_test_zone() {
    let snapshot = snapshot(
        vec![
            node(0, "gemini_image_codec", 3, Polarity::Production),
            node(1, "codec_helper", 3, Polarity::Production),
            node(2, "google_codec", 4, Polarity::Production),
            node(3, "codec_spec_case", 5, Polarity::TestCase),
        ],
        vec![edge(2, 0), edge(2, 1), inherits(3, 0)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "spec", ScopeLevel::Folder, Some(0)),
            container(3, "src/codec.ts", ScopeLevel::File, Some(1)),
            container(4, "src/google.ts", ScopeLevel::File, Some(1)),
            container(5, "spec/codec.spec.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    let pass = SymbolPass::new(pass_inputs(&snapshot, &solver, &assembled));

    // the spec twin nominates nothing: no incident slot at all.
    assert!(
        pass.incident.get(&3).is_none_or(Vec::is_empty),
        "the test-zone resident must carry no priced incident edges"
    );
    // and no priced production link points at it either way.
    for id in [0u32, 1, 2] {
        let links = pass.incident.get(&id).map_or(&[][..], Vec::as_slice);
        assert!(
            links.iter().all(|&(neighbour, _)| neighbour != 3),
            "node {id} still carries a priced link into the test zone"
        );
    }
    // positive control: the production bond survived the cut.
    let subject_links = pass.incident.get(&0).map_or(&[][..], Vec::as_slice);
    assert!(
        subject_links.iter().any(|&(neighbour, _)| neighbour == 2),
        "the codec's priced bond to its consumer must survive the cut"
    );
}

/// Defense in depth: even when a priced link smuggles a cross-boundary
/// destination into a symbol's nomination ranking, the static boundary
/// veto bars the relocation before any evaluation runs.
#[test]
fn should_veto_a_cross_boundary_destination_even_when_priced() {
    let snapshot = snapshot(
        vec![
            node(0, "gemini_image_codec", 3, Polarity::Production),
            node(1, "codec_helper", 3, Polarity::Production),
            node(2, "codec_spec_case", 4, Polarity::TestCase),
        ],
        vec![inherits(2, 0)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "spec", ScopeLevel::Folder, Some(0)),
            container(3, "src/codec.ts", ScopeLevel::File, Some(1)),
            container(4, "spec/codec.spec.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new(pass_inputs(&snapshot, &solver, &assembled));
    // simulate a future nomination path that prices the twin pull despite
    // the tie-cut: the strongest possible lure across the boundary.
    pass.incident.entry(0).or_default().push((2, 5.0));
    // Float surgery: hold strict-J aside so a J-cost rejection cannot
    // masquerade as the veto — with best at infinity, any destination
    // that survives the gate chain is deterministically accepted, so
    // `!accepted` proves THIS veto fired.
    pass.best = f64::INFINITY;

    let accepted = ir
        .nodes
        .first()
        .is_some_and(|subject| pass.try_relocate(subject));
    assert!(
        !accepted && pass.relocations.is_empty(),
        "a cross-boundary destination must be vetoed even when priced; \
             relocations {:?}",
        pass.relocations.iter().map(|r| r.node).collect::<Vec<_>>()
    );
}

/// The FIX13 shape: a type two sibling adapters share must not be buried
/// inside either of them. Both homes cross at `adapters` and sit at the
/// same depth, so every priced term rates them identically — the refusal
/// has to be structural.
#[test]
fn should_veto_burying_a_shared_symbol_inside_one_of_its_consumers() {
    let (accepted, relocated) = relocates_first_symbol(&shared_adapter_type_snapshot());

    assert!(
        !accepted && relocated.is_empty(),
        "a symbol two sibling folders share must not fold into either; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_veto_burying_shared_ownership_despite_unrelated_reach() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_contract", 6, Polarity::Production),
            node(1, "common_resident", 6, Polarity::Production),
            node(2, "left_consumer", 7, Polarity::Production),
            node(3, "right_consumer", 8, Polarity::Production),
            node(4, "left_service", 7, Polarity::Production),
            node(5, "right_client", 8, Polarity::Production),
        ],
        vec![type_ref(2, 0), type_ref(3, 0), edge(5, 4), edge(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "components", ScopeLevel::Domain, Some(0)),
            container(2, "components/common", ScopeLevel::Folder, Some(1)),
            container(3, "components/left", ScopeLevel::Folder, Some(1)),
            container(4, "components/right", ScopeLevel::Folder, Some(1)),
            container(5, "unused", ScopeLevel::Folder, Some(1)),
            container(
                6,
                "components/common/contracts.ts",
                ScopeLevel::File,
                Some(2),
            ),
            container(7, "components/left/service.ts", ScopeLevel::File, Some(3)),
            container(8, "components/right/client.ts", ScopeLevel::File, Some(4)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol_with_dependency_only(&snapshot, 0.0);

    assert!(
        !accepted && relocated.is_empty(),
        "an unrelated right-to-left dependency must not transfer ownership of a shared \
             declaration to the left branch; relocations {relocated:?}"
    );
}

#[test]
fn should_allow_shared_consumers_that_remain_in_one_folder() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_contract", 3, Polarity::Production),
            node(1, "origin_resident", 3, Polarity::Production),
            node(2, "first_consumer", 4, Polarity::Production),
            node(3, "second_consumer", 5, Polarity::Production),
        ],
        vec![type_ref(2, 0), type_ref(2, 0), type_ref(3, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "components", ScopeLevel::Folder, Some(0)),
            container(2, "components/shared.ts", ScopeLevel::File, Some(1)),
            container(3, "components/first.ts", ScopeLevel::File, Some(1)),
            container(4, "components/second.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == [0],
        "consumers in one physical folder do not create competing branch ownership; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_allow_a_shared_declaration_to_move_into_a_neutral_branch() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_contract", 6, Polarity::Production),
            node(1, "origin_resident", 6, Polarity::Production),
            node(2, "left_consumer", 7, Polarity::Production),
            node(3, "right_consumer", 8, Polarity::Production),
            node(4, "neutral_dependency", 9, Polarity::Production),
        ],
        vec![
            type_ref(2, 0),
            type_ref(3, 0),
            type_ref(0, 4),
            type_ref(0, 4),
        ],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "components", ScopeLevel::Domain, Some(0)),
            container(2, "components/holding", ScopeLevel::Folder, Some(1)),
            container(3, "components/left", ScopeLevel::Folder, Some(1)),
            container(4, "components/right", ScopeLevel::Folder, Some(1)),
            container(5, "components/common", ScopeLevel::Folder, Some(1)),
            container(
                6,
                "components/holding/contracts.ts",
                ScopeLevel::File,
                Some(2),
            ),
            container(7, "components/left/consumer.ts", ScopeLevel::File, Some(3)),
            container(8, "components/right/consumer.ts", ScopeLevel::File, Some(4)),
            container(9, "components/common/helpers.ts", ScopeLevel::File, Some(5)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol_with_dependency_only(&snapshot, 0.0);

    assert!(
        accepted && relocated == [0],
        "a neutral sibling branch does not award either consumer branch ownership; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_allow_a_shared_declaration_to_move_toward_the_consumer_lca() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_contract", 6, Polarity::Production),
            node(1, "origin_resident", 6, Polarity::Production),
            node(2, "left_consumer", 7, Polarity::Production),
            node(3, "right_consumer", 8, Polarity::Production),
            node(4, "common_dependency", 9, Polarity::Production),
        ],
        vec![
            type_ref(2, 0),
            type_ref(3, 0),
            type_ref(0, 4),
            type_ref(0, 4),
        ],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "components", ScopeLevel::Domain, Some(0)),
            container(2, "components/holding", ScopeLevel::Folder, Some(1)),
            container(3, "components/left", ScopeLevel::Folder, Some(1)),
            container(4, "components/right", ScopeLevel::Folder, Some(1)),
            container(
                5,
                "components/holding/contracts.ts",
                ScopeLevel::File,
                Some(2),
            ),
            container(6, "components/left/consumer.ts", ScopeLevel::File, Some(3)),
            container(7, "components/right/consumer.ts", ScopeLevel::File, Some(4)),
            container(8, "unused", ScopeLevel::Folder, Some(1)),
            container(9, "components/common.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == [0],
        "the consumers' lowest common folder is their shared ownership boundary; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_keep_a_type_with_its_primary_same_file_consumer_at_default_affinity() {
    let mut subject = node(0, "request_options", 2, Polarity::Production);
    subject.kind = NodeKind::Type;
    subject.visibility = ScopeLevel::PackageGroup;
    let mut first_external = node(3, "first_passthrough", 3, Polarity::Production);
    first_external.kind = NodeKind::Type;
    let mut second_external = node(4, "second_passthrough", 3, Polarity::Production);
    second_external.kind = NodeKind::Type;
    let snapshot = snapshot(
        vec![
            subject,
            node(1, "execute_request", 2, Polarity::Production),
            node(2, "origin_resident", 2, Polarity::Production),
            first_external,
            second_external,
        ],
        vec![type_ref(1, 0), type_ref(0, 3), type_ref(0, 4)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "feature", ScopeLevel::Folder, Some(0)),
            container(2, "feature/execute.ts", ScopeLevel::File, Some(1)),
            container(3, "feature/types.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let (default_accepted, default_relocated) =
        relocates_first_symbol_with_affinities(&snapshot, 3.0, 0.0);
    let (unit_accepted, unit_relocated) =
        relocates_first_symbol_with_affinities(&snapshot, 1.0, 0.0);

    assert!(
        !default_accepted && default_relocated.is_empty(),
        "the default 3x same-file affinity must retain the type with its primary consumer; \
             relocations {default_relocated:?}"
    );
    assert!(
        unit_accepted && unit_relocated == [0],
        "unit affinity permits a dependency-only move whose raw gain exceeds the \
             configured penalty; relocations {unit_relocated:?}"
    );
}

#[test]
fn should_veto_relocating_an_executable_file_body_independently() {
    let mut file_body = node(0, "<module>", 3, Polarity::Production);
    file_body.kind = NodeKind::FileBody;
    let snapshot = snapshot(
        vec![
            file_body,
            node(1, "origin_resident", 3, Polarity::Production),
            node(2, "destination_resident", 4, Polarity::Production),
        ],
        vec![edge(2, 0), edge(2, 0)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "destination", ScopeLevel::Folder, Some(0)),
            container(3, "origin/entry.ts", ScopeLevel::File, Some(1)),
            container(4, "destination/consumer.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "an executable file body must remain attached to its source file; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_veto_moving_a_runtime_symbol_into_a_type_only_file() {
    let mut record_type = node(2, "record_type", 4, Polarity::Production);
    record_type.kind = NodeKind::Type;
    let snapshot = snapshot(
        vec![
            node(0, "build_record", 3, Polarity::Production),
            node(1, "origin_resident", 3, Polarity::Production),
            record_type,
        ],
        vec![type_ref(0, 2)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "runtime", ScopeLevel::Folder, Some(0)),
            container(2, "model", ScopeLevel::Folder, Some(0)),
            container(3, "runtime/build.ts", ScopeLevel::File, Some(1)),
            container(4, "model/types.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "a runtime symbol must not enter a pass-start type-only file; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_veto_giving_the_destination_folder_a_new_outbound_dependency() {
    let snapshot = snapshot(
        vec![
            node(0, "assemble_record", 4, Polarity::Production),
            node(1, "origin_resident", 4, Polarity::Production),
            node(2, "destination_resident", 5, Polarity::Production),
            node(3, "external_service", 6, Polarity::Production),
        ],
        vec![edge(2, 0), edge(2, 0), edge(0, 3)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "destination", ScopeLevel::Folder, Some(0)),
            container(3, "external", ScopeLevel::Folder, Some(0)),
            container(4, "origin/assemble.ts", ScopeLevel::File, Some(1)),
            container(5, "destination/records.ts", ScopeLevel::File, Some(2)),
            container(6, "external/service.ts", ScopeLevel::File, Some(3)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "a relocation must not give its destination folder a new outbound \
             dependency; relocations {relocated:?}"
    );
}

#[test]
fn should_allow_moving_a_runtime_symbol_into_a_mixed_file() {
    let mut record_type = node(3, "record_type", 4, Polarity::Production);
    record_type.kind = NodeKind::Type;
    let snapshot = snapshot(
        vec![
            node(0, "build_record", 3, Polarity::Production),
            node(1, "origin_resident", 3, Polarity::Production),
            node(2, "runtime_resident", 4, Polarity::Production),
            record_type,
        ],
        vec![type_ref(0, 3)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "records", ScopeLevel::Folder, Some(0)),
            container(3, "origin/build.ts", ScopeLevel::File, Some(1)),
            container(4, "records/model.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == [0],
        "a runtime symbol may enter a destination that already contains \
             runtime code; relocations {relocated:?}"
    );
}

#[test]
fn should_treat_a_file_body_as_runtime_when_classifying_a_destination() {
    let mut file_body = node(2, "<module>", 4, Polarity::Production);
    file_body.kind = NodeKind::FileBody;
    let mut record_type = node(3, "record_type", 4, Polarity::Production);
    record_type.kind = NodeKind::Type;
    let snapshot = snapshot(
        vec![
            node(0, "build_record", 3, Polarity::Production),
            node(1, "origin_resident", 3, Polarity::Production),
            file_body,
            record_type,
        ],
        vec![type_ref(0, 3)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "records", ScopeLevel::Folder, Some(0)),
            container(3, "origin/build.ts", ScopeLevel::File, Some(1)),
            container(4, "records/model.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == [0],
        "a file body makes a typed destination mixed rather than type-only; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_allow_moving_a_type_into_a_type_only_file() {
    let mut subject_type = node(0, "input_type", 3, Polarity::Production);
    subject_type.kind = NodeKind::Type;
    let mut destination_type = node(2, "record_type", 4, Polarity::Production);
    destination_type.kind = NodeKind::Type;
    let snapshot = snapshot(
        vec![
            subject_type,
            node(1, "origin_resident", 3, Polarity::Production),
            destination_type,
        ],
        vec![type_ref(0, 2)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "model", ScopeLevel::Folder, Some(0)),
            container(3, "origin/input.ts", ScopeLevel::File, Some(1)),
            container(4, "model/types.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == [0],
        "a type may enter a pass-start type-only file; relocations {relocated:?}"
    );
}

#[test]
fn should_allow_a_move_when_the_destination_already_reaches_the_target_folder() {
    let snapshot = snapshot(
        vec![
            node(0, "assemble_record", 4, Polarity::Production),
            node(1, "origin_resident", 4, Polarity::Production),
            node(2, "destination_resident", 5, Polarity::Production),
            node(3, "external_service", 6, Polarity::Production),
        ],
        vec![edge(2, 0), edge(2, 0), edge(0, 3), edge(2, 3)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "destination", ScopeLevel::Folder, Some(0)),
            container(3, "external", ScopeLevel::Folder, Some(0)),
            container(4, "origin/assemble.ts", ScopeLevel::File, Some(1)),
            container(5, "destination/records.ts", ScopeLevel::File, Some(2)),
            container(6, "external/service.ts", ScopeLevel::File, Some(3)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == [0],
        "an existing destination-to-target folder dependency keeps the \
             relocation eligible; relocations {relocated:?}"
    );
}

#[test]
fn should_ignore_a_self_recursive_edge_when_checking_outbound_dependencies() {
    let snapshot = snapshot(
        vec![
            node(0, "assemble_record", 3, Polarity::Production),
            node(1, "origin_resident", 3, Polarity::Production),
            node(2, "destination_resident", 4, Polarity::Production),
        ],
        vec![type_ref(0, 2), edge(0, 0)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "origin", ScopeLevel::Folder, Some(0)),
            container(2, "destination", ScopeLevel::Folder, Some(0)),
            container(3, "origin/assemble.ts", ScopeLevel::File, Some(1)),
            container(4, "destination/records.ts", ScopeLevel::File, Some(2)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == [0],
        "a self-recursive edge must not invent an outbound folder \
             dependency; relocations {relocated:?}"
    );
}

/// The veto prices *shared* placement, never motion. One consumer means no
/// third party is handed a new neighbour, so co-location stays legal —
/// without this the rule would simply forbid symbols from moving.
#[test]
fn should_still_relocate_a_symbol_with_a_single_consumer() {
    let snapshot = snapshot(
        vec![
            node(0, "computer_use_config", 5, Polarity::Production),
            node(1, "co_resident", 5, Polarity::Production),
            node(2, "format_anthropic_tools", 6, Polarity::Production),
        ],
        vec![type_ref(2, 0)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "adapters", ScopeLevel::Domain, Some(0)),
            container(2, "types", ScopeLevel::Folder, Some(1)),
            container(3, "anthropic", ScopeLevel::Folder, Some(1)),
            container(4, "unused", ScopeLevel::Folder, Some(1)),
            container(5, "types/tools.ts", ScopeLevel::File, Some(2)),
            container(6, "anthropic/tools.ts", ScopeLevel::File, Some(3)),
        ],
    );
    let (accepted, _) = relocates_first_symbol(&snapshot);

    assert!(
        accepted,
        "a sole consumer's home is unimpeachable; the veto must not \
             forbid plain co-location"
    );
}

/// A declaration shared across sibling consumer branches cannot be moved
/// into either occupied branch, even when that move is also a hoist.
#[test]
fn should_veto_a_hoist_into_one_of_multiple_consumer_branches() {
    let snapshot = snapshot(
        vec![
            node(0, "schema_analyst_config", 6, Polarity::Production),
            node(1, "co_resident", 6, Polarity::Production),
            node(2, "shared_schema_helper", 5, Polarity::Production),
            node(3, "outside_consumer", 7, Polarity::Production),
        ],
        vec![type_ref(2, 0), type_ref(3, 0)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "generators", ScopeLevel::Domain, Some(0)),
            container(2, "gen", ScopeLevel::Folder, Some(1)),
            container(3, "gen/analysts", ScopeLevel::Folder, Some(1)),
            container(4, "nav", ScopeLevel::Folder, Some(1)),
            container(5, "gen/schemas.ts", ScopeLevel::File, Some(2)),
            container(6, "gen/analysts/schemas.ts", ScopeLevel::File, Some(3)),
            container(7, "nav/use.ts", ScopeLevel::File, Some(4)),
        ],
    );
    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "a hoist must not bury shared ownership in one occupied consumer branch; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_allow_a_move_toward_an_actual_consumer() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_step", 2, Polarity::Production),
            node(1, "origin_resident", 2, Polarity::Production),
            node(2, "destination_consumer", 3, Polarity::Production),
        ],
        vec![edge(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "area/shared.ts", ScopeLevel::File, Some(1)),
            container(3, "area/consumer.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(accepted && relocated == [0]);
}

#[test]
fn should_price_a_weak_dependency_only_move_by_profile() {
    let snapshot = dependency_only_snapshot();

    let (disabled, disabled_relocations) =
        relocates_first_symbol_with_dependency_only(&snapshot, 0.0);
    let (default, default_relocations) =
        relocates_first_symbol_with_dependency_only(&snapshot, 0.05);

    assert!(
        disabled && disabled_relocations == [0],
        "zero pricing must preserve a dependency-driven relocation; \
             relocations {disabled_relocations:?}"
    );
    assert!(
        !default && default_relocations.is_empty(),
        "the default charge must outweigh this fixture's sub-0.05 raw gain; \
             relocations {default_relocations:?}"
    );
}

#[test]
fn should_move_a_companion_type_to_its_immutable_owner_file() {
    let snapshot = companion_owner_snapshot(false);

    let (accepted, relocated) = relocates_first_symbol_with_companion_separation(&snapshot, 0.05);

    assert!(
        accepted && relocated == [0],
        "co-locating the companion type removes exactly one directional separation; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_not_reward_moving_an_owner_toward_its_companion() {
    let snapshot = companion_owner_snapshot(true);

    let (accepted, relocated) = relocates_first_symbol_with_companion_separation(&snapshot, 0.05);

    assert!(
        !accepted && relocated.is_empty(),
        "owner motion cannot satisfy directional companion affinity; relocations {relocated:?}"
    );
}

#[test]
fn should_apply_companion_separation_to_only_the_changed_profile() {
    let snapshot = companion_owner_snapshot(false);
    let mut anchored = ProfileConfig::default();
    anchored.objective.imbalance = 0.0;
    anchored.objective.naming = 0.0;
    anchored.objective.path = 0.0;
    anchored.objective.anchor = 0.0;
    anchored.objective.capacity = 0.0;
    anchored.objective.dependency_only = 0.0;
    anchored.objective.companion_separation = 0.0;
    let mut greenfield = anchored.clone();
    greenfield.objective.companion_separation = 0.05;

    let (anchored_accepted, anchored_relocations) =
        relocates_first_symbol_with_profile(&snapshot, &anchored);
    let (greenfield_accepted, greenfield_relocations) =
        relocates_first_symbol_with_profile(&snapshot, &greenfield);

    assert!(!anchored_accepted && anchored_relocations.is_empty());
    assert!(greenfield_accepted && greenfield_relocations == [0]);
}

#[test]
fn should_count_duplicate_companion_affinity_exactly_once() {
    let snapshot = companion_owner_snapshot_with_affinity_count(false, 2);
    let placement = |id| {
        snapshot
            .ir()
            .nodes
            .iter()
            .find(|node| node.id.0 == id)
            .map(|node| node.container)
    };
    let pass_start_files = snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.container, node.container))
        .collect();

    assert_eq!(
        companion_separations(&snapshot, &placement, &pass_start_files),
        1,
        "one separated companion contributes one fixed charge despite duplicate input"
    );
}

#[test]
fn should_nominate_duplicate_companion_affinity_exactly_once() {
    let snapshot = companion_owner_snapshot_with_affinity_count(false, 2);

    assert_eq!(
        companion_nomination_count(&snapshot, 0, 2),
        1,
        "one companion-owner relation nominates its owner file exactly once"
    );
}

#[test]
fn should_not_cross_a_transparent_namespace_for_companion_affinity() {
    let snapshot = transparent_namespace_companion_snapshot();

    let (accepted, relocated) = relocates_first_symbol_with_companion_separation(&snapshot, 0.05);

    assert!(
        !accepted && relocated.is_empty(),
        "affinity cannot bypass the pass-start namespace guard; relocations {relocated:?}"
    );
}

#[test]
fn should_apply_dependency_only_pricing_to_only_the_changed_profile() {
    let snapshot = dependency_only_snapshot();
    let mut config = AnalyzeConfig::default();
    config.profiles.anchored.objective.imbalance = 0.0;
    config.profiles.anchored.objective.naming = 0.0;
    config.profiles.anchored.objective.path = 0.0;
    config.profiles.anchored.objective.anchor = 0.0;
    config.profiles.anchored.objective.capacity = 0.0;
    config.profiles.anchored.objective.dependency_only = 0.0;
    config.profiles.greenfield.objective.imbalance = 0.0;
    config.profiles.greenfield.objective.naming = 0.0;
    config.profiles.greenfield.objective.path = 0.0;
    config.profiles.greenfield.objective.anchor = 0.0;
    config.profiles.greenfield.objective.capacity = 0.0;
    config.profiles.greenfield.objective.dependency_only = 0.05;

    let (anchored, anchored_relocations) =
        relocates_first_symbol_with_profile(&snapshot, &config.profiles.anchored);
    let (greenfield, greenfield_relocations) =
        relocates_first_symbol_with_profile(&snapshot, &config.profiles.greenfield);

    assert!(anchored && anchored_relocations == [0]);
    assert!(!greenfield && greenfield_relocations.is_empty());
}

#[test]
fn should_include_dependency_only_pricing_in_the_relocation_delta() {
    let snapshot = dependency_only_snapshot();
    let without_charge = first_symbol_delta_with_dependency_only(&snapshot, 0.0);
    let with_charge = first_symbol_delta_with_dependency_only(&snapshot, 0.001);
    assert!(
        without_charge.is_some(),
        "the uncharged dependency-only move must be eligible"
    );
    assert!(
        with_charge.is_some(),
        "a small dependency-only charge must keep the move eligible"
    );
    let (Some(without_charge), Some(with_charge)) = (without_charge, with_charge) else {
        return;
    };

    assert!(
        (without_charge - with_charge - 0.001).abs() < 1.0e-12,
        "the narrated improvement must include the fixed charge exactly once"
    );
}

#[test]
fn should_count_every_structural_dependency_without_charging_a_whole_file_move() {
    for kind in [
        EdgeKind::ValueImport,
        EdgeKind::Inheritance,
        EdgeKind::Call,
        EdgeKind::TypeReference,
        EdgeKind::ReExport,
    ] {
        let snapshot = snapshot(
            vec![
                node(0, "derived_value", 2, Polarity::Production),
                node(1, "source_value", 3, Polarity::Production),
            ],
            vec![
                Edge {
                    source: NodeId(0),
                    target: NodeId(1),
                    kind,
                    hardness: Hardness::Hard,
                    confidence: 1.0,
                },
                Edge {
                    source: NodeId(0),
                    target: NodeId(0),
                    kind,
                    hardness: Hardness::Hard,
                    confidence: 1.0,
                },
            ],
            vec![
                container(0, "workspace", ScopeLevel::PackageGroup, None),
                container(1, "area", ScopeLevel::Folder, Some(0)),
                container(2, "area/derived.ts", ScopeLevel::File, Some(1)),
                container(3, "area/source.ts", ScopeLevel::File, Some(1)),
            ],
        );
        let placement = |id| {
            Some(if id == 0 {
                ContainerId(20)
            } else {
                ContainerId(21)
            })
        };

        let relocated = BTreeMap::from([
            (ContainerId(20), ContainerId(3)),
            (ContainerId(21), ContainerId(3)),
        ]);
        assert_eq!(
            dependency_only_relocations(&snapshot, &placement, &relocated),
            1,
            "{kind:?} is structural and nominates the dependency's pass-start file"
        );

        let whole_file = BTreeMap::from([
            (ContainerId(20), ContainerId(2)),
            (ContainerId(21), ContainerId(3)),
        ]);
        assert_eq!(
            dependency_only_relocations(&snapshot, &placement, &whole_file),
            0,
            "a fresh candidate id mapped to the original file is not a declaration move"
        );
    }
}

#[test]
fn should_not_charge_a_move_into_its_sole_consumers_file() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_step", 2, Polarity::Production),
            node(1, "origin_resident", 2, Polarity::Production),
            node(2, "destination_consumer", 3, Polarity::Production),
        ],
        vec![edge(2, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "area/shared.ts", ScopeLevel::File, Some(1)),
            container(3, "area/consumer.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol_with_dependency_only(&snapshot, 0.05);

    assert!(
        accepted && relocated == [0],
        "a declaration's sole consumer owns a valid destination under default pricing; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_veto_distinct_declarations_colliding_in_one_file() {
    let snapshot = snapshot(
        vec![
            node(0, "duplicate_name", 2, Polarity::Production),
            node(1, "origin_resident", 2, Polarity::Production),
            node(2, "duplicate_name", 3, Polarity::Production),
            node(3, "destination_consumer", 3, Polarity::Production),
        ],
        vec![edge(3, 0)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "area/origin.ts", ScopeLevel::File, Some(1)),
            container(3, "area/destination.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "distinct declarations with one name cannot share a destination file; \
             relocations {relocated:?}"
    );
}

#[test]
fn should_veto_a_later_declaration_after_an_earlier_arrival_claims_its_name()
-> Result<(), &'static str> {
    let snapshot = snapshot(
        vec![
            node(0, "shared_name", 2, Polarity::Production),
            node(1, "first_resident", 2, Polarity::Production),
            node(2, "shared_name", 3, Polarity::Production),
            node(3, "second_resident", 3, Polarity::Production),
            node(4, "destination_consumer", 4, Polarity::Production),
        ],
        vec![edge(4, 0), edge(4, 2)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "area/first.ts", ScopeLevel::File, Some(1)),
            container(3, "area/second.ts", ScopeLevel::File, Some(1)),
            container(4, "area/destination.ts", ScopeLevel::File, Some(1)),
        ],
    );
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config.profiles.greenfield,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let first_subject = ir
        .nodes
        .first()
        .ok_or("the first declaration witness must exist")?;
    let second_subject = ir
        .nodes
        .get(2)
        .ok_or("the later declaration witness must exist")?;
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new(pass_inputs(&snapshot, &solver, &assembled));
    pass.best = f64::INFINITY;
    let first_moved = pass.try_relocate(first_subject);
    pass.best = f64::INFINITY;
    let second_moved = pass.try_relocate(second_subject);
    let relocated: Vec<_> = pass.relocations.iter().map(|entry| entry.node).collect();

    assert!(
        first_moved,
        "the first declaration must establish the witness"
    );
    assert!(
        !second_moved && relocated == [0],
        "an earlier arrival owns the destination name for the rest of the pass; \
             relocations {relocated:?}"
    );
    Ok(())
}

fn symbol_depth_snapshot(kind: NodeKind, origin: &str, destination: &str) -> Snapshot {
    let mut subject = node(0, "subject", 3, Polarity::Production);
    subject.kind = kind;
    let mut destination_type = node(3, "destination_type", 4, Polarity::Production);
    destination_type.kind = NodeKind::Type;
    snapshot(
        vec![
            subject,
            node(1, "origin_resident", 3, Polarity::Production),
            node(2, "destination_runtime", 4, Polarity::Production),
            destination_type,
        ],
        vec![type_ref(0, 3)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, origin, ScopeLevel::Folder, Some(0)),
            container(2, destination, ScopeLevel::Folder, Some(0)),
            container(3, &format!("{origin}/origin.ts"), ScopeLevel::File, Some(1)),
            container(
                4,
                &format!("{destination}/destination.ts"),
                ScopeLevel::File,
                Some(2),
            ),
        ],
    )
}

#[test]
fn should_veto_moving_a_runtime_symbol_into_a_deeper_directory() {
    let snapshot = symbol_depth_snapshot(NodeKind::Symbol, "shared", "branch/deep");

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "runtime declarations cannot move deeper; relocations {relocated:?}"
    );
}

#[test]
fn should_veto_moving_a_type_into_a_deeper_directory() {
    let snapshot = symbol_depth_snapshot(NodeKind::Type, "shared", "branch/deep");

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "type declarations cannot move deeper; relocations {relocated:?}"
    );
}

#[test]
fn should_allow_an_equal_depth_symbol_move() {
    let snapshot = symbol_depth_snapshot(NodeKind::Symbol, "left", "right");

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == vec![0],
        "equal-depth moves stay eligible; relocations {relocated:?}"
    );
}

#[test]
fn should_allow_an_upward_type_move() {
    let snapshot = symbol_depth_snapshot(NodeKind::Type, "branch/deep", "shared");

    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        accepted && relocated == vec![0],
        "upward moves stay eligible; relocations {relocated:?}"
    );
}

/// The `GOOGLE_DEFAULT_CONTENT_TYPE` shape: the symbol's own file still
/// uses it, and the destination already imports from that file. Carrying
/// the symbol across closes a circular import between the two files.
#[test]
fn should_veto_moving_a_symbol_its_own_file_still_uses() {
    let snapshot = snapshot(
        vec![
            node(0, "google_default_content_type", 2, Polarity::Production),
            node(1, "decode_google", 2, Polarity::Production),
            node(2, "run_batch", 3, Polarity::Production),
            node(3, "batch_resident", 3, Polarity::Production),
        ],
        // the origin's own use of the symbol, plus the destination's two
        // existing imports out of that same origin file.
        vec![type_ref(1, 0), type_ref(2, 0), type_ref(2, 1)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "lib", ScopeLevel::Folder, Some(0)),
            container(2, "lib/codec.ts", ScopeLevel::File, Some(1)),
            container(3, "lib/batch.ts", ScopeLevel::File, Some(1)),
        ],
    );
    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "a symbol its own file still uses must not cross into a file that \
             already imports from that file; relocations {relocated:?}"
    );
}

/// The pass-start claimant must survive sequential draining. Moving the
/// same-file user away first must not erase the dependency that bars the
/// referenced symbol from closing a destination-to-origin cycle.
#[test]
fn should_not_unlock_a_referenced_symbol_by_moving_its_claimant_first() {
    let snapshot = snapshot(
        vec![
            node(0, "claimant", 2, Polarity::Production),
            node(1, "shared_value", 2, Polarity::Production),
            node(2, "claimant_lure", 3, Polarity::Production),
            node(3, "destination_user", 4, Polarity::Production),
            node(4, "origin_resident", 2, Polarity::Production),
        ],
        vec![type_ref(0, 1), type_ref(0, 2), type_ref(3, 1)],
        sequential_claimant_containers(),
    );

    let (claimant_moved, subject_moved, relocated) = relocates_claimant_then_subject(&snapshot, 3);

    assert!(claimant_moved, "the witness must drain the claimant first");
    assert!(
        !subject_moved && relocated == [0],
        "pass-start evidence must still veto the referenced symbol; relocations {relocated:?}"
    );
}

/// Baseline reachability is transitive: a destination reaching the
/// claimant's original file through an intermediate file is equally able
/// to close a cycle after the symbol moves.
#[test]
fn should_veto_a_drained_claim_through_a_transitive_baseline_path() {
    let snapshot = snapshot(
        vec![
            node(0, "claimant", 2, Polarity::Production),
            node(1, "shared_value", 2, Polarity::Production),
            node(2, "claimant_lure", 3, Polarity::Production),
            node(3, "destination_resident", 4, Polarity::Production),
            node(4, "middle_resident", 5, Polarity::Production),
            node(5, "origin_resident", 2, Polarity::Production),
        ],
        vec![
            type_ref(0, 1),
            type_ref(0, 2),
            type_ref(3, 4),
            type_ref(4, 5),
        ],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "lib", ScopeLevel::Folder, Some(0)),
            container(2, "lib/origin.ts", ScopeLevel::File, Some(1)),
            container(3, "lib/claimant-home.ts", ScopeLevel::File, Some(1)),
            container(4, "lib/destination.ts", ScopeLevel::File, Some(1)),
            container(5, "lib/middle.ts", ScopeLevel::File, Some(1)),
        ],
    );

    let (claimant_moved, subject_moved, relocated) = relocates_claimant_then_subject(&snapshot, 3);

    assert!(claimant_moved, "the witness must drain the claimant first");
    assert!(
        !subject_moved && relocated == [0],
        "destination -> middle -> origin must preserve the pass-start veto; relocations {relocated:?}"
    );
}

/// A cycle closed between two files already sitting inside a larger cycle
/// leaves the cyclic *vertex* count untouched, so only a cyclic *edge*
/// measure can see it.
#[test]
fn should_veto_a_move_closing_a_cycle_among_already_cyclic_files() {
    let snapshot = snapshot(
        vec![
            node(0, "shared_helper", 2, Polarity::Production),
            node(1, "first_resident", 2, Polarity::Production),
            node(2, "second_resident", 2, Polarity::Production),
            node(3, "cycle_carrier", 2, Polarity::Production),
            node(4, "middle_resident", 3, Polarity::Production),
            node(5, "last_resident", 4, Polarity::Production),
        ],
        vec![
            // the subject's own file supplies it with two dependencies,
            // both of which would become crossing edges after the move.
            type_ref(0, 1),
            type_ref(0, 2),
            // the priced link that nominates the middle file.
            type_ref(0, 4),
            // first -> middle -> last -> first, so all three files already
            // share one strongly connected component.
            type_ref(3, 4),
            type_ref(4, 5),
            type_ref(5, 3),
        ],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "lib", ScopeLevel::Folder, Some(0)),
            container(2, "lib/first.ts", ScopeLevel::File, Some(1)),
            container(3, "lib/middle.ts", ScopeLevel::File, Some(1)),
            container(4, "lib/last.ts", ScopeLevel::File, Some(1)),
        ],
    );
    let (accepted, relocated) = relocates_first_symbol(&snapshot);

    assert!(
        !accepted && relocated.is_empty(),
        "a move closing a fresh cycle inside an existing component must be \
             refused even though the cyclic vertex count cannot see it; \
             relocations {relocated:?}"
    );
}

/// Runs a greenfield [`SymbolPass`] over `snapshot` and offers it the
/// first node, reporting whether the relocation was accepted and which
/// nodes moved.
///
/// Strict-J is held aside at infinity so that a J-cost rejection can never
/// masquerade as a veto: any destination surviving the gate chain is then
/// deterministically accepted, making a refusal attributable to the gates
/// alone.
fn relocates_first_symbol(snapshot: &Snapshot) -> (bool, Vec<u32>) {
    relocates_first_symbol_with_type_affinity(snapshot, 3.0)
}

fn relocates_first_symbol_with_type_affinity(
    snapshot: &Snapshot,
    same_file_type: f64,
) -> (bool, Vec<u32>) {
    relocates_first_symbol_with_affinities(snapshot, same_file_type, 0.05)
}

fn relocates_first_symbol_with_affinities(
    snapshot: &Snapshot,
    same_file_type: f64,
    dependency_only: f64,
) -> (bool, Vec<u32>) {
    let tests = TestPolicy::defaults();
    let mut config = AnalyzeConfig::default();
    config.profiles.greenfield.weights.same_file_type = same_file_type;
    config.profiles.greenfield.objective.dependency_only = dependency_only;
    let solver = PipelineSolver::new(
        snapshot,
        &config.profiles.greenfield,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new(pass_inputs(snapshot, &solver, &assembled));
    pass.best = f64::INFINITY;
    let accepted = ir
        .nodes
        .first()
        .is_some_and(|subject| pass.try_relocate(subject));
    (
        accepted,
        pass.relocations.iter().map(|entry| entry.node).collect(),
    )
}

fn relocates_first_symbol_with_dependency_only(
    snapshot: &Snapshot,
    dependency_only: f64,
) -> (bool, Vec<u32>) {
    let mut config = AnalyzeConfig::default();
    config.profiles.greenfield.objective.imbalance = 0.0;
    config.profiles.greenfield.objective.naming = 0.0;
    config.profiles.greenfield.objective.path = 0.0;
    config.profiles.greenfield.objective.anchor = 0.0;
    config.profiles.greenfield.objective.capacity = 0.0;
    config.profiles.greenfield.objective.dependency_only = dependency_only;
    relocates_first_symbol_with_profile(snapshot, &config.profiles.greenfield)
}

fn relocates_first_symbol_with_companion_separation(
    snapshot: &Snapshot,
    companion_separation: f64,
) -> (bool, Vec<u32>) {
    let mut profile = ProfileConfig::default();
    profile.objective.imbalance = 0.0;
    profile.objective.naming = 0.0;
    profile.objective.path = 0.0;
    profile.objective.anchor = 0.0;
    profile.objective.capacity = 0.0;
    profile.objective.dependency_only = 0.0;
    profile.objective.companion_separation = companion_separation;
    relocates_first_symbol_with_profile(snapshot, &profile)
}

fn companion_nomination_count(snapshot: &Snapshot, companion: u32, owner: u32) -> usize {
    let tests = TestPolicy::defaults();
    let mut profile = ProfileConfig::default();
    profile.objective.companion_separation = 0.05;
    let solver = PipelineSolver::new(
        snapshot,
        &profile,
        profile.objective.coefficients(),
        false,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    let pass = SymbolPass::new(pass_inputs(snapshot, &solver, &assembled));

    pass.incident
        .get(&companion)
        .into_iter()
        .flatten()
        .filter(|(target, _)| *target == owner)
        .count()
}

fn first_symbol_delta_with_dependency_only(
    snapshot: &Snapshot,
    dependency_only: f64,
) -> Option<f64> {
    let mut profile = ProfileConfig::default();
    profile.objective.imbalance = 0.0;
    profile.objective.naming = 0.0;
    profile.objective.path = 0.0;
    profile.objective.anchor = 0.0;
    profile.objective.capacity = 0.0;
    profile.objective.dependency_only = dependency_only;
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(
        snapshot,
        &profile,
        profile.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new(pass_inputs(snapshot, &solver, &assembled));
    ir.nodes
        .first()
        .and_then(|subject| pass.try_relocate(subject).then_some(()))?;
    pass.relocations.first().map(|relocation| relocation.delta)
}

fn relocates_first_symbol_with_profile(
    snapshot: &Snapshot,
    profile: &ProfileConfig,
) -> (bool, Vec<u32>) {
    let tests = TestPolicy::defaults();
    let solver = PipelineSolver::new(
        snapshot,
        profile,
        profile.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new(pass_inputs(snapshot, &solver, &assembled));
    let accepted = ir
        .nodes
        .first()
        .is_some_and(|subject| pass.try_relocate(subject));
    (
        accepted,
        pass.relocations.iter().map(|entry| entry.node).collect(),
    )
}

fn relocates_claimant_then_subject(
    snapshot: &Snapshot,
    destination_lure: u32,
) -> (bool, bool, Vec<u32>) {
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new(pass_inputs(snapshot, &solver, &assembled));
    pass.incident
        .entry(1)
        .or_default()
        .push((destination_lure, 5.0));
    pass.best = f64::INFINITY;
    let claimant_moved = ir
        .nodes
        .iter()
        .find(|node| node.id == NodeId(0))
        .is_some_and(|claimant| pass.try_relocate(claimant));
    pass.best = f64::INFINITY;
    // Hold visibility aside so the witness isolates cycle admission. The
    // production visibility contract has its own focused relocation cases.
    pass.vis_base = usize::MAX;
    let subject_moved = ir
        .nodes
        .iter()
        .find(|node| node.id == NodeId(1))
        .is_some_and(|subject| pass.try_relocate(subject));
    let relocated = pass.relocations.iter().map(|entry| entry.node).collect();
    (claimant_moved, subject_moved, relocated)
}

fn sequential_claimant_containers() -> Vec<Container> {
    vec![
        container(0, "src", ScopeLevel::PackageGroup, None),
        container(1, "lib", ScopeLevel::Folder, Some(0)),
        container(2, "lib/origin.ts", ScopeLevel::File, Some(1)),
        container(3, "lib/claimant-home.ts", ScopeLevel::File, Some(1)),
        container(4, "lib/destination.ts", ScopeLevel::File, Some(1)),
    ]
}

/// The real `ComputerUseConfig` shape: a shared type in a neutral folder,
/// one consumer in each of two sibling adapter folders, plus a co-resident
/// so the origin never empties.
fn shared_adapter_type_snapshot() -> Snapshot {
    snapshot(
        vec![
            node(0, "computer_use_config", 5, Polarity::Production),
            node(1, "co_resident", 5, Polarity::Production),
            node(2, "format_anthropic_tools", 6, Polarity::Production),
            node(3, "format_openai_tools", 7, Polarity::Production),
        ],
        vec![type_ref(2, 0), type_ref(3, 0)],
        vec![
            container(0, "src", ScopeLevel::PackageGroup, None),
            container(1, "adapters", ScopeLevel::Domain, Some(0)),
            container(2, "types", ScopeLevel::Folder, Some(1)),
            container(3, "anthropic", ScopeLevel::Folder, Some(1)),
            container(4, "openai", ScopeLevel::Folder, Some(1)),
            container(5, "types/tools.ts", ScopeLevel::File, Some(2)),
            container(6, "anthropic/tools.ts", ScopeLevel::File, Some(3)),
            container(7, "openai/tools.ts", ScopeLevel::File, Some(4)),
        ],
    )
}

fn dependency_only_snapshot() -> Snapshot {
    snapshot(
        vec![
            node(0, "derived_value", 2, Polarity::Production),
            node(1, "origin_resident", 2, Polarity::Production),
            node(2, "source_value", 3, Polarity::Production),
            node(3, "stable_reader", 4, Polarity::Production),
            node(4, "stable_source", 5, Polarity::Production),
        ],
        vec![edge(0, 2), edge(3, 4)],
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "area/derived.ts", ScopeLevel::File, Some(1)),
            container(3, "area/source.ts", ScopeLevel::File, Some(1)),
            container(4, "area/reader.ts", ScopeLevel::File, Some(1)),
            container(5, "area/stable.ts", ScopeLevel::File, Some(1)),
        ],
    )
}

fn companion_owner_snapshot(owner_first: bool) -> Snapshot {
    companion_owner_snapshot_with_affinity_count(owner_first, 1)
}

fn companion_owner_snapshot_with_affinity_count(
    owner_first: bool,
    affinity_count: usize,
) -> Snapshot {
    let (companion_id, owner_id) = if owner_first { (2, 0) } else { (0, 2) };
    let mut companion = node(
        companion_id,
        "AssembleArtifactParams",
        if owner_first { 3 } else { 2 },
        Polarity::Production,
    );
    companion.kind = NodeKind::Type;
    let owner = node(
        owner_id,
        "assembleArtifact",
        if owner_first { 2 } else { 3 },
        Polarity::Production,
    );
    let resident = node(1, "origin_resident", 2, Polarity::Production);
    let mut nodes = vec![companion, resident, owner];
    nodes.sort_by_key(|node| node.id.0);

    snapshot_with_affinities(
        nodes,
        Vec::new(),
        (0..affinity_count)
            .map(|_| Affinity {
                owner: NodeId(owner_id),
                companion: NodeId(companion_id),
                kind: AffinityKind::CompanionOwner,
            })
            .collect(),
        vec![
            container(0, "workspace", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "area/origin.ts", ScopeLevel::File, Some(1)),
            container(3, "area/owner.ts", ScopeLevel::File, Some(1)),
        ],
    )
}

fn transparent_namespace_companion_snapshot() -> Snapshot {
    let origin_path = SmolStr::new("left/area/keep/companion.ts");
    let owner_path = SmolStr::new("right/area/sink/owner.ts");
    let paths = vec![origin_path.clone(), owner_path.clone()];
    let layout = Layout {
        package_roots: Vec::new(),
        source_roots: vec![SmolStr::new("left"), SmolStr::new("right")],
    };
    let built = build_laminar_tree(&paths, "workspace", &layout);
    let companion_file = built
        .files
        .get(&origin_path)
        .map_or(u32::MAX, |container| container.0);
    let owner_file = built
        .files
        .get(&owner_path)
        .map_or(u32::MAX, |container| container.0);
    let mut companion = node(
        0,
        "AssembleArtifactParams",
        companion_file,
        Polarity::Production,
    );
    companion.kind = NodeKind::Type;

    snapshot_with_affinities(
        vec![
            companion,
            node(1, "origin_resident", companion_file, Polarity::Production),
            node(2, "assembleArtifact", owner_file, Polarity::Production),
        ],
        Vec::new(),
        vec![Affinity {
            owner: NodeId(2),
            companion: NodeId(0),
            kind: AffinityKind::CompanionOwner,
        }],
        built.tree.containers().to_vec(),
    )
}

/// A production symbol misfiled INSIDE the test zone stays there: the
/// outward crossing is barred the same as the inward one.
#[test]
fn should_keep_a_test_zone_resident_inside_the_test_zone() {
    let snapshot = snapshot(
        vec![
            node(0, "stray_helper", 4, Polarity::Production),
            node(1, "fellow_stray", 4, Polarity::Production),
            node(2, "greeter", 3, Polarity::Production),
        ],
        vec![],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "spec", ScopeLevel::Folder, Some(0)),
            container(3, "src/home.ts", ScopeLevel::File, Some(1)),
            container(4, "spec/legacy.spec.ts", ScopeLevel::File, Some(2)),
        ],
    );
    let tests = policy(&TestsConfig {
        patterns: vec![String::from("*.spec.ts")],
        ..TestsConfig::default()
    });
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new(pass_inputs(&snapshot, &solver, &assembled));
    // a priced-looking lure out toward the production home file.
    pass.incident.entry(0).or_default().push((2, 3.0));
    // Float surgery: hold strict-J aside so a J-cost rejection cannot
    // masquerade as the veto — with best at infinity, any destination
    // that survives the gate chain is deterministically accepted, so
    // `!accepted` proves THIS veto fired.
    pass.best = f64::INFINITY;

    let accepted = ir
        .nodes
        .first()
        .is_some_and(|stray| pass.try_relocate(stray));
    assert!(
        !accepted && pass.relocations.is_empty(),
        "a test-zone resident must never relocate out of the zone; \
             relocations {:?}",
        pass.relocations.iter().map(|r| r.node).collect::<Vec<_>>()
    );
}

/// The boundary vetoes crossings, not motion: with the objective held
/// aside, a misfiled symbol pulled toward callers inside the SAME zone
/// clears the whole veto family — the veto never fires on intra-zone
/// pairs.
#[test]
fn should_still_allow_moves_inside_one_zone() {
    let snapshot = snapshot(
        vec![
            node(0, "misfiled_formatter", 2, Polarity::Production),
            node(1, "zone_bystander", 2, Polarity::Production),
            node(2, "format_caller", 3, Polarity::Production),
            node(3, "second_caller", 3, Polarity::Production),
        ],
        vec![edge(2, 0), edge(3, 0)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "spec", ScopeLevel::Folder, Some(0)),
            container(2, "spec/generated.spec.ts", ScopeLevel::File, Some(1)),
            container(3, "spec/callers.spec.ts", ScopeLevel::File, Some(1)),
        ],
    );
    let tests = policy(&TestsConfig {
        patterns: vec![String::from("*.spec.ts")],
        ..TestsConfig::default()
    });
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let ir = snapshot.ir();
    let assembled = solver.assemble(&solver.real_partition);
    let mut pass = SymbolPass::new(pass_inputs(&snapshot, &solver, &assembled));
    // the tie-cut zeroed the zone edges, so nomination needs the simulated
    // priced pull; the destination sits inside the same zone.
    pass.incident.entry(0).or_default().push((2, 5.0));
    // Float surgery: hold strict-J aside so THIS test exercises the veto
    // family alone — with zone edges priced zero everywhere, an intra-zone
    // move can never pay its own displacement under J, and that pricing
    // doctrine is covered by the zero-price tests, not here.
    pass.best = f64::INFINITY;

    let accepted = ir
        .nodes
        .first()
        .is_some_and(|misfiled| pass.try_relocate(misfiled));
    assert!(
        accepted,
        "an intra-zone relocation must clear the veto family; relocations \
             {:?}",
        pass.relocations.iter().map(|r| r.node).collect::<Vec<_>>()
    );
    assert_eq!(pass.relocations.len(), 1);
    assert_eq!(pass.relocations.first().map(|r| r.node), Some(0));
}

#[test]
fn should_reject_a_pinned_symbol_before_it_can_change_a_later_admission() {
    let snapshot = snapshot(
        vec![
            node(0, "blocked", 2, Polarity::Production),
            node(1, "blocked_resident", 2, Polarity::Production),
            node(2, "eligible", 3, Polarity::Production),
            node(3, "eligible_resident", 3, Polarity::Production),
            node(4, "owner", 4, Polarity::Production),
            node(5, "owner_resident", 4, Polarity::Production),
        ],
        vec![],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "src/blocked.ts", ScopeLevel::File, Some(1)),
            container(3, "src/eligible.ts", ScopeLevel::File, Some(1)),
            container(4, "src/owner.ts", ScopeLevel::File, Some(1)),
        ],
    );
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    let ir = snapshot.ir();
    let blocked_file = assembled.placement.get(&0).copied();
    assert!(
        blocked_file.is_some(),
        "blocked symbol must have a file placement"
    );
    let Some(blocked_file) = blocked_file else {
        return;
    };
    let mut pass = SymbolPass::new_with_policy(
        pass_inputs(&snapshot, &solver, &assembled),
        RelocationPolicy {
            forbidden_sources: BTreeSet::from([blocked_file]),
            ..RelocationPolicy::default()
        },
    );
    pass.incident.entry(0).or_default().push((4, 5.0));
    pass.incident.entry(2).or_default().push((4, 5.0));
    pass.best = f64::INFINITY;

    let blocked_node = ir.nodes.first();
    assert!(blocked_node.is_some(), "blocked symbol must exist");
    let Some(blocked_node) = blocked_node else {
        return;
    };
    assert!(!pass.try_relocate(blocked_node));
    assert!(
        pass.overlay.is_empty(),
        "a pinned trial cannot mutate pass state"
    );
    let eligible_node = ir.nodes.get(2);
    assert!(eligible_node.is_some(), "eligible symbol must exist");
    let Some(eligible_node) = eligible_node else {
        return;
    };
    assert!(pass.try_relocate(eligible_node));
    assert_eq!(
        pass.relocations.iter().map(|r| r.node).collect::<Vec<_>>(),
        vec![2]
    );
}

/// Zoning by `[tests]` patterns alone (no polarity hint): a file matching
/// only the configured pattern is equally out of bounds as a destination.
#[test]
fn should_veto_entry_into_a_pattern_marked_test_zone() {
    let snapshot = snapshot(
        vec![
            node(0, "hero_widget", 2, Polarity::Production),
            node(1, "rival_widget", 3, Polarity::Production),
            node(2, "probe_case", 4, Polarity::TestCase),
        ],
        vec![edge(1, 0), type_ref(2, 0)],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "src", ScopeLevel::Folder, Some(0)),
            container(2, "src/hero.ts", ScopeLevel::File, Some(1)),
            container(3, "src/rival.ts", ScopeLevel::File, Some(1)),
            container(4, "src/probe.custom-test.ts", ScopeLevel::File, Some(1)),
        ],
    );
    let tests = policy(&TestsConfig {
        patterns: vec![String::from("*.custom-test.ts")],
        ..TestsConfig::default()
    });
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    // Fresh candidate ids are arena-issued, so locate the zone file by its
    // snapshot name rather than assuming a literal id.
    let assembled = solver.assemble(&solver.real_partition);
    let probe_file = assembled
        .tree
        .containers()
        .iter()
        .find(|file| {
            file.level == ScopeLevel::File && file.name.as_str() == "src/probe.custom-test.ts"
        })
        .map(|file| file.id);
    // the pattern itself must be what marked the file: pin the zone map
    // before asserting anything about relocations.
    assert_eq!(
        probe_file.and_then(|id| assembled.zone_by_file.get(&id)),
        Some(&true),
        "the [tests] pattern must mark probe.custom-test.ts into the zone"
    );
    let outcome = solver.symbol_polish(&solver.real_partition);
    let crossings: Vec<String> = outcome
        .relocations
        .iter()
        .filter(|relocation| {
            Some(relocation.to_file) == probe_file || Some(relocation.from_file) == probe_file
        })
        .map(|relocation| {
            format!(
                "node {} across file {}",
                relocation.node, relocation.to_file.0
            )
        })
        .collect();
    assert!(
        crossings.is_empty(),
        "a pattern-marked test file is the same boundary; crossed \
             {crossings:?}"
    );
}

/// The R6 companion guard at roof grain: two all-test-zone SCCs sharing a
/// basename token are exactly who the stranger sweep would group — and the
/// rebuild must never sweep spec twins into an invented production place.
#[test]
fn should_not_sweep_a_test_zone_stranger_into_a_token_group() {
    // no priced edges anywhere: both zone files look unanchored, and their
    // shared `case` token would form a group of two without the guard.
    let graph = Csr::from_weighted_edges(2, &[]);
    let condensation = singleton_condensation(2);
    let base = Partition::from_assignment(vec![ClusterId(0), ClusterId(0)], 1);
    let files = vec![
        file_info(0, "refund_case.py", 1),
        file_info(0, "date_case.py", 1),
    ];
    let mut names = vec![SmolStr::new("helpers")];
    let mut synthetic = vec![false];

    let rebuilt = synthesize_roof_rebuild(
        &files,
        &condensation,
        &graph,
        &[true, true],
        &[false, false],
        &base,
        &mut names,
        &mut synthetic,
    );

    assert!(
        rebuilt.is_none(),
        "all-test-zone SCCs must never enter the stranger population, no \
             matter what token they share"
    );
}

#[test]
fn should_rebuild_unrelated_production_groups_while_a_test_scc_is_pinned() {
    let graph = Csr::from_weighted_edges(4, &[(2_u32, 3_u32, 1.0_f32)]);
    let condensation = singleton_condensation(4);
    let base = Partition::from_assignment(vec![ClusterId(0); 4], 1);
    let files = vec![
        file_info(0, "record-read.ts", 1),
        file_info(1, "record-write.ts", 1),
        file_info(2, "anchor.ts", 1),
        file_info(3, "case.spec.ts", 1),
    ];
    let mut names = vec![SmolStr::new("misc")];
    let mut synthetic = vec![false];

    let rebuilt = synthesize_roof_rebuild(
        &files,
        &condensation,
        &graph,
        &[false, false, false, true],
        &[false, false, false, true],
        &base,
        &mut names,
        &mut synthetic,
    );
    assert!(
        rebuilt.is_some(),
        "the unrelated production token group remains eligible"
    );
    let Some(rebuilt) = rebuilt else {
        return;
    };

    assert_ne!(rebuilt.cluster_of(0), Some(ClusterId(0)));
    assert_eq!(rebuilt.cluster_of(3), Some(ClusterId(0)));
}

/// The spec lives in its own real directory, so the real-dir partition
/// starts it apart from its twin; the tie-cut prices their bond to zero
/// so polish never pulls them together. The shadow pass is what joins
/// them.
#[test]
fn should_pin_a_spec_when_its_subject_did_not_move() {
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
    let unit = scc_of(4); // the spec
    let subject = scc_of(3); // the twin
    assert_ne!(
        parts.cluster_of(unit),
        parts.cluster_of(subject),
        "setup: real dirs must start the pair apart"
    );

    let before = parts.cluster_of(unit);
    solver.shadow_tests(&mut parts);

    assert_eq!(parts.cluster_of(unit), before);
}

#[allow(clippy::too_many_lines)]
/// Builds the sequential-relocation churn fixture.
fn churn_snapshot() -> Snapshot {
    snapshot(
        vec![
            node(0, "analyst_config", 6, Polarity::Production),
            node(1, "analyst_mate", 6, Polarity::Production),
            node(2, "generator_config", 5, Polarity::Production),
            node(3, "embedder_config", 8, Polarity::Production),
            node(4, "embedder_mate", 8, Polarity::Production),
            node(5, "navigator_config", 10, Polarity::Production),
            node(6, "display_config", 10, Polarity::Production),
            node(7, "analyst_extra", 6, Polarity::Production),
            // Filler pairs, so the repo-wide balance terms are not
            // dominated by a single seven-symbol neighbourhood.
            node(8, "shared_one", 13, Polarity::Production),
            node(9, "shared_two", 13, Polarity::Production),
            node(10, "shared_three", 14, Polarity::Production),
            node(11, "shared_four", 14, Polarity::Production),
            node(12, "shared_five", 15, Polarity::Production),
            node(13, "shared_six", 15, Polarity::Production),
            node(14, "shared_seven", 16, Polarity::Production),
            node(15, "shared_eight", 16, Polarity::Production),
        ],
        vec![
            inherits(0, 2), // every sibling config extends the shared
            inherits(3, 2), // base, so the base is pulled three ways at
            inherits(4, 2), // once and has no home among them; the
            inherits(5, 2), // embedder file pulls hardest, two ways.
            edge(1, 0),     // co-residents bind their own files, so the
            edge(4, 3),     // only unforced symbol is the lone base.
            edge(7, 1),     // keeps the analyst file off the shell floor.
            edge(9, 8),
            edge(11, 10),
            edge(13, 12),
            edge(15, 14),
        ],
        vec![
            container(0, "app", ScopeLevel::PackageGroup, None),
            container(1, "generators", ScopeLevel::Folder, Some(0)),
            container(2, "analysts", ScopeLevel::Folder, Some(0)),
            container(3, "embedders", ScopeLevel::Folder, Some(0)),
            container(4, "navigators", ScopeLevel::Folder, Some(0)),
            container(5, "generators/schemas.ts", ScopeLevel::File, Some(1)),
            container(6, "analysts/schemas.ts", ScopeLevel::File, Some(2)),
            container(7, "analysts/analyst.ts", ScopeLevel::File, Some(2)),
            container(8, "embedders/schemas.ts", ScopeLevel::File, Some(3)),
            container(9, "embedders/embedder.ts", ScopeLevel::File, Some(3)),
            container(10, "navigators/schemas.ts", ScopeLevel::File, Some(4)),
            container(11, "navigators/navigator.ts", ScopeLevel::File, Some(4)),
            container(12, "shared", ScopeLevel::Folder, Some(0)),
            container(13, "shared/one.ts", ScopeLevel::File, Some(12)),
            container(14, "shared/two.ts", ScopeLevel::File, Some(12)),
            container(15, "shared/three.ts", ScopeLevel::File, Some(12)),
            container(16, "shared/four.ts", ScopeLevel::File, Some(12)),
        ],
    )
}

/// Production residents per file in the layout the pass starts from.
fn native_residents(snapshot: &Snapshot, assembled: &CandidateTree) -> BTreeMap<ContainerId, u32> {
    let mut counts: BTreeMap<ContainerId, u32> = BTreeMap::new();
    for node in &snapshot.ir().nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(&file) = assembled.placement.get(&node.id.0) {
            *counts.entry(file).or_insert(0) += 1;
        }
    }
    counts
}

/// FIX12 defect 1 — the no-empty-shells veto reads a ledger that an
/// ARRIVAL increments, so a symbol moving in can unlock the origin's last
/// native resident to leave. The file survives with a foreign occupant and
/// its own content gone: a file move wearing a symbol costume, which the
/// pass's own doctrine bars.
#[test]
fn should_not_let_an_arrival_unlock_a_lone_resident() {
    let snapshot = churn_snapshot();
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    let natives = native_residents(&snapshot, &assembled);
    let outcome = solver.symbol_polish(&solver.real_partition);

    let drained: Vec<String> = outcome
        .relocations
        .iter()
        .filter(|relocation| natives.get(&relocation.from_file).copied().unwrap_or(0) <= 1)
        .map(|relocation| {
            format!(
                "node {} out of file {}",
                relocation.node, relocation.from_file.0
            )
        })
        .collect();
    assert!(
        drained.is_empty(),
        "a file holding one production symbol at pass start keeps it; an \
             arrival must not unlock the departure. Drained {drained:?}"
    );
}

/// FIX12 defect 2 — destinations rank by each neighbour's EFFECTIVE
/// placement, so a symbol chases a neighbour that moved earlier in the
/// same pass. The resulting suggestion names a destination that nothing
/// about the symbol justifies against the layout the user actually has.
#[test]
fn should_not_nominate_a_destination_only_a_mid_pass_move_created() {
    let snapshot = churn_snapshot();
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let assembled = solver.assemble(&solver.real_partition);
    let ir = snapshot.ir();
    let outcome = solver.symbol_polish(&solver.real_partition);

    // Priced neighbours, resolved through the layout as it stands today.
    let base_neighbour_files = |subject: u32| -> BTreeSet<ContainerId> {
        let mut files = BTreeSet::new();
        for edge in &ir.edges {
            if solver.weights.edge_weight(edge.kind, edge.confidence) <= 0.0 {
                continue;
            }
            let neighbour = if edge.source.0 == subject {
                edge.target.0
            } else if edge.target.0 == subject {
                edge.source.0
            } else {
                continue;
            };
            if let Some(&file) = assembled.placement.get(&neighbour) {
                files.insert(file);
            }
        }
        files
    };

    let unjustified: Vec<String> = outcome
        .relocations
        .iter()
        .filter(|relocation| !base_neighbour_files(relocation.node).contains(&relocation.to_file))
        .map(|relocation| {
            format!(
                "node {} into file {}",
                relocation.node, relocation.to_file.0
            )
        })
        .collect();
    assert!(
        unjustified.is_empty(),
        "every destination must hold a priced neighbour under TODAY's \
             layout; a destination created by an earlier move this pass \
             cannot justify the suggestion. Unjustified {unjustified:?}"
    );
}

/// FIX12 defect 3 — the origin is read from the BASE placement even after
/// the overlay has already moved the symbol, so a second sweep can relocate
/// it again and narrate a second, contradictory destination for the very
/// same symbol and origin.
#[test]
fn should_not_narrate_a_symbol_moving_twice() {
    let snapshot = churn_snapshot();
    let tests = TestPolicy::defaults();
    let config = AnalyzeConfig::default();
    let solver = PipelineSolver::new(
        &snapshot,
        &config,
        config.profiles.greenfield.objective.coefficients(),
        false,
        &tests,
    );
    let outcome = solver.symbol_polish(&solver.real_partition);

    let mut seen: BTreeSet<u32> = BTreeSet::new();
    let repeated: Vec<u32> = outcome
        .relocations
        .iter()
        .filter(|relocation| !seen.insert(relocation.node))
        .map(|relocation| relocation.node)
        .collect();
    assert!(
        repeated.is_empty(),
        "a symbol relocates at most once per pass; twice means the second \
             move was priced against a home the symbol had already left. \
             Repeated {repeated:?}"
    );

    // The same defect at the narrated face: one symbol, one destination.
    // This half passes on this fixture — the DTO path narrates a candidate
    // layout that reaches only the first hop — and stands as the guard for
    // the real-repo duplicates (`summariseItem` narrated into both
    // `src/batch/types.ts` and `src/batch/adapters/types.ts`).
    let mut narrated = AnalyzeConfig::default();
    narrated.profiles.anchored.candidates = 1;
    let contradictions: Vec<String> = analyze(&snapshot, &narrated)
        .ok()
        .map(|result| {
            [&result.profiles.anchored, &result.profiles.greenfield]
                .into_iter()
                .flatten()
                .flat_map(|mode| &mode.candidates)
                .flat_map(|candidate| {
                    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
                    candidate
                        .symbol_moves
                        .iter()
                        .filter(|entry| {
                            !pairs.insert((entry.symbol.clone(), entry.from_path.clone()))
                        })
                        .map(|entry| {
                            format!("{}: {} -> {}", entry.symbol, entry.from_path, entry.to_path)
                        })
                        .collect::<Vec<String>>()
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        contradictions.is_empty(),
        "no candidate may narrate one symbol leaving one origin for two \
             destinations; got {contradictions:?}"
    );
}
