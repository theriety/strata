//! Module-graph resolution and IR-fragment emission.
//!
//! Binding turns the per-file [`ParsedModule`] summaries from [`crate::parse`]
//! into a language-agnostic [`IrFragment`]: a dense node per parsed program
//! entity, a laminar container tree over files/folders/domains/packages, typed
//! dependency edges, and three-valued test polarity.
//!
//! Module specifiers resolve in the order **relative path -> Node.js subpath
//! import -> `tsconfig` `paths` alias -> package entry point**. References that
//! cannot be resolved statically (dynamic `import('...')`) become low-confidence
//! edges rather than being dropped.

mod containers;
mod emission;
mod resolution;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::{IrFragment, Node, NodeId, NodeKind, Polarity, ScopeLevel};

use crate::parse::{DeclarationKind, ParsedModule};

use containers::ContainerBuilder;
use emission::{
    apply_polarity, assign_re_export_nodes, classify_polarity, emit_companion_affinities,
    emit_edges, emit_re_exports,
};
use resolution::Resolver;

/// Per-module, per-name lookup of the node a declaration was assigned.
type ExportTable = HashMap<SmolStr, HashMap<SmolStr, NodeId>>;

/// Resolves the module graph for `modules` and emits a single [`IrFragment`].
///
/// `root` is the repository root the module paths are relative to; it anchors
/// `tsconfig` alias resolution and package-entry lookup.
///
/// # Errors
///
/// This binder never fails on unresolved references — they are emitted as
/// low-confidence or dropped per the spec — so it currently returns `Ok` for
/// every well-formed parse. The `Result` preserves the adapter contract.
pub fn bind(
    modules: &[ParsedModule],
    root: &Path,
    aliases: &BTreeMap<SmolStr, SmolStr>,
    subpath_imports: &BTreeMap<SmolStr, SmolStr>,
) -> Result<IrFragment, BindOutcome> {
    let resolver = Resolver::new(modules, aliases.clone(), subpath_imports.clone());

    // Assign a dense node id to every parsed entity, in module-then-parser order.
    let mut nodes = Vec::new();
    let mut exported = Vec::new();
    let mut exports: ExportTable = HashMap::new();
    let mut local: HashMap<SmolStr, HashMap<SmolStr, NodeId>> = HashMap::new();
    let containers = ContainerBuilder::build(modules, root);

    for module in modules {
        let container = containers.file_of(&module.path);
        let module_local = local.entry(module.path.clone()).or_default();
        let module_exports = exports.entry(module.path.clone()).or_default();
        for declaration in &module.declarations {
            let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
            nodes.push(Node {
                id,
                name: declaration.name.clone(),
                kind: match declaration.kind {
                    DeclarationKind::Symbol => NodeKind::Symbol,
                    DeclarationKind::Type => NodeKind::Type,
                    DeclarationKind::FileBody => NodeKind::FileBody,
                },
                polarity: Polarity::Production,
                container,
                visibility: ScopeLevel::File,
                effective_size: declaration.sloc,
                re_export: false,
            });
            exported.push(declaration.exported);
            module_local.insert(declaration.name.clone(), id);
            if declaration.exported {
                module_exports.insert(declaration.name.clone(), id);
            }
        }
    }

    // A re-export (`export { x } from '...'`) binds `x` in the barrel module even
    // though the barrel has no declaration of its own. Materialize a node for
    // each such binding (after the declaration pass, so target export tables are
    // populated) so the re-export has a real source node and importers of the
    // barrel resolve through it. Self-named re-exports of a local declaration
    // already have a node and are skipped.
    let re_export_links = assign_re_export_nodes(
        modules,
        &resolver,
        &containers,
        &mut nodes,
        &mut exported,
        &mut exports,
        &mut local,
    );

    let mut edges = emit_edges(modules, &resolver, &exports, &local);
    let affinities = emit_companion_affinities(
        modules,
        &resolver,
        &exports,
        &local,
        &nodes,
        &re_export_links,
    );
    emit_re_exports(&re_export_links, &mut edges);
    let polarity = classify_polarity(modules, &nodes, &local, &exported, &edges);
    apply_polarity(&mut nodes, &polarity);

    Ok(IrFragment {
        nodes,
        edges,
        affinities,
        containers: containers.tree.containers().to_vec(),
        visibility_scopes: Vec::new(),
    })
}

