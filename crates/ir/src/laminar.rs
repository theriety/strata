//! Shared construction of the laminar container tree from repo-relative file
//! paths.
//!
//! Every language adapter derives the same file/folder/domain/package/package-
//! group hierarchy, so the mapping lives here once. Two inputs shape it:
//!
//! - **package roots** — the relative directories that own a build manifest (or
//!   are named as a workspace member). Each is the root of one `package`; a file
//!   belongs to its *nearest enclosing* package root. A repo with no members is a
//!   single package rooted at the repository (the empty package-root set).
//! - **source roots** — directory names (`src`, `spec`, `test`, …) treated as
//!   transparent: a single leading source-root segment *below* the package root
//!   is stripped so a file and its test share a domain/folder.
//!
//! Container `name`s remain full-prefix keys (`ai/adapters`, not `adapters`);
//! callers that render a tree collapse adjacent overlapping segments themselves.
//!
//! A third input, the adapter-stated folder companions (ADR-31), places a file
//! `x.ts` that is the entry of the folder `x/` beside it as if it were
//! `x/index.ts`: [`effective_directory`] is the one place that answers "which
//! directory does this file count in", for this builder and for every engine
//! consumer that reasons about structure. The file keeps its physical path.

use std::collections::{BTreeMap, HashMap};

use smol_str::SmolStr;

use crate::container::{Container, ContainerId, ContainerTree};
use crate::file_fact::FileFact;
use crate::node::ScopeLevel;

/// The folder each companion file is the entry of (ADR-31), read from the
/// adapters' [`FileFact::folder_companion`] facts.
///
/// Only a companion folder that sits beside its file (the folder's parent
/// directory is the file's own directory) is kept: that is the shape the fact
/// describes, and it keeps the file's package root and transparent source
/// root unchanged, so a malformed fact degrades to "no companion". Two files
/// may name one folder (a source and a beside-spec): both are its entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Companions {
    /// Repo-relative file path → the repo-relative folder it is the entry of.
    by_file: BTreeMap<SmolStr, SmolStr>,
}

impl Companions {
    /// Collects the well-formed companion facts. A file stated twice keeps its
    /// last fact in input order, which the snapshot merge fixes.
    #[must_use]
    pub fn from_facts(facts: &[FileFact]) -> Self {
        let by_file = facts
            .iter()
            .filter_map(|fact| {
                let folder = fact.folder_companion.as_ref()?;
                is_sibling_folder(&fact.file, folder).then(|| (fact.file.clone(), folder.clone()))
            })
            .collect();
        Self { by_file }
    }

    /// The folder `file` is the entry of, when it is a companion.
    #[must_use]
    pub fn folder_of(&self, file: &str) -> Option<&str> {
        self.by_file.get(file).map(SmolStr::as_str)
    }

    /// Whether no file is a companion.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_file.is_empty()
    }
}

/// The directory `path` counts in for structural reasoning (ADR-31): the
/// folder it is the entry of when it is a companion, otherwise its physical
/// parent directory (empty for a root-level file).
///
/// Clustering, capacity, the depth rule, reach and namespaces read this;
/// reports, narration and move paths keep the physical path.
#[must_use]
pub fn effective_directory<'a>(path: &'a str, companions: &'a Companions) -> &'a str {
    companions
        .folder_of(path)
        .unwrap_or_else(|| parent_directory(path))
}

/// The physical parent directory of `path`, empty for a root-level file.
fn parent_directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

/// Whether `folder` is a named directory beside the file `file`.
fn is_sibling_folder(file: &str, folder: &str) -> bool {
    let (folder_parent, leaf) = folder.rsplit_once('/').unwrap_or(("", folder));
    !leaf.is_empty() && folder_parent == parent_directory(file)
}

/// The manifest-derived shape inputs for [`build_laminar_tree`].
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Relative package-root directories (slash-normalized, no trailing slash).
    /// Empty means the whole repository is a single package.
    pub package_roots: Vec<SmolStr>,
    /// Directory names stripped as transparent below a package root.
    pub source_roots: Vec<SmolStr>,
}

