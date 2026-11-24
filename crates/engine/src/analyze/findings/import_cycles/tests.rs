use std::collections::BTreeMap;

use smol_str::SmolStr;
use unlace_ir::{
    ContainerTree, Edge, EdgeKind, FileImport, Hardness, ImportKind, IntermediateRepresentation,
    Node, NodeId, Polarity, ScopeLevel, Snapshot,
};

use super::graph::ImportGraph;
use super::{carried_references, cut};
use crate::analyze::analyze;
use crate::analyze::test_support::{container, edge, node};
use crate::config::AnalyzeConfig;
use crate::result::{
    AnalyzeResult, CycleAnalysis, ImportBreak, ImportCycleStatus, Violation, ViolationKind,
};

fn import(importer: &str, imported: &str, specifier: &str, line: u32) -> FileImport {
    FileImport {
        importer: SmolStr::new(importer),
        imported: SmolStr::new(imported),
        kind: ImportKind::Value,
        specifier: SmolStr::new(specifier),
        line,
    }
}

/// A snapshot over `files` (one container each, numbered in order) whose
/// adapters state `imports` for the files in `covered`.
fn snapshot_with_imports(
    files: &[&str],
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    imports: Vec<FileImport>,
    covered: &[&str],
) -> Result<Snapshot, String> {
    let containers = files
        .iter()
        .enumerate()
        .map(|(id, file)| {
            container(
                u32::try_from(id).unwrap_or(u32::MAX),
                file,
                ScopeLevel::File,
                None,
            )
        })
        .collect();
    let mut ir = IntermediateRepresentation::new(nodes, edges, ContainerTree::new(containers));
    ir.imports = imports;
    ir.import_coverage = covered.iter().copied().map(SmolStr::new).collect();
    Snapshot::assemble(ir).map_err(|error| error.to_string())
}

fn analyzed(snapshot: &Snapshot) -> Result<AnalyzeResult, String> {
    analyze(snapshot, &AnalyzeConfig::default()).map_err(|error| error.to_string())
}

fn cycles(result: &AnalyzeResult) -> Vec<&Violation> {
    result
        .current
        .shared_findings
        .iter()
        .filter(|finding| finding.kind == ViolationKind::Cycle)
        .collect()
}

/// The first item, or an error naming what was empty.
fn first<T: Copy>(items: &[T]) -> Result<T, String> {
    items
        .first()
        .copied()
        .ok_or_else(|| "expected at least one item".to_owned())
}

fn cut_of(finding: &Violation) -> Vec<&ImportBreak> {
    finding
        .import_cycle
        .iter()
        .flat_map(|cycle| cycle.cut.iter())
        .collect()
}

/// The mdc C1 shape: `parser/adapters.ts` and `parser/build.ts` import each
/// other, and the hard symbol graph also holds same-file recursion inside
/// `adapters.ts` (`applyAdapters -> recognizeAdapter`).
fn mdc_parser_snapshot() -> Result<Snapshot, String> {
    let adapters = "src/parser/adapters.ts";
    let build = "src/parser/build.ts";
    snapshot_with_imports(
        &[adapters, build],
        vec![
            node(0, "applyAdapters", 0, Polarity::Production),
            node(1, "recognizeAdapter", 0, Polarity::Production),
            node(2, "buildAst", 1, Polarity::Production),
            node(3, "buildNode", 1, Polarity::Production),
        ],
        vec![
            edge(0, 1),
            edge(1, 0),
            edge(1, 3),
            edge(2, 0),
            edge(2, 3),
            edge(3, 2),
        ],
        vec![
            import(adapters, build, "./build", 7),
            import(build, adapters, "./adapters", 14),
        ],
        &[adapters, build],
    )
}

#[test]
fn should_name_an_import_cut_for_the_mdc_parser_cycle_and_no_same_file_edge() -> Result<(), String>
{
    let result = analyzed(&mdc_parser_snapshot()?)?;

    let found = cycles(&result);
    assert_eq!(found.len(), 1, "{found:?}");
    let finding = first(&found)?;
    assert_eq!(
        finding.location,
        vec!["src/parser/adapters.ts", "src/parser/build.ts"]
    );
    assert_eq!(finding.break_suggestions, None);
    let breaks = cut_of(finding);
    assert_eq!(breaks.len(), 1, "a 2-file cycle needs one import removed");
    for removed in &breaks {
        assert_ne!(removed.importer, removed.imported);
        let statement = removed
            .statements
            .first()
            .map(|statement| (statement.specifier.as_str(), statement.line));
        assert!(
            matches!(statement, Some(("./build", 7) | ("./adapters", 14))),
            "{removed:?}"
        );
    }
    assert!(
        finding
            .detail
            .starts_with("2-file import cycle; to break it, remove")
    );
    assert!(!finding.detail.contains("same file"));
    assert_eq!(
        result.current.cycle_coverage.status,
        CycleAnalysis::Complete
    );
    Ok(())
}