/// A binding failure. Reserved for future resolution errors; the binder is
/// currently infallible, so this enum is never constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {}

impl std::fmt::Display for BindOutcome {
    fn fmt(&self, _formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}

impl std::error::Error for BindOutcome {}

#[cfg(test)]
mod tests {
    use strata_ir::{Edge, EdgeKind, Hardness};

    use super::containers::prefix_key;
    use super::emission::{CONFIDENCE_STATIC, is_test_path};
    use super::resolution::normalize_join;
    use super::*;

    /// Builds an empty module summary at `path` for resolver tests.
    fn module_at(path: &str) -> ParsedModule {
        ParsedModule {
            path: SmolStr::new(path),
            declarations: Vec::new(),
            imports: Vec::new(),
            re_exports: Vec::new(),
        }
    }

    /// Builds an exported, non-type declaration that calls the given names.
    fn caller(name: &str, called: &[&str]) -> crate::parse::Declaration {
        crate::parse::Declaration {
            name: SmolStr::new(name),
            kind: DeclarationKind::Symbol,
            exported: true,
            sloc: 1,
            supertypes: Vec::new(),
            referenced: called.iter().copied().map(SmolStr::new).collect(),
            called: called.iter().copied().map(SmolStr::new).collect(),
            dynamic_imports: Vec::new(),
            signature_companions: Vec::new(),
        }
    }