/// A built container tree plus the file-path → file-container-id index adapters
/// use to attach nodes to their owning file.
#[derive(Debug, Clone)]
pub struct LaminarTree {
    /// The interned container tree.
    pub tree: ContainerTree,
    /// Repo-relative file path → its file-level container id.
    pub files: HashMap<SmolStr, ContainerId>,
}

/// Builds the laminar container tree for `paths` under a package group named
/// `root_name`, honoring the manifest-derived `layout`.
///
/// Each file resolves to `package` = its nearest enclosing package root (the
/// repository when none applies); one leading source-root segment below that
/// root is dropped; the first remaining segment keys the domain and the full
/// remaining directory path keys the folder; a file with no directory below
/// its package root hangs off a synthetic `workspace` bucket so every file
/// keeps a strictly-ascending ancestor chain.
#[must_use]
pub fn build_laminar_tree(paths: &[SmolStr], root_name: &str, layout: &Layout) -> LaminarTree {
    build_laminar_tree_with_companions(paths, root_name, layout, &Companions::default())
}

/// [`build_laminar_tree`] that also places each folder companion as its
/// folder's entry (ADR-31): the domain and folder of `x.ts` are read from its
/// [`effective_directory`] `x`, so its file container hangs under the folder
/// container of `x/` exactly as `x/index.ts` would. The package root still
/// comes from the physical path, and the file container keeps its physical
/// name. A companion folder at the package or source root becomes a domain of
/// its own, as `x/index.ts` would.
#[must_use]
pub fn build_laminar_tree_with_companions(
    paths: &[SmolStr],
    root_name: &str,
    layout: &Layout,
    companions: &Companions,
) -> LaminarTree {
    // no files means no tree: an empty repository has no root container, so
    // snapshot validation rejects it exactly as before this shared builder.
    if paths.is_empty() {
        return LaminarTree {
            tree: ContainerTree::new(Vec::new()),
            files: HashMap::new(),
        };
    }

    let root_name = if root_name.is_empty() {
        "root"
    } else {
        root_name
    };

    // package roots, nearest (most segments) first so the tightest enclosing
    // root wins; empties are ignored (they denote the repository itself).
    let mut package_roots: Vec<&str> = layout
        .package_roots
        .iter()
        .map(SmolStr::as_str)
        .filter(|root| !root.is_empty())
        .collect();
    package_roots.sort_by(|left, right| {
        segment_count(right)
            .cmp(&segment_count(left))
            .then_with(|| left.cmp(right))
    });

    let mut containers: Vec<Container> = Vec::new();
    let mut by_key: BTreeMap<(ScopeLevel, SmolStr), ContainerId> = BTreeMap::new();
    let mut files = HashMap::new();

    let group = intern(
        &mut containers,
        &mut by_key,
        ScopeLevel::PackageGroup,
        SmolStr::new(root_name),
        None,
        false,
        false,
    );

    let mut sorted: Vec<&SmolStr> = paths.iter().collect();
    sorted.sort();
    sorted.dedup();

    for path in sorted {
        let directory = effective_directory(path, companions);
        let (package_root, scope) = locate(path, directory, &package_roots, &layout.source_roots);
        let package_key: SmolStr = if package_root.is_empty() {
            SmolStr::new(root_name)
        } else {
            SmolStr::new(package_root)
        };

        // an empty scope means the file sits at the package/source root with no
        // real directory of its own, so both the domain and the folder minted
        // for it are the synthetic `workspace` bucket the render later collapses.
        let synthetic = scope.is_empty();
        // a file outside every manifest package is keyed under the group label.
        let label_keyed = package_root.is_empty();
        let package = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::Package,
            package_key.clone(),
            Some(group),
            false,
            label_keyed,
        );
        let domain = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::Domain,
            scoped_key(&package_key, &scope, 1),
            Some(package),
            synthetic,
            label_keyed,
        );
        let folder = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::Folder,
            scoped_key(&package_key, &scope, scope.len()),
            Some(domain),
            synthetic,
            label_keyed,
        );
        let file = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::File,
            path.clone(),
            Some(folder),
            false,
            false,
        );
        files.insert(path.clone(), file);
    }

    LaminarTree {
        tree: ContainerTree::new(containers),
        files,
    }
}

