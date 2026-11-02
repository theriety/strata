//! Module-graph resolution and IR-fragment emission for Python.
//!
//! Binding turns the per-file [`ParsedModule`] summaries from [`crate::parse`]
//! into a language-agnostic [`IrFragment`]: a dense node per top-level symbol, a
//! laminar container tree over files/folders/domains/packages, typed dependency
//! edges, and three-valued test polarity.
//!
//! Python has no off-the-shelf semantic database, so resolution is a custom
//! binder. Each file maps to a dotted module name (`pkg/sub/mod.py` ->
//! `pkg.sub.mod`; `pkg/__init__.py` -> `pkg`). Absolute imports resolve against
//! that dotted-name table; relative imports (`from . import x`) resolve against
//! the importing module's package. `__init__.py` is the folder barrel — its
//! re-exports are emitted as-is. Python's dynamic surface (`getattr`,
//! `importlib`, star imports through `__all__`) is recorded as honest edges
//! below full confidence rather than dropped.

mod containers;
mod dynamic;
mod edges;
mod polarity;
mod reexport;
mod resolver;

use std::collections::HashMap;
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::{
    Edge, EdgeKind, Hardness, IrFragment, Node, NodeId, NodeKind, Polarity, ScopeLevel,
};

use self::containers::ContainerBuilder;
use self::edges::emit_edges;
use self::polarity::{apply_polarity, classify_polarity};
use self::reexport::{assign_re_export_nodes, emit_re_exports};
use self::resolver::Resolver;
use crate::parse::ParsedModule;

/// Per-module, per-name lookup of the node a declaration was assigned.
type ExportTable = HashMap<SmolStr, HashMap<SmolStr, NodeId>>;

/// Confidence assigned to a statically resolved edge.
const CONFIDENCE_STATIC: f64 = 1.0;

/// Confidence assigned to a dynamic edge (`getattr`, `importlib`, star import).
const CONFIDENCE_DYNAMIC: f64 = 0.5;

/// Resolves the module graph for `modules` and emits a single [`IrFragment`].
///
/// `root` is the repository root the module paths are relative to; it anchors
/// the package container at the top of the laminar tree.
pub fn bind(modules: &[ParsedModule], root: &Path) -> IrFragment {
    let resolver = Resolver::new(modules);
    let containers = ContainerBuilder::build(modules, root);

    let mut nodes = Vec::new();
    let mut exported = Vec::new();
    let mut exports: ExportTable = HashMap::new();
    let mut local: HashMap<SmolStr, HashMap<SmolStr, NodeId>> = HashMap::new();

    for module in modules {
        let container = containers.file_of(&module.path);
        let module_local = local.entry(module.path.clone()).or_default();
        let module_exports = exports.entry(module.path.clone()).or_default();
        for declaration in &module.declarations {
            let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
            nodes.push(Node {
                id,
                name: declaration.name.clone(),
                kind: if declaration.is_class {
                    NodeKind::Type
                } else {
                    NodeKind::Symbol
                },
                polarity: Polarity::Production,
                container,
                visibility: ScopeLevel::File,
                effective_size: declaration.sloc,
                re_export: false,
            });
            // Python has no `export` keyword: a module-level definition not
            // prefixed with `_` is part of the importable surface.
            exported.push(is_public(&declaration.name));
            module_local.insert(declaration.name.clone(), id);
            module_exports.insert(declaration.name.clone(), id);
        }
    }

    // `__init__.py` is the folder barrel: a `from .mod import x` in it re-exports
    // `x` even though the package never declares it. Materialize a node for each
    // such binding so importers of the package resolve through it.
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
    emit_re_exports(&re_export_links, &mut edges);
    let polarity = classify_polarity(modules, &nodes, &local, &exported, &edges);
    apply_polarity(&mut nodes, &polarity);

    IrFragment {
        nodes,
        edges,
        affinities: Vec::new(),
        containers: containers.tree.containers().to_vec(),
        visibility_scopes: Vec::new(),
    }
}