    #[test]
    fn should_emit_a_hard_call_edge_for_an_invoked_import() {
        let mut consumer = module_at("src/app.ts");
        consumer.imports.push(crate::parse::StaticImport {
            source: SmolStr::new("./target"),
            names: vec![SmolStr::new("target")],
            type_only: false,
        });
        consumer.declarations.push(caller("run", &["target"]));
        let mut provider = module_at("src/target.ts");
        provider.declarations.push(crate::parse::Declaration {
            name: SmolStr::new("target"),
            kind: DeclarationKind::Symbol,
            exported: true,
            sloc: 1,
            supertypes: Vec::new(),
            referenced: Vec::new(),
            called: Vec::new(),
            dynamic_imports: Vec::new(),
            signature_companions: Vec::new(),
        });

        let fragment = bind(
            &[consumer, provider],
            Path::new("repo"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("bind succeeds");

        let calls: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Call)
            .collect();
        assert_eq!(calls.len(), 1, "exactly one call edge expected");
        let hardness = calls.first().map(|edge| edge.hardness);
        let confidence = calls.first().map(|edge| edge.confidence);
        assert_eq!(hardness, Some(Hardness::Hard));
        assert_eq!(
            confidence.map(f64::to_bits),
            Some(CONFIDENCE_STATIC.to_bits())
        );
        // The invoked import surfaces only as a call, never a value-import.
        assert!(
            fragment
                .edges
                .iter()
                .all(|edge| edge.kind != EdgeKind::ValueImport),
            "an invoked import must not also be a value-import"
        );
    }

    /// Builds an exported declaration that references names without calling them.
    fn referencer(name: &str, is_type: bool, referenced: &[&str]) -> crate::parse::Declaration {
        crate::parse::Declaration {
            name: SmolStr::new(name),
            kind: if is_type {
                DeclarationKind::Type
            } else {
                DeclarationKind::Symbol
            },
            exported: true,
            sloc: 1,
            supertypes: Vec::new(),
            referenced: referenced.iter().copied().map(SmolStr::new).collect(),
            called: Vec::new(),
            dynamic_imports: Vec::new(),
            signature_companions: Vec::new(),
        }
    }

    #[test]
    fn should_emit_a_value_edge_for_a_same_module_reference() {
        let mut module = module_at("src/codec.ts");
        module
            .declarations
            .push(referencer("encode", false, &["LIMIT"]));
        module.declarations.push(referencer("LIMIT", false, &[]));

        let fragment = bind(
            &[module],
            Path::new("repo"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("bind succeeds");

        let values: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::ValueImport)
            .collect();
        assert_eq!(
            values.len(),
            1,
            "a same-module value reference is a dependency the graph must carry"
        );
        assert_eq!(
            values.first().map(|edge| edge.hardness),
            Some(Hardness::Hard)
        );
    }

    #[test]
    fn should_emit_a_type_edge_for_a_same_module_type_reference() {
        let mut module = module_at("src/tools.ts");
        module
            .declarations
            .push(referencer("build", false, &["Request"]));
        module.declarations.push(referencer("Request", true, &[]));

        let fragment = bind(
            &[module],
            Path::new("repo"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("bind succeeds");

        let types: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::TypeReference)
            .collect();
        assert_eq!(
            types.len(),
            1,
            "a same-module type reference is a dependency the graph must carry"
        );
        assert_eq!(
            types.first().map(|edge| edge.hardness),
            Some(Hardness::Soft)
        );
        assert!(
            fragment
                .edges
                .iter()
                .all(|edge| edge.kind != EdgeKind::ValueImport),
            "a type target must not also surface as a value-import"
        );
    }

    #[test]
    fn should_not_emit_a_self_edge_for_a_recursive_declaration() {
        let mut module = module_at("src/walk.ts");
        module
            .declarations
            .push(referencer("walk", false, &["walk"]));

        let fragment = bind(
            &[module],
            Path::new("repo"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("bind succeeds");

        assert!(
            fragment.edges.is_empty(),
            "a declaration naming itself depends on nothing"
        );
    }

    #[test]
    fn should_emit_a_soft_re_export_edge_for_a_barrel_without_a_local_declaration() {
        let mut barrel = module_at("src/index.ts");
        barrel.re_exports.push(crate::parse::ReExport {
            source: SmolStr::new("./widget"),
            names: vec![crate::parse::ReExportBinding::Named {
                original: SmolStr::new("Widget"),
                exported: SmolStr::new("Widget"),
            }],
            type_only: false,
        });
        let mut provider = module_at("src/widget.ts");
        provider.declarations.push(crate::parse::Declaration {
            name: SmolStr::new("Widget"),
            kind: DeclarationKind::Symbol,
            exported: true,
            sloc: 1,
            supertypes: Vec::new(),
            referenced: Vec::new(),
            called: Vec::new(),
            dynamic_imports: Vec::new(),
            signature_companions: Vec::new(),
        });

        let fragment = bind(
            &[barrel, provider],
            Path::new("repo"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("bind succeeds");

        let re_exports: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::ReExport)
            .collect();
        assert_eq!(re_exports.len(), 1, "exactly one re-export edge expected");
        let edge = re_exports.first().copied();
        assert_eq!(edge.map(|edge| edge.hardness), Some(Hardness::Soft));
        assert_eq!(
            edge.map(|edge| edge.confidence.to_bits()),
            Some(CONFIDENCE_STATIC.to_bits())
        );
        // The barrel materializes its own node for the re-exported name, flagged
        // as a re-export (ADR-0020), and the edge runs from that node to the
        // original declaration.
        let flag_of = |id: Option<NodeId>| -> Option<bool> {
            id.and_then(|id| fragment.nodes.iter().find(|node| node.id == id))
                .map(|node| node.re_export)
        };
        assert_eq!(flag_of(edge.map(|edge| edge.source)), Some(true));
        assert_eq!(flag_of(edge.map(|edge| edge.target)), Some(false));
        let name_of = |id: Option<NodeId>| -> Option<SmolStr> {
            id.and_then(|id| fragment.nodes.iter().find(|node| node.id == id))
                .map(|node| node.name.clone())
        };
        assert_eq!(
            name_of(edge.map(|edge| edge.source)),
            Some(SmolStr::new("Widget"))
        );
        assert_eq!(
            name_of(edge.map(|edge| edge.target)),
            Some(SmolStr::new("Widget"))
        );
        assert_ne!(
            edge.map(|edge| edge.source),
            edge.map(|edge| edge.target),
            "barrel binding is a distinct node"
        );
    }

    #[test]
    fn should_resolve_a_relative_specifier_against_the_importer_directory() {
        let modules = [module_at("src/app.ts"), module_at("src/geometry/shape.ts")];
        let resolver = Resolver::new(&modules, BTreeMap::new(), BTreeMap::new());

        let resolved = resolver.resolve("src/app.ts", "./geometry/shape");

        assert_eq!(resolved, Some(SmolStr::new("src/geometry/shape.ts")));
    }

    #[test]
    fn should_resolve_a_parent_relative_specifier() {
        let modules = [
            module_at("src/__tests__/support.ts"),
            module_at("src/geometry/rectangle.ts"),
        ];
        let resolver = Resolver::new(&modules, BTreeMap::new(), BTreeMap::new());

        let resolved = resolver.resolve("src/__tests__/support.ts", "../geometry/rectangle");

        assert_eq!(resolved, Some(SmolStr::new("src/geometry/rectangle.ts")));
    }

    #[test]
    fn should_resolve_a_relative_directory_to_its_index_barrel() {
        let modules = [module_at("src/app.ts"), module_at("src/geometry/index.ts")];
        let resolver = Resolver::new(&modules, BTreeMap::new(), BTreeMap::new());

        let resolved = resolver.resolve("src/app.ts", "./geometry");

        assert_eq!(resolved, Some(SmolStr::new("src/geometry/index.ts")));
    }

    #[test]
    fn should_resolve_a_specifier_through_a_tsconfig_alias() {
        let modules = [module_at("src/lib/util.ts"), module_at("src/app.ts")];
        let mut aliases = BTreeMap::new();
        aliases.insert(SmolStr::new("@app/"), SmolStr::new("src/"));
        let resolver = Resolver::new(&modules, aliases, BTreeMap::new());

        let resolved = resolver.resolve("src/app.ts", "@app/lib/util");

        assert_eq!(resolved, Some(SmolStr::new("src/lib/util.ts")));
    }

    #[test]
    fn should_resolve_a_bare_package_specifier_to_its_entry_module() {
        let modules = [
            module_at("packages/core/src/index.ts"),
            module_at("src/app.ts"),
        ];
        let resolver = Resolver::new(&modules, BTreeMap::new(), BTreeMap::new());

        let resolved = resolver.resolve("src/app.ts", "packages/core");

        assert_eq!(resolved, Some(SmolStr::new("packages/core/src/index.ts")));
    }

    #[test]
    fn should_return_none_for_an_unknown_external_specifier() {
        let modules = [module_at("src/app.ts")];
        let resolver = Resolver::new(&modules, BTreeMap::new(), BTreeMap::new());

        assert_eq!(resolver.resolve("src/app.ts", "react"), None);
    }

    #[test]
    fn should_collapse_dot_and_dotdot_segments_when_joining() {
        assert_eq!(normalize_join("src/geometry", "../app"), "src/app");
        assert_eq!(normalize_join("src", "./util"), "src/util");
        assert_eq!(normalize_join("", "./root"), "root");
    }

    #[test]
    fn should_key_containers_by_their_bounded_path_prefix() {
        let segments = ["src", "geometry", "shape.ts"];

        assert_eq!(prefix_key(&segments, 1), SmolStr::new("src"));
        assert_eq!(prefix_key(&segments, 2), SmolStr::new("src/geometry"));
        // A take deeper than the path reuses the full prefix.
        assert_eq!(prefix_key(&["only"], 3), SmolStr::new("only"));
    }

    #[test]
    fn should_recognise_test_paths_by_convention() {
        assert!(is_test_path("src/__tests__/app.spec.ts"));
        assert!(is_test_path("src/app.test.ts"));
        assert!(is_test_path("src/__tests__/support.ts"));
        assert!(!is_test_path("src/app.ts"));
    }

    #[test]
    fn should_recognize_qualified_test_paths_without_matching_words() {
        assert!(is_test_path("src/worker.spec.int.ts"));
        assert!(is_test_path("src/worker.test.integration.ts"));
        assert!(is_test_path("src/worker.spec.browser.tsx"));
        assert!(!is_test_path("src/specification.ts"));
        assert!(!is_test_path("src/worker.testable.tsx"));
    }

    #[test]
    fn should_bind_an_executable_file_body_with_its_semantic_kind_and_sloc() {
        let mut module = module_at("src/entry.ts");
        module.declarations.push(crate::parse::Declaration {
            name: SmolStr::new("<module>"),
            kind: DeclarationKind::FileBody,
            exported: false,
            sloc: 2,
            supertypes: Vec::new(),
            referenced: Vec::new(),
            called: Vec::new(),
            dynamic_imports: Vec::new(),
            signature_companions: Vec::new(),
        });

        let fragment = bind(
            &[module],
            Path::new("repo"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("bind succeeds");
        let body = fragment
            .nodes
            .iter()
            .find(|node| node.name == "<module>")
            .map(|node| (node.kind, node.effective_size));

        assert_eq!(
            body,
            Some((NodeKind::FileBody, 2)),
            "an executable file body must retain its semantic role and attributed SLOC; \
             body {body:?}"
        );
    }

    #[test]
    fn should_emit_an_edge_from_a_module_body_declaration_to_its_twin() {
        let mut spec = module_at("src/subject.spec.ts");
        spec.imports.push(crate::parse::StaticImport {
            source: SmolStr::new("./subject"),
            names: vec![SmolStr::new("executeSubject")],
            type_only: false,
        });
        spec.declarations.push(crate::parse::Declaration {
            name: SmolStr::new("<module>"),
            kind: DeclarationKind::FileBody,
            exported: false,
            sloc: 4,
            supertypes: Vec::new(),
            referenced: vec![SmolStr::new("describe"), SmolStr::new("executeSubject")],
            called: vec![SmolStr::new("describe"), SmolStr::new("executeSubject")],
            dynamic_imports: Vec::new(),
            signature_companions: Vec::new(),
        });
        let mut twin = module_at("src/subject.ts");
        twin.declarations.push(crate::parse::Declaration {
            name: SmolStr::new("executeSubject"),
            kind: DeclarationKind::Symbol,
            exported: true,
            sloc: 3,
            supertypes: Vec::new(),
            referenced: Vec::new(),
            called: Vec::new(),
            dynamic_imports: Vec::new(),
            signature_companions: Vec::new(),
        });

        let fragment = bind(
            &[spec, twin],
            Path::new("repo"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("bind succeeds");

        let body = fragment
            .nodes
            .iter()
            .find(|node| node.name == "<module>")
            .map(|node| node.id);
        let target = fragment
            .nodes
            .iter()
            .find(|node| node.name == "executeSubject")
            .map(|node| node.id);
        let linked = match (body, target) {
            (Some(body), Some(target)) => fragment.edges.iter().any(|edge| {
                edge.source == body && edge.target == target && edge.kind == EdgeKind::Call
            }),
            _ => false,
        };
        assert!(
            linked,
            "the module body's callback reference must couple the spec to its twin"
        );
    }

    #[test]
    fn should_resolve_a_subpath_import_through_an_exact_key() {
        let modules = [module_at("src/app.ts"), module_at("src/agent/schemas.ts")];
        let mut imports = BTreeMap::new();
        imports.insert(
            SmolStr::new("#agent/schemas"),
            SmolStr::new("./src/agent/schemas"),
        );
        let resolver = Resolver::new(&modules, BTreeMap::new(), imports);

        let resolved = resolver.resolve("src/app.ts", "#agent/schemas");

        assert_eq!(resolved, Some(SmolStr::new("src/agent/schemas.ts")));
    }

    #[test]
    fn should_resolve_a_subpath_import_through_a_star_pattern() {
        let modules = [
            module_at("src/app.ts"),
            module_at("src/adapters/openai/index.ts"),
        ];
        let mut imports = BTreeMap::new();
        imports.insert(
            SmolStr::new("#adapters/*"),
            SmolStr::new("./src/adapters/*"),
        );
        let resolver = Resolver::new(&modules, BTreeMap::new(), imports);

        let resolved = resolver.resolve("src/app.ts", "#adapters/openai");

        assert_eq!(resolved, Some(SmolStr::new("src/adapters/openai/index.ts")),);
    }

    #[test]
    fn should_return_none_for_a_subpath_import_without_a_matching_key() {
        let modules = [module_at("src/app.ts")];
        let mut imports = BTreeMap::new();
        imports.insert(SmolStr::new("#agent/*"), SmolStr::new("./src/agent/*"));
        let resolver = Resolver::new(&modules, BTreeMap::new(), imports);

        assert_eq!(resolver.resolve("src/app.ts", "#other/thing"), None);
    }
}