/// Resolves the package root a file belongs to (the empty root when no
/// manifest package encloses it) and the segments of `directory`, the file's
/// effective directory, below that root, with one transparent source-root
/// segment stripped. The package root is matched against the physical `path`,
/// so a companion stays in its own package.
fn locate<'path>(
    path: &'path str,
    directory: &'path str,
    package_roots: &[&'path str],
    source_roots: &[SmolStr],
) -> (&'path str, Vec<&'path str>) {
    let all: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let segments: Vec<&str> = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let dir = segments.as_slice();
    let package_root = package_roots
        .iter()
        .copied()
        .find(|root| is_ancestor_dir(root, &all))
        .unwrap_or("");
    let package_depth = if package_root.is_empty() {
        0
    } else {
        segment_count(package_root)
    };
    let within = dir.get(package_depth.min(dir.len())..).unwrap_or(&[]);
    let scope = match within.first() {
        Some(first) if source_roots.iter().any(|root| root == first) => {
            within.get(1..).unwrap_or(&[])
        }
        _ => within,
    };
    (package_root, scope.to_vec())
}

/// Counts the non-empty slash-separated segments of a path.
fn segment_count(path: &str) -> usize {
    path.split('/').filter(|part| !part.is_empty()).count()
}

/// Returns `true` if `root` is a segment-aligned ancestor *directory* of the
/// file whose full segments (including the filename) are `all` — its segments
/// must be a strict prefix of the file's, leaving at least the filename.
fn is_ancestor_dir(root: &str, all: &[&str]) -> bool {
    let root_segments: Vec<&str> = root.split('/').filter(|part| !part.is_empty()).collect();
    root_segments.len() < all.len()
        && all.get(..root_segments.len()) == Some(root_segments.as_slice())
}

/// Builds a domain/folder key: the package key joined with up to `take` scope
/// segments. An empty scope yields the synthetic `workspace` bucket.
fn scoped_key(package_key: &str, scope: &[&str], take: usize) -> SmolStr {
    let taken = take.min(scope.len());
    if taken == 0 {
        return SmolStr::new(format!("{package_key}/workspace"));
    }
    let mut key = String::from(package_key);
    for segment in scope.get(..taken).unwrap_or(&[]) {
        key.push('/');
        key.push_str(segment);
    }
    SmolStr::new(key)
}

