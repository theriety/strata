//! Module-graph resolution and IR-fragment emission.
//!
//! Binding turns the per-file [`ParsedModule`] summaries from [`crate::parse`]
//! into a language-agnostic [`IrFragment`]: a dense node per top-level symbol, a
//! laminar container tree over files/folders/domains/packages, typed dependency
//! edges, and three-valued test polarity.
//!
//! Module specifiers resolve in the order **relative path -> `tsconfig` `paths`
//! alias -> package entry point**. References that cannot be resolved statically
//! (dynamic `import('...')`) become low-confidence edges rather than being
//! dropped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::{
    Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness, IrFragment, Node, NodeId,
    NodeKind, Polarity, ScopeLevel,
};

use crate::parse::ParsedModule;

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
) -> Result<IrFragment, BindOutcome> {
    let resolver = Resolver::new(modules, aliases.clone());

    // Assign a dense node id to every declaration, in module-then-source order.
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
                kind: if declaration.is_type {
                    NodeKind::Type
                } else {
                    NodeKind::Symbol
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
    emit_re_exports(&re_export_links, &mut edges);
    let polarity = classify_polarity(modules, &nodes, &local, &exported, &edges);
    apply_polarity(&mut nodes, &polarity);

    Ok(IrFragment {
        nodes,
        edges,
        containers: containers.tree.containers().to_vec(),
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
    let imported = resolve_imports(module, resolver, exports);

    for declaration in &module.declarations {
        let Some(&source) = module_local.and_then(|table| table.get(&declaration.name)) else {
            continue;
        };
        emit_inheritance(declaration, source, &imported, module_local, edges);
        let called = emit_calls(declaration, source, &imported, module_local, edges);
        emit_references(declaration, source, &imported, &called, edges);
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
/// `called` carries the targets already linked by [`emit_calls`]; a referenced
/// import that was also invoked is skipped here so it surfaces only as a call.
fn emit_references(
    declaration: &crate::parse::Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, (NodeId, bool)>,
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
        if let Some(&(target, type_only)) = imported.get(name) {
            if called.contains(&target) || !seen.insert(target) {
                continue;
            }
            let (kind, hardness) = if type_only {
                (EdgeKind::TypeReference, Hardness::Soft)
            } else {
                (EdgeKind::ValueImport, Hardness::Hard)
            };
            push_edge(edges, source, target, kind, hardness, CONFIDENCE_STATIC);
        }
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
    for module in modules {
        let container = containers.file_of(&module.path);
        for re_export in &module.re_exports {
            let Some(target_module) = resolver.resolve(&module.path, &re_export.source) else {
                continue;
            };
            let Some(target_exports) = exports.get(&target_module) else {
                continue;
            };
            // Snapshot the (name, target) bindings up front so the `exports` table
            // can be mutated below without aliasing the immutable target borrow.
            let mut bindings: Vec<(SmolStr, NodeId)> = if re_export.names.is_empty() {
                target_exports
                    .iter()
                    .map(|(name, &target)| (name.clone(), target))
                    .collect()
            } else {
                re_export
                    .names
                    .iter()
                    .filter_map(|name| target_exports.get(name).map(|&t| (name.clone(), t)))
                    .collect()
            };
            bindings.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, target) in bindings {
                // A pre-existing local declaration already provides the binding.
                if local
                    .get(&module.path)
                    .is_some_and(|t| t.contains_key(&name))
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
                links.push(ReExportLink { source: id, target });
            }
        }
    }
    links
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
    path.contains(".spec.ts")
        || path.contains(".test.ts")
        || path.contains("__tests__/")
        || path.contains("/__tests__/")
}

/// Resolves module specifiers to canonical module paths.
struct Resolver {
    /// Set of known module paths, used to verify a resolution target exists.
    known: HashSet<SmolStr>,
    /// `tsconfig`-style alias prefix -> target path prefix mappings.
    aliases: BTreeMap<SmolStr, SmolStr>,
}

impl Resolver {
    /// Builds a resolver over the known module set and `tsconfig` aliases.
    fn new(modules: &[ParsedModule], aliases: BTreeMap<SmolStr, SmolStr>) -> Self {
        Self {
            known: modules.iter().map(|module| module.path.clone()).collect(),
            aliases,
        }
    }

    /// Resolves `specifier` imported from `importer` to a known module path.
    ///
    /// Order: relative path -> alias prefix -> package entry (`index`).
    fn resolve(&self, importer: &str, specifier: &str) -> Option<SmolStr> {
        if specifier.starts_with('.') {
            return self.resolve_relative(importer, specifier);
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
            is_type: false,
            exported: true,
            sloc: 1,
            supertypes: Vec::new(),
            referenced: called.iter().copied().map(SmolStr::new).collect(),
            called: called.iter().copied().map(SmolStr::new).collect(),
            dynamic_imports: Vec::new(),
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
            is_type: false,
            exported: true,
            sloc: 1,
            supertypes: Vec::new(),
            referenced: Vec::new(),
            called: Vec::new(),
            dynamic_imports: Vec::new(),
        });

        let fragment = bind(&[consumer, provider], Path::new("repo"), &BTreeMap::new())
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

    #[test]
    fn should_emit_a_soft_re_export_edge_for_a_barrel_without_a_local_declaration() {
        let mut barrel = module_at("src/index.ts");
        barrel.re_exports.push(crate::parse::ReExport {
            source: SmolStr::new("./widget"),
            names: vec![SmolStr::new("Widget")],
            type_only: false,
        });
        let mut provider = module_at("src/widget.ts");
        provider.declarations.push(crate::parse::Declaration {
            name: SmolStr::new("Widget"),
            is_type: false,
            exported: true,
            sloc: 1,
            supertypes: Vec::new(),
            referenced: Vec::new(),
            called: Vec::new(),
            dynamic_imports: Vec::new(),
        });

        let fragment =
            bind(&[barrel, provider], Path::new("repo"), &BTreeMap::new()).expect("bind succeeds");

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
        let resolver = Resolver::new(&modules, BTreeMap::new());

        let resolved = resolver.resolve("src/app.ts", "./geometry/shape");

        assert_eq!(resolved, Some(SmolStr::new("src/geometry/shape.ts")));
    }

    #[test]
    fn should_resolve_a_parent_relative_specifier() {
        let modules = [
            module_at("src/__tests__/support.ts"),
            module_at("src/geometry/rectangle.ts"),
        ];
        let resolver = Resolver::new(&modules, BTreeMap::new());

        let resolved = resolver.resolve("src/__tests__/support.ts", "../geometry/rectangle");

        assert_eq!(resolved, Some(SmolStr::new("src/geometry/rectangle.ts")));
    }

    #[test]
    fn should_resolve_a_relative_directory_to_its_index_barrel() {
        let modules = [module_at("src/app.ts"), module_at("src/geometry/index.ts")];
        let resolver = Resolver::new(&modules, BTreeMap::new());

        let resolved = resolver.resolve("src/app.ts", "./geometry");

        assert_eq!(resolved, Some(SmolStr::new("src/geometry/index.ts")));
    }

    #[test]
    fn should_resolve_a_specifier_through_a_tsconfig_alias() {
        let modules = [module_at("src/lib/util.ts"), module_at("src/app.ts")];
        let mut aliases = BTreeMap::new();
        aliases.insert(SmolStr::new("@app/"), SmolStr::new("src/"));
        let resolver = Resolver::new(&modules, aliases);

        let resolved = resolver.resolve("src/app.ts", "@app/lib/util");

        assert_eq!(resolved, Some(SmolStr::new("src/lib/util.ts")));
    }

    #[test]
    fn should_resolve_a_bare_package_specifier_to_its_entry_module() {
        let modules = [
            module_at("packages/core/src/index.ts"),
            module_at("src/app.ts"),
        ];
        let resolver = Resolver::new(&modules, BTreeMap::new());

        let resolved = resolver.resolve("src/app.ts", "packages/core");

        assert_eq!(resolved, Some(SmolStr::new("packages/core/src/index.ts")));
    }

    #[test]
    fn should_return_none_for_an_unknown_external_specifier() {
        let modules = [module_at("src/app.ts")];
        let resolver = Resolver::new(&modules, BTreeMap::new());

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
}
