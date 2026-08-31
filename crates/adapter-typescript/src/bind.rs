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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::{
    Affinity, AffinityKind, Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness,
    IrFragment, Node, NodeId, NodeKind, Polarity, ScopeLevel,
};

use crate::parse::{DeclarationKind, ParsedModule, ReExport, ReExportBinding};

/// Per-module, per-name lookup of the node a declaration was assigned.
type ExportTable = HashMap<SmolStr, HashMap<SmolStr, NodeId>>;

/// Confidence assigned to a statically resolved edge.
const CONFIDENCE_STATIC: f64 = 1.0;

/// Confidence assigned to a dynamic `import('...')` edge.
const CONFIDENCE_DYNAMIC: f64 = 0.5;

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
    })
}

fn emit_companion_affinities(
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

/// Emits all dependency edges for the bound module set.
fn emit_edges(
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

/// Maps each locally imported name to its resolved target node and type-only flag.
fn resolve_imports(
    module: &ParsedModule,
    resolver: &Resolver,
    exports: &ExportTable,
) -> HashMap<SmolStr, (NodeId, bool)> {
    let mut imported: HashMap<SmolStr, (NodeId, bool)> = HashMap::new();
    for import in &module.imports {
        let Some(target_module) = resolver.resolve(&module.path, &import.source) else {
            continue;
        };
        let Some(target_exports) = exports.get(&target_module) else {
            continue;
        };
        for name in &import.names {
            if let Some(&target) = target_exports.get(name) {
                imported.insert(name.clone(), (target, import.type_only));
            }
        }
    }
    imported
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
struct ReExportLink {
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
fn assign_re_export_nodes(
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
    });
    exported.push(true);
    id
}

/// Emits the soft re-export edges from the resolved barrel bindings.
///
/// Re-exports are emitted as-is — barrel flattening is the engine's job. The
/// edge runs from the barrel binding to the original declaration, soft because a
/// re-export imposes no runtime dependency of its own.
fn emit_re_exports(links: &[ReExportLink], edges: &mut Vec<Edge>) {
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
fn classify_polarity(
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
fn apply_polarity(nodes: &mut [Node], polarity: &[Polarity]) {
    for (node, &class) in nodes.iter_mut().zip(polarity) {
        node.polarity = class;
    }
}

/// Returns `true` if `path` is a test file by convention.
fn is_test_path(path: &str) -> bool {
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

/// Resolves module specifiers to canonical module paths.
struct Resolver {
    /// Set of known module paths, used to verify a resolution target exists.
    known: HashSet<SmolStr>,
    /// `tsconfig`-style alias prefix -> target path prefix mappings.
    aliases: BTreeMap<SmolStr, SmolStr>,
    /// Node.js subpath-import specifier -> target mappings (`package.json`).
    imports: BTreeMap<SmolStr, SmolStr>,
}

impl Resolver {
    /// Builds a resolver over the known module set, `tsconfig` aliases, and
    /// Node.js subpath imports.
    fn new(
        modules: &[ParsedModule],
        aliases: BTreeMap<SmolStr, SmolStr>,
        imports: BTreeMap<SmolStr, SmolStr>,
    ) -> Self {
        Self {
            known: modules.iter().map(|module| module.path.clone()).collect(),
            aliases,
            imports,
        }
    }

    /// Resolves `specifier` imported from `importer` to a known module path.
    ///
    /// Order: relative path -> Node.js subpath import -> alias prefix ->
    /// package entry (`index`).
    fn resolve(&self, importer: &str, specifier: &str) -> Option<SmolStr> {
        if specifier.starts_with('.') {
            return self.resolve_relative(importer, specifier);
        }
        if specifier.starts_with('#') && !self.imports.is_empty() {
            return self.resolve_subpath_import(specifier);
        }
        if let Some(resolved) = self.resolve_alias(specifier) {
            return Some(resolved);
        }
        self.resolve_package(specifier)
    }

    /// Resolves a relative specifier against the importer's directory.
    fn resolve_relative(&self, importer: &str, specifier: &str) -> Option<SmolStr> {
        let importer_dir = importer.rsplit_once('/').map_or("", |(dir, _)| dir);
        let joined = normalize_join(importer_dir, specifier);
        self.with_extensions(&joined)
    }

    /// Resolves a specifier through the `tsconfig` `paths` aliases.
    fn resolve_alias(&self, specifier: &str) -> Option<SmolStr> {
        for (alias, target) in &self.aliases {
            if let Some(rest) = specifier.strip_prefix(alias.as_str()) {
                let candidate = format!("{target}{rest}");
                if let Some(resolved) = self.with_extensions(&candidate) {
                    return Some(resolved);
                }
            }
        }
        None
    }

    /// Resolves a bare package specifier to its entry module, if it maps to a
    /// known in-repo module (workspace package) rather than an external dep.
    fn resolve_package(&self, specifier: &str) -> Option<SmolStr> {
        let base = format!("{specifier}/src/index");
        self.with_extensions(&base)
            .or_else(|| self.with_extensions(&format!("{specifier}/index")))
    }

    /// Resolves a Node.js subpath import (`#agent/schemas`) through the root
    /// `package.json` `imports` map. Exact keys win over single-star patterns;
    /// the matched pattern's `*` substitutes once into the target's own `*`.
    fn resolve_subpath_import(&self, specifier: &str) -> Option<SmolStr> {
        if let Some(direct) = self.imports.get(specifier) {
            return self.resolve_import_target(direct);
        }
        for (pattern, target) in &self.imports {
            let Some((prefix, suffix)) = pattern.split_once('*') else {
                continue;
            };
            let Some(rest) = specifier.strip_prefix(prefix) else {
                continue;
            };
            let Some(rest) = rest.strip_suffix(suffix) else {
                continue;
            };
            if rest.contains('*') {
                continue;
            }
            return self.resolve_import_target(&target.replace('*', rest));
        }
        None
    }

    /// Resolves an `imports` target against the repository root: literal known
    /// paths win, otherwise extension and index-barrel forms apply.
    fn resolve_import_target(&self, target: &str) -> Option<SmolStr> {
        let trimmed = target.trim_start_matches("./");
        let literal = SmolStr::new(trimmed);
        if self.known.contains(&literal) {
            return Some(literal);
        }
        self.with_extensions(trimmed)
    }

    /// Tries the candidate path with each TypeScript extension and `index` form.
    fn with_extensions(&self, base: &str) -> Option<SmolStr> {
        for extension in [".ts", ".tsx", ".d.ts"] {
            let candidate = SmolStr::new(format!("{base}{extension}"));
            if self.known.contains(&candidate) {
                return Some(candidate);
            }
        }
        for index in ["/index.ts", "/index.tsx"] {
            let candidate = SmolStr::new(format!("{base}{index}"));
            if self.known.contains(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
}

/// Joins `dir` and a relative `specifier`, collapsing `.` and `..` segments.
fn normalize_join(dir: &str, specifier: &str) -> String {
    let mut segments: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for segment in specifier.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

/// Interns containers (file/folder/domain/package/package group) for the modules.
struct ContainerBuilder {
    /// The interned container tree.
    tree: ContainerTree,
    /// Module path -> its file container id.
    files: HashMap<SmolStr, ContainerId>,
}

impl ContainerBuilder {
    /// Returns the file container id for a module path.
    fn file_of(&self, path: &SmolStr) -> ContainerId {
        self.files.get(path).copied().unwrap_or(ContainerId(0))
    }

    /// Builds the laminar container tree for `modules` under `root`.
    ///
    /// Levels follow the fixed five-level mapping: the repository is the package
    /// group, the first path segment a package, the next two directory levels
    /// domain and folder, and the file itself a leaf. Shallow paths reuse a
    /// single implicit container at each missing level so every file still hangs
    /// off a valid, strictly-ascending chain.
    fn build(modules: &[ParsedModule], root: &Path) -> Self {
        let root_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(|| SmolStr::new("root"), SmolStr::new);

        let mut containers: Vec<Container> = Vec::new();
        let mut by_key: BTreeMap<(ScopeLevel, SmolStr), ContainerId> = BTreeMap::new();
        let mut files = HashMap::new();

        let group = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::PackageGroup,
            root_name,
            None,
        );

        let mut paths: Vec<&SmolStr> = modules.iter().map(|module| &module.path).collect();
        paths.sort();

        for path in paths {
            let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            // the trailing segment is the file itself, not a directory; a
            // root-level file hangs under the synthetic `workspace` chain.
            segments.pop();
            if segments.is_empty() {
                segments.push("workspace");
            }

            let package = intern(
                &mut containers,
                &mut by_key,
                ScopeLevel::Package,
                prefix_key(&segments, 1),
                Some(group),
            );
            let domain = intern(
                &mut containers,
                &mut by_key,
                ScopeLevel::Domain,
                prefix_key(&segments, 2),
                Some(package),
            );
            let folder = intern(
                &mut containers,
                &mut by_key,
                ScopeLevel::Folder,
                prefix_key(&segments, 3),
                Some(domain),
            );
            let file = intern(
                &mut containers,
                &mut by_key,
                ScopeLevel::File,
                path.clone(),
                Some(folder),
            );
            files.insert(path.clone(), file);
        }

        Self {
            tree: ContainerTree::new(containers),
            files,
        }
    }
}

/// Builds a stable container key from the first `take` path segments.
///
/// Using the path prefix as the key keeps sibling directories distinct while
/// collapsing every file under the same prefix into one container. Paths shorter
/// than `take` reuse their full prefix, realizing the spec's implicit levels:
/// shallow files still hang off a valid, strictly-ascending chain.
fn prefix_key(segments: &[&str], take: usize) -> SmolStr {
    let bounded = take.clamp(1, segments.len().max(1));
    SmolStr::new(segments.get(..bounded).unwrap_or(segments).join("/"))
}

/// Interns a container by `(level, key)`, returning the existing id on a hit.
fn intern(
    containers: &mut Vec<Container>,
    by_key: &mut BTreeMap<(ScopeLevel, SmolStr), ContainerId>,
    level: ScopeLevel,
    key: SmolStr,
    parent: Option<ContainerId>,
) -> ContainerId {
    if let Some(&id) = by_key.get(&(level, key.clone())) {
        return id;
    }
    let id = ContainerId(u32::try_from(containers.len()).unwrap_or(u32::MAX));
    containers.push(Container {
        id,
        name: key.clone(),
        level,
        parent,
        synthetic: false,
    });
    by_key.insert((level, key), id);
    id
}

#[cfg(test)]
mod tests {
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
        // The barrel materializes its own node for the re-exported name, and the
        // edge runs from that node to the original declaration.
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
