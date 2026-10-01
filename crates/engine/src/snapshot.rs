//! Source discovery, adapter dispatch, fragment merging, and snapshot assembly.
//!
//! [`snapshot_from_root`] is the engine's only filesystem entry point. It globs
//! sources under a root (honoring the config's include / exclude patterns),
//! groups files by language, dispatches each language's [`Adapter`] in parallel
//! (rayon), then merges the resulting [`IrFragment`]s into one namespace:
//! per-adapter node and container ids are re-interned to a single dense range so
//! cross-fragment edges resolve. Re-export edges are flattened to their original
//! definitions under a depth guard, and the merged IR is handed to
//! [`Snapshot::assemble`](strata_ir::Snapshot::assemble) for validation and
//! content hashing.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rayon::prelude::*;
use smol_str::SmolStr;
use strata_adapter_python::PythonAdapter;
use strata_adapter_rust::RustAdapter;
use strata_adapter_typescript::TypeScriptAdapter;
use strata_ir::{
    Adapter, ContainerId, ContainerTree, Edge, EdgeKind, IntermediateRepresentation, IrFragment,
    Layout, Node, NodeId, ScopeLadder, ScopeLevel, SourceFile, VisibilityScope, build_laminar_tree,
};

use crate::config::AnalyzeConfig;
use crate::error::StrataError;

/// The maximum re-export chain depth before flattening gives up.
///
/// A legitimate barrel chain is shallow; a chain deeper than this is almost
/// always a cycle, so the guard raises [`StrataError::ReExportDepthExceeded`]
/// rather than looping.
const RE_EXPORT_DEPTH_LIMIT: u32 = 64;

/// A language Strata can analyze, with its file extensions and adapter factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Language {
    /// TypeScript / TSX.
    TypeScript,
    /// Rust.
    Rust,
    /// Python.
    Python,
}

impl Language {
    /// Every supported language, for extension-based classification.
    pub(crate) const ALL: [Self; 3] = [Self::TypeScript, Self::Rust, Self::Python];

    /// Resolves a config language name to a [`Language`], if recognized.
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "typescript" => Some(Self::TypeScript),
            "rust" => Some(Self::Rust),
            "python" => Some(Self::Python),
            _ => None,
        }
    }

    /// Returns the config-facing name of this language.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Rust => "rust",
            Self::Python => "python",
        }
    }

    /// Returns whether `path` belongs to this language by extension.
    pub(crate) fn matches_extension(self, path: &str) -> bool {
        let extension = path.rsplit('.').next().unwrap_or("");
        match self {
            Self::TypeScript => matches!(extension, "ts" | "tsx" | "mts" | "cts"),
            Self::Rust => extension == "rs",
            Self::Python => matches!(extension, "py" | "pyi"),
        }
    }

    /// Whether `path` names a file the language's module system is built
    /// around, so emptying or retiring it breaks the module structure: a Rust
    /// crate or module root (`lib.rs`, `main.rs`, `mod.rs`), a TypeScript
    /// directory index (`index.ts` and its variants), or a Python package
    /// marker or entry point (`__init__.py`, `__main__.py`).
    pub(crate) fn is_module_root(path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        Self::ALL.iter().any(|language| {
            language.matches_extension(name)
                && match language {
                    Self::TypeScript => {
                        matches!(name, "index.ts" | "index.tsx" | "index.mts" | "index.cts")
                    }
                    Self::Rust => matches!(name, "lib.rs" | "main.rs" | "mod.rs"),
                    Self::Python => matches!(
                        name,
                        "__init__.py" | "__init__.pyi" | "__main__.py" | "__main__.pyi"
                    ),
                }
        })
    }

    /// Builds the adapter for this language anchored at `root`.
    fn adapter(self, root: &Path) -> Box<dyn Adapter + Send + Sync> {
        match self {
            Self::TypeScript => Box::new(TypeScriptAdapter::new(root)),
            Self::Rust => Box::new(RustAdapter::new(root.join("Cargo.toml"))),
            Self::Python => Box::new(PythonAdapter::new(root)),
        }
    }
}

/// Discovers sources under `root`, runs the enabled adapters, merges their IR
/// fragments, flattens re-export chains, and assembles a validated, hashed
/// [`Snapshot`].
///
/// File discovery honors `config.adapters.include` and `.exclude` globs relative
/// to `root`, plus any `.gitignore` files under `root`; `.git` and
/// `node_modules` directories are always skipped. Files are grouped by language
/// and the adapters run in parallel. Merging re-interns each fragment's node and
/// container ids into one dense namespace before assembly.
///
/// [`Snapshot`]: strata_ir::Snapshot
///
/// # Errors
///
/// Returns [`StrataError::InputUnreadable`] when discovery cannot read the tree,
/// [`StrataError::AdapterParseFailure`] / [`StrataError::AdapterBindFailure`] on
/// adapter failure, [`StrataError::ReExportDepthExceeded`] on a pathological
/// barrel chain, and [`StrataError::SnapshotInvalid`] when assembly rejects the
/// merged IR.
pub fn snapshot_from_root(
    root: impl AsRef<Path>,
    config: &AnalyzeConfig,
) -> Result<strata_ir::Snapshot, StrataError> {
    let root = root.as_ref();
    // rust-analyzer canonicalizes its VFS to an absolute path, so canonicalize the
    // root once here — the engine's only filesystem entry point — and every
    // downstream consumer (discovery, the group-naming path below, and the
    // adapter VFS bind) sees the same canonical path. Fail loud on failure: a root
    // that cannot be canonicalized (missing, unreadable, a broken symlink) is
    // exactly when the VFS would not match, so falling back to the raw path would
    // silently degrade semantic-edge resolution instead of surfacing the bad root.
    let root = std::fs::canonicalize(root).map_err(|error| StrataError::InputUnreadable {
        path: root.to_path_buf(),
        reason: error.to_string(),
    })?;
    let root = root.as_path();
    let languages = enabled_languages(config);
    let discovery = discover_sources(root, config, &languages)?;

    let grouped = group_by_language(&discovery.sources, &languages);
    let fragments = grouped
        .into_par_iter()
        .map(|(language, sources)| run_adapter(language, &sources, root))
        .collect::<Result<Vec<_>, _>>()?;

    // the package group is named after the repository directory; package and
    // source-root boundaries come from the discovered manifests and config.
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let layout = Layout {
        package_roots: discovery.package_roots,
        source_roots: config
            .adapters
            .source_roots
            .iter()
            .map(SmolStr::new)
            .collect(),
    };
    let merged = merge_fragments(fragments, root_name, &layout);
    let flattened = flatten_re_exports(merged)?;

    strata_ir::Snapshot::assemble(flattened).map_err(StrataError::from)
}

/// Maps the config's language names to [`Language`] values, dropping any the
/// engine does not recognize (validation rejects unknown names upstream).
fn enabled_languages(config: &AnalyzeConfig) -> Vec<Language> {
    config
        .adapters
        .languages
        .iter()
        .filter_map(|name| Language::from_name(name))
        .collect()
}

