use std::collections::BTreeMap;

use smol_str::SmolStr;
use unlace_ir::laminar::{Companions, build_laminar_tree_with_companions};
use unlace_ir::{
    Container, ContainerId, ContainerTree, FileFact, IntermediateRepresentation, Layout, Snapshot,
};

use super::UnitFacts;
use crate::analyze::test_support::sloc_node;

const ENTRY: &str = "src/lexer/block.ts";
const MEMBER: &str = "src/lexer/block/mdcx.ts";
const SIBLING: &str = "src/lexer/block/table.ts";
const OUTSIDE: &str = "src/lexer/source.ts";

/// A unit `src/lexer/block/` with its entry `block.ts`, plus a file outside
/// it, one declaration per file (node `i` lives in path `i`). With
/// `companion` false the folder carries no companion fact, so no unit exists.
struct Fixture {
    snapshot: Snapshot,
    files: BTreeMap<&'static str, ContainerId>,
}

#[allow(clippy::panic)] // a broken fixture must fail loudly, never hide behind a fallback
fn fixture(companion: bool) -> Fixture {
    let order = [ENTRY, MEMBER, SIBLING, OUTSIDE];
    let paths: Vec<SmolStr> = order.into_iter().map(SmolStr::new).collect();
    let facts: Vec<FileFact> = if companion {
        vec![FileFact::folder_companion(ENTRY, "src/lexer/block")]
    } else {
        Vec::new()
    };
    let layout = Layout {
        package_roots: Vec::new(),
        source_roots: vec![SmolStr::new("src")],
    };
    let built =
        build_laminar_tree_with_companions(&paths, "mdc", &layout, &Companions::from_facts(&facts));
    let files: BTreeMap<&'static str, ContainerId> = order
        .into_iter()
        .filter_map(|path| Some((path, *built.files.get(path)?)))
        .collect();
    let nodes = order
        .into_iter()
        .enumerate()
        .filter_map(|(index, path)| {
            // reason: four fixture nodes index well inside u32.
            #[allow(clippy::cast_possible_truncation)]
            let id = index as u32;
            Some(sloc_node(id, path, *files.get(path)?, 10))
        })
        .collect();
    let mut ir = IntermediateRepresentation::new(nodes, Vec::new(), built.tree);
    ir.file_facts = facts;
    let snapshot = Snapshot::assemble(ir)
        .unwrap_or_else(|error| panic!("test snapshot failed to assemble: {error}"));
    Fixture { snapshot, files }
}

impl Fixture {
    fn file(&self, path: &str) -> ContainerId {
        self.files
            .get(path)
            .copied()
            .unwrap_or(ContainerId(u32::MAX))
    }

    /// Node id of the declaration that starts in `path`.
    fn node(path: &str) -> u32 {
        match path {
            ENTRY => 0,
            MEMBER => 1,
            SIBLING => 2,
            _ => 3,
        }
    }

    /// The flows when only the declaration of `from` moves, into `to`.
    fn flows_of_one_move(&self, from: &str, to: &str) -> (u32, u32) {
        let moved = Self::node(from);
        let destination = self.file(to);
        let start: BTreeMap<u32, ContainerId> = self
            .snapshot
            .ir()
            .nodes
            .iter()
            .map(|node| (node.id.0, node.container))
            .collect();
        UnitFacts::new(&self.snapshot).declaration_flows(&|node| {
            if node == moved {
                Some(destination)
            } else {
                start.get(&node).copied()
            }
        })
    }

    /// The file outflows of the pass-start tree with `path` reparented under
    /// the folder that holds `beside`.
    fn file_outflows_moving(&self, path: &str, beside: &str) -> u32 {
        let containers = self.snapshot.ir().containers.containers();
        let target_parent = containers
            .iter()
            .find(|container| container.id == self.file(beside))
            .and_then(|container| container.parent);
        let moved: Vec<Container> = containers
            .iter()
            .cloned()
            .map(|mut container| {
                if container.id == self.file(path) {
                    container.parent = target_parent;
                }
                container
            })
            .collect();
        let identity: BTreeMap<ContainerId, ContainerId> =
            self.files.values().map(|file| (*file, *file)).collect();
        UnitFacts::new(&self.snapshot).file_outflows(&ContainerTree::new(moved), &identity)
    }
}

#[test]
fn should_count_a_declaration_moved_from_the_folder_into_its_entry_as_inflow() {
    let fixture = fixture(true);

    assert_eq!(fixture.flows_of_one_move(MEMBER, ENTRY), (1, 0));
}

#[test]
fn should_not_count_a_declaration_moved_from_the_entry_into_its_folder() {
    let fixture = fixture(true);

    assert_eq!(fixture.flows_of_one_move(ENTRY, MEMBER), (0, 0));
}

#[test]
fn should_count_a_declaration_leaving_the_unit_as_outflow() {
    let fixture = fixture(true);

    assert_eq!(fixture.flows_of_one_move(MEMBER, OUTSIDE), (0, 1));
    assert_eq!(fixture.flows_of_one_move(ENTRY, OUTSIDE), (0, 1));
}

#[test]
fn should_not_count_a_declaration_entering_the_unit_from_outside() {
    let fixture = fixture(true);

    assert_eq!(fixture.flows_of_one_move(OUTSIDE, ENTRY), (0, 0));
}

#[test]
fn should_count_a_member_file_hoisted_to_the_parent_once_as_outflow() {
    let fixture = fixture(true);

    assert_eq!(fixture.file_outflows_moving(MEMBER, OUTSIDE), 1);
}

#[test]
fn should_count_neither_term_for_a_move_within_the_folder() {
    let fixture = fixture(true);

    assert_eq!(fixture.flows_of_one_move(MEMBER, SIBLING), (0, 0));
    assert_eq!(fixture.file_outflows_moving(MEMBER, SIBLING), 0);
}

#[test]
fn should_count_neither_term_for_an_unmoved_layout() {
    let fixture = fixture(true);

    assert_eq!(fixture.flows_of_one_move(MEMBER, MEMBER), (0, 0));
    assert_eq!(fixture.file_outflows_moving(MEMBER, MEMBER), 0);
}

#[test]
fn should_count_neither_term_for_a_folder_without_a_companion() {
    let fixture = fixture(false);

    assert_eq!(fixture.flows_of_one_move(MEMBER, ENTRY), (0, 0));
    assert_eq!(fixture.flows_of_one_move(MEMBER, OUTSIDE), (0, 0));
    assert_eq!(fixture.file_outflows_moving(MEMBER, OUTSIDE), 0);
}
