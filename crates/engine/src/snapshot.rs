//! Source discovery, adapter dispatch, fragment merging, and snapshot assembly.
//!
//! [`snapshot_from_root`] is the engine's only filesystem entry point. It globs
//! sources under a root (honoring the config's include / exclude patterns),
//! groups files by language, dispatches each language's [`Adapter`] in parallel
//! (rayon), then merges the resulting [`IrFragment`]s into one namespace:
//! per-adapter node and container ids are re-interned to a single dense range so
//! cross-fragment edges resolve. Re-export edges are flattened to their original
//! definitions under a depth guard, and the merged IR is handed to
//! [`Snapshot::assemble`] for validation and content hashing.

use std::collections::HashMap;
use std::path::Path;

use rayon::prelude::*;
use smol_str::SmolStr;
use strata_adapter_python::PythonAdapter;
use strata_adapter_rust::RustAdapter;
use strata_adapter_typescript::TypeScriptAdapter;
use strata_ir::{
    Adapter, ContainerId, EdgeKind, IntermediateRepresentation, IrFragment, Node, NodeId,
    SourceFile,
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
    let languages = enabled_languages(config);
    let files = discover_sources(root, config, &languages)?;

    let grouped = group_by_language(&files, &languages);
    let fragments = grouped
        .into_par_iter()
        .map(|(language, sources)| run_adapter(language, &sources, root))
        .collect::<Result<Vec<_>, _>>()?;

    let merged = merge_fragments(fragments);
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
) -> Result<Vec<SourceFile>, StrataError> {
    let includes = compile_globs(&config.adapters.include)?;
    let excludes = compile_globs(&config.adapters.exclude)?;

    let mut sources = Vec::new();
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
    Ok(sources)
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
/// re-interning every fragment's node and container ids into a single dense
/// namespace so cross-fragment references resolve.
fn merge_fragments(fragments: Vec<IrFragment>) -> IntermediateRepresentation {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut containers = Vec::new();

    let mut node_offset = 0_u32;
    let mut container_offset = 0_u32;
    for fragment in fragments {
        // each fragment's ids span 0..=max; advancing the base by one past its own
        // maximum guarantees the next fragment's re-interned ids never collide.
        let node_span = id_span(fragment.nodes.iter().map(|node| node.id.0));
        let container_span = id_span(fragment.containers.iter().map(|container| container.id.0));

        for mut node in fragment.nodes {
            node.id = NodeId(node.id.0 + node_offset);
            node.container = ContainerId(node.container.0 + container_offset);
            nodes.push(node);
        }
        for mut edge in fragment.edges {
            edge.source = NodeId(edge.source.0 + node_offset);
            edge.target = NodeId(edge.target.0 + node_offset);
            edges.push(edge);
        }
        for mut container in fragment.containers {
            container.id = ContainerId(container.id.0 + container_offset);
            container.parent = container
                .parent
                .map(|parent| ContainerId(parent.0 + container_offset));
            containers.push(container);
        }

        node_offset += node_span;
        container_offset += container_span;
    }

    IntermediateRepresentation::new(nodes, edges, strata_ir::ContainerTree::new(containers))
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
    let mut re_export_target: HashMap<NodeId, NodeId> = HashMap::new();
    for edge in &ir.edges {
        if edge.kind == EdgeKind::ReExport {
            re_export_target.insert(edge.source, edge.target);
        }
    }

    let name_of = name_lookup(&ir.nodes);
    for edge in &mut ir.edges {
        if edge.kind == EdgeKind::ReExport {
            continue;
        }
        edge.target = resolve_re_export(edge.target, &re_export_target, &name_of)?;
    }

    Ok(ir)
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
    use strata_ir::{Container, Edge, Hardness, NodeKind, Polarity, ScopeLevel};

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
        }
    }

    /// Builds a file-level container.
    fn container(id: u32) -> Container {
        Container {
            id: ContainerId(id),
            name: SmolStr::new(format!("c{id}")),
            level: ScopeLevel::File,
            parent: None,
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
        let sources = result.unwrap_or_else(|_| Vec::new());
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

        let sources = result.unwrap_or_else(|_| Vec::new());
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

        let sources = result.unwrap_or_else(|_| Vec::new());
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
    fn should_reintern_ids_across_fragments_into_one_namespace() {
        let first = IrFragment {
            nodes: vec![node(0, "a", 0)],
            edges: vec![edge(0, 0, EdgeKind::Call)],
            containers: vec![container(0)],
        };
        let second = IrFragment {
            nodes: vec![node(0, "b", 0)],
            edges: vec![edge(0, 0, EdgeKind::Call)],
            containers: vec![container(0)],
        };

        let merged = merge_fragments(vec![first, second]);

        // the second fragment's ids are shifted past the first's.
        assert_eq!(merged.nodes.len(), 2);
        assert_eq!(merged.nodes.get(1).map(|n| n.id), Some(NodeId(1)));
        assert_eq!(
            merged.nodes.get(1).map(|n| n.container),
            Some(ContainerId(1))
        );
        assert_eq!(merged.edges.get(1).map(|e| e.source), Some(NodeId(1)));
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
            strata_ir::ContainerTree::new(vec![container(0)]),
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
            strata_ir::ContainerTree::new(vec![container(0)]),
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
}
