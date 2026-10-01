//! `ra_ap`-backed cross-crate resolution and IR-fragment emission.
//!
//! Binding turns the per-file [`ParsedFile`] summaries from [`crate::parse`]
//! into a language-agnostic [`IrFragment`]: a dense node per top-level
//! declaration, a laminar container tree over files/folders/domains/packages,
//! typed dependency edges, and three-valued test polarity.
//!
//! Unlike a purely textual binder, resolution runs through the rust-analyzer
//! semantic database loaded from the on-disk cargo workspace at `manifest`.
//! Each reference recorded during parsing carries the byte offset at which it
//! occurs; `goto_definition` resolves that offset to a definition `(file,
//! offset)`, which is mapped back to the declaration whose byte range contains
//! it. References that originate inside a macro invocation resolve at confidence
//! below `1.0` — honest uncertainty rather than silence.

mod assignment;
mod containers;
mod database;
mod edges;
mod fallback;
mod polarity;
mod visibility;

use std::path::Path;

use strata_ir::IrFragment;

use self::assignment::NodeAssignment;
use self::containers::assignment_containers;
use self::database::Database;
use self::edges::{dedup_edges, resolve_edges, resolve_re_exports};
use self::polarity::classify_polarity;
use self::visibility::resolve_visibility_scopes;
use crate::parse::ParsedFile;

/// A binding failure that aborts fragment emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {
    /// The cargo workspace could not be loaded (broken manifest, missing
    /// lockfile, unreadable sources).
    LoadFailed {
        /// Human-readable explanation of why the workspace failed to load.
        reason: String,
    },
}

impl std::fmt::Display for BindOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadFailed { reason } => {
                write!(formatter, "failed to load cargo workspace: {reason}")
            }
        }
    }
}

impl std::error::Error for BindOutcome {}

