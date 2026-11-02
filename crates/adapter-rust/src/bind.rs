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

mod advisory;
mod assignment;
mod containers;
mod database;
mod dropped_use;
mod edges;
mod fallback;
mod file_facts;
mod polarity;
mod public_api;
mod visibility;

use std::path::Path;

use strata_ir::IrFragment;

use self::assignment::NodeAssignment;
use self::containers::assignment_containers;
use self::database::Database;
use self::edges::{dedup_edges, resolve_edges, resolve_re_exports};
use self::polarity::classify_polarity;
use self::public_api::public_api_nodes;
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
    let (mut edges, mut exposures) = resolve_edges(files, &assignment, &database, workspace_root);
    // a re-export reaches as far as it is declared, so its target must too.
    exposures.extend(resolve_re_exports(
        &assignment,
        &database,
        workspace_root,
        &mut edges,
    ));
    let visibility_scopes = resolve_visibility_scopes(files, &assignment, &database, root);
    let (public_api, loaded) = public_api_nodes(files, &assignment, &database, root);
    // a file the loader never reached is one maybe-dropped body (ADR-25 rule 7).
    let unloaded: std::collections::HashSet<_> = files
        .iter()
        .filter(|file| !loaded.contains(file.path.as_str()))
        .map(|file| file.path.clone())
        .collect();
    let edges = dedup_edges(edges);
    let advisory = advisory::candidates(files, &assignment, &unloaded);
    let exempt: std::collections::HashSet<_> =
        dropped_use::exempt_nodes(files, &assignment, &unloaded)
            .into_iter()
            .collect();
    let mut nodes = assignment.into_nodes();
    link_owners(&mut nodes);
    for node in &mut nodes {
        node.public_api = public_api.contains(&node.id);
        node.advice_exempt |= exempt.contains(&node.id);
    }
    classify_polarity(files, &mut nodes, &edges);
    let advisory_uses = advisory::settle(advisory, &nodes);
    let file_facts = file_facts::file_facts(files, &database, root);
    let test_support_roots = database.test_support_roots(root);

    Ok(IrFragment {
        nodes,
        edges,
        affinities: Vec::new(),
        containers: assignment_containers(files, root),
        visibility_scopes,
        exposures,
        advisory_uses,
        file_facts,
        test_support_roots,
    })
}

/// Points each inherent member (`Type::member`) at the type node of the same
/// file; an ambiguous or missing owner leaves the member without one.
fn link_owners(nodes: &mut [strata_ir::Node]) {
    let types: Vec<(smol_str::SmolStr, strata_ir::ContainerId, strata_ir::NodeId)> = nodes
        .iter()
        .filter(|node| node.kind == strata_ir::NodeKind::Type && !node.re_export)
        .map(|node| (node.name.clone(), node.container, node.id))
        .collect();
    for node in nodes.iter_mut() {
        let Some((owner, _)) = node
            .display_name
            .as_deref()
            .and_then(|shown| shown.split_once("::"))
        else {
            continue;
        };
        let mut found = types
            .iter()
            .filter(|(name, container, _)| name == owner && *container == node.container);
        if let (Some((_, _, id)), None) = (found.next(), found.next()) {
            node.owner = Some(*id);
        }
    }
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
            owner: None,
            references: Vec::new(),
            advisory_names: Vec::new(),
            trait_impl: false,
        }
    }

    fn file_with(path: &str, declarations: Vec<Declaration>) -> ParsedFile {
        ParsedFile {
            path: SmolStr::new(path),
            declarations,
            re_exports: Vec::new(),
            names: Vec::new(),
            dropped_uses: Vec::new(),
            dropped_re_exports: Vec::new(),
            dropped_globs: Vec::new(),
            dropped_glob_files: Vec::new(),
            test_dropped_uses: Vec::new(),
            pub_use: false,
            test_of: None,
            root_main: false,
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
    fn should_give_an_impl_method_the_display_name_type_method_and_keep_its_name() {
        let mut method = declaration("new", 0, 10);
        method.owner = Some(SmolStr::new("Interner"));
        let files = [file_with(
            "crates/app/src/lib.rs",
            vec![method, declaration("free", 10, 20)],
        )];

        let nodes = NodeAssignment::build(&files, Path::new("repo")).into_nodes();

        let shown: Vec<_> = nodes
            .iter()
            .map(|node| (node.name.as_str(), node.display_name.as_deref()))
            .collect();
        assert_eq!(shown, vec![("new", Some("Interner::new")), ("free", None)]);
    }

    #[test]
    fn should_link_an_impl_method_to_the_type_of_its_file_and_only_when_unambiguous() {
        let mut ty = declaration("Interner", 0, 10);
        ty.kind = DeclKind::Type;
        let mut method = declaration("new", 10, 20);
        method.owner = Some(SmolStr::new("Interner"));
        let files = [file_with("crates/app/src/lib.rs", vec![ty, method])];
        let mut nodes = NodeAssignment::build(&files, Path::new("repo")).into_nodes();
        super::link_owners(&mut nodes);
        let owners: Vec<_> = nodes.iter().map(|node| node.owner.map(|id| id.0)).collect();
        assert_eq!(owners, vec![None, Some(0)]);

        let mut orphan = declaration("new", 0, 10);
        orphan.owner = Some(SmolStr::new("Missing"));
        let lone = [file_with("crates/app/src/lib.rs", vec![orphan])];
        let mut lone_nodes = NodeAssignment::build(&lone, Path::new("repo")).into_nodes();
        super::link_owners(&mut lone_nodes);
        assert_eq!(
            lone_nodes.iter().map(|node| node.owner).collect::<Vec<_>>(),
            vec![None]
        );
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
            public_api: false,
            advice_exempt: false,
            id: NodeId(0),
            name: SmolStr::new("helper"),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
            display_name: None,
            test_source: false,
            owner: None,
        };
        let test_case = Node {
            public_api: false,
            advice_exempt: false,
            id: NodeId(1),
            name: SmolStr::new("a_test"),
            kind: NodeKind::Symbol,
            polarity: Polarity::TestCase,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 0,
            re_export: false,
            display_name: None,
            test_source: false,
            owner: None,
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
        // demoted production is not test code in source; the test case is.
        assert_eq!(
            nodes
                .iter()
                .map(|node| node.test_source)
                .collect::<Vec<_>>(),
            [false, true]
        );
    }

    /// A helper also reachable from production stays production, even if a test
    /// uses it too — production reachability dominates.
    #[test]
    fn should_keep_a_helper_used_by_production_as_production() {
        let helper = Node {
            public_api: false,
            advice_exempt: false,
            id: NodeId(0),
            name: SmolStr::new("helper"),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
            display_name: None,
            test_source: false,
            owner: None,
        };
        let producer = Node {
            public_api: false,
            advice_exempt: false,
            id: NodeId(1),
            name: SmolStr::new("run"),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
            display_name: None,
            test_source: false,
            owner: None,
        };
        let test_case = Node {
            public_api: false,
            advice_exempt: false,
            id: NodeId(2),
            name: SmolStr::new("a_test"),
            kind: NodeKind::Symbol,
            polarity: Polarity::TestCase,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 0,
            re_export: false,
            display_name: None,
            test_source: false,
            owner: None,
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
                cfg_test: false,
            }],
            names: Vec::new(),
            dropped_uses: Vec::new(),
            dropped_re_exports: Vec::new(),
            dropped_globs: Vec::new(),
            dropped_glob_files: Vec::new(),
            test_dropped_uses: Vec::new(),
            pub_use: false,
            test_of: None,
            root_main: false,
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
