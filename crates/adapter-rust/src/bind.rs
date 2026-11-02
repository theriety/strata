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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use ra_ap_ide::{
    AnalysisHost, FilePosition, GotoDefinitionConfig, RaFixtureConfig, Semantics, SymbolKind,
    TextSize,
};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_project_model::{CargoConfig, RustLibSource};
use ra_ap_syntax::{
    AstNode as _, SyntaxNode, T,
    algo::previous_non_trivia_token,
    ast::{self, HasModuleItem as _, HasName as _},
};
use ra_ap_vfs::Vfs;
use smol_str::SmolStr;
use strata_ir::{
    Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness, IrFragment, Node, NodeId,
    NodeKind, Polarity, ScopeLevel, VisibilityScope,
};

use crate::parse::{
    DeclKind, Declaration, ParsedFile, ReExport, RefKind, Reference, VisibilityKind,
};

/// Confidence assigned to a soft re-export edge: the `pub use` binding is
/// statically certain even though it imposes no runtime dependency of its own.
const CONFIDENCE_REEXPORT: f64 = 1.0;

/// Confidence assigned to a statically resolved (non-macro) edge.
const CONFIDENCE_STATIC: f64 = 1.0;

/// Confidence assigned to a macro-expanded reference's edge: resolved, but with
/// honest uncertainty because the reference text is produced by a macro.
const CONFIDENCE_MACRO: f64 = 0.5;