/// Resolves the cross-crate graph for `files` and emits a single [`IrFragment`].
///
/// `manifest` points at the workspace `Cargo.toml`; the rust-analyzer database
/// is loaded from it so cross-crate references resolve. `root` is the repository
/// root the parsed file paths are relative to; it anchors container naming and
/// the vfs-path-to-relative-path mapping.
///
/// # Errors
///
/// Returns [`BindOutcome::LoadFailed`] when the cargo workspace cannot be loaded.
pub(super) fn bind(
    files: &[ParsedFile],
    manifest: &Path,
    root: &Path,
) -> Result<IrFragment, BindOutcome> {
    let workspace_root = manifest.parent().unwrap_or(manifest);
    let database = Database::load(manifest)?;

    let assignment = NodeAssignment::build(files, root);
    let mut edges = resolve_edges(files, &assignment, &database, workspace_root);
    resolve_re_exports(&assignment, &database, workspace_root, &mut edges);
    let visibility_scopes = resolve_visibility_scopes(files, &assignment, &database, root);
    let edges = dedup_edges(edges);
    let mut nodes = assignment.into_nodes();
    classify_polarity(files, &mut nodes, &edges);

    Ok(IrFragment {
        nodes,
        edges,
        affinities: Vec::new(),
        containers: assignment_containers(files, root),
        visibility_scopes,
    })
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use strata_ir::{
        ContainerId, ContainerTree, Edge, EdgeKind, Hardness, Node, NodeId, NodeKind, Polarity,
        ScopeLevel,
    };

    use super::assignment::NodeAssignment;
    use super::containers::assignment_containers;
    use super::edges::dedup_edges;
    use super::polarity::classify_polarity;
    use crate::parse::{DeclKind, Declaration, ParsedFile, VisibilityKind};
    use std::path::Path;

    fn declaration(name: &str, start: u32, end: u32) -> Declaration {
        Declaration {
            name: SmolStr::new(name),
            kind: DeclKind::Symbol,
            exported: true,
            visibility_kind: VisibilityKind::Legacy,
            visibility_path: None,
            cfg_test: false,
            test_case: false,
            byte_start: start,
            byte_end: end,
            sloc: 1,
            references: Vec::new(),
        }
    }

    fn file_with(path: &str, declarations: Vec<Declaration>) -> ParsedFile {
        ParsedFile {
            path: SmolStr::new(path),
            declarations,
            re_exports: Vec::new(),
        }
    }

    #[test]
    fn should_assign_dense_node_ids_in_order() {
        let files = [file_with(
            "crates/app/src/lib.rs",
            vec![declaration("a", 0, 10), declaration("b", 10, 20)],
        )];

        let assignment = NodeAssignment::build(&files, Path::new("repo"));
        let nodes = assignment.into_nodes();

        let ids: Vec<u32> = nodes.iter().map(|node| node.id.0).collect();
        assert_eq!(ids, vec![0, 1]);
    }

    #[test]
    fn should_not_let_a_same_stem_file_in_another_crate_name_a_module() {
        let files = [
            file_with("crates/app/src/lib.rs", vec![declaration("a", 0, 10)]),
            file_with("crates/other/src/render.rs", vec![declaration("b", 0, 10)]),
        ];

        let assignment = NodeAssignment::build(&files, Path::new("repo"));

        assert!(
            !assignment.names_a_module("crates/app/src/lib.rs", "render"),
            "`render.rs` lives in another crate and cannot be `app`'s module"
        );
        assert!(
            assignment.names_a_module("crates/other/src/lib.rs", "render"),
            "a sibling file in the same crate does name the module"
        );
    }

    #[test]
    fn should_name_a_module_by_its_mod_rs_directory_within_the_crate() {
        let files = [
            file_with("crates/app/src/lib.rs", vec![declaration("a", 0, 10)]),
            file_with(
                "crates/app/src/render/mod.rs",
                vec![declaration("b", 0, 10)],
            ),
        ];

        let assignment = NodeAssignment::build(&files, Path::new("repo"));

        assert!(assignment.names_a_module("crates/app/src/lib.rs", "render"));
        assert!(!assignment.names_a_module("crates/app/src/lib.rs", "mod"));
    }

    #[test]
    fn should_map_an_offset_to_the_innermost_declaration() {
        let files = [file_with(
            "crates/app/src/lib.rs",
            vec![declaration("outer", 0, 100), declaration("inner", 40, 60)],
        )];

        let assignment = NodeAssignment::build(&files, Path::new("repo"));

        // 50 sits inside both; the innermost (latest start) wins.
        assert_eq!(
            assignment.node_at("crates/app/src/lib.rs", 50),
            Some(NodeId(1))
        );
        // 10 sits only inside the outer declaration.
        assert_eq!(
            assignment.node_at("crates/app/src/lib.rs", 10),
            Some(NodeId(0))
        );
    }

    #[test]
    fn should_return_none_for_an_offset_outside_every_declaration() {
        let files = [file_with(
            "crates/app/src/lib.rs",
            vec![declaration("a", 0, 10)],
        )];

        let assignment = NodeAssignment::build(&files, Path::new("repo"));

        assert_eq!(assignment.node_at("crates/app/src/lib.rs", 99), None);
    }

    #[test]
    fn should_classify_a_test_attributed_declaration_as_a_test_case() {
        let mut decl = declaration("t", 0, 10);
        decl.cfg_test = true;
        decl.test_case = true;
        let files = [file_with("crates/app/src/lib.rs", vec![decl])];

        let assignment = NodeAssignment::build(&files, Path::new("repo"));
        let nodes = assignment.into_nodes();

        assert_eq!(
            nodes.first().map(|node| node.polarity),
            Some(Polarity::TestCase)
        );
    }

    #[test]
    fn should_classify_a_cfg_test_non_test_declaration_as_test_support() {
        let mut decl = declaration("fixture", 0, 10);
        decl.cfg_test = true;
        let files = [file_with("crates/app/src/lib.rs", vec![decl])];

        let assignment = NodeAssignment::build(&files, Path::new("repo"));
        let nodes = assignment.into_nodes();

        assert_eq!(
            nodes.first().map(|node| node.polarity),
            Some(Polarity::TestSupport)
        );
    }

    #[test]
    fn should_build_a_strictly_ascending_container_chain() {
        let files = [file_with(
            "crates/app/src/lib.rs",
            vec![declaration("a", 0, 10)],
        )];

        let containers = assignment_containers(&files, Path::new("repo"));
        let tree = ContainerTree::new(containers);

        assert_eq!(tree.validate(), Ok(()));
    }

    /// A private (file-visible) node consumed only by a test-case node is
    /// reclassified as test support; an edge is the consumption signal.
    #[test]
    fn should_classify_a_private_helper_used_only_by_tests_as_test_support() {
        let helper = Node {
            id: NodeId(0),
            name: SmolStr::new("helper"),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
        };
        let test_case = Node {
            id: NodeId(1),
            name: SmolStr::new("a_test"),
            kind: NodeKind::Symbol,
            polarity: Polarity::TestCase,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 0,
            re_export: false,
        };
        let mut nodes = vec![helper, test_case];
        let edges = vec![Edge {
            source: NodeId(1),
            target: NodeId(0),
            kind: EdgeKind::Call,
            hardness: Hardness::Hard,
            confidence: 1.0,
        }];

        classify_polarity(&[], &mut nodes, &edges);

        assert_eq!(
            nodes.first().map(|node| node.polarity),
            Some(Polarity::TestSupport)
        );
    }

    /// A helper also reachable from production stays production, even if a test
    /// uses it too — production reachability dominates.
    #[test]
    fn should_keep_a_helper_used_by_production_as_production() {
        let helper = Node {
            id: NodeId(0),
            name: SmolStr::new("helper"),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
        };
        let producer = Node {
            id: NodeId(1),
            name: SmolStr::new("run"),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
        };
        let test_case = Node {
            id: NodeId(2),
            name: SmolStr::new("a_test"),
            kind: NodeKind::Symbol,
            polarity: Polarity::TestCase,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 0,
            re_export: false,
        };
        let mut nodes = vec![helper, producer, test_case];
        let edges = vec![
            Edge {
                source: NodeId(1),
                target: NodeId(0),
                kind: EdgeKind::Call,
                hardness: Hardness::Hard,
                confidence: 1.0,
            },
            Edge {
                source: NodeId(2),
                target: NodeId(0),
                kind: EdgeKind::Call,
                hardness: Hardness::Hard,
                confidence: 1.0,
            },
        ];

        classify_polarity(&[], &mut nodes, &edges);

        assert_eq!(
            nodes.first().map(|node| node.polarity),
            Some(Polarity::Production)
        );
    }

    /// A `tests/`-directory declaration is promoted to a test case by path
    /// convention, independent of any `#[cfg(test)]` attribute.
    #[test]
    fn should_promote_a_tests_directory_declaration_to_a_test_case() {
        let files = [file_with(
            "crates/app/tests/it.rs",
            vec![declaration("scenario", 0, 10)],
        )];
        let assignment = NodeAssignment::build(&files, Path::new("repo"));
        let mut nodes = assignment.into_nodes();

        classify_polarity(&files, &mut nodes, &[]);

        assert_eq!(
            nodes.first().map(|node| node.polarity),
            Some(Polarity::TestCase)
        );
    }

    /// A `pub use` re-export materializes a barrel-local node that participates in
    /// the node set even before its target is resolved, and is flagged as a
    /// re-export (ADR-20) whether or not a target ever resolves.
    #[test]
    fn should_materialize_a_barrel_node_for_a_pub_use_re_export() {
        let file = ParsedFile {
            path: SmolStr::new("crates/app/src/lib.rs"),
            declarations: Vec::new(),
            re_exports: vec![crate::parse::ReExport {
                name: SmolStr::new("Measure"),
                visibility_kind: VisibilityKind::Public,
                visibility_path: None,
                offset: 0,
            }],
        };
        let assignment = NodeAssignment::build(&[file], Path::new("repo"));

        assert_eq!(assignment.re_export_links.len(), 1);
        let nodes = assignment.into_nodes();
        assert_eq!(
            nodes.first().map(|node| node.name.clone()),
            Some(SmolStr::new("Measure"))
        );
        assert_eq!(
            nodes.first().map(|node| node.visibility),
            Some(ScopeLevel::Package)
        );
        assert_eq!(nodes.first().map(|node| node.re_export), Some(true));
    }

    #[test]
    fn should_not_flag_a_declaration_as_a_re_export() {
        let files = [file_with(
            "crates/a/src/lib.rs",
            vec![declaration("unique", 0, 10)],
        )];
        let nodes = NodeAssignment::build(&files, Path::new("repo")).into_nodes();

        assert_eq!(nodes.first().map(|node| node.re_export), Some(false));
    }

    #[test]
    fn should_resolve_a_uniquely_named_declaration_by_name() {
        let files = [
            file_with("crates/a/src/lib.rs", vec![declaration("unique", 0, 10)]),
            file_with("crates/b/src/lib.rs", vec![declaration("other", 0, 10)]),
        ];
        let assignment = NodeAssignment::build(&files, Path::new("repo"));

        // a name carried by exactly one declaration resolves to its node.
        assert_eq!(assignment.unique_node_named("unique"), Some(NodeId(0)));
        // an absent name resolves to nothing.
        assert_eq!(assignment.unique_node_named("missing"), None);
    }

    #[test]
    fn should_not_resolve_an_ambiguous_name_by_name() {
        let files = [
            file_with("crates/a/src/lib.rs", vec![declaration("build", 0, 10)]),
            file_with("crates/b/src/lib.rs", vec![declaration("build", 0, 10)]),
        ];
        let assignment = NodeAssignment::build(&files, Path::new("repo"));

        // two declarations share the name, so the fallback declines to guess.
        assert_eq!(assignment.unique_node_named("build"), None);
    }

    #[test]
    fn should_keep_the_strongest_of_duplicate_edges() {
        let edges = vec![
            Edge {
                source: NodeId(0),
                target: NodeId(1),
                kind: EdgeKind::Call,
                hardness: Hardness::Soft,
                confidence: 0.6,
            },
            Edge {
                source: NodeId(0),
                target: NodeId(1),
                kind: EdgeKind::Call,
                hardness: Hardness::Hard,
                confidence: 1.0,
            },
        ];

        let deduped = dedup_edges(edges);

        assert_eq!(deduped.len(), 1);
        assert_eq!(
            deduped.first().map(|edge| edge.hardness),
            Some(Hardness::Hard)
        );
    }
}
