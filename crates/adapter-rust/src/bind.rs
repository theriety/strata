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

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use ra_ap_ide::{AnalysisHost, FilePosition, GotoDefinitionConfig, RaFixtureConfig, TextSize};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_project_model::{CargoConfig, RustLibSource};
use ra_ap_vfs::Vfs;
use smol_str::SmolStr;
use strata_ir::{
    Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness, IrFragment, Node, NodeId,
    NodeKind, Polarity, ScopeLevel,
};

use crate::parse::{DeclKind, Declaration, ParsedFile, RefKind, Reference};

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
    let edges = dedup_edges(edges);
    let mut nodes = assignment.into_nodes();
    classify_polarity(files, &mut nodes, &edges);

    Ok(IrFragment {
        nodes,
        edges,
        containers: assignment_containers(files, root),
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
    /// repository-relative path and byte offset, if `goto_definition` lands on a
    /// known in-workspace target.
    fn resolve(&self, path: &str, offset: u32, workspace_root: &Path) -> Option<(SmolStr, u32)> {
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
        let target_path = self.relative_path(target.file_id, workspace_root)?;
        let target_offset = u32::from(target.full_range.start());
        Some((target_path, target_offset))
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

    // The semantic database is authoritative: when `goto_definition` lands on a
    // known declaration, emit that edge with full (or macro-reduced) confidence.
    if let Some((target_path, target_offset)) =
        database.resolve(path, reference.offset, workspace_root)
        && let Some(target) = assignment.node_at(&target_path, target_offset)
    {
        let confidence = if reference.macro_expanded {
            CONFIDENCE_MACRO
        } else {
            CONFIDENCE_STATIC
        };
        push_edge(edges, source, target, kind, hardness, confidence);
        return;
    }

    // Fallback: when resolution comes up empty (intra-crate references RA cannot
    // pin, module paths, out-of-tree noise), a uniquely-named in-workspace
    // declaration is a likely target — emitted soft, at reduced confidence.
    if let Some(target) = assignment.unique_node_named(&reference.name) {
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
        let Some((target_path, target_offset)) =
            database.resolve(&link.path, link.offset, workspace_root)
        else {
            continue;
        };
        let Some(target) = assignment.node_at(&target_path, target_offset) else {
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
        RefKind::TypeRef => (EdgeKind::TypeReference, Hardness::Hard),
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
                nodes.push(re_export_node(&re_export.name, id, container));
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
        visibility: if declaration.exported {
            ScopeLevel::Package
        } else {
            ScopeLevel::File
        },
        effective_size: declaration.sloc,
    }
}

/// Builds the barrel-local IR [`Node`] for a `pub use` re-export.
///
/// The node carries the re-exported name within the re-exporting file's
/// container, is `pub`-visible (a re-export is always part of the export
/// surface), and has zero size — it owns no source of its own.
fn re_export_node(name: &SmolStr, id: NodeId, container: ContainerId) -> Node {
    Node {
        id,
        name: name.clone(),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container,
        visibility: ScopeLevel::Package,
        effective_size: 0,
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
        };
        let test_case = Node {
            id: NodeId(1),
            name: SmolStr::new("a_test"),
            kind: NodeKind::Symbol,
            polarity: Polarity::TestCase,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 0,
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
        };
        let producer = Node {
            id: NodeId(1),
            name: SmolStr::new("run"),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
        };
        let test_case = Node {
            id: NodeId(2),
            name: SmolStr::new("a_test"),
            kind: NodeKind::Symbol,
            polarity: Polarity::TestCase,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 0,
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
    /// the node set even before its target is resolved.
    #[test]
    fn should_materialize_a_barrel_node_for_a_pub_use_re_export() {
        let file = ParsedFile {
            path: SmolStr::new("crates/app/src/lib.rs"),
            declarations: Vec::new(),
            re_exports: vec![crate::parse::ReExport {
                name: SmolStr::new("Measure"),
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
            edge_shape(RefKind::TraitImpl),
            (EdgeKind::Inheritance, Hardness::Hard)
        );
    }
}