/// Appends an edge, skipping self-loops which carry no dependency information.
fn push_edge(
    edges: &mut Vec<Edge>,
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
    hardness: Hardness,
    confidence: f64,
) {
    if source == target {
        return;
    }
    edges.push(Edge {
        source,
        target,
        kind,
        hardness,
        confidence,
    });
}

/// Returns `true` if a name is part of the importable surface (no `_` prefix).
fn is_public(name: &str) -> bool {
    !name.starts_with('_')
}

#[cfg(test)]
mod tests {
    use strata_ir::{Edge, EdgeKind, Hardness};

    use super::*;
    use crate::parse::{Declaration, DynamicRef, Import};

    /// Builds an empty module summary at `path`.
    fn module_at(path: &str) -> ParsedModule {
        ParsedModule {
            path: SmolStr::new(path),
            declarations: Vec::new(),
            imports: Vec::new(),
            dunder_all: Vec::new(),
        }
    }

    /// Builds a public function declaration calling the given names.
    fn caller(name: &str, called: &[&str]) -> Declaration {
        Declaration {
            name: SmolStr::new(name),
            is_class: false,
            sloc: 1,
            bases: Vec::new(),
            annotations: Vec::new(),
            referenced: called.iter().copied().map(SmolStr::new).collect(),
            called: called.iter().copied().map(SmolStr::new).collect(),
            dynamic: Vec::new(),
        }
    }

    /// Builds a public, leaf function declaration named `name`.
    fn leaf(name: &str) -> Declaration {
        Declaration {
            name: SmolStr::new(name),
            is_class: false,
            sloc: 1,
            bases: Vec::new(),
            annotations: Vec::new(),
            referenced: Vec::new(),
            called: Vec::new(),
            dynamic: Vec::new(),
        }
    }

    /// Builds a leaf declaration that reads the given names without calling
    /// them (the value-import shape of a constant consumer).
    fn reader(name: &str, reads: &[&str]) -> Declaration {
        Declaration {
            name: SmolStr::new(name),
            is_class: false,
            sloc: 1,
            bases: Vec::new(),
            annotations: Vec::new(),
            referenced: reads.iter().copied().map(SmolStr::new).collect(),
            called: Vec::new(),
            dynamic: Vec::new(),
        }
    }