/// Confidence assigned to an edge resolved only by the name-based fallback: the
/// semantic database could not pin the reference, but a uniquely-named
/// in-workspace declaration matches, so the dependency is likely but uncertain.
const CONFIDENCE_NAME_FALLBACK: f64 = 0.6;

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
pub fn bind(files: &[ParsedFile], manifest: &Path, root: &Path) -> Result<IrFragment, BindOutcome> {
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

/// Loads the cargo workspace into a rust-analyzer database, wrapping the loader
/// errors in [`BindOutcome`].
struct Database {
    /// The analysis host owning the loaded semantic database.
    host: AnalysisHost,
    /// The virtual file system mapping file ids to on-disk paths.
    vfs: Vfs,
}

/// A semantic target, including definitions outside the analyzed workspace.
enum ResolvedTarget {
    /// A definition that can be looked up in the snapshot's node assignment.
    /// `module` marks a module definition, whose range (a whole file, for a
    /// file module) says nothing about the declarations it happens to contain.
    Workspace {
        path: SmolStr,
        offset: u32,
        module: bool,
    },
    /// A known definition outside the workspace, never a name-fallback candidate.
    External,
}

impl Database {
    /// Loads the workspace whose manifest is at `manifest`.
    fn load(manifest: &Path) -> Result<Self, BindOutcome> {
        let cargo_config = CargoConfig {
            sysroot: Some(RustLibSource::Discover),
            ..CargoConfig::default()
        };
        let load_config = LoadCargoConfig {
            load_out_dirs_from_check: false,
            with_proc_macro_server: ProcMacroServerChoice::None,
            prefill_caches: false,
            num_worker_threads: 1,
            proc_macro_processes: 0,
        };
        let (db, vfs, _proc_macro) =
            load_workspace_at(manifest, &cargo_config, &load_config, &|_progress| {}).map_err(
                |error| BindOutcome::LoadFailed {
                    reason: error.to_string(),
                },
            )?;
        Ok(Self {
            host: AnalysisHost::with_database(db),
            vfs,
        })
    }

    /// Resolves the reference at `(relative_path, offset)` to the definition's
    /// workspace location or an external target. Only absent semantic targets
    /// return `None`; a known external definition must not trigger name fallback.
    fn resolve(&self, path: &str, offset: u32, workspace_root: &Path) -> Option<ResolvedTarget> {
        let file_id = self.file_id_for(path, workspace_root)?;
        let analysis = self.host.analysis();
        let position = FilePosition {
            file_id,
            offset: TextSize::new(offset),
        };
        let config = GotoDefinitionConfig {
            ra_fixture: RaFixtureConfig {
                disable_ra_fixture: true,
                ..RaFixtureConfig::default()
            },
        };
        let range_info = analysis.goto_definition(position, &config).ok()??;
        let target = range_info.info.into_iter().next()?;
        Some(self.relative_path(target.file_id, workspace_root).map_or(
            ResolvedTarget::External,
            |target_path| ResolvedTarget::Workspace {
                path: target_path,
                offset: u32::from(target.full_range.start()),
                module: target.kind == Some(SymbolKind::Module),
            },
        ))
    }

    /// Allows fallback only for a verified nonreceiver reference token. Token
    /// inspection also covers method names inside unexpanded macro arguments.
    fn allows_name_fallback(
        &self,
        path: &str,
        reference: &Reference,
        workspace_root: &Path,
    ) -> bool {
        let Some(file_id) = self.file_id_for(path, workspace_root) else {
            return false;
        };
        let Ok(parsed) = self.host.analysis().parse(file_id) else {
            return false;
        };
        let offset = TextSize::new(reference.offset);
        if !parsed.syntax().text_range().contains(offset) {
            return false;
        }
        let Some(token) = parsed.syntax().token_at_offset(offset).right_biased() else {
            return false;
        };
        token.text_range().start() == offset
            && token.text() == reference.name.as_str()
            && previous_non_trivia_token(token).is_none_or(|previous| previous.kind() != T![.])
    }

    /// Whether the path a qualifier belongs to starts inside the workspace, so a
    /// name fallback on the qualifier cannot capture a foreign type: in
    /// `std::io::Error::new` the qualifier `Error` must not bind to a workspace
    /// `Error`. A qualifier that leads its path (`Type::new`) keeps the fallback;
    /// a longer path needs a `crate`/`self`/`super`/`Self` root, a root the
    /// semantic database places in the workspace, or, when the database cannot
    /// resolve it, a root named after a workspace module file.
    fn roots_in_workspace(
        &self,
        path: &str,
        reference: &Reference,
        assignment: &NodeAssignment,
        workspace_root: &Path,
    ) -> bool {
        let Some(file_id) = self.file_id_for(path, workspace_root) else {
            return false;
        };
        let Ok(parsed) = self.host.analysis().parse(file_id) else {
            return false;
        };
        let offset = TextSize::new(reference.offset);
        if !parsed.syntax().text_range().contains(offset) {
            return false;
        }
        let Some(qualified) = parsed
            .syntax()
            .token_at_offset(offset)
            .right_biased()
            .and_then(|token| token.parent_ancestors().find_map(ast::Path::cast))
        else {
            return false;
        };
        if qualified.qualifier().is_none() {
            return true;
        }
        let Some(root) = qualified.first_segment() else {
            return false;
        };
        match root.kind() {
            Some(
                ast::PathSegmentKind::CrateKw
                | ast::PathSegmentKind::SelfKw
                | ast::PathSegmentKind::SuperKw
                | ast::PathSegmentKind::SelfTypeKw,
            ) => true,
            Some(ast::PathSegmentKind::Name(name)) if root.coloncolon_token().is_none() => {
                let start = u32::from(name.syntax().text_range().start());
                match self.resolve(path, start, workspace_root) {
                    Some(ResolvedTarget::Workspace { .. }) => true,
                    Some(ResolvedTarget::External) => false,
                    None => assignment.names_a_module(path, &name.text()),
                }
            }
            _ => false,
        }
    }

    /// Maps a repository-relative source path to its vfs file id.
    fn file_id_for(&self, path: &str, workspace_root: &Path) -> Option<ra_ap_ide::FileId> {
        let absolute = workspace_root.join(path);
        self.vfs.iter().find_map(|(file_id, vfs_path)| {
            let on_disk = vfs_path.as_path()?;
            (AsRef::<Path>::as_ref(on_disk) == absolute).then_some(file_id)
        })
    }

    /// Maps a vfs file id back to a repository-relative path within the
    /// workspace, or `None` for sysroot / out-of-tree files.
    fn relative_path(&self, file_id: ra_ap_ide::FileId, workspace_root: &Path) -> Option<SmolStr> {
        let vfs_path = self.vfs.file_path(file_id);
        let on_disk = vfs_path.as_path()?;
        let raw: &Path = on_disk.as_ref();
        let relative = raw.strip_prefix(workspace_root).ok()?;
        Some(SmolStr::new(relative.to_string_lossy().replace('\\', "/")))
    }

    /// Resolves a restricted path to the complete analyzed module subtree.
    fn visibility_files(
        &self,
        path: &str,
        offset: u32,
        visibility_path: &str,
        repository_root: &Path,
        parsed_paths: &BTreeSet<SmolStr>,
    ) -> Option<Vec<SmolStr>> {
        let file_id = self.file_id_for(path, repository_root)?;
        let semantics = Semantics::new(self.host.raw_database());
        if semantics.file_to_module_defs(file_id).count() != 1 {
            return None;
        }

        let parsed = semantics.parse_guess_edition(file_id);
        let position = TextSize::new(offset);
        if !parsed.syntax().text_range().contains(position) {
            return None;
        }
        let token = parsed.syntax().token_at_offset(position).right_biased()?;
        if token.kind().is_trivia() || !token.text_range().contains(position) {
            return None;
        }
        let scope_node = token.parent()?;
        let current = semantics.scope_at_offset(&scope_node, position)?.module();
        let mut target = current;
        for (index, segment) in visibility_path.split("::").enumerate() {
            match segment {
                "crate" if index == 0 => target = current.crate_root(semantics.db),
                "self" if index == 0 => {}
                "super" => target = target.parent(semantics.db)?,
                "crate" | "self" | "" => return None,
                name => {
                    let expected = name.strip_prefix("r#").unwrap_or(name);
                    let child = {
                        let mut matches = target.children(semantics.db).filter(|child| {
                            child
                                .name(semantics.db)
                                .is_some_and(|child_name| child_name.as_str() == expected)
                        });
                        let only = matches.next()?;
                        if matches.next().is_some() {
                            return None;
                        }
                        only
                    };
                    target = child;
                }
            }
        }
        if !current.path_to_root(semantics.db).contains(&target) {
            return None;
        }

        let mut files = BTreeSet::new();
        let mut pending = vec![target];
        while let Some(module) = pending.pop() {
            let definition = semantics.module_definition_node(module);
            let original = semantics.original_range_opt(&definition.value)?;
            let relative =
                self.relative_path(original.file_id.file_id(semantics.db), repository_root)?;
            if !parsed_paths.contains(&relative) {
                return None;
            }

            let children = module.children(semantics.db).collect::<Vec<_>>();
            let mut semantic_names = children
                .iter()
                .map(|child| {
                    child
                        .name(semantics.db)
                        .map(|name| name.as_str().to_owned())
                })
                .collect::<Option<Vec<_>>>()?;
            let mut syntactic_names = direct_module_names(&definition.value)?;
            semantic_names.sort();
            syntactic_names.sort();
            if semantic_names != syntactic_names {
                return None;
            }

            files.insert(relative);
            pending.extend(children);
        }
        (!files.is_empty()).then(|| files.into_iter().collect())
    }
}

/// Returns direct syntactic child-module names for a module definition.
fn direct_module_names(node: &SyntaxNode) -> Option<Vec<String>> {
    if let Some(source) = ast::SourceFile::cast(node.clone()) {
        return module_item_names(source.items());
    }
    let module = ast::Module::cast(node.clone())?;
    module_item_names(module.item_list()?.items())
}

/// Collects module names from one syntactic module-item list.
fn module_item_names(items: impl Iterator<Item = ast::Item>) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for item in items {
        if let ast::Item::Module(module) = item {
            let name = module.name()?.text().to_string();
            names.push(name.strip_prefix("r#").unwrap_or(&name).to_owned());
        }
    }
    Some(names)
}

