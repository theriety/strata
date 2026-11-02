//! Snapshot unit tests.

use smol_str::SmolStr;
use strata_ir::{
    AdvisoryUse, Affinity, AffinityKind, Container, ContainerId, Edge, EdgeKind, Exposure,
    FileFact, Hardness, IntermediateRepresentation, IrFragment, Layout, Node, NodeId, NodeKind,
    Polarity, ScopeLadder, ScopeLevel, VisibilityScope, build_laminar_tree,
};

use super::discovery::{discover_sources, manifest_dir};
use super::dispatch::enabled_languages;
use super::merge::{id_span, merge_fragments};
use super::reexport::flatten_re_exports;
use super::visibility::apply_visibility_scopes;
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
        public_api: false,
        advice_exempt: false,
        id: NodeId(id),
        name: SmolStr::new(name),
        kind: NodeKind::Symbol,
        polarity: Polarity::Production,
        container: ContainerId(container),
        visibility: ScopeLevel::File,
        effective_size: 1,
        re_export: false,
        display_name: None,
        test_source: false,
        owner: None,
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
        let path =
            std::env::temp_dir().join(format!("strata-discover-{}-{unique}", std::process::id()));
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
    let wrote_index = std::fs::write(root.join(".git").join("index"), [0xff, 0xfe, 0x00]).is_ok();
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
        exposures: Vec::new(),
        advisory_uses: Vec::new(),
        file_facts: Vec::new(),
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
        exposures: Vec::new(),
        advisory_uses: Vec::new(),
        file_facts: Vec::new(),
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
        exposures: Vec::new(),
        advisory_uses: Vec::new(),
        file_facts: Vec::new(),
    };
    let test = IrFragment {
        nodes: vec![node(0, "widget_test", 0)],
        edges: Vec::new(),
        affinities: Vec::new(),
        containers: vec![container(0, "spec/ui/widget.spec.ts")],
        visibility_scopes: Vec::new(),
        exposures: Vec::new(),
        advisory_uses: Vec::new(),
        file_facts: Vec::new(),
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
        declared_rung: None,
    }];
    apply_visibility_scopes(
        &mut nodes,
        &built.tree,
        &built.files,
        &[],
        &scopes,
        &[],
        &[],
    );
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

type FixtureRung = (&'static [&'static str], Option<&'static str>);

/// One ladder verdict scenario: an `item` (node 0) homed in `home`, extra
/// `others` (nodes 1..) with `edges` over them, and the stated `rungs`.
struct Case<'a> {
    paths: &'a [&'a str],
    home: &'a str,
    others: &'a [&'a str],
    edges: &'a [(u32, u32, EdgeKind)],
    rungs: &'a [FixtureRung],
    declared: Option<u32>,
    exposures: &'a [Exposure],
    advisory: &'a [AdvisoryUse],
    /// A scope statement for node 1, the exposer.
    exposer_scope: Option<VisibilityScope>,
}

impl<'a> Case<'a> {
    fn new(
        paths: &'a [&'a str],
        home: &'a str,
        rungs: &'a [FixtureRung],
        declared: Option<u32>,
    ) -> Self {
        Self {
            paths,
            home,
            others: &[],
            edges: &[],
            rungs,
            declared,
            exposures: &[],
            advisory: &[],
            exposer_scope: None,
        }
    }

    /// Consumers (nodes 1..) in `files`, each calling the item.
    fn consumed_by(mut self, files: &'a [&'a str], edges: &'a [(u32, u32, EdgeKind)]) -> Self {
        self.others = files;
        self.edges = edges;
        self
    }

    /// Projects the verdict the engine reaches for node 0.
    #[allow(clippy::expect_used)] // loud failure is the point of this test helper
    fn run(&self) -> ScopeLadder {
        let paths: Vec<SmolStr> = self.paths.iter().map(|path| SmolStr::new(*path)).collect();
        let built = build_laminar_tree(&paths, "pkg", &Layout::default());
        let container = |path: &str| built.files.get(path).copied().expect("fixture file").0;
        let mut nodes = vec![node(0, "item", container(self.home))];
        for (index, file) in self.others.iter().enumerate() {
            let id = u32::try_from(index).expect("small fixture") + 1;
            nodes.push(node(id, "other", container(file)));
        }
        let edges: Vec<Edge> = self
            .edges
            .iter()
            .map(|(source, target, kind)| edge(*source, *target, *kind))
            .collect();
        let mut scopes = vec![scope_of(NodeId(0), self.rungs, self.declared)];
        scopes.extend(self.exposer_scope.clone());
        apply_visibility_scopes(
            &mut nodes,
            &built.tree,
            &built.files,
            &edges,
            &scopes,
            self.exposures,
            self.advisory,
        )
        .into_iter()
        .find(|ladder| ladder.node == NodeId(0))
        .expect("the item states a scope")
    }
}