#[test]
fn should_cut_the_import_carrying_the_fewest_references() -> Result<(), String> {
    let result = analyzed(&mdc_parser_snapshot()?)?;

    // adapters -> build carries recognizeAdapter -> buildNode (1); build ->
    // adapters carries buildAst -> applyAdapters (1): a tie on weight that the
    // path order breaks toward adapters.ts's import.
    let breaks: Vec<(String, String, u32)> = cycles(&result)
        .iter()
        .flat_map(|finding| cut_of(finding))
        .map(|removed| {
            (
                removed.importer.clone(),
                removed.imported.clone(),
                removed.weight,
            )
        })
        .collect();
    assert_eq!(
        breaks,
        vec![(
            "src/parser/adapters.ts".to_owned(),
            "src/parser/build.ts".to_owned(),
            1
        )]
    );
    let references: Vec<(String, String)> = cycles(&result)
        .iter()
        .flat_map(|finding| cut_of(finding))
        .flat_map(|removed| removed.references.iter())
        .map(|reference| (reference.source.clone(), reference.target.clone()))
        .collect();
    assert_eq!(
        references,
        vec![("recognizeAdapter".to_owned(), "buildNode".to_owned())]
    );
    Ok(())
}

/// The mdc lexer barrel: `block.ts` imports `readList` through
/// `container.ts`, which re-exports it from `container/list.ts`, and the
/// adapter resolves the call straight to `list.ts`.
#[test]
fn should_count_a_reference_resolved_through_a_barrel_against_the_barrel_import()
-> Result<(), String> {
    let block = "src/lexer/block.ts";
    let barrel = "src/lexer/container.ts";
    let list = "src/lexer/container/list.ts";
    let mut re_export = node(2, "readList", 1, Polarity::Production);
    re_export.re_export = true;
    let snapshot = snapshot_with_imports(
        &[block, barrel, list],
        vec![
            node(0, "readBlock", 0, Polarity::Production),
            node(1, "canInterruptParagraph", 0, Polarity::Production),
            re_export,
            node(3, "readList", 2, Polarity::Production),
        ],
        vec![
            edge(0, 3),
            edge(3, 1),
            Edge {
                source: NodeId(2),
                target: NodeId(3),
                kind: EdgeKind::ReExport,
                hardness: Hardness::Soft,
                confidence: 1.0,
            },
        ],
        vec![
            import(block, barrel, "./container", 10),
            import(barrel, list, "./container/list", 1),
            import(list, block, "../block", 1),
        ],
        &[block, barrel, list],
    )?;
    let graph = ImportGraph::new(&snapshot.ir().imports);
    let references = carried_references(&snapshot, &graph);
    let carried = |from: &str, to: &str| -> Vec<(String, String)> {
        graph
            .index_of(from)
            .zip(graph.index_of(to))
            .and_then(|pair| references.get(&pair))
            .into_iter()
            .flatten()
            .map(|reference| (reference.source.clone(), reference.target.clone()))
            .collect()
    };

    assert_eq!(
        carried(block, barrel),
        vec![("readBlock".to_owned(), "readList".to_owned())],
        "the barrel import carries the call it resolves"
    );
    assert_eq!(
        carried(list, block),
        vec![("readList".to_owned(), "canInterruptParagraph".to_owned())]
    );
    assert!(
        carried(barrel, list).is_empty(),
        "a soft re-export edge is no hard reference"
    );
    Ok(())
}

#[test]
fn should_cut_one_import_of_a_three_file_cycle_at_its_cheapest_edge() -> Result<(), String> {
    let (a, b, c) = ("src/a.ts", "src/b.ts", "src/c.ts");
    // a -> b carries 2 references, b -> c carries 2, c -> a carries 1.
    let snapshot = snapshot_with_imports(
        &[a, b, c],
        vec![
            node(0, "a1", 0, Polarity::Production),
            node(1, "a2", 0, Polarity::Production),
            node(2, "b1", 1, Polarity::Production),
            node(3, "b2", 1, Polarity::Production),
            node(4, "c1", 2, Polarity::Production),
            node(5, "c2", 2, Polarity::Production),
        ],
        vec![edge(0, 2), edge(1, 3), edge(2, 4), edge(3, 5), edge(4, 0)],
        vec![
            import(a, b, "./b", 1),
            import(b, c, "./c", 1),
            import(c, a, "./a", 3),
        ],
        &[a, b, c],
    )?;

    let result = analyzed(&snapshot)?;

    let found = cycles(&result);
    assert_eq!(found.len(), 1);
    assert_eq!((first(&found)?).location, vec![a, b, c]);
    let breaks = cut_of(first(&found)?);
    assert_eq!(breaks.len(), 1);
    assert_eq!(
        (
            (first(&breaks)?).importer.as_str(),
            (first(&breaks)?).imported.as_str()
        ),
        (c, a)
    );
    assert_eq!((first(&breaks)?).weight, 1);
    assert!(
        (first(&found)?)
            .import_cycle
            .as_ref()
            .is_some_and(|cycle| cycle.exact)
    );
    Ok(())
}