/// Resolves every restricted declaration and re-export into an IR sidecar.
fn resolve_visibility_scopes(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    database: &Database,
    repository_root: &Path,
) -> Vec<VisibilityScope> {
    let parsed_paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let mut scopes = Vec::new();
    for file in files {
        for declaration in &file.declarations {
            let Some((node, visibility_path)) = assignment
                .node_at(&file.path, declaration.byte_start)
                .zip(declaration.visibility_path.as_deref())
                .filter(|_| declaration.visibility_kind == VisibilityKind::Restricted)
            else {
                continue;
            };
            if let Some(scope_files) = database.visibility_files(
                &file.path,
                declaration.byte_start,
                visibility_path,
                repository_root,
                &parsed_paths,
            ) {
                scopes.push(VisibilityScope {
                    node,
                    files: scope_files,
                });
            }
        }
        for re_export in &file.re_exports {
            let Some((node, visibility_path)) = assignment
                .re_export_node_at(&file.path, re_export.offset)
                .zip(re_export.visibility_path.as_deref())
                .filter(|_| re_export.visibility_kind == VisibilityKind::Restricted)
            else {
                continue;
            };
            if let Some(scope_files) = database.visibility_files(
                &file.path,
                re_export.offset,
                visibility_path,
                repository_root,
                &parsed_paths,
            ) {
                scopes.push(VisibilityScope {
                    node,
                    files: scope_files,
                });
            }
        }
    }
    scopes
}

