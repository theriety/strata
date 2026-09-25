//! Node, edge, affinity, re-export, and polarity emission.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use smol_str::SmolStr;
use strata_ir::{
    Affinity, AffinityKind, ContainerId, Edge, EdgeKind, Hardness, Node, NodeId, NodeKind,
    Polarity, ScopeLevel,
};

use crate::parse::{DeclarationKind, ParsedModule, ReExport, ReExportBinding};

use super::ExportTable;
use super::containers::ContainerBuilder;
use super::resolution::{Resolver, resolve_imports};

/// Confidence assigned to a statically resolved edge.
pub(super) const CONFIDENCE_STATIC: f64 = 1.0;

/// Confidence assigned to a dynamic `import('...')` edge.
const CONFIDENCE_DYNAMIC: f64 = 0.5;

pub(super) fn emit_companion_affinities(
    modules: &[ParsedModule],
    resolver: &Resolver,
    exports: &ExportTable,
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    nodes: &[Node],
    re_export_links: &[ReExportLink],
) -> Vec<Affinity> {
    let kinds: HashMap<NodeId, NodeKind> = nodes.iter().map(|node| (node.id, node.kind)).collect();
    let mut targets_by_source: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for link in re_export_links {
        if link.namespace {
            continue;
        }
        targets_by_source
            .entry(link.source)
            .or_default()
            .insert(link.target);
    }
    let re_export_targets: HashMap<NodeId, NodeId> = targets_by_source
        .into_iter()
        .filter_map(|(source, targets)| {
            let mut targets = targets.into_iter();
            let target = targets.next()?;
            targets.next().is_none().then_some((source, target))
        })
        .collect();
    let mut owners_by_companion: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for module in modules {
        let module_local = local.get(&module.path);
        let imported = resolve_imports(module, resolver, exports);
        for declaration in &module.declarations {
            let Some(&owner) = module_local.and_then(|table| table.get(&declaration.name)) else {
                continue;
            };
            for name in &declaration.signature_companions {
                let companion = imported
                    .get(name)
                    .map(|(target, _)| *target)
                    .or_else(|| module_local.and_then(|table| table.get(name)).copied());
                let companion = companion.and_then(|companion| {
                    originating_re_export_target(companion, &re_export_targets)
                });
                if let Some(companion) = companion
                    && kinds.get(&companion) == Some(&NodeKind::Type)
                {
                    owners_by_companion
                        .entry(companion)
                        .or_default()
                        .push(owner);
                }
            }
        }
    }
    owners_by_companion
        .into_iter()
        .filter_map(|(companion, owners)| {
            let [owner] = owners.as_slice() else {
                return None;
            };
            Some(Affinity {
                owner: *owner,
                companion,
                kind: AffinityKind::CompanionOwner,
            })
        })
        .collect()
}

/// Follows affinity identity through barrel bindings to the declaration that
/// originated the exported name.
///
/// This lookup is intentionally affinity-only: ordinary imports continue to
/// resolve to barrel nodes so dependency and re-export edge semantics remain
/// unchanged. A malformed link cycle has no authoritative origin, so it emits
/// no affinity rather than choosing an arbitrary barrel node.
fn originating_re_export_target(
    start: NodeId,
    re_export_targets: &HashMap<NodeId, NodeId>,
) -> Option<NodeId> {
    let mut current = start;
    let mut visited = HashSet::new();
    while let Some(&target) = re_export_targets.get(&current) {
        if !visited.insert(current) {
            return None;
        }
        current = target;
    }
    Some(current)
}

/// Emits all dependency edges for the bound module set.
pub(super) fn emit_edges(
    modules: &[ParsedModule],
    resolver: &Resolver,
    exports: &ExportTable,
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    for module in modules {
        emit_module_edges(module, resolver, exports, local, &mut edges);
    }
    edges
}

