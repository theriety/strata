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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::{
    Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness, IrFragment, Node, NodeId,
    NodeKind, Polarity, ScopeLevel,
};

use crate::parse::{Declaration, DynamicRef, Import, ParsedModule};

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
///
/// # Errors
///
/// This binder never fails on unresolved references — Python's dynamic
/// constructs become low-confidence edges and unknown names are dropped — so it
/// returns `Ok` for every well-formed parse. The `Result` preserves the adapter
/// contract.
pub fn bind(modules: &[ParsedModule], root: &Path) -> Result<IrFragment, BindOutcome> {
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

    Ok(IrFragment {
        nodes,
        edges,
        affinities: Vec::new(),
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

/// Returns `true` if a name is part of the importable surface (no `_` prefix).
fn is_public(name: &str) -> bool {
    !name.starts_with('_')
}

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
    let star_targets = resolve_star_imports(module, resolver, exports);

    for declaration in &module.declarations {
        let Some(&source) = module_local.and_then(|table| table.get(&declaration.name)) else {
            continue;
        };
        emit_inheritance(declaration, source, &imported, module_local, edges);
        let called = emit_calls(declaration, source, &imported, module_local, edges);
        emit_annotations(declaration, source, &imported, module_local, edges);
        emit_references(declaration, source, &imported, module_local, &called, edges);
        emit_dynamic(
            declaration,
            source,
            &imported,
            &star_targets,
            resolver,
            exports,
            edges,
        );
    }
}

/// Maps each locally imported name to its resolved target node.
fn resolve_imports(
    module: &ParsedModule,
    resolver: &Resolver,
    exports: &ExportTable,
) -> HashMap<SmolStr, NodeId> {
    let mut imported: HashMap<SmolStr, NodeId> = HashMap::new();
    for import in &module.imports {
        if import.star {
            continue;
        }
        let Some(target_module) = resolver.resolve(&module.path, import) else {
            continue;
        };
        let Some(target_exports) = exports.get(&target_module) else {
            continue;
        };
        // `names` is the locally bound name; `targets` the original export name.
        for (bound, original) in import.names.iter().zip(&import.targets) {
            if let Some(&target) = target_exports.get(original) {
                imported.insert(bound.clone(), target);
            }
        }
    }
    imported
}

/// Resolves the exported nodes reachable through each `from m import *` in a
/// module, fanning out over the target's public surface (its `__all__` when
/// declared, else every non-underscore export).
fn resolve_star_imports(
    module: &ParsedModule,
    resolver: &Resolver,
    exports: &ExportTable,
) -> Vec<NodeId> {
    let mut targets = Vec::new();
    for import in &module.imports {
        if !import.star {
            continue;
        }
        let Some(target_module) = resolver.resolve(&module.path, import) else {
            continue;
        };
        let Some(target_exports) = exports.get(&target_module) else {
            continue;
        };
        let surface = resolver.public_surface(&target_module);
        for (name, &node) in target_exports {
            let included = surface
                .as_ref()
                .map_or_else(|| is_public(name), |all| all.contains(name));
            if included {
                targets.push(node);
            }
        }
    }
    targets
}

/// Emits inheritance edges (class bases) for one declaration.
fn emit_inheritance(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) {
    for base in &declaration.bases {
        if let Some(target) = lookup(base, imported, module_local) {
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

/// Emits call edges for identifiers a declaration invokes.
///
/// A called name is resolved first through an imported binding, then a
/// same-module declaration. Returns the set of resolved call targets so neither
/// [`emit_references`] nor [`emit_annotations`] re-links the same target.
fn emit_calls(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) -> HashSet<NodeId> {
    let mut called: HashSet<NodeId> = HashSet::new();
    for name in &declaration.called {
        let Some(target) = lookup(name, imported, module_local) else {
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

/// Emits soft type-reference edges for a declaration's annotation references.
fn emit_annotations(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    edges: &mut Vec<Edge>,
) {
    let bases: HashSet<&SmolStr> = declaration.bases.iter().collect();
    let mut seen: HashSet<NodeId> = HashSet::new();
    for name in &declaration.annotations {
        if bases.contains(name) {
            continue;
        }
        if let Some(target) = lookup(name, imported, module_local)
            && seen.insert(target)
        {
            push_edge(
                edges,
                source,
                target,
                EdgeKind::TypeReference,
                Hardness::Soft,
                CONFIDENCE_STATIC,
            );
        }
    }
}

/// Emits hard value-import edges for names a declaration references.
///
/// These edges model **dependency**, not the import list: a name resolved
/// inside the declaration's own module is as real a dependency as one pulled
/// across a module boundary, and the engine relocates symbols against exactly
/// that graph. So a referenced name is resolved first through the imports, then
/// through the same-module declarations — the order [`emit_calls`] already
/// uses.
///
/// `called` carries the targets already linked by [`emit_calls`]; a referenced
/// name that was also invoked is skipped here so it surfaces only as a call. A
/// declaration naming itself depends on nothing and emits no edge.
fn emit_references(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
    called: &HashSet<NodeId>,
    edges: &mut Vec<Edge>,
) {
    let bases: HashSet<&SmolStr> = declaration.bases.iter().collect();
    let annotations: HashSet<&SmolStr> = declaration.annotations.iter().collect();
    let mut seen: HashSet<NodeId> = HashSet::new();
    for name in &declaration.referenced {
        if bases.contains(name) || annotations.contains(name) {
            continue;
        }
        let Some(target) = imported
            .get(name)
            .copied()
            .or_else(|| module_local.and_then(|table| table.get(name)).copied())
        else {
            continue;
        };
        if target == source || called.contains(&target) || !seen.insert(target) {
            continue;
        }
        push_edge(
            edges,
            source,
            target,
            EdgeKind::ValueImport,
            Hardness::Hard,
            CONFIDENCE_STATIC,
        );
    }
}

/// Emits low-confidence edges for dynamic constructs and star imports.
///
/// `getattr(obj, "x")` and `importlib.import_module("m")` cannot be resolved to
/// a precise target; a star import pulls in an unknown subset of the target's
/// surface. Each becomes an honest edge below full confidence rather than a
/// silent drop. A `getattr` on an imported binding links to that binding; an
/// `importlib.import_module` of a string-literal module fans out over that
/// module's public surface; a star import does the same over its target.
fn emit_dynamic(
    declaration: &Declaration,
    source: NodeId,
    imported: &HashMap<SmolStr, NodeId>,
    star_targets: &[NodeId],
    resolver: &Resolver,
    exports: &ExportTable,
    edges: &mut Vec<Edge>,
) {
    for dynamic in &declaration.dynamic {
        match dynamic {
            DynamicRef::GetAttr(base) => {
                if let Some(&target) = imported.get(base) {
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
            DynamicRef::ImportModule(dotted) => {
                for target in import_module_targets(dotted, resolver, exports) {
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
    }
    for &target in star_targets {
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

/// Resolves a string-literal `importlib.import_module` target to its public
/// surface nodes.
///
/// The dotted argument is matched against the known module set; an unknown
/// module yields no edge (the import is genuinely unresolvable). A resolved
/// module fans out over its declared `__all__` surface when present, else every
/// non-underscore export — the same surface a star import would pull in.
fn import_module_targets(
    dotted: &SmolStr,
    resolver: &Resolver,
    exports: &ExportTable,
) -> Vec<NodeId> {
    let Some(module) = resolver.module_of_dotted(dotted) else {
        return Vec::new();
    };
    let Some(target_exports) = exports.get(module) else {
        return Vec::new();
    };
    let surface = resolver.public_surface(module);
    let mut targets: Vec<NodeId> = target_exports
        .iter()
        .filter(|(name, _)| {
            surface
                .as_ref()
                .map_or_else(|| is_public(name), |all| all.contains(*name))
        })
        .map(|(_, &node)| node)
        .collect();
    targets.sort_unstable();
    targets
}

/// Resolves a referenced name to a node: imported binding first, then local.
fn lookup(
    name: &SmolStr,
    imported: &HashMap<SmolStr, NodeId>,
    module_local: Option<&HashMap<SmolStr, NodeId>>,
) -> Option<NodeId> {
    imported
        .get(name)
        .copied()
        .or_else(|| module_local.and_then(|table| table.get(name)).copied())
}

/// A resolved re-export binding: the barrel-local node that re-exports `target`.
struct ReExportLink {
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
        if !is_package_init(&module.path) {
            continue;
        }
        let container = containers.file_of(&module.path);
        for import in &module.imports {
            let Some(target_module) = resolver.resolve(&module.path, import) else {
                continue;
            };
            let Some(target_exports) = exports.get(&target_module) else {
                continue;
            };
            // Snapshot the (name, target) bindings before mutating `exports`.
            let mut bindings: Vec<(SmolStr, NodeId)> = if import.star {
                let surface = resolver.public_surface(&target_module);
                target_exports
                    .iter()
                    .filter(|(name, _)| {
                        surface
                            .as_ref()
                            .map_or_else(|| is_public(name), |all| all.contains(*name))
                    })
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
            for (name, target) in bindings {
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
                    id,
                    name: name.clone(),
                    kind,
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
/// nodes; `conftest.py` and production helpers reachable (over edges) only from
/// test nodes become [`Polarity::TestSupport`]; everything else is production.
fn classify_polarity(
    modules: &[ParsedModule],
    nodes: &[Node],
    local: &HashMap<SmolStr, HashMap<SmolStr, NodeId>>,
    exported: &[bool],
    edges: &[Edge],
) -> Vec<Polarity> {
    let mut polarity = vec![Polarity::Production; nodes.len()];

    let mut is_test_node = vec![false; nodes.len()];
    let mut is_support_node = vec![false; nodes.len()];
    for module in modules {
        let support = is_support_path(&module.path);
        let test = is_test_path(&module.path);
        if !support && !test {
            continue;
        }
        if let Some(table) = local.get(&module.path) {
            for &id in table.values() {
                let index = id.0 as usize;
                if support {
                    if let Some(slot) = polarity.get_mut(index) {
                        *slot = Polarity::TestSupport;
                    }
                    if let Some(slot) = is_support_node.get_mut(index) {
                        *slot = true;
                    }
                } else {
                    if let Some(slot) = polarity.get_mut(index) {
                        *slot = Polarity::TestCase;
                    }
                    if let Some(slot) = is_test_node.get_mut(index) {
                        *slot = true;
                    }
                }
            }
        }
    }

    // Reachability: a production helper consumed only by tests is TestSupport.
    let mut consumed_by_production = vec![false; nodes.len()];
    let mut consumed_by_test = vec![false; nodes.len()];
    for edge in edges {
        let from_test = is_test_node
            .get(edge.source.0 as usize)
            .copied()
            .unwrap_or(false)
            || is_support_node
                .get(edge.source.0 as usize)
                .copied()
                .unwrap_or(false);
        let table = if from_test {
            &mut consumed_by_test
        } else {
            &mut consumed_by_production
        };
        if let Some(slot) = table.get_mut(edge.target.0 as usize) {
            *slot = true;
        }
    }

    for (index, slot) in polarity.iter_mut().enumerate() {
        if *slot != Polarity::Production {
            continue;
        }
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

/// Returns `true` if `path` is a test case file by convention.
fn is_test_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.strip_suffix(".py").unwrap_or(file);
    stem.starts_with("test_")
        || stem.ends_with("_test")
        || (path.contains("tests/") && !is_support_path(path))
}

/// Returns `true` if `path` is test-support by convention (`conftest.py`).
fn is_support_path(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path) == "conftest.py"
}

/// Returns `true` if `path` is a package initializer (`__init__.py`).
fn is_package_init(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path) == "__init__.py"
}

/// Resolves Python import statements to canonical module paths.
struct Resolver {
    /// Dotted module name -> source file path.
    by_dotted: HashMap<SmolStr, SmolStr>,
    /// Source file path -> its dotted module name.
    dotted_of: HashMap<SmolStr, SmolStr>,
    /// Source file path -> its `__all__` public surface, when declared.
    surfaces: HashMap<SmolStr, HashSet<SmolStr>>,
}

impl Resolver {
    /// Builds a resolver over the known module set.
    fn new(modules: &[ParsedModule]) -> Self {
        let mut by_dotted = HashMap::new();
        let mut dotted_of = HashMap::new();
        let mut surfaces = HashMap::new();
        for module in modules {
            let dotted = dotted_name(&module.path);
            by_dotted.insert(dotted.clone(), module.path.clone());
            dotted_of.insert(module.path.clone(), dotted);
            if !module.dunder_all.is_empty() {
                surfaces.insert(
                    module.path.clone(),
                    module.dunder_all.iter().cloned().collect(),
                );
            }
        }
        Self {
            by_dotted,
            dotted_of,
            surfaces,
        }
    }

    /// Resolves an import statement issued from `importer` to a module path.
    ///
    /// Absolute imports resolve against the dotted-name table; relative imports
    /// resolve their leading-dot level against the importer's package.
    fn resolve(&self, importer: &str, import: &Import) -> Option<SmolStr> {
        let dotted = if import.level == 0 {
            import.module.to_string()
        } else {
            self.relative_dotted(importer, import)?
        };
        if dotted.is_empty() {
            return None;
        }
        self.by_dotted.get(dotted.as_str()).cloned()
    }

    /// Computes the absolute dotted name a relative import refers to.
    ///
    /// Level 1 (`from . import x`) is the importer's own package; each extra dot
    /// ascends one package. The `module` suffix, when present, is appended.
    fn relative_dotted(&self, importer: &str, import: &Import) -> Option<String> {
        let importer_dotted = self.dotted_of.get(importer)?;
        let mut segments: Vec<&str> = importer_dotted.split('.').collect();
        // A non-package module drops its own final segment to reach its package;
        // an `__init__.py` already names its package, so it keeps every segment.
        if !is_package_init(importer) {
            segments.pop();
        }
        // Each dot beyond the first ascends one further package level.
        for _ in 1..import.level {
            segments.pop()?;
        }
        if !import.module.is_empty() {
            segments.extend(import.module.split('.'));
        }
        Some(segments.join("."))
    }

    /// Returns the declared `__all__` surface of a module, if any.
    fn public_surface(&self, module: &SmolStr) -> Option<&HashSet<SmolStr>> {
        self.surfaces.get(module)
    }

    /// Resolves a dotted module name to its source file path, if known.
    fn module_of_dotted(&self, dotted: &SmolStr) -> Option<&SmolStr> {
        self.by_dotted.get(dotted)
    }
}

/// Converts a source file path to its dotted Python module name.
///
/// `pkg/sub/mod.py` becomes `pkg.sub.mod`; `pkg/sub/__init__.py` becomes
/// `pkg.sub` (the package the initializer represents).
fn dotted_name(path: &str) -> SmolStr {
    let without_extension = path.strip_suffix(".py").unwrap_or(path);
    let trimmed = without_extension
        .strip_suffix("/__init__")
        .unwrap_or(without_extension);
    SmolStr::new(trimmed.replace('/', "."))
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
    use crate::parse::Import;

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

        let fragment = bind(&[consumer, provider], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[module], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[module], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[consumer, provider], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[holder, builder], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[consumer, provider], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[barrel, provider], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[consumer, provider], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[consumer, provider], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[consumer, provider], Path::new("repo")).expect("bind succeeds");

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

        let fragment = bind(&[consumer], Path::new("repo")).expect("bind succeeds");

        let dynamic = fragment
            .edges
            .iter()
            .filter(|edge| edge.confidence.to_bits() == CONFIDENCE_DYNAMIC.to_bits())
            .count();
        assert_eq!(dynamic, 0, "an unknown module yields no edge");
    }

    #[test]
    fn should_convert_a_module_path_to_a_dotted_name() {
        assert_eq!(dotted_name("pkg/sub/mod.py"), SmolStr::new("pkg.sub.mod"));
        assert_eq!(dotted_name("pkg/sub/__init__.py"), SmolStr::new("pkg.sub"));
        assert_eq!(dotted_name("mod.py"), SmolStr::new("mod"));
    }

    #[test]
    fn should_recognise_test_paths_by_convention() {
        assert!(is_test_path("pkg/test_app.py"));
        assert!(is_test_path("pkg/app_test.py"));
        assert!(is_test_path("tests/test_thing.py"));
        assert!(!is_test_path("pkg/app.py"));
        assert!(is_support_path("tests/conftest.py"));
        assert!(!is_support_path("pkg/app.py"));
    }

    #[test]
    fn should_classify_conftest_helpers_as_test_support() {
        let mut support = module_at("tests/conftest.py");
        support.declarations.push(leaf("make_fixture"));

        let fragment = bind(&[support], Path::new("repo")).expect("bind succeeds");

        assert!(
            fragment
                .nodes
                .iter()
                .all(|node| node.polarity == Polarity::TestSupport)
        );
    }
}