#[test]
fn should_keep_every_statement_of_a_collapsed_import() -> Result<(), String> {
    let (a, b) = ("src/a.ts", "src/b.ts");
    let snapshot = snapshot_with_imports(
        &[a, b],
        vec![
            node(0, "a1", 0, Polarity::Production),
            node(1, "b1", 1, Polarity::Production),
        ],
        vec![edge(1, 0)],
        vec![
            import(a, b, "./b", 2),
            import(a, b, "./b", 5),
            import(b, a, "./a", 1),
            import(b, a, "./a", 9),
        ],
        &[a, b],
    )?;

    let result = analyzed(&snapshot)?;

    // a -> b carries no reference (weight 1) and b -> a carries one (weight 1);
    // the path-order tie cuts a's import, both of its statements.
    let breaks: Vec<Vec<(String, u32)>> = cycles(&result)
        .iter()
        .flat_map(|finding| cut_of(finding))
        .map(|removed| {
            removed
                .statements
                .iter()
                .map(|statement| (statement.specifier.clone(), statement.line))
                .collect()
        })
        .collect();
    assert_eq!(
        breaks,
        vec![vec![("./b".to_owned(), 2), ("./b".to_owned(), 5)]]
    );
    Ok(())
}

#[test]
fn should_report_a_cycle_through_a_file_without_facts_as_unavailable() -> Result<(), String> {
    let (a, b, c) = ("src/a.ts", "src/b.ts", "src/c.ts");
    let snapshot = snapshot_with_imports(
        &[a, b, c],
        vec![
            node(0, "a1", 0, Polarity::Production),
            node(1, "b1", 1, Polarity::Production),
            node(2, "c1", 2, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 2), edge(2, 0)],
        vec![import(a, b, "./b", 1), import(b, c, "./c", 1)],
        &[a, b],
    )?;

    let result = analyzed(&snapshot)?;

    let found = cycles(&result);
    assert_eq!(found.len(), 1);
    assert_eq!((first(&found)?).location, vec![a, b, c]);
    let cycle = (first(&found)?)
        .import_cycle
        .as_ref()
        .ok_or("no import cycle")?;
    assert_eq!(cycle.status, ImportCycleStatus::Unavailable);
    assert!(cycle.cut.is_empty());
    assert_eq!(cycle.files_without_imports, vec![c]);
    assert!(
        (first(&found)?)
            .detail
            .contains("import facts are unavailable for src/c.ts")
    );
    assert_eq!(result.current.cycle_coverage.status, CycleAnalysis::Partial);
    assert_eq!(result.current.cycle_coverage.files_without_imports, 1);
    Ok(())
}

