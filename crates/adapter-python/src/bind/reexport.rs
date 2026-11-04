//! `__init__.py` re-export bindings: barrel nodes and their soft edges.

use std::collections::HashMap;

use smol_str::SmolStr;
use unlace_ir::{Edge, EdgeKind, Hardness, Node, NodeId, NodeKind, Polarity, ScopeLevel};

use super::containers::ContainerBuilder;
use super::resolver::{Resolver, is_package_init};
use super::{CONFIDENCE_STATIC, ExportTable, push_edge};
use crate::parse::{Import, ParsedModule};

/// A resolved re-export binding: the barrel-local node that re-exports `target`.
pub(super) struct ReExportLink {
    /// The node created in the `__init__.py` package for the re-exported name.
    source: NodeId,
    /// The original declaration the name is re-exported from.
    target: NodeId,
}

/// Materializes a node for every `__init__.py` re-export binding and records its
/// link to the original declaration.
///
/// A `from .mod import x` inside a package's `__init__.py` introduces `x` into
/// the package's importable surface even though the package never declares it. A
/// node is created for each such name, registered in the `local` and `exports`
/// tables, and linked to the resolved original — so an importer of the package
/// resolves `x` through this barrel binding. Star imports in `__init__.py` fan
/// out over the target's public surface.
///
/// A barrel may re-export from another barrel, so the pass repeats until it
/// binds nothing new: the result does not depend on the order of the files.
pub(super) fn assign_re_export_nodes(
    modules: &[ParsedModule],
    resolver: &Resolver,
    containers: &ContainerBuilder,
    nodes: &mut Vec<Node>,
    exported: &mut Vec<bool>,
    exports: &mut ExportTable,
    local: &mut HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
) -> Vec<ReExportLink> {
    let mut links = Vec::new();
    loop {
        let bound_before = nodes.len();
        for module in modules
            .iter()
            .filter(|module| is_package_init(&module.path))
        {
            let container = containers.file_of(&module.path);
            for import in &module.imports {
                let Some(target_module) = resolver.resolve(&module.path, import) else {
                    continue;
                };
                for (name, target) in barrel_bindings(import, &target_module, resolver, exports) {
                    if local
                        .get(&module.path)
                        .is_some_and(|t| t.contains_key(&name))
                    {
                        continue;
                    }
                    let kind = nodes
                        .iter()
                        .find(|node| node.id == target)
                        .map_or(NodeKind::Symbol, |node| node.kind);
                    let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
                    nodes.push(Node {
                        public_api: false,
                        advice_exempt: false,
                        id,
                        name: name.clone(),
                        kind,
                        polarity: Polarity::Production,
                        container,
                        visibility: ScopeLevel::File,
                        effective_size: 0,
                        re_export: true,
                        display_name: None,
                        test_source: false,
                        owner: None,
                    });
                    exported.push(true);
                    local
                        .entry(module.path.clone())
                        .or_default()
                        .insert(name.clone(), id);
                    exports
                        .entry(module.path.clone())
                        .or_default()
                        .insert(name.clone(), id);
                    links.push(ReExportLink { source: id, target });
                }
            }
        }
        if nodes.len() == bound_before {
            return links;
        }
    }
}

/// The `(bound name, original node)` pairs one import statement of a barrel
/// brings in from `target_module`, sorted by bound name.
fn barrel_bindings(
    import: &Import,
    target_module: &SmolStr,
    resolver: &Resolver,
    exports: &ExportTable,
) -> Vec<(SmolStr, NodeId)> {
    let Some(target_exports) = exports.get(target_module) else {
        return Vec::new();
    };
    let mut bindings: Vec<(SmolStr, NodeId)> = if import.star {
        target_exports
            .iter()
            .filter(|(name, _)| resolver.star_includes(target_module, name))
            .map(|(name, &target)| (name.clone(), target))
            .collect()
    } else {
        import
            .names
            .iter()
            .zip(&import.targets)
            .filter_map(|(bound, original)| {
                target_exports.get(original).map(|&t| (bound.clone(), t))
            })
            .collect()
    };
    bindings.sort_by(|a, b| a.0.cmp(&b.0));
    bindings
}

/// Emits the soft re-export edges from the resolved barrel bindings.
pub(super) fn emit_re_exports(links: &[ReExportLink], edges: &mut Vec<Edge>) {
    for link in links {
        push_edge(
            edges,
            link.source,
            link.target,
            EdgeKind::ReExport,
            Hardness::Soft,
            CONFIDENCE_STATIC,
        );
    }
}