    #[test]
    fn should_emit_a_hard_call_edge_for_an_invoked_absolute_import() {
        let mut consumer = module_at("pkg/app.py");
        consumer.imports.push(Import {
            module: SmolStr::new("pkg.target"),
            level: 0,
            names: vec![SmolStr::new("run_target")],
            targets: vec![SmolStr::new("run_target")],
            star: false,
        });
        consumer.declarations.push(caller("run", &["run_target"]));
        let mut provider = module_at("pkg/target.py");
        provider.declarations.push(leaf("run_target"));

        let fragment = bind(&[consumer, provider], Path::new("repo"));

        let calls: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Call)
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls.first().map(|edge| edge.hardness),
            Some(Hardness::Hard)
        );
        assert!(
            fragment
                .edges
                .iter()
                .all(|edge| edge.kind != EdgeKind::ValueImport),
            "an invoked import must not also be a value-import"
        );
    }

    #[test]
    fn should_emit_a_value_edge_for_a_same_module_reference() {
        let mut module = module_at("pkg/codec.py");
        module.declarations.push(reader("encode", &["LIMIT"]));
        module.declarations.push(leaf("LIMIT"));

        let fragment = bind(&[module], Path::new("repo"));

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
    fn should_not_emit_a_self_edge_for_a_recursive_declaration() {
        let mut module = module_at("pkg/walk.py");
        module.declarations.push(reader("walk", &["walk"]));

        let fragment = bind(&[module], Path::new("repo"));

        assert!(
            fragment.edges.is_empty(),
            "a declaration naming itself depends on nothing"
        );
    }

    #[test]
    fn should_bind_a_private_constant_import_to_its_reader() {
        let mut provider = module_at("pkg/charge.py");
        // A `_`-prefixed constant is private to the surface but still a
        // module-level binding an in-package sibling may import.
        let ledger = leaf("_ledger");
        provider.declarations.push(ledger);
        let mut consumer = module_at("pkg/refund.py");
        consumer.imports.push(Import {
            module: SmolStr::new("charge"),
            level: 1,
            names: vec![SmolStr::new("_ledger")],
            targets: vec![SmolStr::new("_ledger")],
            star: false,
        });
        consumer.declarations.push(reader("refund", &["_ledger"]));

        let fragment = bind(&[consumer, provider], Path::new("repo"));

        let value_imports: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::ValueImport)
            .collect();
        assert_eq!(value_imports.len(), 1, "the constant import prices an edge");
        assert_eq!(
            value_imports
                .first()
                .map(|edge| (edge.hardness, edge.confidence)),
            Some((Hardness::Hard, CONFIDENCE_STATIC))
        );
    }

    #[test]
    fn should_price_a_constant_initializer_call_to_an_imported_builder() {
        let mut holder = module_at("pkg/config.py");
        holder.imports.push(Import {
            module: SmolStr::new("pkg.build"),
            level: 0,
            names: vec![SmolStr::new("build")],
            targets: vec![SmolStr::new("build")],
            star: false,
        });
        let mut cache = leaf("_cache");
        cache.called = vec![SmolStr::new("build")];
        holder.declarations.push(cache);
        let mut builder = module_at("pkg/build.py");
        builder.declarations.push(leaf("build"));

        let fragment = bind(&[holder, builder], Path::new("repo"));

        let calls: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Call)
            .collect();
        assert_eq!(calls.len(), 1, "the initializer's call prices an edge");
    }

    #[test]
    fn should_resolve_a_relative_import_against_the_importer_package() {
        let mut consumer = module_at("pkg/sub/app.py");
        consumer.imports.push(Import {
            module: SmolStr::new("util"),
            level: 2,
            names: vec![SmolStr::new("helper")],
            targets: vec![SmolStr::new("helper")],
            star: false,
        });
        consumer.declarations.push(caller("run", &["helper"]));
        let mut provider = module_at("pkg/util.py");
        provider.declarations.push(leaf("helper"));

        let fragment = bind(&[consumer, provider], Path::new("repo"));

        let calls: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Call)
            .collect();
        assert_eq!(calls.len(), 1, "the relative `..util` import resolves");
    }

    #[test]
    fn should_emit_a_soft_re_export_edge_from_a_package_init() {
        let mut barrel = module_at("pkg/__init__.py");
        barrel.imports.push(Import {
            module: SmolStr::new("widget"),
            level: 1,
            names: vec![SmolStr::new("Widget")],
            targets: vec![SmolStr::new("Widget")],
            star: false,
        });
        let mut provider = module_at("pkg/widget.py");
        let mut widget = leaf("Widget");
        widget.is_class = true;
        provider.declarations.push(widget);

        let fragment = bind(&[barrel, provider], Path::new("repo"));

        let re_exports: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::ReExport)
            .collect();
        assert_eq!(re_exports.len(), 1);
        assert_eq!(
            re_exports.first().map(|edge| edge.hardness),
            Some(Hardness::Soft)
        );
        // the barrel binding is flagged as a re-export (ADR-20); the original
        // declaration is not.
        let flag_of = |id: Option<NodeId>| -> Option<bool> {
            id.and_then(|id| fragment.nodes.iter().find(|node| node.id == id))
                .map(|node| node.re_export)
        };
        let edge = re_exports.first().copied();
        assert_eq!(flag_of(edge.map(|edge| edge.source)), Some(true));
        assert_eq!(flag_of(edge.map(|edge| edge.target)), Some(false));
    }

    #[test]
    fn should_emit_a_low_confidence_edge_for_a_star_import() {
        let mut consumer = module_at("pkg/app.py");
        consumer.imports.push(Import {
            module: SmolStr::new("pkg.api"),
            level: 0,
            names: Vec::new(),
            targets: Vec::new(),
            star: true,
        });
        consumer.declarations.push(leaf("run"));
        let mut provider = module_at("pkg/api.py");
        provider.dunder_all = vec![SmolStr::new("public")];
        provider.declarations.push(leaf("public"));
        provider.declarations.push(leaf("hidden"));

        let fragment = bind(&[consumer, provider], Path::new("repo"));

        let dynamic: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.confidence.to_bits() == CONFIDENCE_DYNAMIC.to_bits())
            .collect();
        // `__all__` limits the star surface to `public` only.
        assert_eq!(dynamic.len(), 1, "only the __all__ surface is pulled in");
    }

    #[test]
    fn should_emit_a_low_confidence_edge_for_getattr_on_an_import() {
        let mut consumer = module_at("pkg/app.py");
        consumer.imports.push(Import {
            module: SmolStr::new("pkg.plugins"),
            level: 0,
            names: vec![SmolStr::new("plugins")],
            targets: vec![SmolStr::new("registry")],
            star: false,
        });
        let mut runner = leaf("run");
        runner.dynamic = vec![DynamicRef::GetAttr(SmolStr::new("plugins"))];
        consumer.declarations.push(runner);
        let mut provider = module_at("pkg/plugins.py");
        provider.declarations.push(leaf("registry"));

        let fragment = bind(&[consumer, provider], Path::new("repo"));

        let dynamic: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.confidence.to_bits() == CONFIDENCE_DYNAMIC.to_bits())
            .collect();
        assert_eq!(dynamic.len(), 1);
    }

    #[test]
    fn should_emit_a_low_confidence_edge_for_importlib_import_module() {
        let mut consumer = module_at("pkg/app.py");
        let mut loader = leaf("load");
        loader.dynamic = vec![DynamicRef::ImportModule(SmolStr::new("pkg.plugin"))];
        consumer.declarations.push(loader);
        let mut provider = module_at("pkg/plugin.py");
        provider.dunder_all = vec![SmolStr::new("entry")];
        provider.declarations.push(leaf("entry"));
        provider.declarations.push(leaf("hidden"));

        let fragment = bind(&[consumer, provider], Path::new("repo"));

        let dynamic: Vec<&Edge> = fragment
            .edges
            .iter()
            .filter(|edge| edge.confidence.to_bits() == CONFIDENCE_DYNAMIC.to_bits())
            .collect();
        // The `__all__` surface limits the fan-out to `entry` alone, and the
        // edge is below full confidence.
        assert_eq!(dynamic.len(), 1, "only the __all__ surface is pulled in");
        assert_eq!(
            dynamic.first().map(|edge| edge.kind),
            Some(EdgeKind::ValueImport)
        );
    }

    #[test]
    fn should_drop_importlib_import_module_for_an_unknown_module() {
        let mut consumer = module_at("pkg/app.py");
        let mut loader = leaf("load");
        loader.dynamic = vec![DynamicRef::ImportModule(SmolStr::new("does.not.exist"))];
        consumer.declarations.push(loader);

        let fragment = bind(&[consumer], Path::new("repo"));

        let dynamic = fragment
            .edges
            .iter()
            .filter(|edge| edge.confidence.to_bits() == CONFIDENCE_DYNAMIC.to_bits())
            .count();
        assert_eq!(dynamic, 0, "an unknown module yields no edge");
    }

    #[test]
    fn should_classify_conftest_helpers_as_test_support() {
        let mut support = module_at("tests/conftest.py");
        support.declarations.push(leaf("make_fixture"));

        let fragment = bind(&[support], Path::new("repo"));

        assert!(
            fragment
                .nodes
                .iter()
                .all(|node| node.polarity == Polarity::TestSupport)
        );
    }
}