#[test]
fn should_report_cycle_analysis_unavailable_when_no_file_states_imports() -> Result<(), String> {
    let (a, b) = ("src/a.py", "src/b.py");
    let snapshot = snapshot_with_imports(
        &[a, b],
        vec![
            node(0, "a1", 0, Polarity::Production),
            node(1, "b1", 1, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        Vec::new(),
        &[],
    )?;

    let result = analyzed(&snapshot)?;

    assert_eq!(
        result.current.cycle_coverage.status,
        CycleAnalysis::Unavailable
    );
    assert_eq!(result.current.cycle_coverage.files_without_imports, 2);
    let found = cycles(&result);
    assert_eq!(found.len(), 1);
    let cycle = (first(&found)?)
        .import_cycle
        .as_ref()
        .ok_or("no import cycle")?;
    assert_eq!(cycle.status, ImportCycleStatus::Unavailable);
    assert_eq!(cycle.files_without_imports, vec![a, b]);
    Ok(())
}

#[test]
fn should_report_no_finding_for_a_cycle_the_imports_do_not_close() -> Result<(), String> {
    // a type-only back reference: a hard symbol cycle with no import cycle.
    let (a, b) = ("src/a.ts", "src/b.ts");
    let snapshot = snapshot_with_imports(
        &[a, b],
        vec![
            node(0, "a1", 0, Polarity::Production),
            node(1, "b1", 1, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![import(a, b, "./b", 1)],
        &[a, b],
    )?;

    let result = analyzed(&snapshot)?;

    assert!(cycles(&result).is_empty());
    assert_eq!(
        result.current.cycle_coverage.status,
        CycleAnalysis::Complete
    );
    Ok(())
}

#[test]
fn should_drop_rust_module_declarations_from_the_cycle_graph() -> Result<(), String> {
    let (lib, child, tests, sibling) = (
        "src/lib.rs",
        "src/child.rs",
        "src/child/tests.rs",
        "src/sibling.rs",
    );
    // lib.rs declares child and sibling; child declares its cfg(test) tests
    // module; each child globs or names its parent back.
    let snapshot = snapshot_with_imports(
        &[lib, child, tests, sibling],
        vec![
            node(0, "Config", 0, Polarity::Production),
            node(1, "run", 1, Polarity::Production),
            node(2, "should_run", 2, Polarity::TestCase),
            node(3, "helper", 3, Polarity::Production),
        ],
        vec![edge(1, 0), edge(2, 1), edge(3, 0)],
        vec![
            declaration(lib, child, "child", 1),
            declaration(lib, sibling, "sibling", 2),
            import(child, lib, "crate::Config", 1),
            declaration(child, tests, "tests", 9),
            import(tests, child, "super::*", 1),
            import(sibling, lib, "super::*", 1),
        ],
        &[lib, child, tests, sibling],
    )?;

    let result = analyzed(&snapshot)?;

    assert!(cycles(&result).is_empty(), "{:?}", cycles(&result));
    Ok(())
}

#[test]
fn should_still_report_a_rust_use_cycle_between_sibling_modules() -> Result<(), String> {
    let (lib, left, right) = ("src/lib.rs", "src/left.rs", "src/right.rs");
    let snapshot = snapshot_with_imports(
        &[lib, left, right],
        vec![
            node(0, "Root", 0, Polarity::Production),
            node(1, "Left", 1, Polarity::Production),
            node(2, "Right", 2, Polarity::Production),
        ],
        vec![edge(1, 2), edge(2, 1)],
        vec![
            declaration(lib, left, "left", 1),
            declaration(lib, right, "right", 2),
            import(left, right, "crate::right::Right", 1),
            import(right, left, "crate::left::Left", 1),
        ],
        &[lib, left, right],
    )?;

    let result = analyzed(&snapshot)?;

    let found = cycles(&result);
    assert_eq!(found.len(), 1);
    assert_eq!((first(&found)?).location, vec![left, right]);
    let statements: Vec<&str> = cut_of(first(&found)?)
        .iter()
        .flat_map(|removed| removed.statements.iter())
        .map(|statement| statement.specifier.as_str())
        .collect();
    assert_eq!(statements, vec!["crate::right::Right"]);
    Ok(())
}

#[test]
fn should_keep_a_bare_python_import_in_the_cycle_graph() -> Result<(), String> {
    let (alpha, beta) = ("alpha.py", "beta.py");
    let snapshot = snapshot_with_imports(
        &[alpha, beta],
        vec![
            node(0, "alpha_step", 0, Polarity::Production),
            node(1, "beta_step", 1, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![
            import(alpha, beta, "beta", 3),
            import(beta, alpha, "alpha", 3),
        ],
        &[alpha, beta],
    )?;

    let result = analyzed(&snapshot)?;

    assert_eq!(cycles(&result).len(), 1);
    Ok(())
}

#[test]
fn should_break_a_weight_tie_on_path_order_whatever_the_fact_order() -> Result<(), String> {
    let (a, b) = ("src/a.ts", "src/b.ts");
    let forward = vec![import(a, b, "./b", 1), import(b, a, "./a", 1)];
    let mut reversed = forward.clone();
    reversed.reverse();
    let mut cuts = Vec::new();
    for imports in [forward, reversed] {
        let snapshot = snapshot_with_imports(
            &[b, a],
            vec![
                node(0, "b1", 0, Polarity::Production),
                node(1, "a1", 1, Polarity::Production),
            ],
            Vec::new(),
            imports,
            &[a, b],
        )?;
        let result = analyzed(&snapshot)?;
        cuts.push(
            cycles(&result)
                .iter()
                .flat_map(|finding| cut_of(finding))
                .map(|removed| (removed.importer.clone(), removed.imported.clone()))
                .collect::<Vec<_>>(),
        );
    }

    let expected = vec![(a.to_owned(), b.to_owned())];
    assert_eq!(cuts, vec![expected.clone(), expected]);
    Ok(())
}

#[test]
fn should_solve_a_ring_beyond_the_exact_limit_with_one_heuristic_cut() {
    let count = cut::EXACT_LIMIT + 3;
    let mut weights: BTreeMap<(usize, usize), u64> = (0..count)
        .map(|file| ((file, (file + 1) % count), 3))
        .collect();
    weights.insert((4, 5), 1);

    let solved = cut::solve(count, &weights);

    assert_eq!(
        solved,
        cut::Cut {
            edges: vec![(4, 5)],
            exact: false
        }
    );
}

#[test]
fn should_cut_the_cheap_side_of_two_two_cycles_sharing_a_file() {
    // two 2-cycles sharing file 0: the minimum removes 0 -> 1 and 0 -> 2
    // (weight 2) rather than both heavy back edges.
    let weights: BTreeMap<(usize, usize), u64> =
        [((0, 1), 1), ((1, 0), 5), ((0, 2), 1), ((2, 0), 5)]
            .into_iter()
            .collect();

    let solved = cut::solve(3, &weights);

    assert_eq!(
        solved,
        cut::Cut {
            edges: vec![(0, 1), (0, 2)],
            exact: true
        }
    );
}

#[test]
fn should_solve_exactly_where_a_greedy_order_overpays() {
    // one ring 0 -> 1 -> 2 -> 0. The greedy order takes file 0 first (largest
    // outgoing-minus-incoming weight), so its heavier incoming edge 2 -> 0 runs
    // backward, and on a single ring no restore can undo that.
    let weights: BTreeMap<(usize, usize), u64> = [((0, 1), 4), ((1, 2), 1), ((2, 0), 2)]
        .into_iter()
        .collect();

    let greedy = cut::greedy(3, &weights);
    let solved = cut::solve(3, &weights);

    assert_eq!(greedy, vec![(2, 0)]);
    assert_eq!(
        solved,
        cut::Cut {
            edges: vec![(1, 2)],
            exact: true
        }
    );
}

/// Knuth's MMIX linear congruential generator: seeded, tiny, and stable.
struct Lcg(u64);

impl Lcg {
    /// A draw in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let bound = u64::try_from(bound.max(1)).unwrap_or(u64::MAX);
        usize::try_from((self.0 >> 33) % bound).unwrap_or(0)
    }

    /// A weight in `1..=top`.
    fn weight(&mut self, top: usize) -> u64 {
        u64::try_from(self.below(top) + 1).unwrap_or(1)
    }
}

/// Whether `edges` over `count` files form no cycle (Kahn's algorithm).
fn is_acyclic(count: usize, edges: &[(usize, usize)]) -> bool {
    let mut incoming = vec![0_usize; count];
    let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &(importer, imported) in edges {
        if let Some(degree) = incoming.get_mut(imported) {
            *degree += 1;
        }
        if let Some(targets) = outgoing.get_mut(importer) {
            targets.push(imported);
        }
    }
    let mut ready: Vec<usize> = (0..count)
        .filter(|&file| incoming.get(file) == Some(&0))
        .collect();
    let mut seen = 0;
    while let Some(file) = ready.pop() {
        seen += 1;
        for &target in outgoing.get(file).into_iter().flatten() {
            if let Some(degree) = incoming.get_mut(target) {
                *degree -= 1;
                if *degree == 0 {
                    ready.push(target);
                }
            }
        }
    }
    seen == count
}

/// The edges of `weights` outside `cut`.
fn kept(weights: &BTreeMap<(usize, usize), u64>, cut: &[(usize, usize)]) -> Vec<(usize, usize)> {
    weights
        .keys()
        .copied()
        .filter(|edge| !cut.contains(edge))
        .collect()
}

/// The cheapest edge subset whose removal leaves no cycle, by exhaustive
/// enumeration, under the cut's own key: weight, then import count, then
/// the ascending edge list.
fn brute_force_cut(count: usize, weights: &BTreeMap<(usize, usize), u64>) -> Vec<(usize, usize)> {
    /// A cut's ordering key: weight, import count, ascending edge list.
    type Key = (u64, usize, Vec<(usize, usize)>);
    let edges: Vec<((usize, usize), u64)> = weights.iter().map(|(&edge, &w)| (edge, w)).collect();
    let mut best: Option<Key> = None;
    for mask in 0_u32..(1_u32 << edges.len()) {
        let chosen = |index: usize| mask & (1_u32 << index) != 0;
        let removed: Vec<(usize, usize)> = edges
            .iter()
            .enumerate()
            .filter(|(index, _)| chosen(*index))
            .map(|(_, (edge, _))| *edge)
            .collect();
        let cost = edges
            .iter()
            .enumerate()
            .filter(|(index, _)| chosen(*index))
            .map(|(_, (_, weight))| *weight)
            .sum();
        let key = (cost, removed.len(), removed);
        if best.as_ref().is_some_and(|current| key >= *current) {
            continue;
        }
        let remaining: Vec<(usize, usize)> = edges
            .iter()
            .enumerate()
            .filter(|(index, _)| !chosen(*index))
            .map(|(_, (edge, _))| *edge)
            .collect();
        if is_acyclic(count, &remaining) {
            best = Some(key);
        }
    }
    best.map(|(_, _, cut)| cut).unwrap_or_default()
}

#[test]
fn should_match_exhaustive_enumeration_on_small_seeded_graphs() {
    let mut random = Lcg(0x00d1_f14b);
    for case in 0..200 {
        let count = 2 + random.below(5);
        let mut weights: BTreeMap<(usize, usize), u64> = BTreeMap::new();
        // at most 12 attempts, so at most 2^12 subsets to enumerate.
        for _ in 0..random.below(13) {
            let (importer, imported) = (random.below(count), random.below(count));
            if importer != imported {
                // weights 1..=3 so ties on weight and count are common.
                weights.insert((importer, imported), random.weight(3));
            }
        }

        let solved = cut::solve(count, &weights);

        assert!(solved.exact);
        assert_eq!(
            solved.edges,
            brute_force_cut(count, &weights),
            "case {case}: {count} files, {weights:?}"
        );
    }
}

/// A component of `count` files made of overlapping cycles: each cycle walks
/// 3 to 8 files, and every file lies on a ring through all of them.
fn overlapping_cycles(random: &mut Lcg, count: usize) -> BTreeMap<(usize, usize), u64> {
    let mut weights: BTreeMap<(usize, usize), u64> = (0..count)
        .map(|file| ((file, (file + 1) % count), random.weight(4)))
        .collect();
    for _ in 0..count {
        let length = 3 + random.below(6);
        let walk: Vec<usize> = (0..length).map(|_| random.below(count)).collect();
        for (step, &importer) in walk.iter().enumerate() {
            let imported = walk.get((step + 1) % length).copied().unwrap_or(importer);
            if importer != imported {
                weights.insert((importer, imported), random.weight(4));
            }
        }
    }
    weights
}

#[test]
fn should_leave_no_cycle_and_no_needless_cut_above_the_exact_limit() {
    let mut random = Lcg(35);
    for case in 0..40 {
        let count = cut::EXACT_LIMIT + 1 + random.below(28);
        let weights = overlapping_cycles(&mut random, count);

        let solved = cut::solve(count, &weights);

        assert!(!solved.exact);
        assert!(
            is_acyclic(count, &kept(&weights, &solved.edges)),
            "case {case}: the cut leaves a cycle"
        );
        // the restore pass keeps no edge whose return closes no cycle.
        for edge in &solved.edges {
            let without: Vec<(usize, usize)> = solved
                .edges
                .iter()
                .copied()
                .filter(|other| other != edge)
                .collect();
            assert!(
                !is_acyclic(count, &kept(&weights, &without)),
                "case {case}: {edge:?} need not be cut"
            );
        }
    }
}

#[test]
#[expect(
    clippy::print_stderr,
    reason = "records the measured cost under --nocapture; wall time is never asserted"
)]
fn should_cut_a_three_thousand_file_component_in_near_linear_time() {
    // one strongly connected group: a ring through every file plus chords.
    let count = 3000;
    let mut random = Lcg(3000);
    let mut weights: BTreeMap<(usize, usize), u64> = (0..count)
        .map(|file| ((file, (file + 1) % count), random.weight(5)))
        .collect();
    while weights.len() < 15_000 {
        let (importer, imported) = (random.below(count), random.below(count));
        if importer != imported {
            weights.insert((importer, imported), random.weight(5));
        }
    }

    let started = std::time::Instant::now();
    let solved = cut::solve(count, &weights);
    let elapsed = started.elapsed();

    // no wall-time assertion: the measurement is printed for the record.
    eprintln!(
        "large cut: {count} files, {} imports, {} cut, {elapsed:?}",
        weights.len(),
        solved.edges.len()
    );
    assert!(!solved.exact);
    assert!(!solved.edges.is_empty());
    assert!(is_acyclic(count, &kept(&weights, &solved.edges)));
}

/// One removed import: importer, imported, and its statements.
type Removed<'a> = (&'a str, &'a str, Vec<(&'a str, u32)>);

/// A Rust `mod child;` declaration fact.
fn declaration(importer: &str, imported: &str, name: &str, line: u32) -> FileImport {
    FileImport {
        kind: ImportKind::ModuleDeclaration,
        ..import(importer, imported, name, line)
    }
}

#[test]
fn should_report_a_cycle_when_a_parent_also_uses_its_declared_child() -> Result<(), String> {
    let (lib, child) = ("src/lib.rs", "src/child.rs");
    // lib.rs declares child and calls child::run; child uses super::{Config,
    // Mode}. The declaration never shows in the cut, the qualified use does.
    let snapshot = snapshot_with_imports(
        &[lib, child],
        vec![
            node(0, "main", 0, Polarity::Production),
            node(1, "Config", 0, Polarity::Production),
            node(2, "Mode", 0, Polarity::Production),
            node(3, "run", 1, Polarity::Production),
        ],
        vec![edge(0, 3), edge(3, 1), edge(3, 2)],
        vec![
            declaration(lib, child, "child", 1),
            import(lib, child, "child::run", 4),
            import(child, lib, "super::Config", 1),
            import(child, lib, "super::Mode", 2),
        ],
        &[lib, child],
    )?;

    let result = analyzed(&snapshot)?;

    let found = cycles(&result);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((first(&found)?).location, vec![child, lib]);
    let removed: Vec<Removed<'_>> = cut_of(first(&found)?)
        .iter()
        .map(|removed| {
            (
                removed.importer.as_str(),
                removed.imported.as_str(),
                removed
                    .statements
                    .iter()
                    .map(|statement| (statement.specifier.as_str(), statement.line))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(removed, vec![(lib, child, vec![("child::run", 4)])]);
    Ok(())
}

#[test]
fn should_report_no_cycle_for_a_bare_declaration_and_a_use_super_back() -> Result<(), String> {
    let (lib, child) = ("src/lib.rs", "src/child.rs");
    let snapshot = snapshot_with_imports(
        &[lib, child],
        vec![
            node(0, "Config", 0, Polarity::Production),
            node(1, "run", 1, Polarity::Production),
        ],
        vec![edge(1, 0)],
        vec![
            declaration(lib, child, "child", 1),
            import(child, lib, "super::Config", 1),
        ],
        &[lib, child],
    )?;

    let result = analyzed(&snapshot)?;

    assert!(cycles(&result).is_empty(), "{:?}", cycles(&result));
    Ok(())
}

#[test]
fn should_keep_a_single_segment_value_import_beside_path_spelled_ones() -> Result<(), String> {
    // the retired heuristic dropped `b` as a module declaration because a.rs
    // also takes part in a `::` import; only the fact's kind decides now.
    let (a, b) = ("src/a.rs", "src/b.rs");
    let snapshot = snapshot_with_imports(
        &[a, b],
        vec![
            node(0, "Item", 0, Polarity::Production),
            node(1, "helper", 1, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0)],
        vec![import(a, b, "b", 2), import(b, a, "crate::a::Item", 1)],
        &[a, b],
    )?;

    let result = analyzed(&snapshot)?;

    let found = cycles(&result);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((first(&found)?).location, vec![a, b]);
    Ok(())
}

/// The status of each cycle finding with its location and the files it says
/// state no imports.
fn statuses(result: &AnalyzeResult) -> Vec<(ImportCycleStatus, Vec<String>, Vec<String>)> {
    cycles(result)
        .iter()
        .filter_map(|finding| {
            let cycle = finding.import_cycle.as_ref()?;
            Some((
                cycle.status,
                finding.location.clone(),
                cycle.files_without_imports.clone(),
            ))
        })
        .collect()
}

#[test]
fn should_list_only_the_uncovered_part_of_a_symbol_cycle_beside_an_import_cycle()
-> Result<(), String> {
    let (a, b, c) = ("src/a.ts", "src/b.ts", "src/c.ts");
    // one symbol component over a, b and c; c states no imports, and a ⇄ b
    // is an import cycle with a cut of its own.
    let snapshot = snapshot_with_imports(
        &[a, b, c],
        vec![
            node(0, "a1", 0, Polarity::Production),
            node(1, "b1", 1, Polarity::Production),
            node(2, "c1", 2, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0), edge(1, 2), edge(2, 0)],
        vec![import(a, b, "./b", 1), import(b, a, "./a", 1)],
        &[a, b],
    )?;

    let result = analyzed(&snapshot)?;

    let mut found = statuses(&result);
    found.sort_by(|left, right| left.1.cmp(&right.1));
    let owned = |files: &[&str]| files.iter().map(|file| (*file).to_owned()).collect();
    assert_eq!(
        found,
        vec![
            (ImportCycleStatus::Cut, owned(&[a, b]), Vec::new()),
            (ImportCycleStatus::Unavailable, owned(&[c]), owned(&[c])),
        ]
    );
    let unavailable = cycles(&result)
        .into_iter()
        .find(|finding| finding.location == [c])
        .map(|finding| finding.detail.clone());
    assert_eq!(
        unavailable.as_deref(),
        Some(
            "dependency cycle through 1 file; import facts are unavailable for src/c.ts, \
             so the imports to remove cannot be named"
        )
    );
    Ok(())
}

#[test]
fn should_drop_the_unavailable_finding_when_cut_findings_cover_its_files() -> Result<(), String> {
    // c lacks coverage but still holds facts that close a ⇄ b ⇄ c, so the cut
    // finding covers every file of the symbol component.
    let (a, b, c) = ("src/a.ts", "src/b.ts", "src/c.ts");
    let snapshot = snapshot_with_imports(
        &[a, b, c],
        vec![
            node(0, "a1", 0, Polarity::Production),
            node(1, "b1", 1, Polarity::Production),
            node(2, "c1", 2, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 2), edge(2, 0)],
        vec![
            import(a, b, "./b", 1),
            import(b, c, "./c", 1),
            import(c, a, "./a", 1),
        ],
        &[a, b],
    )?;

    let result = analyzed(&snapshot)?;

    let owned = |files: &[&str]| files.iter().map(|file| (*file).to_owned()).collect();
    assert_eq!(
        statuses(&result),
        vec![(ImportCycleStatus::Cut, owned(&[a, b, c]), Vec::new())]
    );
    Ok(())
}

#[test]
fn should_keep_an_uncovered_file_reached_through_a_covered_fact_less_file() -> Result<(), String> {
    // a lacks coverage yet sits in the cut finding a ⇄ b; the symbol cycle
    // a1 ⇄ d1 closes through an unstated a → d, so d's cycle is reported as
    // unavailable through a rather than dropped.
    let (a, b, d) = ("src/a.ts", "src/b.ts", "src/d.ts");
    let snapshot = snapshot_with_imports(
        &[a, b, d],
        vec![
            node(0, "a1", 0, Polarity::Production),
            node(1, "b1", 1, Polarity::Production),
            node(2, "d1", 2, Polarity::Production),
        ],
        vec![edge(0, 1), edge(1, 0), edge(0, 2), edge(2, 0)],
        vec![
            import(a, b, "./b", 1),
            import(b, a, "./a", 1),
            import(d, a, "./a", 1),
        ],
        &[b, d],
    )?;

    let result = analyzed(&snapshot)?;

    let owned = |files: &[&str]| files.iter().map(|file| (*file).to_owned()).collect();
    let mut found = statuses(&result);
    found.sort_by(|left, right| left.1.cmp(&right.1));
    assert_eq!(
        found,
        vec![
            (ImportCycleStatus::Cut, owned(&[a, b]), Vec::new()),
            (ImportCycleStatus::Unavailable, owned(&[a, d]), owned(&[a])),
        ]
    );
    Ok(())
}

#[test]
fn should_count_references_by_symbol_identity_not_display_name() -> Result<(), String> {
    let (a, b) = ("src/a.ts", "src/b.ts");
    // two distinct `helper` symbols in a.ts each call b.ts's `run`: two
    // references, though both read `helper -> run`.
    let snapshot = snapshot_with_imports(
        &[a, b],
        vec![
            node(0, "helper", 0, Polarity::Production),
            node(1, "helper", 0, Polarity::Production),
            node(2, "run", 1, Polarity::Production),
            node(3, "go", 1, Polarity::Production),
        ],
        vec![edge(0, 2), edge(1, 2), edge(2, 0), edge(2, 1), edge(3, 0)],
        vec![import(a, b, "./b", 1), import(b, a, "./a", 1)],
        &[a, b],
    )?;

    let result = analyzed(&snapshot)?;

    let found = cycles(&result);
    let breaks = cut_of(first(&found)?);
    let removed = first(&breaks)?;
    assert_eq!(
        (removed.importer.as_str(), removed.imported.as_str()),
        (a, b)
    );
    assert_eq!(removed.weight, 2);
    let named: Vec<(&str, &str)> = removed
        .references
        .iter()
        .map(|reference| (reference.source.as_str(), reference.target.as_str()))
        .collect();
    assert_eq!(named, vec![("helper", "run"), ("helper", "run")]);
    Ok(())
}