/// Interns a container by `(level, key)`, returning the existing id on a hit.
///
/// A bucket is synthetic only when *every* mint of its key was synthetic: if a
/// real directory ever shares the key (a genuine `workspace/` folder), the
/// non-synthetic mint clears the flag so the render never hides a real path.
/// The same holds for `label_keyed`: a key a real package root also mints is a
/// real directory, so the label is never dropped from it.
fn intern(
    containers: &mut Vec<Container>,
    by_key: &mut BTreeMap<(ScopeLevel, SmolStr), ContainerId>,
    level: ScopeLevel,
    key: SmolStr,
    parent: Option<ContainerId>,
    synthetic: bool,
    label_keyed: bool,
) -> ContainerId {
    if let Some(&id) = by_key.get(&(level, key.clone())) {
        if let Some(existing) = containers.get_mut(id.0 as usize) {
            existing.synthetic &= synthetic;
            existing.label_keyed &= label_keyed;
        }
        return id;
    }
    let id = ContainerId(u32::try_from(containers.len()).unwrap_or(u32::MAX));
    containers.push(Container {
        id,
        name: key.clone(),
        level,
        parent,
        synthetic,
        label_keyed,
    });
    by_key.insert((level, key), id);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects `(level, name)` pairs for every container, sorted for stable
    /// assertions regardless of insertion order.
    fn levels(tree: &ContainerTree) -> Vec<(ScopeLevel, String)> {
        let mut pairs: Vec<(ScopeLevel, String)> = tree
            .containers()
            .iter()
            .map(|container| (container.level, container.name.to_string()))
            .collect();
        pairs.sort();
        pairs
    }

    /// Returns the full package-group → file chain of names for one file path.
    /// A missing file or dangling parent yields a short chain the caller's
    /// equality assertion catches, so no `unwrap`/`expect` is needed.
    fn chain(built: &LaminarTree, path: &str) -> Vec<String> {
        let containers = built.tree.containers();
        let by_id = |id: ContainerId| containers.iter().find(|c| c.id == id).cloned();
        let mut names = Vec::new();
        let mut current = built
            .files
            .get(&SmolStr::new(path))
            .copied()
            .and_then(by_id);
        while let Some(node) = current {
            names.push(node.name.to_string());
            current = node.parent.and_then(by_id);
        }
        names.reverse();
        names
    }

    fn layout(package_roots: &[&str], source_roots: &[&str]) -> Layout {
        Layout {
            package_roots: package_roots.iter().copied().map(SmolStr::new).collect(),
            source_roots: source_roots.iter().copied().map(SmolStr::new).collect(),
        }
    }

    #[test]
    fn should_unify_src_and_spec_into_one_package() {
        let paths = [
            SmolStr::new("src/adapters/openai.ts"),
            SmolStr::new("spec/adapters/openai.spec.ts"),
        ];
        let built = build_laminar_tree(&paths, "ai", &layout(&[], &["src", "spec"]));

        // exactly one package, named after the repo, and both files share the
        // adapters domain/folder.
        let packages: Vec<_> = built
            .tree
            .containers()
            .iter()
            .filter(|c| c.level == ScopeLevel::Package)
            .map(|c| c.name.to_string())
            .collect();
        assert_eq!(packages, vec!["ai".to_string()]);
        assert_eq!(
            chain(&built, "src/adapters/openai.ts"),
            vec![
                "ai",
                "ai",
                "ai/adapters",
                "ai/adapters",
                "src/adapters/openai.ts"
            ]
        );
        assert_eq!(
            chain(&built, "spec/adapters/openai.spec.ts"),
            vec![
                "ai",
                "ai",
                "ai/adapters",
                "ai/adapters",
                "spec/adapters/openai.spec.ts"
            ]
        );
    }

    #[test]
    fn should_make_each_workspace_member_its_own_package() {
        let paths = [
            SmolStr::new("packages/foo/src/a.ts"),
            SmolStr::new("packages/bar/src/b.ts"),
        ];
        let built = build_laminar_tree(
            &paths,
            "mono",
            &layout(&["packages/foo", "packages/bar"], &["src"]),
        );

        let mut packages: Vec<_> = built
            .tree
            .containers()
            .iter()
            .filter(|c| c.level == ScopeLevel::Package)
            .map(|c| c.name.to_string())
            .collect();
        packages.sort();
        assert_eq!(
            packages,
            vec!["packages/bar".to_string(), "packages/foo".to_string()]
        );
        // the two adapters domains stay distinct (package-prefixed keys).
        assert!(
            levels(&built.tree)
                .contains(&(ScopeLevel::Domain, "packages/foo/workspace".to_string()))
        );
    }

    #[test]
    fn should_pick_the_nearest_enclosing_package_root() {
        let paths = [SmolStr::new("packages/foo/nested/src/deep/x.ts")];
        let built = build_laminar_tree(
            &paths,
            "mono",
            &layout(&["packages/foo", "packages/foo/nested"], &["src"]),
        );
        // nested wins over foo; src stripped; deep is the domain.
        assert_eq!(
            chain(&built, "packages/foo/nested/src/deep/x.ts"),
            vec![
                "mono",
                "packages/foo/nested",
                "packages/foo/nested/deep",
                "packages/foo/nested/deep",
                "packages/foo/nested/src/deep/x.ts"
            ]
        );
    }

    #[test]
    fn should_hang_root_level_files_off_a_workspace_bucket() {
        let paths = [SmolStr::new("index.ts"), SmolStr::new("src/top.ts")];
        let built = build_laminar_tree(&paths, "ai", &layout(&[], &["src"]));
        // both a bare root file and a source-root-top file land in ai/workspace.
        assert_eq!(
            chain(&built, "index.ts"),
            vec!["ai", "ai", "ai/workspace", "ai/workspace", "index.ts"]
        );
        assert_eq!(
            chain(&built, "src/top.ts"),
            vec!["ai", "ai", "ai/workspace", "ai/workspace", "src/top.ts"]
        );
    }

    #[test]
    fn should_produce_a_valid_strictly_ascending_tree() {
        let paths = [
            SmolStr::new("src/adapters/openai/client.ts"),
            SmolStr::new("spec/adapters/openai/client.spec.ts"),
            SmolStr::new("lib.ts"),
            SmolStr::new("packages/foo/src/x.ts"),
        ];
        let built = build_laminar_tree(&paths, "ai", &layout(&["packages/foo"], &["src", "spec"]));
        assert!(built.tree.validate().is_ok());
    }

    #[test]
    fn should_build_an_empty_tree_for_no_paths() {
        // an empty repository has no files, so it has no root container — snapshot
        // validation must still reject it as before this shared builder existed.
        let built = build_laminar_tree(&[], "ai", &layout(&[], &["src"]));
        assert_eq!(built.tree.containers(), []);
        assert!(built.files.is_empty());
    }

    #[test]
    fn should_only_strip_a_leading_source_root_segment() {
        let paths = [SmolStr::new("src/src/deep.ts")];
        let built = build_laminar_tree(&paths, "ai", &layout(&[], &["src"]));
        // only the first `src` is transparent; the second is a real domain.
        assert_eq!(
            chain(&built, "src/src/deep.ts"),
            vec!["ai", "ai", "ai/src", "ai/src", "src/src/deep.ts"]
        );
    }

    #[test]
    fn should_carry_full_depth_folder_keys_below_the_domain() {
        // folders are reality: the folder key keeps every real directory
        // segment below the package instead of truncating at two, so a deep
        // file and a shallow sibling share one domain while their folders
        // reflect their true directories.
        let paths = [
            SmolStr::new("src/google/capabilities/handlers/x.ts"),
            SmolStr::new("src/google/y.ts"),
        ];
        let built = build_laminar_tree(&paths, "ai", &layout(&[], &["src"]));

        // the deep file's folder carries the full directory path.
        assert_eq!(
            chain(&built, "src/google/capabilities/handlers/x.ts"),
            vec![
                "ai",
                "ai",
                "ai/google",
                "ai/google/capabilities/handlers",
                "src/google/capabilities/handlers/x.ts"
            ]
        );
        // the shallow sibling shares the `ai/google` domain, and its folder
        // stays at its real (one-segment) directory.
        assert_eq!(
            chain(&built, "src/google/y.ts"),
            vec!["ai", "ai", "ai/google", "ai/google", "src/google/y.ts"]
        );
    }

    /// The names of the containers at `level` flagged label-keyed, sorted.
    fn label_keyed(built: &LaminarTree, level: ScopeLevel) -> Vec<String> {
        let mut names: Vec<String> = built
            .tree
            .containers()
            .iter()
            .filter(|c| c.level == level && c.label_keyed)
            .map(|c| c.name.to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn should_flag_only_the_containers_minted_outside_every_manifest_package() {
        let paths = [
            SmolStr::new("atlas/src/agent/loop.ts"),
            SmolStr::new("loose/y.ts"),
        ];
        let built = build_laminar_tree(&paths, "ws", &layout(&["atlas"], &["src"]));

        assert_eq!(label_keyed(&built, ScopeLevel::Package), vec!["ws"]);
        assert_eq!(label_keyed(&built, ScopeLevel::Domain), vec!["ws/loose"]);
        assert_eq!(label_keyed(&built, ScopeLevel::Folder), vec!["ws/loose"]);
    }

    #[test]
    fn should_not_flag_a_manifest_package_named_like_the_package_group() {
        // `ws/ws` holds the manifest: its key equals the label but is a real
        // directory, so nothing under it is label-keyed.
        let paths = [SmolStr::new("ws/src/agent/loop.ts")];
        let built = build_laminar_tree(&paths, "ws", &layout(&["ws"], &["src"]));

        assert_eq!(
            label_keyed(&built, ScopeLevel::Package),
            Vec::<String>::new()
        );
        assert_eq!(
            label_keyed(&built, ScopeLevel::Domain),
            Vec::<String>::new()
        );
        assert_eq!(
            label_keyed(&built, ScopeLevel::Folder),
            Vec::<String>::new()
        );
    }

    #[test]
    fn should_keep_the_real_mint_when_loose_files_share_a_package_key() {
        // loose files and a manifest package directory named like the group
        // intern one `(Package, ws)` container: a real mint always wins, the
        // way it does for a synthetic bucket, while the loose files' own
        // domain and folder stay flagged.
        let paths = [
            SmolStr::new("ws/src/agent/loop.ts"),
            SmolStr::new("loose/y.ts"),
        ];
        let built = build_laminar_tree(&paths, "ws", &layout(&["ws"], &["src"]));

        assert_eq!(
            label_keyed(&built, ScopeLevel::Package),
            Vec::<String>::new()
        );
        assert_eq!(label_keyed(&built, ScopeLevel::Domain), vec!["ws/loose"]);
    }

    /// The folder container name of one file: the fourth link of its chain,
    /// empty when the chain is short.
    fn folder(built: &LaminarTree, path: &str) -> String {
        chain(built, path).get(3).cloned().unwrap_or_default()
    }

    /// A companion index from `(file, folder)` facts.
    fn companions(pairs: &[(&str, &str)]) -> Companions {
        let facts: Vec<FileFact> = pairs
            .iter()
            .map(|(file, folder)| FileFact::folder_companion(*file, *folder))
            .collect();
        Companions::from_facts(&facts)
    }

    #[test]
    fn should_return_the_companion_folder_as_the_effective_directory() {
        let index = companions(&[("src/tree/validation.ts", "src/tree/validation")]);

        assert_eq!(
            effective_directory("src/tree/validation.ts", &index),
            "src/tree/validation"
        );
        assert_eq!(
            effective_directory("src/tree/traversal.ts", &index),
            "src/tree"
        );
        assert_eq!(effective_directory("index.ts", &index), "");
    }

    #[test]
    fn should_ignore_a_companion_folder_that_is_not_beside_its_file() {
        // a folder elsewhere, a nested folder and an empty leaf are not the
        // shape the fact describes, so the file keeps its own directory.
        let index = companions(&[
            ("src/a/x.ts", "src/b/x"),
            ("src/a/y.ts", "src/a/y/deeper"),
            ("src/a/z.ts", "src/a/"),
        ]);

        assert!(index.is_empty());
        assert_eq!(effective_directory("src/a/x.ts", &index), "src/a");
    }

    #[test]
    fn should_place_a_companion_file_as_its_folders_entry() {
        let paths = [
            SmolStr::new("src/tree/validation.ts"),
            SmolStr::new("src/tree/validation/shape.ts"),
            SmolStr::new("src/tree/traversal.ts"),
        ];
        let index = companions(&[("src/tree/validation.ts", "src/tree/validation")]);
        let built =
            build_laminar_tree_with_companions(&paths, "mdc", &layout(&[], &["src"]), &index);

        // the companion shares the folder of `validation/`, keeping its name.
        assert_eq!(
            chain(&built, "src/tree/validation.ts"),
            vec![
                "mdc",
                "mdc",
                "mdc/tree",
                "mdc/tree/validation",
                "src/tree/validation.ts"
            ]
        );
        assert_eq!(
            folder(&built, "src/tree/validation/shape.ts"),
            "mdc/tree/validation"
        );
        assert_eq!(folder(&built, "src/tree/traversal.ts"), "mdc/tree");
        assert!(built.tree.validate().is_ok());
    }

    #[test]
    fn should_keep_a_file_without_a_companion_in_its_parent_folder() {
        let paths = [
            SmolStr::new("src/tree/validation.ts"),
            SmolStr::new("src/tree/validation/shape.ts"),
        ];
        let without = build_laminar_tree(&paths, "mdc", &layout(&[], &["src"]));
        let empty = build_laminar_tree_with_companions(
            &paths,
            "mdc",
            &layout(&[], &["src"]),
            &Companions::default(),
        );

        assert_eq!(folder(&without, "src/tree/validation.ts"), "mdc/tree");
        assert_eq!(levels(&without.tree), levels(&empty.tree));
    }

    #[test]
    fn should_nest_companions_of_companion_folders() {
        let paths = [
            SmolStr::new("src/lexer/block.ts"),
            SmolStr::new("src/lexer/block/mdcx.ts"),
            SmolStr::new("src/lexer/block/mdcx/body.ts"),
        ];
        let index = companions(&[
            ("src/lexer/block.ts", "src/lexer/block"),
            ("src/lexer/block/mdcx.ts", "src/lexer/block/mdcx"),
        ]);
        let built =
            build_laminar_tree_with_companions(&paths, "mdc", &layout(&[], &["src"]), &index);

        assert_eq!(folder(&built, "src/lexer/block.ts"), "mdc/lexer/block");
        assert_eq!(
            folder(&built, "src/lexer/block/mdcx.ts"),
            "mdc/lexer/block/mdcx"
        );
        assert_eq!(
            folder(&built, "src/lexer/block/mdcx/body.ts"),
            "mdc/lexer/block/mdcx"
        );
    }

    #[test]
    fn should_make_a_root_level_companion_folder_its_own_domain() {
        // `src` is a transparent source root, so `src/lexer.ts` would sit in
        // the synthetic workspace bucket; as the entry of `src/lexer/` it
        // belongs to the `lexer` domain, as `src/lexer/index.ts` would. A
        // repository-root `cli.ts` beside `cli/` does the same.
        let paths = [
            SmolStr::new("src/lexer.ts"),
            SmolStr::new("src/lexer/source.ts"),
            SmolStr::new("cli.ts"),
            SmolStr::new("cli/run.ts"),
        ];
        let index = companions(&[("src/lexer.ts", "src/lexer"), ("cli.ts", "cli")]);
        let built =
            build_laminar_tree_with_companions(&paths, "mdc", &layout(&[], &["src"]), &index);

        assert_eq!(
            chain(&built, "src/lexer.ts"),
            vec!["mdc", "mdc", "mdc/lexer", "mdc/lexer", "src/lexer.ts"]
        );
        assert_eq!(
            chain(&built, "cli.ts"),
            vec!["mdc", "mdc", "mdc/cli", "mdc/cli", "cli.ts"]
        );
        assert!(built.tree.validate().is_ok());
    }

    #[test]
    fn should_make_two_files_naming_one_folder_both_its_entries() {
        let paths = [
            SmolStr::new("src/a/foo.ts"),
            SmolStr::new("src/a/foo.spec.ts"),
            SmolStr::new("src/a/foo/bar.ts"),
        ];
        let index = companions(&[
            ("src/a/foo.ts", "src/a/foo"),
            ("src/a/foo.spec.ts", "src/a/foo"),
        ]);
        let built =
            build_laminar_tree_with_companions(&paths, "mdc", &layout(&[], &["src"]), &index);

        assert_eq!(folder(&built, "src/a/foo.ts"), "mdc/a/foo");
        assert_eq!(folder(&built, "src/a/foo.spec.ts"), "mdc/a/foo");
        assert_eq!(folder(&built, "src/a/foo/bar.ts"), "mdc/a/foo");
        // one folder container, however many entries name it.
        let folders = levels(&built.tree)
            .into_iter()
            .filter(|(level, name)| *level == ScopeLevel::Folder && name == "mdc/a/foo")
            .count();
        assert_eq!(folders, 1);
    }
}
