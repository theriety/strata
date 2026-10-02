//! Barrel re-export node materialization and re-export edge emission.

use std::collections::HashMap;

use smol_str::SmolStr;
use strata_ir::{
    ContainerId, Edge, EdgeKind, Hardness, Node, NodeId, NodeKind, Polarity, ScopeLevel,
};

use crate::parse::{ParsedModule, ReExport, ReExportBinding};

use super::super::ExportTable;
use super::super::containers::ContainerBuilder;
use super::super::resolution::Resolver;
use super::CONFIDENCE_STATIC;
use super::edges::push_edge;

/// A resolved re-export binding: the barrel-local node that re-exports `target`.
pub(in crate::bind) struct ReExportLink {
    /// The node created in the barrel module for the re-exported name.
    pub(super) source: NodeId,
    /// The original declaration the name is re-exported from.
    pub(super) target: NodeId,
    /// Whether the source is a namespace object rather than a named binding.
    pub(super) namespace: bool,
}

/// Materializes a node for every barrel re-export binding and records its link
/// to the original declaration.
///
/// `export { x } from '...'` introduces `x` into the barrel module even though
/// the barrel never declares it; that binding becomes part of the barrel's
/// export surface, so importers of the barrel must resolve through it. A node is
/// created for each such name (typed per `export type`), registered in the
/// `local` and `exports` tables, and linked to the resolved original. A name
/// that already has a local declaration (`export { local } from './self'` is not
/// expressible, but a same-name local declaration can coexist) is left untouched.
/// Wildcard `export * from '...'` fans out over every export of the resolved
/// target — the declaration pass has already populated those tables.
pub(in crate::bind) fn assign_re_export_nodes(
    modules: &[ParsedModule],
    resolver: &Resolver,
    containers: &ContainerBuilder,
    nodes: &mut Vec<Node>,
    exported: &mut Vec<bool>,
    exports: &mut ExportTable,
    local: &mut HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
) -> Vec<ReExportLink> {
    let mut links = Vec::new();
    let ordered_modules = modules_by_path(modules);

    loop {
        let mut made_progress = false;
        for module in &ordered_modules {
            let container = containers.file_of(&module.path);
            for re_export in &module.re_exports {
                let Some(target_module) = resolver.resolve(&module.path, &re_export.source) else {
                    continue;
                };
                let Some(target_exports) = exports.get(&target_module) else {
                    continue;
                };
                if let Some(namespace) = re_export.names.iter().find_map(|binding| match binding {
                    ReExportBinding::Namespace { namespace } => Some(namespace.as_str()),
                    ReExportBinding::Legacy(_) | ReExportBinding::Named { .. } => None,
                }) {
                    if local
                        .get(&module.path)
                        .is_some_and(|table| table.contains_key(namespace))
                    {
                        continue;
                    }
                    let targets = sorted_unique_targets(target_exports);
                    let id = push_namespace_node(nodes, exported, container, namespace);
                    local
                        .entry(module.path.clone())
                        .or_default()
                        .insert(SmolStr::new(namespace), id);
                    exports
                        .entry(module.path.clone())
                        .or_default()
                        .insert(SmolStr::new(namespace), id);
                    links.extend(targets.into_iter().map(|target| ReExportLink {
                        source: id,
                        target,
                        namespace: true,
                    }));
                    made_progress = true;
                    continue;
                }
                // Snapshot the (name, target) bindings up front so the `exports`
                // table can be mutated below without aliasing its target borrow.
                let mut bindings = resolved_re_export_bindings(re_export, target_exports);
                bindings.sort_by(|a, b| a.0.cmp(&b.0));
                for (name, target) in bindings {
                    // A prior pass or source declaration already provides this
                    // binding. Skipping it makes every pass monotonic.
                    if local
                        .get(&module.path)
                        .is_some_and(|table| table.contains_key(&name))
                    {
                        continue;
                    }
                    let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
                    nodes.push(Node {
                        id,
                        name: name.clone(),
                        kind: if re_export.type_only {
                            NodeKind::Type
                        } else {
                            NodeKind::Symbol
                        },
                        polarity: Polarity::Production,
                        container,
                        visibility: ScopeLevel::File,
                        effective_size: 0,
                        re_export: true,
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
                    links.push(ReExportLink {
                        source: id,
                        target,
                        namespace: false,
                    });
                    made_progress = true;
                }
            }
        }
        if !made_progress {
            break;
        }
    }
    links
}

fn resolved_re_export_bindings(
    re_export: &ReExport,
    target_exports: &HashMap<SmolStr, NodeId>,
) -> Vec<(SmolStr, NodeId)> {
    if re_export.names.is_empty() {
        return target_exports
            .iter()
            .map(|(name, &target)| (name.clone(), target))
            .collect();
    }
    re_export
        .names
        .iter()
        .filter_map(|binding| {
            let (original, exported_name) = match binding {
                ReExportBinding::Legacy(name) => (name.as_str(), name.as_str()),
                ReExportBinding::Named {
                    original,
                    exported: exported_name,
                } => (original.as_str(), exported_name.as_str()),
                ReExportBinding::Namespace { .. } => return None,
            };
            target_exports
                .get(original)
                .map(|&target| (SmolStr::new(exported_name), target))
        })
        .collect()
}

fn modules_by_path(modules: &[ParsedModule]) -> Vec<&ParsedModule> {
    let mut ordered: Vec<&ParsedModule> = modules.iter().collect();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));
    ordered
}

fn sorted_unique_targets(exports: &HashMap<SmolStr, NodeId>) -> Vec<NodeId> {
    let mut targets: Vec<NodeId> = exports.values().copied().collect();
    targets.sort();
    targets.dedup();
    targets
}

fn push_namespace_node(
    nodes: &mut Vec<Node>,
    exported: &mut Vec<bool>,
    container: ContainerId,
    namespace: &str,
) -> NodeId {
    let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
    nodes.push(Node {
        id,
        name: SmolStr::new(namespace),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container,
        visibility: ScopeLevel::File,
        effective_size: 0,
        re_export: true,
    });
    exported.push(true);
    id
}

/// Emits the soft re-export edges from the resolved barrel bindings.
///
/// Re-exports are emitted as-is — barrel flattening is the engine's job. The
/// edge runs from the barrel binding to the original declaration, soft because a
/// re-export imposes no runtime dependency of its own.
pub(in crate::bind) fn emit_re_exports(links: &[ReExportLink], edges: &mut Vec<Edge>) {
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