/// Emits every dependency edge originating in a single module.
fn emit_module_edges(
    module: &ParsedModule,
    resolver: &Resolver,
    exports: &ExportTable,
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) {
    let module_local = local.get(&module.path);
    let local_types: HashMap<SmolStr, bool> = module
        .declarations
        .iter()
        .map(|declaration| {
            (
                declaration.name.clone(),
                declaration.kind == DeclarationKind::Type,
            )
        })
        .collect();
    let imported = resolve_imports(module, resolver, exports);

    for declaration in &module.declarations {
        let Some(&source) = module_local.and_then(|table| table.get(&declaration.name)) else {
            continue;
        };
        emit_inheritance(declaration, source, &imported, module_local, edges);
        let called = emit_calls(declaration, source, &imported, module_local, edges);
        emit_references(
            declaration,
            source,
            &imported,
            module_local,
            &local_types,
            &called,
            edges,
        );
        emit_dynamic_imports(declaration, source, &module.path, resolver, exports, edges);
    }
}

/// Emits inheritance edges (`extends` / `implements`) for one declaration.
fn emit_inheritance(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, (NodeId, bool)>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) {
    for supertype in &declaration.supertypes {
        if let Some(target) = imported
            .get(supertype)
            .map(|(target, _)| *target)
            .or_else(|| module_local.and_then(|table| table.get(supertype)).copied())
        {
            push_edge(
                edges,
                source,
                target,
                EdgeKind::Inheritance,
                Hardness::Hard,
                CONFIDENCE_STATIC,
            );
        }
    }
}

/// Emits call edges for identifiers a declaration invokes (call or `new`).
///
/// A called name is resolved through the symbol tables — first an imported
/// binding, then a same-module declaration — to the node it denotes; the
/// invocation produces a hard [`EdgeKind::Call`]. Returns the set of resolved
/// call targets so [`emit_references`] does not also emit a value-import edge
/// for the same target (a call is the more specific relationship).
fn emit_calls(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, (NodeId, bool)>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) -> HashSet<NodeId> {
    let mut called: HashSet<NodeId> = HashSet::new();
    for name in &declaration.called {
        let Some(target) = imported
            .get(name)
            .map(|(target, _)| *target)
            .or_else(|| module_local.and_then(|table| table.get(name)).copied())
        else {
            continue;
        };
        if !called.insert(target) {
            continue;
        }
        push_edge(
            edges,
            source,
            target,
            EdgeKind::Call,
            Hardness::Hard,
            CONFIDENCE_STATIC,
        );
    }
    called
}

/// Emits value-import / type-reference edges for names a declaration references.
///
/// These edges model **dependency**, not the import list: a name resolved
/// inside the declaration's own module is as real a dependency as one pulled
/// across a module boundary, and the engine relocates symbols against exactly
/// that graph. So a referenced name is resolved first through the imports, then
/// through the same-module declarations — the order [`emit_calls`] already
/// uses. The edge kind follows the *target*: a type target is a soft
/// [`EdgeKind::TypeReference`], anything else a hard [`EdgeKind::ValueImport`].
///
/// `called` carries the targets already linked by [`emit_calls`]; a referenced
/// name that was also invoked is skipped here so it surfaces only as a call. A
/// declaration naming itself depends on nothing and emits no edge.
fn emit_references(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, (NodeId, bool)>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    local_types: &HashMap<SmolStr, bool>,
    called: &HashSet<NodeId>,
    edges: &mut Vec<Edge>,
) {
    // Supertypes already produced inheritance edges; skip them here so an
    // `implements Shape` clause does not also surface as a value/type edge.
    let supertypes: HashSet<&SmolStr> = declaration.supertypes.iter().collect();
    let mut seen: HashSet<NodeId> = HashSet::new();
    for name in &declaration.referenced {
        if supertypes.contains(name) {
            continue;
        }
        let resolved = imported.get(name).copied().or_else(|| {
            let target = module_local.and_then(|table| table.get(name)).copied()?;
            Some((target, local_types.get(name).copied().unwrap_or(false)))
        });
        let Some((target, is_type)) = resolved else {
            continue;
        };
        if target == source || called.contains(&target) || !seen.insert(target) {
            continue;
        }
        let (kind, hardness) = if is_type {
            (EdgeKind::TypeReference, Hardness::Soft)
        } else {
            (EdgeKind::ValueImport, Hardness::Hard)
        };
        push_edge(edges, source, target, kind, hardness, CONFIDENCE_STATIC);
    }
}