/// Resolves every recorded reference into the typed edge set.
fn resolve_edges(
    files: &[ParsedFile],
    assignment: &NodeAssignment,
    database: &Database,
    workspace_root: &Path,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    for file in files {
        for declaration in &file.declarations {
            let Some(source) = assignment.node_at(&file.path, declaration.byte_start) else {
                continue;
            };
            for reference in &declaration.references {
                resolve_reference(
                    source,
                    &file.path,
                    reference,
                    assignment,
                    database,
                    workspace_root,
                    &mut edges,
                );
            }
        }
    }
    edges
}

/// Resolves a single reference and appends its edge when it lands on a node.
fn resolve_reference(
    source: NodeId,
    path: &str,
    reference: &Reference,
    assignment: &NodeAssignment,
    database: &Database,
    workspace_root: &Path,
    edges: &mut Vec<Edge>,
) {
    let (kind, hardness) = edge_shape(reference.kind);

    // a known definition stays authoritative even when absent from this snapshot.
    if let Some(resolved) = database.resolve(path, reference.offset, workspace_root) {
        if let ResolvedTarget::Workspace {
            path: target_path,
            offset: target_offset,
            module,
        } = resolved
            && !(module && reference.kind == RefKind::Qualifier)
            && let Some(target) = assignment.node_at(&target_path, target_offset)
            && binds(reference.kind, assignment, target)
        {
            let confidence = if reference.macro_expanded {
                CONFIDENCE_MACRO
            } else {
                CONFIDENCE_STATIC
            };
            push_edge(edges, source, target, kind, hardness, confidence);
        }
        return;
    }

    // unresolved receiver calls require type information; a matching global
    // name alone cannot identify their target.
    if database.allows_name_fallback(path, reference, workspace_root)
        && (reference.kind != RefKind::Qualifier
            || database.roots_in_workspace(path, reference, assignment, workspace_root))
        && let Some(target) = assignment.unique_node_named(&reference.name)
        && binds(reference.kind, assignment, target)
    {
        push_edge(
            edges,
            source,
            target,
            kind,
            Hardness::Soft,
            CONFIDENCE_NAME_FALLBACK,
        );
    }
}

/// Whether a reference of `kind` may bind to `target`. A path qualifier is a
/// type reference only when it names a type; landing on a value (a module file
/// whose first item is a function, or a same-named function reached by name
/// fallback) is not a dependency on a type, so no edge is emitted.
fn binds(kind: RefKind, assignment: &NodeAssignment, target: NodeId) -> bool {
    kind != RefKind::Qualifier || assignment.kind_of(target) == Some(NodeKind::Type)
}

/// Resolves every `pub use` re-export to its original declaration and emits a
/// soft [`EdgeKind::ReExport`] edge from the barrel-local node to that original.
///
/// The edge is emitted as-is — chain flattening is the engine's job — and is soft
/// because a re-export imposes no runtime dependency of its own.
fn resolve_re_exports(
    assignment: &NodeAssignment,
    database: &Database,
    workspace_root: &Path,
    edges: &mut Vec<Edge>,
) {
    for link in &assignment.re_export_links {
        let Some(ResolvedTarget::Workspace { path, offset, .. }) =
            database.resolve(&link.path, link.offset, workspace_root)
        else {
            continue;
        };
        let Some(target) = assignment.node_at(&path, offset) else {
            continue;
        };
        push_edge(
            edges,
            link.source,
            target,
            EdgeKind::ReExport,
            Hardness::Soft,
            CONFIDENCE_REEXPORT,
        );
    }
}

