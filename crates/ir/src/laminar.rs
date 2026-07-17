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

use std::collections::{BTreeMap, HashMap};

use smol_str::SmolStr;

use crate::container::{Container, ContainerId, ContainerTree};
use crate::node::ScopeLevel;

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
    );

    let mut sorted: Vec<&SmolStr> = paths.iter().collect();
    sorted.sort();
    sorted.dedup();

    for path in sorted {
        let all: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        // the trailing segment is the filename, not a directory.
        let dir = all.get(..all.len().saturating_sub(1)).unwrap_or(&[]);

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
        let package_key: SmolStr = if package_root.is_empty() {
            SmolStr::new(root_name)
        } else {
            SmolStr::new(package_root)
        };

        // directory segments below the package root, with one transparent
        // source-root segment stripped.
        let within = dir.get(package_depth.min(dir.len())..).unwrap_or(&[]);
        let scope = match within.first() {
            Some(first) if layout.source_roots.iter().any(|root| root == first) => {
                within.get(1..).unwrap_or(&[])
            }
            _ => within,
        };

        // an empty scope means the file sits at the package/source root with no
        // real directory of its own, so both the domain and the folder minted
        // for it are the synthetic `workspace` bucket the render later collapses.
        let synthetic = scope.is_empty();
        let package = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::Package,
            package_key.clone(),
            Some(group),
            false,
        );
        let domain = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::Domain,
            scoped_key(&package_key, scope, 1),
            Some(package),
            synthetic,
        );
        let folder = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::Folder,
            scoped_key(&package_key, scope, scope.len()),
            Some(domain),
            synthetic,
        );
        let file = intern(
            &mut containers,
            &mut by_key,
            ScopeLevel::File,
            path.clone(),
            Some(folder),
            false,
        );
        files.insert(path.clone(), file);
    }

    LaminarTree {
        tree: ContainerTree::new(containers),
        files,
    }
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
fn intern(
    containers: &mut Vec<Container>,
    by_key: &mut BTreeMap<(ScopeLevel, SmolStr), ContainerId>,
    level: ScopeLevel,
    key: SmolStr,
    parent: Option<ContainerId>,
    synthetic: bool,
) -> ContainerId {
    if let Some(&id) = by_key.get(&(level, key.clone())) {
        if !synthetic && let Some(existing) = containers.get_mut(id.0 as usize) {
            existing.synthetic = false;
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
        assert!(built.tree.containers().is_empty());
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
}