/// Emits low-confidence value-import edges for a declaration's dynamic imports.
///
/// The specific symbol pulled from a dynamic `import('...')` is unknowable
/// statically, so every exported symbol of the resolved module receives an edge.
fn emit_dynamic_imports(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    importer: &SmolStr,
    resolver: &Resolver,
    exports: &ExportTable,
    edges: &mut Vec<Edge>,
) {
    for specifier in &declaration.dynamic_imports {
        let Some(target_module) = resolver.resolve(importer, specifier) else {
            continue;
        };
        let Some(target_exports) = exports.get(&target_module) else {
            continue;
        };
        for &target in target_exports.values() {
            push_edge(
                edges,
                source,
                target,
                EdgeKind::ValueImport,
                Hardness::Hard,
                CONFIDENCE_DYNAMIC,
            );
        }
    }
}

/// A resolved re-export binding: the barrel-local node that re-exports `target`.
pub(super) struct ReExportLink {
    /// The node created in the barrel module for the re-exported name.
    source: NodeId,
    /// The original declaration the name is re-exported from.
    target: NodeId,
    /// Whether the source is a namespace object rather than a named binding.
    namespace: bool,
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

/// Classifies every node's polarity.
///
/// A module whose path matches a test convention contributes [`Polarity::TestCase`]
/// nodes. Production helpers reachable (over edges) only from test nodes become
/// [`Polarity::TestSupport`]; everything else stays [`Polarity::Production`].
pub(super) fn classify_polarity(
    modules: &[ParsedModule],
    nodes: &[Node],
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    exported: &[bool],
    edges: &[Edge],
) -> Vec<Polarity> {
    let mut polarity = vec![Polarity::Production; nodes.len()];

    // Test cases: every node declared in a test file.
    let mut is_test_node = vec![false; nodes.len()];
    for module in modules {
        if !is_test_path(&module.path) {
            continue;
        }
        if let Some(table) = local.get(&module.path) {
            for &id in table.values() {
                if let Some(slot) = polarity.get_mut(id.0 as usize) {
                    *slot = Polarity::TestCase;
                }
                if let Some(slot) = is_test_node.get_mut(id.0 as usize) {
                    *slot = true;
                }
            }
        }
    }

    // Reachability: a production node consumed only by test nodes is TestSupport.
    // Build the set of production nodes reachable from production nodes; anything
    // referenced from a test but not reachable from production is support.
    let mut consumed_by_production = vec![false; nodes.len()];
    let mut consumed_by_test = vec![false; nodes.len()];
    for edge in edges {
        let from_test = is_test_node
            .get(edge.source.0 as usize)
            .copied()
            .unwrap_or(false);
        if let Some(slot) = (if from_test {
            &mut consumed_by_test
        } else {
            &mut consumed_by_production
        })
        .get_mut(edge.target.0 as usize)
        {
            *slot = true;
        }
    }

    for (index, slot) in polarity.iter_mut().enumerate() {
        if *slot == Polarity::TestCase {
            continue;
        }
        // An exported symbol is part of the production API surface; only a
        // private helper consumed solely by tests is genuine test support.
        if exported.get(index).copied().unwrap_or(false) {
            continue;
        }
        let production = consumed_by_production.get(index).copied().unwrap_or(false);
        let test = consumed_by_test.get(index).copied().unwrap_or(false);
        if test && !production {
            *slot = Polarity::TestSupport;
        }
    }

    polarity
}

/// Overwrites each node's polarity from the classification vector.
pub(super) fn apply_polarity(nodes: &mut [Node], polarity: &[Polarity]) {
    for (node, &class) in nodes.iter_mut().zip(polarity) {
        node.polarity = class;
    }
}

/// Returns `true` if `path` is a test file by convention.
pub(super) fn is_test_path(path: &str) -> bool {
    if path.split('/').any(|segment| segment == "__tests__") {
        return true;
    }

    let Some(stem) = path
        .strip_suffix(".tsx")
        .or_else(|| path.strip_suffix(".ts"))
    else {
        return false;
    };
    let file_name = stem.rsplit('/').next().unwrap_or(stem);

    file_name
        .split('.')
        .skip(1)
        .any(|segment| matches!(segment, "spec" | "test"))
}