/// Reclassifies node polarity into the three-valued scheme.
///
/// `#[test]` functions are already [`Polarity::TestCase`] and other `#[cfg(test)]`
/// items already [`Polarity::TestSupport`] from [`node_for`]; declarations under a
/// `tests/` directory are promoted to test cases here, since cargo compiles that
/// tree only under `cfg(test)`. Finally, a *production* node consumed only by test
/// nodes — never reachable from production — is a shared test utility and becomes
/// [`Polarity::TestSupport`]; an exported symbol stays production, as it is part
/// of the public surface regardless of who happens to use it in-tree.
fn classify_polarity(files: &[ParsedFile], nodes: &mut [Node], edges: &[Edge]) {
    let test_containers = test_file_containers(files);
    for node in nodes.iter_mut() {
        if test_containers.contains(&node.container) {
            node.polarity = Polarity::TestCase;
        }
    }

    // Both test cases and test-support already carry test polarity, so anything
    // they consume is a test consumer for the reachability rule below.
    let is_test_node: Vec<bool> = nodes
        .iter()
        .map(|node| node.polarity != Polarity::Production)
        .collect();

    let mut consumed_by_production = vec![false; nodes.len()];
    let mut consumed_by_test = vec![false; nodes.len()];
    for edge in edges {
        let from_test = is_test_node
            .get(edge.source.0 as usize)
            .copied()
            .unwrap_or(false);
        let bucket = if from_test {
            &mut consumed_by_test
        } else {
            &mut consumed_by_production
        };
        if let Some(slot) = bucket.get_mut(edge.target.0 as usize) {
            *slot = true;
        }
    }

    for (index, node) in nodes.iter_mut().enumerate() {
        // Only production nodes are candidates for promotion to test support;
        // nodes already in the test zone keep their polarity.
        if node.polarity != Polarity::Production {
            continue;
        }
        // An exported symbol is part of the production API surface; only a
        // private helper consumed solely by tests is genuine test support.
        if node.visibility >= ScopeLevel::Package {
            continue;
        }
        let production = consumed_by_production.get(index).copied().unwrap_or(false);
        let test = consumed_by_test.get(index).copied().unwrap_or(false);
        if test && !production {
            node.polarity = Polarity::TestSupport;
        }
    }
}

/// Returns the set of leaf container ids for files that live under a `tests/`
/// directory, recomputed via [`ContainerBuilder`] so it matches assignment.
fn test_file_containers(files: &[ParsedFile]) -> std::collections::HashSet<ContainerId> {
    let builder = ContainerBuilder::build(files, Path::new(""));
    files
        .iter()
        .filter(|file| is_test_path(&file.path))
        .map(|file| builder.file_of(&file.path))
        .collect()
}

/// Returns `true` when a repository-relative path is a test path by convention.
///
/// A path is a test path when any segment is exactly `tests` — cargo's integration
/// test directory — so `crates/app/tests/it.rs` classifies as test code.
fn is_test_path(path: &str) -> bool {
    path.split('/').any(|segment| segment == "tests")
}

/// Maps a reference category to its emitted edge kind and hardness.
///
/// Rust types constrain compilation, so type references are hard; only erased
/// positions would be soft, and the parser does not surface those distinctly, so
/// every emitted type reference is hard here.
fn edge_shape(kind: RefKind) -> (EdgeKind, Hardness) {
    match kind {
        RefKind::UsePath => (EdgeKind::ValueImport, Hardness::Hard),
        RefKind::Call => (EdgeKind::Call, Hardness::Hard),
        RefKind::TypeRef | RefKind::Qualifier => (EdgeKind::TypeReference, Hardness::Hard),
        RefKind::TraitImpl => (EdgeKind::Inheritance, Hardness::Hard),
    }
}