/// States `rungs` for `node`, with the `declared` rung's files as its label.
fn scope_of(node: NodeId, rungs: &[FixtureRung], declared: Option<u32>) -> VisibilityScope {
    let sorted = |files: &[&str]| -> Vec<SmolStr> {
        let mut files: Vec<SmolStr> = files.iter().map(|file| SmolStr::new(*file)).collect();
        files.sort();
        files
    };
    let label = declared.and_then(|index| rungs.get(usize::try_from(index).ok()?));
    VisibilityScope {
        node,
        files: label.map(|rung| sorted(rung.0)).unwrap_or_default(),
        definition_file: label.and_then(|rung| rung.1).map(SmolStr::new),
        expressible: rungs
            .iter()
            .map(|(files, definition)| strata_ir::ScopeRung {
                files: sorted(files),
                definition_file: definition.map(SmolStr::new),
                spelling: None,
            })
            .collect(),
        declared_rung: declared,
    }
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

const BCD_PATHS: [&str; 4] = ["src/lib.rs", "src/a.rs", "src/b/c.rs", "src/b/d.rs"];
const BCD_ALL: [&str; 4] = ["src/a.rs", "src/b/c.rs", "src/b/d.rs", "src/lib.rs"];

#[test]
fn should_name_the_narrowest_rung_that_holds_the_home_and_every_consumer() {
    let rungs: [FixtureRung; 3] = [
        (&["src/b/c.rs"], None),
        (&["src/b/c.rs", "src/b/d.rs"], None),
        (&BCD_ALL, None),
    ];
    let case = Case::new(&BCD_PATHS, "src/b/c.rs", &rungs, Some(2))
        .consumed_by(&["src/b/d.rs"], &[(1, 0, EdgeKind::Call)]);
    let ladder = case.run();
    assert_eq!(ladder.declared, Some(ScopeLevel::Domain));
    assert_eq!(ladder.needed, Some(ScopeLevel::Folder));
}

#[test]
fn should_state_the_declared_level_of_a_restricted_item_from_its_files() {
    let rungs: [FixtureRung; 2] = [(&["src/b/c.rs", "src/b/d.rs"], None), (&BCD_ALL, None)];
    let paths: Vec<SmolStr> = BCD_PATHS.iter().map(|path| SmolStr::new(*path)).collect();
    let built = build_laminar_tree(&paths, "pkg", &Layout::default());
    let home = built.files.get("src/b/c.rs").map_or(0, |id| id.0);
    let mut nodes = vec![node(0, "item", home)];
    let scopes = [scope_of(NodeId(0), &rungs, Some(0))];
    apply_visibility_scopes(
        &mut nodes,
        &built.tree,
        &built.files,
        &[],
        &scopes,
        &[],
        &[],
    );
    assert_eq!(
        nodes.first().map(|item| item.visibility),
        Some(ScopeLevel::Folder)
    );
}

#[test]
fn should_stay_silent_when_the_needed_rung_cannot_project() {
    // the narrowest rung names a file the tree does not hold.
    let rungs: [FixtureRung; 2] = [
        (&["src/b/c.rs", "src/missing.rs"], None),
        (&["src/b/c.rs", "src/b/d.rs"], None),
    ];
    let ladder = Case::new(&BCD_PATHS, "src/b/c.rs", &rungs, Some(1)).run();
    assert_eq!(ladder.needed, None);
    assert_eq!(ladder.declared, None);
}

#[test]
fn should_stay_silent_when_the_declared_rung_is_unresolved() {
    let rungs: [FixtureRung; 2] = [(&["src/b/c.rs"], None), (&BCD_ALL, None)];
    let ladder = Case::new(&BCD_PATHS, "src/b/c.rs", &rungs, None).run();
    assert_eq!((ladder.declared, ladder.needed), (None, None));
}

#[test]
fn should_not_advise_when_the_needed_rung_is_the_declared_one() {
    // `lines` in `render/report.rs` is `pub(super)` and used by `render.rs`: the
    // `report` rung never reaches `render.rs`, so the parent rung is both
    // needed and declared.
    let rungs = sibling_rungs();
    let consumed = Case::new(&SIBLING_PATHS, "src/render/report.rs", &rungs, Some(1))
        .consumed_by(&["src/render.rs"], &[(1, 0, EdgeKind::Call)])
        .run();
    let alone = Case::new(&SIBLING_PATHS, "src/render/report.rs", &rungs, Some(1)).run();
    assert_eq!(alone.needed, Some(ScopeLevel::Folder));
    assert_eq!(consumed.needed, None);
    assert_eq!(consumed.declared, Some(ScopeLevel::Domain));
}

#[test]
fn should_read_consumers_after_re_export_flattening() {
    // node 1 re-exports `item` from inside the private rung, node 2 consumes the
    // re-export from `other/x.rs`; flattened, node 2 consumes `item` directly.
    let rungs = sibling_rungs();
    let ladder = Case::new(&SIBLING_PATHS, "src/render/report.rs", &rungs, Some(1))
        .consumed_by(
            &["src/render/report/fit.rs", "src/other/x.rs"],
            &[(1, 0, EdgeKind::ReExport), (2, 1, EdgeKind::Call)],
        )
        .run();
    assert_eq!(ladder.needed, None);
}

#[test]
fn should_stay_silent_for_a_consumer_that_no_rung_can_place() {
    // a consumer in a file no rung lists (a `cfg(test)` module, another crate
    // target) may need more than any spelling gives, so nothing is advised.
    let mut paths = SIBLING_PATHS.to_vec();
    paths.push("src/stray.rs");
    let rungs = sibling_rungs();
    let alone = Case::new(&paths, "src/render/report.rs", &rungs, Some(1)).run();
    let stray = Case::new(&paths, "src/render/report.rs", &rungs, Some(1))
        .consumed_by(&["src/stray.rs"], &[(1, 0, EdgeKind::Call)])
        .run();
    assert_eq!(alone.needed, Some(ScopeLevel::Folder));
    assert_eq!(stray.needed, None);
}

#[test]
fn should_advise_the_sibling_folder_for_a_same_folder_consumer() {
    const PATHS: [&str; 5] = [
        "src/lib.rs",
        "src/other/x.rs",
        "src/relocation.rs",
        "src/relocation/collision.rs",
        "src/relocation/solver.rs",
    ];
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
    let alone = Case::new(&PATHS, "src/relocation/collision.rs", &rungs, Some(2)).run();
    let consumed = Case::new(&PATHS, "src/relocation/collision.rs", &rungs, Some(2))
        .consumed_by(&["src/relocation/solver.rs"], &[(1, 0, EdgeKind::Call)])
        .run();
    assert_eq!(alone.needed, Some(ScopeLevel::File));
    assert_eq!(consumed.needed, Some(ScopeLevel::Folder));
}

#[test]
fn should_keep_the_private_rung_when_every_consumer_sits_inside_it() {
    let rungs = sibling_rungs();
    let consumed = Case::new(&SIBLING_PATHS, "src/render/report.rs", &rungs, Some(1))
        .consumed_by(&["src/render/report/fit.rs"], &[(1, 0, EdgeKind::Call)])
        .run();
    assert_eq!(consumed.needed, Some(ScopeLevel::Folder));
}

#[test]
fn should_not_advise_a_crate_root_child_item_used_by_the_crate_root() {
    const PATHS: [&str; 4] = [
        "src/lib.rs",
        "src/parse.rs",
        "src/parse/a.rs",
        "src/parse/b.rs",
    ];
    let paths = PATHS;
    let rungs: [FixtureRung; 2] = [
        (
            &["src/parse.rs", "src/parse/a.rs", "src/parse/b.rs"],
            Some("src/parse.rs"),
        ),
        (&PATHS, None),
    ];
    let alone = Case::new(&paths, "src/parse.rs", &rungs, Some(1)).run();
    let consumed = Case::new(&paths, "src/parse.rs", &rungs, Some(1))
        .consumed_by(&["src/lib.rs"], &[(1, 0, EdgeKind::Call)])
        .run();
    assert_eq!(alone.needed, Some(ScopeLevel::Folder));
    assert_eq!(consumed.needed, None);
}

#[test]
fn should_pick_the_last_of_rungs_with_the_same_files() {
    let rungs: [FixtureRung; 3] = [
        (&["src/b/c.rs"], None),
        (&["src/b/c.rs"], None),
        (&BCD_ALL, None),
    ];
    let mut case = Case::new(&BCD_PATHS, "src/b/c.rs", &rungs, Some(2));
    case.others = &[];
    // the first rung is a file rung: the identical second rung is its spelling
    // twin, and the later one wins, so the finding names the wider spelling.
    let ladder = case.run();
    assert_eq!(ladder.needed, Some(ScopeLevel::File));
}

#[test]
fn should_count_a_wider_exposer_as_a_user_of_the_exposed_item() {
    let rungs: [FixtureRung; 3] = [
        (&["src/b/c.rs"], None),
        (&["src/b/c.rs", "src/b/d.rs"], None),
        (&BCD_ALL, None),
    ];
    let exposures = [Exposure {
        exposer: NodeId(1),
        exposed: NodeId(0),
        reach: true,
    }];
    let exposer_rungs: [FixtureRung; 1] = [(&BCD_ALL, None)];
    let mut case =
        Case::new(&BCD_PATHS, "src/b/c.rs", &rungs, Some(2)).consumed_by(&["src/b/d.rs"], &[]);
    case.exposures = &exposures;
    case.exposer_scope = Some(scope_of(NodeId(1), &exposer_rungs, Some(0)));
    // the exposer's declared reach spans the crate, so the item is needed there.
    assert_eq!(case.run().needed, None);

    // an exposer with no scope statement (a trait impl) leaves the reach unknown.
    case.exposer_scope = None;
    let ladder = case.run();
    assert_eq!((ladder.declared, ladder.needed), (None, None));

    // a header self type only needs the exposer's own file.
    let header = [Exposure {
        reach: false,
        ..exposures[0]
    }];
    case.exposures = &header;
    assert_eq!(case.run().needed, Some(ScopeLevel::Folder));
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

#[test]
fn should_count_an_advisory_use_as_a_user_without_an_edge() {
    let rungs: [FixtureRung; 3] = [
        (&["src/b/c.rs"], None),
        (&["src/b/c.rs", "src/b/d.rs"], None),
        (&BCD_ALL, None),
    ];
    let advisory = [AdvisoryUse {
        user: NodeId(1),
        used: NodeId(0),
        from_test: false,
    }];
    let mut case =
        Case::new(&BCD_PATHS, "src/b/c.rs", &rungs, Some(2)).consumed_by(&["src/b/d.rs"], &[]);
    assert_eq!(case.run().needed, Some(ScopeLevel::File));

    case.advisory = &advisory;
    assert_eq!(case.run().needed, Some(ScopeLevel::Folder));
}

#[test]
fn should_merge_advisory_uses_without_changing_nodes_edges_or_containers() {
    let fragment = |advisory_uses: Vec<AdvisoryUse>| IrFragment {
        nodes: vec![node(0, "used", 1), node(1, "user", 2)],
        containers: vec![container(1, "src/a.rs"), container(2, "src/b.rs")],
        advisory_uses,
        ..IrFragment::default()
    };
    let advisory = vec![AdvisoryUse {
        user: NodeId(1),
        used: NodeId(0),
        from_test: false,
    }];
    let plain = merge_fragments(vec![fragment(Vec::new())], "pkg", &Layout::default());
    let mut with = merge_fragments(vec![fragment(advisory.clone())], "pkg", &Layout::default());
    // the merged uses are kept for advice (test-only reads them), as-is.
    assert_eq!(with.advisory_uses, advisory);
    with.advisory_uses.clear();

    // the whole graph side of the IR is untouched; advisory uses reach only the
    // ladder verdicts and the advisory list, never nodes, edges or containers.
    assert_eq!(plain, with);
}

#[test]
fn should_merge_file_facts_from_every_fragment_without_touching_the_graph() {
    let fact = |file: &str, test_of: Option<&str>, entry: bool| FileFact {
        file: SmolStr::new(file),
        test_of: test_of.map(SmolStr::new),
        entry,
    };
    let fragment = |path: &str, facts: Vec<FileFact>| IrFragment {
        nodes: vec![node(0, "a", 0)],
        containers: vec![container(0, path)],
        file_facts: facts,
        ..IrFragment::default()
    };
    let merged = merge_fragments(
        vec![
            fragment("src/a.ts", vec![fact("src/a.ts", None, true)]),
            fragment(
                "src/b.ts",
                vec![fact("test/b.test.ts", Some("src/b.ts"), false)],
            ),
        ],
        "pkg",
        &Layout::default(),
    );
    assert_eq!(
        merged.file_facts,
        vec![
            fact("src/a.ts", None, true),
            fact("test/b.test.ts", Some("src/b.ts"), false),
        ]
    );
    let plain = merge_fragments(
        vec![
            fragment("src/a.ts", Vec::new()),
            fragment("src/b.ts", Vec::new()),
        ],
        "pkg",
        &Layout::default(),
    );
    assert_eq!(plain.file_facts, Vec::<FileFact>::new());
    assert_eq!(plain.nodes, merged.nodes);
    assert_eq!(plain.containers, merged.containers);
}