/// Reads every file under `root` that matches the include globs, clears the
/// exclude globs, survives the repo's own `.gitignore` rules, and is claimed by
/// an enabled language, returning the source set as repo-relative paths plus
/// contents.
///
/// `.gitignore` files under `root` are honored even outside a git checkout, so
/// build output never pollutes the snapshot; only rules inside `root` apply —
/// no parent, global, or `.git/info/exclude` sources — keeping the same tree
/// deterministic across machines. `.git` and `node_modules` directories are
/// skipped unconditionally, independent of the configurable exclude globs.
///
/// Extension filtering happens here, *before* any file is read, so discovery
/// never touches non-source files (binaries, lockfiles, images, VCS metadata).
/// This keeps a file like `.git/index` — invalid UTF-8 — from aborting the walk:
/// it matches no enabled language and is skipped silently.
fn discover_sources(
    root: &Path,
    config: &AnalyzeConfig,
    languages: &[Language],
) -> Result<Discovery, StrataError> {
    let includes = compile_globs(&config.adapters.include)?;
    let excludes = compile_globs(&config.adapters.exclude)?;

    let mut sources = Vec::new();
    let mut package_roots: Vec<SmolStr> = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .require_git(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .filter_entry(|entry| {
            let name = entry.file_name();
            name != ".git" && name != "node_modules"
        })
        .build();
    for entry in walker {
        let entry = entry.map_err(|error| StrataError::InputUnreadable {
            path: root.to_path_buf(),
            reason: error.to_string(),
        })?;
        if entry.file_type().is_none_or(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        let Some(relative) = relative_path(root, path) else {
            continue;
        };
        // a build manifest marks its directory as a package root, independent of
        // the source include/exclude globs (a manifest is never a source file).
        // lean: package boundaries follow manifest *presence* — the ecosystem
        // norm (Nx/Turbo/Cargo). Upgrade path: parse each root's workspace
        // declaration (package.json `workspaces`, Cargo `[workspace].members`,
        // pnpm-workspace `packages:`) to scope members authoritatively.
        if let Some(package_root) = manifest_dir(&relative) {
            package_roots.push(package_root);
        }
        if !includes.iter().any(|glob| glob.matches(&relative)) {
            continue;
        }
        if excludes.iter().any(|glob| glob.matches(&relative)) {
            continue;
        }
        // skip files no enabled language claims, *before* reading them, so a
        // binary or non-UTF-8 file (e.g. `.git/index`) never aborts the walk.
        if !languages
            .iter()
            .any(|language| language.matches_extension(&relative))
        {
            continue;
        }
        let contents =
            std::fs::read_to_string(path).map_err(|error| StrataError::InputUnreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        sources.push(SourceFile {
            path: SmolStr::new(&relative),
            contents,
        });
    }
    // sort so discovery order — and therefore every downstream id — is stable.
    sources.sort_by(|left, right| left.path.cmp(&right.path));
    package_roots.sort();
    package_roots.dedup();
    Ok(Discovery {
        sources,
        package_roots,
    })
}

/// The output of source discovery: the language sources plus the repo-relative
/// directories that own a build manifest (the package roots).
struct Discovery {
    /// Every discovered source file, sorted by repo-relative path.
    sources: Vec<SourceFile>,
    /// Directories containing a build manifest, sorted and deduped. Empty (the
    /// repository root) entries denote a single-package repo.
    package_roots: Vec<SmolStr>,
}

/// The manifest filenames whose presence marks a directory as a package root.
const PACKAGE_MANIFESTS: [&str; 4] = ["package.json", "Cargo.toml", "pyproject.toml", "setup.py"];

/// Returns the repo-relative directory of `relative` when its filename is a
/// build manifest, or `None` otherwise. A manifest at the repository root yields
/// the empty string.
fn manifest_dir(relative: &str) -> Option<SmolStr> {
    let filename = relative.rsplit('/').next().unwrap_or(relative);
    if !PACKAGE_MANIFESTS.contains(&filename) {
        return None;
    }
    let dir = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
    Some(SmolStr::new(dir))
}

/// Compiles each glob pattern, attributing a config error on a bad pattern.
fn compile_globs(patterns: &[String]) -> Result<Vec<glob::Pattern>, StrataError> {
    patterns
        .iter()
        .map(|pattern| {
            glob::Pattern::new(pattern).map_err(|error| StrataError::ConfigInvalid {
                key: Some("adapters".to_owned()),
                reason: format!("invalid glob `{pattern}`: {error}"),
            })
        })
        .collect()
}

/// Returns the slash-normalized path of `path` relative to `root`, if `path` is
/// under `root`.
fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    Some(
        relative
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Partitions the discovered sources by language, dropping files that match no
/// enabled language.
fn group_by_language(
    files: &[SourceFile],
    languages: &[Language],
) -> Vec<(Language, Vec<SourceFile>)> {
    languages
        .iter()
        .map(|&language| {
            let sources = files
                .iter()
                .filter(|file| language.matches_extension(&file.path))
                .cloned()
                .collect::<Vec<_>>();
            (language, sources)
        })
        .filter(|(_, sources)| !sources.is_empty())
        .collect()
}

/// Runs one language adapter's parse and bind phases over its source set.
fn run_adapter(
    language: Language,
    sources: &[SourceFile],
    root: &Path,
) -> Result<IrFragment, StrataError> {
    let adapter = language.adapter(root);
    let trees = adapter
        .parse(sources)
        .map_err(|source| StrataError::AdapterParseFailure { source })?;
    adapter
        .bind(trees)
        .map_err(|source| StrataError::AdapterBindFailure { source })
}

/// Merges per-adapter fragments into one [`IntermediateRepresentation`],
/// re-interning every fragment's node ids into a single dense namespace and
/// rebuilding the container tree from every file path so package, domain, and
/// folder boundaries follow the manifest-derived `layout` — not the per-adapter
/// positional path slices the fragments arrive with.
///
/// Each fragment's own file-level containers name the repo-relative path of the
/// file every node lives in; those paths seed [`build_laminar_tree`], and each
/// node is reattached to its file's container in the rebuilt tree.
fn merge_fragments(
    fragments: Vec<IrFragment>,
    root_name: &str,
    layout: &Layout,
) -> IntermediateRepresentation {
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges = Vec::new();
    let mut affinities = Vec::new();
    let mut visibility_scopes = Vec::new();
    // the file path each merged node belongs to, parallel to `nodes`.
    let mut node_paths: Vec<SmolStr> = Vec::new();
    let mut all_paths: Vec<SmolStr> = Vec::new();

    let mut node_offset = 0_u32;
    for fragment in fragments {
        // file-level containers are keyed by full repo-relative path; map each
        // fragment-local container id to that path so nodes can be re-homed.
        let file_path: HashMap<u32, SmolStr> = fragment
            .containers
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id.0, container.name.clone()))
            .collect();
        all_paths.extend(file_path.values().cloned());

        // each fragment's ids span 0..=max; advancing the base by one past its
        // own maximum guarantees the next fragment's re-interned ids never
        // collide.
        let node_span = id_span(fragment.nodes.iter().map(|node| node.id.0));
        for mut node in fragment.nodes {
            let path = file_path
                .get(&node.container.0)
                .cloned()
                .unwrap_or_default();
            node.id = NodeId(node.id.0 + node_offset);
            nodes.push(node);
            node_paths.push(path);
        }
        for mut edge in fragment.edges {
            edge.source = NodeId(edge.source.0 + node_offset);
            edge.target = NodeId(edge.target.0 + node_offset);
            edges.push(edge);
        }
        for mut affinity in fragment.affinities {
            affinity.owner = NodeId(affinity.owner.0 + node_offset);
            affinity.companion = NodeId(affinity.companion.0 + node_offset);
            affinities.push(affinity);
        }
        for mut visibility_scope in fragment.visibility_scopes {
            visibility_scope.node = NodeId(visibility_scope.node.0 + node_offset);
            visibility_scopes.push(visibility_scope);
        }
        node_offset += node_span;
    }

    let built = build_laminar_tree(&all_paths, root_name, layout);
    for (node, path) in nodes.iter_mut().zip(&node_paths) {
        node.container = built.files.get(path).copied().unwrap_or(ContainerId(0));
    }
    let ladders = apply_visibility_scopes(
        &mut nodes,
        &built.tree,
        &built.files,
        &edges,
        &visibility_scopes,
    );

    let mut ir = IntermediateRepresentation::new(nodes, edges, built.tree);
    ir.affinities = affinities;
    ir.scope_ladders = ladders;
    ir
}

/// Projects complete visibility sidecars onto the final laminar tree and
/// returns the expressible-scope ladders they state.
fn apply_visibility_scopes(
    nodes: &mut [Node],
    tree: &ContainerTree,
    files: &HashMap<SmolStr, ContainerId>,
    edges: &[Edge],
    visibility_scopes: &[VisibilityScope],
) -> Vec<ScopeLadder> {
    // a rung is a real spelling for a declaration only if it reaches every file
    // that uses it; the file containers of each declaration's consumers decide.
    // the set is read the way `derive_visibility` reads it, after re-export
    // flattening: a consumer of a re-export node is a consumer of its original.
    let node_containers: HashMap<NodeId, ContainerId> =
        nodes.iter().map(|node| (node.id, node.container)).collect();
    let re_export_target = re_export_targets(edges);
    let name_of = name_lookup(nodes);
    let mut consumers: HashMap<NodeId, HashSet<ContainerId>> = HashMap::new();
    for edge in edges {
        if let Some(container) = node_containers.get(&edge.source) {
            let target = if edge.kind == EdgeKind::ReExport {
                edge.target
            } else {
                resolve_re_export(edge.target, &re_export_target, &name_of).unwrap_or(edge.target)
            };
            consumers.entry(target).or_default().insert(*container);
        }
    }
    let path_of: HashMap<ContainerId, &SmolStr> =
        files.iter().map(|(path, id)| (*id, path)).collect();

    let mut scopes_by_node: HashMap<NodeId, Vec<&VisibilityScope>> = HashMap::new();
    for visibility_scope in visibility_scopes {
        scopes_by_node
            .entry(visibility_scope.node)
            .or_default()
            .push(visibility_scope);
    }

    let mut ladders = Vec::new();
    for node in nodes {
        let Some([visibility_scope]) = scopes_by_node.get(&node.id).map(Vec::as_slice) else {
            continue;
        };
        let Some(owner) = tree.containers().get(node.container.0 as usize) else {
            continue;
        };
        if owner.id != node.container || owner.level != ScopeLevel::File {
            continue;
        }

        let Some(level) = scope_level(
            tree,
            files,
            node.container,
            &visibility_scope.files,
            visibility_scope.definition_file.as_ref(),
        ) else {
            continue;
        };
        node.visibility = level;

        // a consumer no rung lists (a `cfg(test)` module the analyzer never
        // defined, a file outside the module tree) cannot be judged by the
        // ladder, so it is left out of the rung check rather than wiping the
        // ladder. `derive_visibility` still counts it in the derived need, so
        // the floored level is never below that need. An item whose only
        // unplaced consumer sits in a folder whose rung shares the need's level
        // label may still be flagged, as it would be with no ladder.
        let used_from: Option<HashSet<ContainerId>> = consumers.get(&node.id).map(|containers| {
            containers
                .iter()
                .copied()
                .filter(|consumer| {
                    path_of.get(consumer).is_some_and(|path| {
                        visibility_scope
                            .expressible
                            .iter()
                            .any(|rung| rung.files.binary_search(path).is_ok())
                    })
                })
                .collect()
        });
        // all-or-nothing: one rung that cannot project drops the whole ladder
        // (a gap could only hide a narrower spelling); rungs that project but
        // miss a placed consumer are skipped individually.
        let mut levels = visibility_scope
            .expressible
            .iter()
            .map(|rung| {
                rung_reaches(
                    tree,
                    files,
                    &path_of,
                    node.container,
                    used_from.as_ref(),
                    rung,
                )
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(covered, reaches)| reaches.then_some(covered))
            .collect::<Vec<_>>();
        levels.sort();
        levels.dedup();
        if !levels.is_empty() {
            ladders.push(ScopeLadder {
                node: node.id,
                levels,
            });
        }
    }
    ladders
}

/// Projects one rung to its level, or `None` when it cannot project.
///
/// The flag is `false` when the rung leaves a consumer file out: such a rung can
/// share its level with the need yet cover another folder, so it is not a
/// spelling for this declaration.
fn rung_reaches(
    tree: &ContainerTree,
    files: &HashMap<SmolStr, ContainerId>,
    path_of: &HashMap<ContainerId, &SmolStr>,
    home: ContainerId,
    used_from: Option<&HashSet<ContainerId>>,
    rung: &strata_ir::ScopeRung,
) -> Option<(ScopeLevel, bool)> {
    let level = scope_level(
        tree,
        files,
        home,
        &rung.files,
        rung.definition_file.as_ref(),
    )?;
    // `scope_level` verified `rung.files` is strictly sorted.
    let reaches = used_from.is_none_or(|containers| {
        containers.iter().all(|consumer| {
            *consumer == home
                || path_of
                    .get(consumer)
                    .is_some_and(|path| rung.files.binary_search(path).is_ok())
        })
    });
    Some((level, reaches))
}

/// Resolves one stated scope to its level on the final tree, or `None` when the
/// scope is unsorted, empty, or names a file the tree does not hold.
fn scope_level(
    tree: &ContainerTree,
    files: &HashMap<SmolStr, ContainerId>,
    home: ContainerId,
    scope_files: &[SmolStr],
    definition_file: Option<&SmolStr>,
) -> Option<ScopeLevel> {
    if scope_files.is_empty()
        || !scope_files.windows(2).all(|pair| {
            pair.first()
                .zip(pair.get(1))
                .is_some_and(|(left, right)| left < right)
        })
    {
        return None;
    }

    // a `dir/foo.rs` definition file sits beside the `dir/foo/` folder it
    // owns; the module's own level is that folder, so the definition file
    // is left out of the common-ancestor walk. Dropping it is deliberate
    // per R2 (pub(super)/pub(in) resolve to the module's own folder), and
    // the adapter states it: it is never guessed from file names, so a
    // miss widens instead of narrowing.
    // a definition file outside the stated scope is not trusted: keeping
    // it in the walk can only widen.
    let definition = definition_file.filter(|path| scope_files.contains(path));
    let definition_container = definition.and_then(|path| files.get(path).copied());

    let mut containers = Vec::with_capacity(scope_files.len() + 1);
    if definition_container != Some(home) {
        containers.push(home);
    }
    for path in scope_files {
        let container = files.get(path).copied()?;
        if Some(path) != definition {
            containers.push(container);
        }
    }
    let level = common_ancestor_level(tree, &containers)?;
    // a lone remaining file still lives in the module's own folder.
    Some(if definition.is_some() && level == ScopeLevel::File {
        ScopeLevel::Folder
    } else {
        level
    })
}

/// Returns the deepest actual ancestor shared by every supplied container.
fn common_ancestor_level(tree: &ContainerTree, containers: &[ContainerId]) -> Option<ScopeLevel> {
    let mut containers = containers.iter().copied();
    let mut common = ancestor_chain(tree, containers.next()?)?;
    for container in containers {
        let ancestors = ancestor_chain(tree, container)?;
        common.retain(|candidate| ancestors.contains(candidate));
    }
    let common_id = common.first()?;
    let common_container = tree.containers().get(common_id.0 as usize)?;
    (common_container.id == *common_id).then_some(common_container.level)
}

/// Returns one container's validated leaf-to-root ancestry.
fn ancestor_chain(tree: &ContainerTree, start: ContainerId) -> Option<Vec<ContainerId>> {
    let mut chain = Vec::new();
    let mut current = Some(start);
    while let Some(id) = current {
        if chain.len() >= tree.containers().len() || chain.contains(&id) {
            return None;
        }
        let container = tree.containers().get(id.0 as usize)?;
        if container.id != id {
            return None;
        }
        chain.push(id);
        current = container.parent;
    }
    Some(chain)
}

/// Returns one past the maximum id in `ids`, i.e. the id range width a fragment
/// occupies, or zero when the fragment is empty.
fn id_span(ids: impl Iterator<Item = u32>) -> u32 {
    ids.max().map_or(0, |max| max + 1)
}

/// Flattens re-export edges to their original definitions, guarding against
/// pathological chains.
///
/// A re-export edge `source -> target` means `source` re-publishes `target`; a
/// consumer of `source` truly depends on whatever `target` ultimately resolves
/// to. Each re-export is rewritten to point at the end of its chain; a chain
/// longer than [`RE_EXPORT_DEPTH_LIMIT`] (almost always a cycle) raises an error.
///
/// # Errors
///
/// Returns [`StrataError::ReExportDepthExceeded`] when a chain exceeds the guard.
fn flatten_re_exports(
    mut ir: IntermediateRepresentation,
) -> Result<IntermediateRepresentation, StrataError> {
    // map each re-export source to its immediate target, then resolve transitively.
    let re_export_target = re_export_targets(&ir.edges);

    let name_of = name_lookup(&ir.nodes);
    for edge in &mut ir.edges {
        if edge.kind == EdgeKind::ReExport {
            continue;
        }
        edge.target = resolve_re_export(edge.target, &re_export_target, &name_of)?;
    }

    Ok(ir)
}

/// Maps each re-export source to its immediate target.
fn re_export_targets(edges: &[Edge]) -> HashMap<NodeId, NodeId> {
    edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::ReExport)
        .map(|edge| (edge.source, edge.target))
        .collect()
}

/// Follows the re-export chain from `start` to its original definition, bounded
/// by the depth guard.
fn resolve_re_export(
    start: NodeId,
    re_export_target: &HashMap<NodeId, NodeId>,
    name_of: &HashMap<NodeId, SmolStr>,
) -> Result<NodeId, StrataError> {
    let mut current = start;
    let mut depth = 0_u32;
    while let Some(&next) = re_export_target.get(&current) {
        if next == current {
            break;
        }
        depth += 1;
        if depth > RE_EXPORT_DEPTH_LIMIT {
            return Err(StrataError::ReExportDepthExceeded {
                origin: name_of
                    .get(&start)
                    .map_or_else(|| format!("node {}", start.0), SmolStr::to_string),
                limit: RE_EXPORT_DEPTH_LIMIT,
            });
        }
        current = next;
    }
    Ok(current)
}

/// Builds a node-id to name lookup for diagnostics.
fn name_lookup(nodes: &[Node]) -> HashMap<NodeId, SmolStr> {
    nodes
        .iter()
        .map(|node| (node.id, node.name.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use strata_ir::{Affinity, AffinityKind, Container, Hardness, NodeKind, Polarity, ScopeLevel};

    use super::*;

    /// Builds a value-import edge over a node pair.
    fn edge(source: u32, target: u32, kind: EdgeKind) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind,
            hardness: Hardness::Hard,
            confidence: 1.0,
        }
    }

    /// Builds a symbol node owning a container.
    fn node(id: u32, name: &str, container: u32) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(name),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(container),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
        }
    }

    /// Builds a file-level container named by its repo-relative path.
    fn container(id: u32, path: &str) -> Container {
        Container {
            id: ContainerId(id),
            name: SmolStr::new(path),
            level: ScopeLevel::File,
            parent: None,
            synthetic: false,
        }
    }

    /// A self-cleaning unique temp directory for filesystem fixtures.
    ///
    /// The repo carries no `tempfile` dependency, so this wraps a uniquely named
    /// directory under [`std::env::temp_dir`] and removes it on drop. The name
    /// mixes the process id and a per-call counter to stay unique across parallel
    /// tests within one binary.
    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        /// Creates a fresh, empty temp directory.
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};

            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("strata-discover-{}-{unique}", std::process::id()));
            let created = std::fs::create_dir_all(&path).is_ok();
            assert!(created, "could not create temp dir {}", path.display());
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn should_skip_non_source_files_during_discovery() {
        // a real repo carries a `.git/` whose `index` is binary, plus non-source
        // text like markdown; discovery must read only the source file and never
        // choke on the non-UTF-8 bytes.
        let dir = TempDir::new();
        let root = &dir.path;

        let wrote_source = std::fs::write(root.join("a.ts"), "export const a = 1;\n").is_ok();
        let wrote_readme = std::fs::write(root.join("README.md"), "# readme\n").is_ok();
        let made_git = std::fs::create_dir_all(root.join(".git")).is_ok();
        let wrote_index =
            std::fs::write(root.join(".git").join("index"), [0xff, 0xfe, 0x00]).is_ok();
        assert!(
            wrote_source && wrote_readme && made_git && wrote_index,
            "fixture setup failed"
        );

        // an exclude list *without* `.git` proves the extension filter alone makes
        // discovery robust to the binary index — independent of the VCS default.
        let mut config = AnalyzeConfig::default();
        config.adapters.exclude = vec!["**/node_modules/**".to_owned()];
        let languages = enabled_languages(&config);

        let result = discover_sources(root, &config, &languages);

        // discovery succeeds despite the binary `.git/index`, reading only `a.ts`.
        let sources = result
            .map(|discovery| discovery.sources)
            .unwrap_or_default();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources.first().map(|file| file.path.as_str()), Some("a.ts"));
    }

    #[test]
    fn should_honor_gitignore_rules_during_discovery() {
        // a `.gitignore` under the analyzed root excludes build output (`lib/`)
        // even when no `.git` directory exists and the config excludes are empty.
        let dir = TempDir::new();
        let root = &dir.path;

        let made_lib = std::fs::create_dir_all(root.join("lib")).is_ok();
        let wrote = std::fs::write(root.join(".gitignore"), "lib/\n").is_ok()
            && std::fs::write(root.join("kept.ts"), "export const kept = 1;\n").is_ok()
            && std::fs::write(root.join("lib").join("built.ts"), "export const b = 1;\n").is_ok();
        assert!(made_lib && wrote, "fixture setup failed");

        let mut config = AnalyzeConfig::default();
        config.adapters.exclude = Vec::new();
        let languages = enabled_languages(&config);

        let result = discover_sources(root, &config, &languages);

        let sources = result
            .map(|discovery| discovery.sources)
            .unwrap_or_default();
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources.first().map(|file| file.path.as_str()),
            Some("kept.ts")
        );
    }

    #[test]
    fn should_always_skip_node_modules_even_without_exclude_globs() {
        // `node_modules` is skipped by the walker itself, so a config override
        // that drops the default exclude globs cannot re-include dependencies.
        let dir = TempDir::new();
        let root = &dir.path;

        let made = std::fs::create_dir_all(root.join("node_modules").join("dep")).is_ok();
        let wrote = std::fs::write(root.join("app.ts"), "export const app = 1;\n").is_ok()
            && std::fs::write(
                root.join("node_modules").join("dep").join("index.ts"),
                "export const dep = 1;\n",
            )
            .is_ok();
        assert!(made && wrote, "fixture setup failed");

        let mut config = AnalyzeConfig::default();
        config.adapters.exclude = Vec::new();
        let languages = enabled_languages(&config);

        let result = discover_sources(root, &config, &languages);

        let sources = result
            .map(|discovery| discovery.sources)
            .unwrap_or_default();
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources.first().map(|file| file.path.as_str()),
            Some("app.ts")
        );
    }

    #[test]
    fn should_match_extensions_per_language() {
        assert!(Language::TypeScript.matches_extension("src/a.ts"));
        assert!(Language::Rust.matches_extension("lib.rs"));
        assert!(Language::Python.matches_extension("mod.py"));
        assert!(!Language::Rust.matches_extension("a.ts"));
    }

    #[test]
    fn should_recognize_each_languages_module_roots() {
        for root in [
            "crates/util/src/lib.rs",
            "src/main.rs",
            "src/alpha/mod.rs",
            "atlas/src/index.ts",
            "web/index.tsx",
            "pkg/__init__.py",
            "pkg/__main__.py",
        ] {
            assert!(Language::is_module_root(root), "{root} is a module root");
        }
        for leaf in [
            "crates/util/src/weight.rs",
            "src/index.rs",
            "src/lib.ts",
            "pkg/init.py",
            "docs/index.md",
        ] {
            assert!(
                !Language::is_module_root(leaf),
                "{leaf} is not a module root"
            );
        }
    }

    #[test]
    fn should_reintern_ids_across_fragments_into_one_namespace() {
        // each fragment's node lives in its own file; both fragments number their
        // ids from zero, so the second must be shifted past the first.
        let first = IrFragment {
            nodes: vec![node(0, "a", 0)],
            edges: vec![edge(0, 0, EdgeKind::Call)],
            affinities: vec![Affinity {
                owner: NodeId(0),
                companion: NodeId(0),
                kind: AffinityKind::CompanionOwner,
            }],
            containers: vec![container(0, "src/a.ts")],
            visibility_scopes: Vec::new(),
        };
        let second = IrFragment {
            nodes: vec![node(0, "b", 0)],
            edges: vec![edge(0, 0, EdgeKind::Call)],
            affinities: vec![Affinity {
                owner: NodeId(0),
                companion: NodeId(0),
                kind: AffinityKind::CompanionOwner,
            }],
            containers: vec![container(0, "src/b.ts")],
            visibility_scopes: Vec::new(),
        };

        let merged = merge_fragments(vec![first, second], "pkg", &Layout::default());

        // node and edge ids are shifted into one dense namespace...
        assert_eq!(merged.nodes.len(), 2);
        assert_eq!(merged.nodes.get(1).map(|n| n.id), Some(NodeId(1)));
        assert_eq!(merged.edges.get(1).map(|e| e.source), Some(NodeId(1)));
        assert_eq!(merged.affinities.get(1).map(|a| a.owner), Some(NodeId(1)));
        assert_eq!(
            merged.affinities.get(1).map(|a| a.companion),
            Some(NodeId(1))
        );

        // ...and each node is re-homed to its own file container in the rebuilt
        // tree, so the two distinct files never collapse together.
        let first_home = merged.nodes.first().map(|n| n.container);
        let second_home = merged.nodes.get(1).map(|n| n.container);
        assert!(first_home.is_some() && first_home != second_home);
        assert!(merged.containers.validate().is_ok());
    }

    #[test]
    fn should_rehome_a_file_and_its_test_into_one_package() {
        // a source file and its sibling test under transparent source roots must
        // land under a single package named after the repository — the W0 fix for
        // `src` and `spec` being read as two projects.
        let source = IrFragment {
            nodes: vec![node(0, "widget", 0)],
            edges: Vec::new(),
            affinities: Vec::new(),
            containers: vec![container(0, "src/ui/widget.ts")],
            visibility_scopes: Vec::new(),
        };
        let test = IrFragment {
            nodes: vec![node(0, "widget_test", 0)],
            edges: Vec::new(),
            affinities: Vec::new(),
            containers: vec![container(0, "spec/ui/widget.spec.ts")],
            visibility_scopes: Vec::new(),
        };
        let layout = Layout {
            package_roots: Vec::new(),
            source_roots: vec![SmolStr::new("src"), SmolStr::new("spec")],
        };

        let merged = merge_fragments(vec![source, test], "ai", &layout);

        let packages: Vec<_> = merged
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::Package)
            .map(|container| container.name.to_string())
            .collect();
        assert_eq!(packages, vec!["ai".to_string()]);
    }

    /// Projects one scope sidecar over a laminar tree built from `paths` and
    /// returns the node's resulting level, its container, and the container the
    /// tree assigned to `home` (the node must not move).
    #[allow(clippy::expect_used)] // loud failure is the point of this test helper
    fn scoped_node(
        paths: &[&str],
        home: &str,
        scope: &[&str],
        definition_file: Option<&str>,
    ) -> (ScopeLevel, ContainerId, ContainerId) {
        let paths: Vec<SmolStr> = paths.iter().map(|path| SmolStr::new(*path)).collect();
        let layout = Layout {
            package_roots: Vec::new(),
            source_roots: Vec::new(),
        };
        let built = build_laminar_tree(&paths, "pkg", &layout);
        let expected = built
            .files
            .get(home)
            .copied()
            .expect("home file must be part of the fixture paths");
        let mut nodes = vec![node(0, "item", expected.0)];
        let mut files: Vec<SmolStr> = scope.iter().map(|path| SmolStr::new(*path)).collect();
        files.sort();
        let scopes = vec![VisibilityScope {
            node: NodeId(0),
            files,
            definition_file: definition_file.map(SmolStr::new),
            expressible: Vec::new(),
        }];
        apply_visibility_scopes(&mut nodes, &built.tree, &built.files, &[], &scopes);
        let item = nodes.first().expect("the fixture node is present");
        (item.visibility, item.container, expected)
    }

    #[test]
    fn should_resolve_a_pub_super_in_a_file_plus_folder_module_to_its_own_folder() {
        // `dir/foo.rs` owns `dir/foo/`; the module's own level is that folder,
        // not the `dir` domain the sibling definition file would drag in.
        let paths = [
            "a/lib.rs",
            "dir/foo.rs",
            "dir/foo/a.rs",
            "dir/foo/b.rs",
            "other/x.rs",
        ];
        let (level, container, expected) = scoped_node(
            &paths,
            "dir/foo/a.rs",
            &["dir/foo.rs", "dir/foo/a.rs", "dir/foo/b.rs"],
            Some("dir/foo.rs"),
        );
        assert_eq!(level, ScopeLevel::Folder);
        assert_eq!(container, expected);
    }

    #[test]
    fn should_promote_a_lone_remaining_file_to_its_module_folder() {
        // `dir/foo.rs` plus a single `dir/foo/a.rs`: once the definition file
        // is set aside only one file remains, which still lives in `dir/foo/`.
        let paths = ["a/lib.rs", "dir/foo.rs", "dir/foo/a.rs", "other/x.rs"];
        let (level, _, _) = scoped_node(
            &paths,
            "dir/foo/a.rs",
            &["dir/foo.rs", "dir/foo/a.rs"],
            Some("dir/foo.rs"),
        );
        assert_eq!(level, ScopeLevel::Folder);
    }

    #[test]
    fn should_resolve_a_pub_super_in_a_mod_rs_module_to_its_own_folder() {
        let paths = ["a/lib.rs", "dir/foo/mod.rs", "dir/foo/a.rs", "other/x.rs"];
        let (level, _, _) = scoped_node(
            &paths,
            "dir/foo/a.rs",
            &["dir/foo/mod.rs", "dir/foo/a.rs"],
            Some("dir/foo/mod.rs"),
        );
        assert_eq!(level, ScopeLevel::Folder);
    }

    #[test]
    #[allow(clippy::expect_used)] // loud failure is the point of this test
    fn should_project_each_stated_rung_to_a_sorted_level_ladder() {
        let paths: Vec<SmolStr> = ["src/lib.rs", "src/a.rs", "src/b/c.rs", "src/b/d.rs"]
            .iter()
            .map(|path| SmolStr::new(*path))
            .collect();
        let built = build_laminar_tree(&paths, "pkg", &Layout::default());
        let home = built.files.get("src/b/c.rs").copied().expect("home file");
        let mut nodes = vec![node(0, "item", home.0)];
        let rung = |files: &[&str]| strata_ir::ScopeRung {
            files: files.iter().map(|file| SmolStr::new(*file)).collect(),
            definition_file: None,
        };
        let scopes = vec![VisibilityScope {
            node: NodeId(0),
            files: vec![
                "src/a.rs".into(),
                "src/b/c.rs".into(),
                "src/b/d.rs".into(),
                "src/lib.rs".into(),
            ],
            definition_file: None,
            expressible: vec![
                rung(&["src/b/c.rs"]),
                rung(&["src/b/c.rs", "src/b/d.rs"]),
                rung(&["src/a.rs", "src/b/c.rs", "src/b/d.rs", "src/lib.rs"]),
            ],
        }];
        let ladders = apply_visibility_scopes(&mut nodes, &built.tree, &built.files, &[], &scopes);
        let ladder = ladders.first().expect("a ladder is projected");
        assert_eq!(
            ladder.levels,
            vec![ScopeLevel::File, ScopeLevel::Folder, ScopeLevel::Domain]
        );
        // the ladder is a sidecar: the declared level still comes from `files`
        assert_eq!(
            nodes.first().map(|node| node.visibility),
            Some(ScopeLevel::Domain)
        );
    }

    #[test]
    #[allow(clippy::expect_used)] // loud failure is the point of this test
    fn should_drop_the_whole_ladder_when_any_rung_cannot_project() {
        let paths: Vec<SmolStr> = ["src/lib.rs", "src/a.rs", "src/b/c.rs", "src/b/d.rs"]
            .iter()
            .map(|path| SmolStr::new(*path))
            .collect();
        let built = build_laminar_tree(&paths, "pkg", &Layout::default());
        let home = built.files.get("src/b/c.rs").copied().expect("home file");
        let mut nodes = vec![node(0, "item", home.0)];
        let rung = |files: &[&str]| strata_ir::ScopeRung {
            files: files.iter().map(|file| SmolStr::new(*file)).collect(),
            definition_file: None,
        };
        let scopes = vec![VisibilityScope {
            node: NodeId(0),
            files: vec!["src/b/c.rs".into(), "src/b/d.rs".into()],
            definition_file: None,
            // the middle rung names a file the tree does not hold
            expressible: vec![
                rung(&["src/b/c.rs"]),
                rung(&["src/missing.rs"]),
                rung(&["src/b/c.rs", "src/b/d.rs"]),
            ],
        }];
        let ladders = apply_visibility_scopes(&mut nodes, &built.tree, &built.files, &[], &scopes);
        assert!(ladders.is_empty());
        // the declared scope still projects; only the ladder is dropped
        assert_eq!(
            nodes.first().map(|node| node.visibility),
            Some(ScopeLevel::Folder)
        );
    }

    /// Projects the ladder of an `item` homed in `home` and consumed from the
    /// `consumers` files, over a laminar tree built from `paths`; each rung
    /// lists its files and optional definition file.
    #[allow(clippy::expect_used)] // loud failure is the point of this test helper
    fn consumed_ladder(
        paths: &[&str],
        home: &str,
        consumers: &[&str],
        rungs: &[(&[&str], Option<&str>)],
    ) -> Vec<ScopeLevel> {
        let paths: Vec<SmolStr> = paths.iter().map(|path| SmolStr::new(*path)).collect();
        let built = build_laminar_tree(&paths, "pkg", &Layout::default());
        let container = |path: &str| built.files.get(path).copied().expect("fixture file").0;
        let mut nodes = vec![node(0, "item", container(home))];
        let mut edges = Vec::new();
        for (index, consumer) in consumers.iter().enumerate() {
            let id = u32::try_from(index).expect("small fixture") + 1;
            nodes.push(node(id, "user", container(consumer)));
            edges.push(edge(id, 0, EdgeKind::Call));
        }
        let sorted = |files: &[&str]| -> Vec<SmolStr> {
            let mut files: Vec<SmolStr> = files.iter().map(|file| SmolStr::new(*file)).collect();
            files.sort();
            files
        };
        let scopes = vec![VisibilityScope {
            node: NodeId(0),
            files: sorted(rungs.first().expect("a rung").0),
            definition_file: rungs.first().and_then(|rung| rung.1).map(SmolStr::new),
            expressible: rungs
                .iter()
                .map(|(files, definition)| strata_ir::ScopeRung {
                    files: sorted(files),
                    definition_file: definition.map(SmolStr::new),
                })
                .collect(),
        }];
        apply_visibility_scopes(&mut nodes, &built.tree, &built.files, &edges, &scopes)
            .first()
            .map(|ladder| ladder.levels.clone())
            .unwrap_or_default()
    }

    const SIBLING_PATHS: [&str; 8] = [
        "src/lib.rs",
        "src/other/x.rs",
        "src/render.rs",
        "src/render/report.rs",
        "src/render/report/fit.rs",
        "src/render/report/profile.rs",
        "src/render/tree.rs",
        "src/render/tree/walk.rs",
    ];

    type FixtureRung = (&'static [&'static str], Option<&'static str>);

    /// Rungs of a `pub(super)` item in `src/render/report.rs`: `report` itself,
    /// the `render` parent, then the crate.
    fn sibling_rungs() -> [FixtureRung; 3] {
        [
            (
                &[
                    "src/render/report.rs",
                    "src/render/report/fit.rs",
                    "src/render/report/profile.rs",
                ],
                Some("src/render/report.rs"),
            ),
            (
                &[
                    "src/render.rs",
                    "src/render/report.rs",
                    "src/render/report/fit.rs",
                    "src/render/report/profile.rs",
                    "src/render/tree.rs",
                    "src/render/tree/walk.rs",
                    "src/other/x.rs",
                ],
                Some("src/render.rs"),
            ),
            (&SIBLING_PATHS, None),
        ]
    }

    #[test]
    fn should_drop_a_rung_that_misses_a_consumer_in_the_parent_definition_file() {
        // sibling case: `lines` in `render/report.rs`, used by `render.rs`. The
        // `report` rung projects to the same folder label as the need but never
        // reaches `render.rs`, so it must not stay on the ladder.
        let rungs = sibling_rungs();
        let consumed = consumed_ladder(
            &SIBLING_PATHS,
            "src/render/report.rs",
            &["src/render.rs"],
            &rungs,
        );
        let alone = consumed_ladder(&SIBLING_PATHS, "src/render/report.rs", &[], &rungs);
        assert_eq!(alone, [ScopeLevel::Folder, ScopeLevel::Domain]);
        assert_eq!(consumed, [ScopeLevel::Domain]);
    }

    /// Like [`consumed_ladder`] but with free-form extra nodes (`(file, kind)`
    /// pairs numbered from 1) and edges over them.
    #[allow(clippy::expect_used)] // loud failure is the point of this test helper
    fn ladder_over_graph(
        paths: &[&str],
        home: &str,
        others: &[&str],
        edges: &[(u32, u32, EdgeKind)],
        rungs: &[FixtureRung],
    ) -> Vec<ScopeLevel> {
        let paths: Vec<SmolStr> = paths.iter().map(|path| SmolStr::new(*path)).collect();
        let built = build_laminar_tree(&paths, "pkg", &Layout::default());
        let container = |path: &str| built.files.get(path).copied().expect("fixture file").0;
        let mut nodes = vec![node(0, "item", container(home))];
        for (index, file) in others.iter().enumerate() {
            let id = u32::try_from(index).expect("small fixture") + 1;
            nodes.push(node(id, "other", container(file)));
        }
        let edges: Vec<Edge> = edges
            .iter()
            .map(|(source, target, kind)| edge(*source, *target, *kind))
            .collect();
        let sorted = |files: &[&str]| -> Vec<SmolStr> {
            let mut files: Vec<SmolStr> = files.iter().map(|file| SmolStr::new(*file)).collect();
            files.sort();
            files
        };
        let scopes = vec![VisibilityScope {
            node: NodeId(0),
            files: sorted(rungs.first().expect("a rung").0),
            definition_file: rungs.first().and_then(|rung| rung.1).map(SmolStr::new),
            expressible: rungs
                .iter()
                .map(|(files, definition)| strata_ir::ScopeRung {
                    files: sorted(files),
                    definition_file: definition.map(SmolStr::new),
                })
                .collect(),
        }];
        apply_visibility_scopes(&mut nodes, &built.tree, &built.files, &edges, &scopes)
            .first()
            .map(|ladder| ladder.levels.clone())
            .unwrap_or_default()
    }

    #[test]
    fn should_read_consumers_after_re_export_flattening() {
        // `item` is re-exported by node 1 in `report/fit.rs` (inside the private
        // rung); node 2 in `other/x.rs` consumes the re-export node. Flattened,
        // node 2 consumes `item` directly, so the private rung must go.
        let edges = [(1, 0, EdgeKind::ReExport), (2, 1, EdgeKind::Call)];
        let ladder = ladder_over_graph(
            &SIBLING_PATHS,
            "src/render/report.rs",
            &["src/render/report/fit.rs", "src/other/x.rs"],
            &edges,
            &sibling_rungs(),
        );
        assert_eq!(ladder, [ScopeLevel::Domain]);
    }

    #[test]
    fn should_ignore_a_consumer_that_no_rung_can_place() {
        // a consumer in a file no rung lists (a `cfg(test)` module the analyzer
        // never defined) cannot be judged by the ladder; `derive_visibility`
        // already counts it in the derived need, so it must not wipe the ladder.
        let mut paths = SIBLING_PATHS.to_vec();
        paths.push("src/stray.rs");
        let rungs = sibling_rungs();
        let alone = ladder_over_graph(&paths, "src/render/report.rs", &[], &[], &rungs);
        let stray = ladder_over_graph(
            &paths,
            "src/render/report.rs",
            &["src/stray.rs"],
            &[(1, 0, EdgeKind::Call)],
            &rungs,
        );
        assert!(!alone.is_empty());
        assert_eq!(stray, alone);

        // a placed consumer still prunes rungs while the stray one is ignored.
        let both = ladder_over_graph(
            &paths,
            "src/render/report.rs",
            &["src/stray.rs", "src/render.rs"],
            &[(1, 0, EdgeKind::Call), (2, 0, EdgeKind::Call)],
            &rungs,
        );
        assert_eq!(both, [ScopeLevel::Domain]);
    }

    #[test]
    fn should_drop_the_private_rung_for_a_same_folder_sibling_consumer() {
        // `relocation/collision.rs` used by `relocation/solver.rs`, with
        // `relocation.rs` as the parent definition file.
        const PATHS: [&str; 5] = [
            "src/lib.rs",
            "src/other/x.rs",
            "src/relocation.rs",
            "src/relocation/collision.rs",
            "src/relocation/solver.rs",
        ];
        let paths = PATHS;
        let rungs: [FixtureRung; 3] = [
            (&["src/relocation/collision.rs"], None),
            (
                &[
                    "src/relocation.rs",
                    "src/relocation/collision.rs",
                    "src/relocation/solver.rs",
                ],
                Some("src/relocation.rs"),
            ),
            (&PATHS, None),
        ];
        let alone = ladder_over_graph(&paths, "src/relocation/collision.rs", &[], &[], &rungs);
        let consumed = ladder_over_graph(
            &paths,
            "src/relocation/collision.rs",
            &["src/relocation/solver.rs"],
            &[(1, 0, EdgeKind::Call)],
            &rungs,
        );
        assert_eq!(alone.first(), Some(&ScopeLevel::File));
        assert_eq!(consumed.first(), alone.get(1));
        assert!(!consumed.contains(&ScopeLevel::File), "{consumed:?}");
    }

    #[test]
    fn should_keep_the_private_rung_when_every_consumer_sits_inside_it() {
        // a consumer inside `report/` is reached by the `report` rung.
        let rungs = sibling_rungs();
        let consumed = consumed_ladder(
            &SIBLING_PATHS,
            "src/render/report.rs",
            &["src/render/report/fit.rs"],
            &rungs,
        );
        let alone = consumed_ladder(&SIBLING_PATHS, "src/render/report.rs", &[], &rungs);
        assert_eq!(consumed, alone);
    }

    #[test]
    fn should_drop_a_crate_root_child_rung_that_misses_the_crate_root_consumer() {
        // crate-root-child case: `parse` in `src/parse.rs` (owning `src/parse/`)
        // is `pub(super)` and used by `lib.rs`; only the crate rung reaches it.
        let paths = [
            "src/lib.rs",
            "src/parse.rs",
            "src/parse/a.rs",
            "src/parse/b.rs",
        ];
        let rungs: [FixtureRung; 2] = [
            (
                &["src/parse.rs", "src/parse/a.rs", "src/parse/b.rs"],
                Some("src/parse.rs"),
            ),
            (
                &[
                    "src/lib.rs",
                    "src/parse.rs",
                    "src/parse/a.rs",
                    "src/parse/b.rs",
                ],
                None,
            ),
        ];
        let consumed = consumed_ladder(&paths, "src/parse.rs", &["src/lib.rs"], &rungs);
        let alone = consumed_ladder(&paths, "src/parse.rs", &[], &rungs);
        assert_eq!(alone, [ScopeLevel::Folder, ScopeLevel::Domain]);
        assert_eq!(consumed, [ScopeLevel::Domain]);
    }

    #[test]
    fn should_keep_a_pub_crate_item_at_package_level() {
        let paths = ["a/lib.rs", "dir/foo.rs", "dir/foo/a.rs", "other/x.rs"];
        let (level, _, _) = scoped_node(&paths, "dir/foo/a.rs", &paths, None);
        assert_eq!(level, ScopeLevel::Package);
    }

    #[test]
    fn should_keep_a_crate_root_scope_beside_its_folder_at_package_level() {
        // `tests/it.rs` is a crate root with a sibling `tests/it/` folder; the
        // adapter states no definition file for a root, so nothing is dropped
        // and the lone-file promotion cannot narrow the crate-wide scope.
        let paths = ["src/lib.rs", "tests/it.rs", "tests/it/a.rs"];
        let (level, _, _) = scoped_node(&paths, "tests/it/a.rs", &paths, None);
        assert_eq!(level, ScopeLevel::Package);
    }

    #[test]
    fn should_ignore_a_definition_file_outside_the_scope_files() {
        // the scope lists only `dir/foo/a.rs`; a stated definition file that is
        // not among `files` must not trigger the lone-file promotion to Folder.
        let paths = ["a/lib.rs", "dir/foo.rs", "dir/foo/a.rs", "other/x.rs"];
        let (level, _, _) = scoped_node(
            &paths,
            "dir/foo/a.rs",
            &["dir/foo/a.rs"],
            Some("dir/foo.rs"),
        );
        assert_eq!(level, ScopeLevel::File);
    }

    #[test]
    fn should_treat_a_manifest_directory_as_a_package_root() {
        assert_eq!(manifest_dir("package.json"), Some(SmolStr::new("")));
        assert_eq!(
            manifest_dir("packages/foo/Cargo.toml"),
            Some(SmolStr::new("packages/foo"))
        );
        assert_eq!(manifest_dir("src/app.ts"), None);
    }

    #[test]
    fn should_flatten_a_re_export_chain_to_its_definition() {
        // node 0 re-exports node 1, which re-exports node 2; a call to 0 must
        // resolve to 2.
        let ir = IntermediateRepresentation::new(
            vec![
                node(0, "barrel", 0),
                node(1, "mid", 0),
                node(2, "def", 0),
                node(3, "caller", 0),
            ],
            vec![
                edge(0, 1, EdgeKind::ReExport),
                edge(1, 2, EdgeKind::ReExport),
                edge(3, 0, EdgeKind::Call),
            ],
            strata_ir::ContainerTree::new(vec![container(0, "src/a.ts")]),
        );

        let flattened = flatten_re_exports(ir).unwrap_or_else(|_| {
            IntermediateRepresentation::new(vec![], vec![], strata_ir::ContainerTree::new(vec![]))
        });

        let call = flattened
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Call);
        assert_eq!(call.map(|edge| edge.target), Some(NodeId(2)));
    }

    #[test]
    fn should_raise_on_a_pathological_re_export_cycle() {
        // a re-export cycle 0 -> 1 -> 0 exceeds the depth guard.
        let ir = IntermediateRepresentation::new(
            vec![node(0, "a", 0), node(1, "b", 0), node(2, "caller", 0)],
            vec![
                edge(0, 1, EdgeKind::ReExport),
                edge(1, 0, EdgeKind::ReExport),
                edge(2, 0, EdgeKind::Call),
            ],
            strata_ir::ContainerTree::new(vec![container(0, "src/a.ts")]),
        );

        let result = flatten_re_exports(ir);

        assert!(matches!(
            result,
            Err(StrataError::ReExportDepthExceeded { .. })
        ));
    }

    #[test]
    fn should_compute_an_id_span_as_one_past_the_max() {
        assert_eq!(id_span([0, 1, 2].into_iter()), 3);
        assert_eq!(id_span(std::iter::empty()), 0);
    }

    #[test]
    fn should_name_the_package_group_for_a_dotdot_roots_real_directory() {
        // Bug B: a non-canonical root (here an absolute path ending in `..`) must
        // be canonicalized so the package group is named for the real directory,
        // not the empty-name fallback `root`. The same canonicalization is what
        // lets rust-analyzer's absolute VFS match, restoring the full edge graph;
        // this seam pins the observable name (the edge restoration is pinned
        // end-to-end in the cli acceptance suite). `..` — not just a relative
        // form — proves `fs::canonicalize`, not lexical `path::absolute`, is used.
        let dir = TempDir::new();
        let root = &dir.path;
        let wrote = std::fs::write(root.join("a.ts"), "export const a = 1;\n").is_ok();
        let made_sub = std::fs::create_dir_all(root.join("sub")).is_ok();
        assert!(wrote && made_sub, "fixture setup failed");
        // `<root>/sub/..` is a non-canonical absolute path to `<root>` itself.
        // `Path::file_name()` returns `None` for a path ending in `..`, so an
        // un-canonicalized root falls back to the empty name and the group naming
        // yields `root`; canonicalizing resolves `..` back to the real directory.
        let dotdot = root.join("sub").join("..");
        let config = AnalyzeConfig::default();

        let group_name = snapshot_from_root(&dotdot, &config)
            .ok()
            .and_then(|snapshot| {
                snapshot
                    .ir()
                    .containers
                    .containers()
                    .iter()
                    .find(|container| container.level == ScopeLevel::PackageGroup)
                    .map(|container| container.name.to_string())
            });

        let expected = root
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        assert!(expected.is_some(), "the temp dir has a real file name");
        assert_eq!(
            group_name, expected,
            "a ..-containing root is canonicalized, so the package group is named \
             for the real directory rather than the fallback `root`"
        );
    }

    #[test]
    fn should_fail_loud_from_the_canonicalize_guard_when_the_root_does_not_exist() {
        // `fs::canonicalize` requires the path to exist, making this strata's
        // de-facto "does --root exist?" guard: a root that cannot be canonicalized
        // is exactly when rust-analyzer's absolute VFS would not match, so a
        // fallback-to-raw would silently reproduce the degraded 4-edge snapshot.
        // Fail loud with the stable InputUnreadable code instead.
        //
        // This test pins the failure to the *guard*, not just to some
        // InputUnreadable. No input distinguishes the two by outcome: every root
        // that fails canonicalization also fails the downstream discovery walker
        // (the walker follows the root symlink too, so a dangling link, a missing
        // path, and a symlink loop all error in both places). What differs is
        // provenance — the guard surfaces canonicalize's own bare io error, whereas
        // the walker (`ignore`) wraps it ("IO error for operation on <path>: ...").
        // Comparing the reason against canonicalize's live output makes a revert to
        // a raw-path fallback fail here, since it would surface the wrapped error.
        let dir = TempDir::new();
        let missing = dir.path.join("does-not-exist");
        let config = AnalyzeConfig::default();
        let canonicalize_error = std::fs::canonicalize(&missing).err().map(|e| e.to_string());
        assert!(
            canonicalize_error.is_some(),
            "a nonexistent path cannot be canonicalized"
        );

        let result = snapshot_from_root(&missing, &config);

        let (path, reason) = match result {
            Err(StrataError::InputUnreadable { path, reason }) => (Some(path), Some(reason)),
            _ => (None, None),
        };
        assert_eq!(
            path.as_ref(),
            Some(&missing),
            "the error names the offending root, un-canonicalized"
        );
        assert_eq!(
            reason, canonicalize_error,
            "the failure originates from the canonicalize guard (canonicalize's own \
             io error), not the discovery walker (which wraps the io error)"
        );
    }
}