/// Collapses duplicate edges into one per `(source, target, kind)`, keeping the
/// strongest: a hard edge dominates a soft one, and within the same hardness the
/// higher confidence wins. The result is ordered deterministically by endpoints
/// and kind, so a reference cited many ways contributes a single, meaningful
/// edge rather than an inflated weight.
fn dedup_edges(edges: Vec<Edge>) -> Vec<Edge> {
    use std::collections::btree_map::Entry;
    let mut best: BTreeMap<(u32, u32, u8), Edge> = BTreeMap::new();
    for edge in edges {
        let key = (edge.source.0, edge.target.0, edge_kind_rank(edge.kind));
        match best.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(edge);
            }
            Entry::Occupied(mut slot) => {
                if is_stronger(&edge, slot.get()) {
                    slot.insert(edge);
                }
            }
        }
    }
    best.into_values().collect()
}

/// Returns `true` when `candidate` is a stronger edge than `current`: a hard edge
/// beats a soft one, and within equal hardness a higher confidence beats a lower.
fn is_stronger(candidate: &Edge, current: &Edge) -> bool {
    let candidate_hard = candidate.hardness == Hardness::Hard;
    let current_hard = current.hardness == Hardness::Hard;
    if candidate_hard != current_hard {
        return candidate_hard;
    }
    candidate.confidence > current.confidence
}

/// Maps an edge kind to a stable rank, so `(source, target, kind)` keys order
/// deterministically without relying on an `Ord` impl for [`EdgeKind`].
fn edge_kind_rank(kind: EdgeKind) -> u8 {
    match kind {
        EdgeKind::ValueImport => 0,
        EdgeKind::TypeReference => 1,
        EdgeKind::Inheritance => 2,
        EdgeKind::Call => 3,
        EdgeKind::ReExport => 4,
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

/// Assigns a dense node id to every declaration and indexes them by location so
/// resolved definition offsets can be mapped back to nodes.
struct NodeAssignment {
    /// Nodes in assignment order; the index equals the [`NodeId`] value.
    nodes: Vec<Node>,
    /// Per-file declaration intervals, sorted by start, for offset lookup.
    intervals: HashMap<SmolStr, Vec<Interval>>,
    /// Declaration name -> the node(s) declaring it, for the name-based
    /// resolution fallback. A name mapping to exactly one node is resolvable;
    /// an ambiguous name (two or more) is left unresolved to stay deterministic.
    by_name: HashMap<SmolStr, Vec<NodeId>>,
    /// Barrel-local nodes materialized for `pub use` re-exports, each carrying
    /// the source location whose `goto_definition` finds the original symbol.
    re_export_links: Vec<ReExportLink>,
}

/// A materialized `pub use` re-export: the barrel-local node and the source
/// location [`crate::bind`] resolves to find the re-exported original.
struct ReExportLink {
    /// The node created in the re-exporting file for the re-exported name.
    source: NodeId,
    /// Repository-relative path of the file that holds the `pub use`.
    path: SmolStr,
    /// Byte offset of the re-exported leaf identifier, for `goto_definition`.
    offset: u32,
}

/// One declaration's byte interval and the node it was assigned.
struct Interval {
    /// Inclusive lower byte bound.
    start: u32,
    /// Exclusive upper byte bound.
    end: u32,
    /// The node assigned to the declaration.
    node: NodeId,
}

impl NodeAssignment {
    /// Builds the node set and location index from the parsed files.
    fn build(files: &[ParsedFile], root: &Path) -> Self {
        let containers = ContainerBuilder::build(files, root);
        let mut nodes = Vec::new();
        let mut intervals: HashMap<SmolStr, Vec<Interval>> = HashMap::new();
        let mut by_name: HashMap<SmolStr, Vec<NodeId>> = HashMap::new();
        let mut re_export_links = Vec::new();

        for file in files {
            let container = containers.file_of(&file.path);
            let file_intervals = intervals.entry(file.path.clone()).or_default();
            for declaration in &file.declarations {
                let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
                nodes.push(node_for(declaration, id, container));
                file_intervals.push(Interval {
                    start: declaration.byte_start,
                    end: declaration.byte_end,
                    node: id,
                });
                by_name
                    .entry(declaration.name.clone())
                    .or_default()
                    .push(id);
            }
        }
        for file_intervals in intervals.values_mut() {
            file_intervals.sort_by_key(|interval| interval.start);
        }

        // Barrel nodes for `pub use` re-exports come after all declarations so
        // declaration node ids stay dense and offset lookups are unaffected.
        for file in files {
            let container = containers.file_of(&file.path);
            for re_export in &file.re_exports {
                let id = NodeId(u32::try_from(nodes.len()).unwrap_or(u32::MAX));
                nodes.push(re_export_node(re_export, id, container));
                re_export_links.push(ReExportLink {
                    source: id,
                    path: file.path.clone(),
                    offset: re_export.offset,
                });
            }
        }

        Self {
            nodes,
            intervals,
            by_name,
            re_export_links,
        }
    }

    /// Returns the node whose declaration interval contains `offset` in `path`.
    ///
    /// When intervals nest (a method inside an impl, say) the innermost — the
    /// one with the latest start that still contains the offset — wins, so a
    /// resolution lands on the most specific declaration.
    fn node_at(&self, path: &str, offset: u32) -> Option<NodeId> {
        let file_intervals = self.intervals.get(path)?;
        file_intervals
            .iter()
            .filter(|interval| interval.start <= offset && offset < interval.end)
            .max_by_key(|interval| interval.start)
            .map(|interval| interval.node)
    }

    /// Returns the barrel node materialized at one re-export source location.
    fn re_export_node_at(&self, path: &str, offset: u32) -> Option<NodeId> {
        let mut matches = self
            .re_export_links
            .iter()
            .filter(|link| link.path == path && link.offset == offset)
            .map(|link| link.source);
        let node = matches.next()?;
        matches.next().is_none().then_some(node)
    }

    /// Returns the kind of the node assigned `id`.
    fn kind_of(&self, id: NodeId) -> Option<NodeKind> {
        let index = usize::try_from(id.0).ok()?;
        self.nodes.get(index).map(|node| node.kind)
    }

    /// Whether a source file in the same crate as `from` declares a module
    /// called `name`: the file stem, or the directory holding a `mod.rs`. A
    /// same-named file in another crate cannot be what `from` refers to.
    fn names_a_module(&self, from: &str, name: &str) -> bool {
        let crate_src = crate_src_dir(from);
        self.intervals.keys().any(|file| {
            if crate_src_dir(file) != crate_src {
                return false;
            }
            let file = Path::new(file.as_str());
            let stem = file.file_stem().and_then(|stem| stem.to_str());
            let module = match stem {
                Some("mod") => file
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|folder| folder.to_str()),
                other => other,
            };
            module == Some(name)
        })
    }

    /// Returns the single in-workspace declaration named `name`, or `None` when
    /// no declaration or more than one carries that name. Ambiguous names are
    /// deliberately left unresolved so the fallback never invents an edge.
    fn unique_node_named(&self, name: &str) -> Option<NodeId> {
        match self.by_name.get(name)?.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Consumes the assignment, yielding the assembled node set.
    fn into_nodes(self) -> Vec<Node> {
        self.nodes
    }
}

/// The repository-relative prefix up to a path's first `src` directory, which
/// identifies the crate owning a source file; a path with no `src` directory
/// yields the empty prefix and so shares one scope with every other such path.
/// The scope is the crate's `src` prefix, not the module path: two files in
/// different modules of one crate still count as the same scope.
fn crate_src_dir(path: &str) -> &str {
    path.split_once("/src/")
        .map_or("", |(crate_dir, _)| crate_dir)
}

/// Builds the container tree for the parsed files (free function so the node
/// builder and the public emitter share one implementation).
fn assignment_containers(files: &[ParsedFile], root: &Path) -> Vec<Container> {
    ContainerBuilder::build(files, root)
        .tree
        .containers()
        .to_vec()
}

/// Builds an IR [`Node`] for a declaration, fixing its base polarity.
///
/// A `#[test]`-attributed function is a [`Polarity::TestCase`] — the entry point
/// of a test. Any other `#[cfg(test)]` item is a shared test utility,
/// [`Polarity::TestSupport`]; everything else starts as [`Polarity::Production`].
/// [`classify_polarity`] refines this with cross-node reachability afterwards.
/// Test-zoned nodes carry zero production SLOC.
fn node_for(declaration: &Declaration, id: NodeId, container: ContainerId) -> Node {
    let polarity = if declaration.test_case {
        Polarity::TestCase
    } else if declaration.cfg_test {
        Polarity::TestSupport
    } else {
        Polarity::Production
    };
    Node {
        id,
        name: declaration.name.clone(),
        kind: match declaration.kind {
            DeclKind::Symbol => NodeKind::Symbol,
            DeclKind::Type => NodeKind::Type,
        },
        polarity,
        container,
        visibility: declared_visibility(declaration.visibility_kind, declaration.exported),
        effective_size: declaration.sloc,
        re_export: false,
    }
}

/// Builds the barrel-local IR [`Node`] for a `pub use` re-export.
///
/// The node carries the re-exported name within the re-exporting file's
/// container, is `pub`-visible (a re-export is always part of the export
/// surface), and has zero size — it owns no source of its own.
fn re_export_node(re_export: &ReExport, id: NodeId, container: ContainerId) -> Node {
    Node {
        id,
        name: re_export.name.clone(),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container,
        visibility: declared_visibility(re_export.visibility_kind, true),
        effective_size: 0,
        re_export: true,
    }
}

/// Maps source visibility to the conservative pre-merge IR scope.
fn declared_visibility(kind: VisibilityKind, legacy_exported: bool) -> ScopeLevel {
    match kind {
        VisibilityKind::Legacy if legacy_exported => ScopeLevel::Package,
        VisibilityKind::Legacy | VisibilityKind::Inherited => ScopeLevel::File,
        VisibilityKind::Public | VisibilityKind::Crate | VisibilityKind::Restricted => {
            ScopeLevel::Package
        }
    }
}

/// Interns containers (file/folder/domain/package/package group) for the files.
struct ContainerBuilder {
    /// The interned container tree.
    tree: ContainerTree,
    /// File path -> its file container id.
    files: HashMap<SmolStr, ContainerId>,
}

impl ContainerBuilder {
    /// Returns the file container id for a file path.
    fn file_of(&self, path: &SmolStr) -> ContainerId {
        self.files.get(path).copied().unwrap_or(ContainerId(0))
    }

    /// Builds the laminar container tree for `files` under `root`.
    ///
    /// Levels follow the fixed five-level mapping: the repository is the package
    /// group, the first path segment a package, the next two directory levels
    /// domain and folder, and the file itself a leaf. Shallow paths reuse a
    /// single implicit container at each missing level so every file still hangs
    /// off a valid, strictly-ascending chain.
    fn build(files: &[ParsedFile], root: &Path) -> Self {
        let root_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(|| SmolStr::new("root"), SmolStr::new);

        let mut containers: Vec<Container> = Vec::new();
        let mut by_key: BTreeMap<(ScopeLevel, SmolStr), ContainerId> = BTreeMap::new();
        let mut file_ids = HashMap::new();

        let group = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::PackageGroup,
            root_name,
            None,
        );

        let mut paths: Vec<&SmolStr> = files.iter().map(|file| &file.path).collect();
        paths.sort();
        paths.dedup();

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
            file_ids.insert(path.clone(), file);
        }

        Self {
            tree: ContainerTree::new(containers),
            files: file_ids,
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
    use crate::parse::Declaration;

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
    /// re-export (ADR-0020) whether or not a target ever resolves.
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

    #[test]
    fn should_map_a_bare_package_specifier_edge_shape() {
        assert_eq!(
            edge_shape(RefKind::UsePath),
            (EdgeKind::ValueImport, Hardness::Hard)
        );
        assert_eq!(edge_shape(RefKind::Call), (EdgeKind::Call, Hardness::Hard));
        assert_eq!(
            edge_shape(RefKind::TypeRef),
            (EdgeKind::TypeReference, Hardness::Hard)
        );
        assert_eq!(
            edge_shape(RefKind::Qualifier),
            (EdgeKind::TypeReference, Hardness::Hard)
        );
        assert_eq!(
            edge_shape(RefKind::TraitImpl),
            (EdgeKind::Inheritance, Hardness::Hard)
        );
    }
}
