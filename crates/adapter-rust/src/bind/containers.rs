//! Laminar container tree construction.
//!
//! Interns the file/folder/domain/package/package-group containers for the
//! parsed files so every file hangs off a strictly-ascending chain.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use smol_str::SmolStr;
use strata_ir::{Container, ContainerId, ContainerTree, ScopeLevel};

use crate::parse::ParsedFile;

/// Builds the container tree for the parsed files (free function so the node
/// builder and the public emitter share one implementation).
pub(super) fn assignment_containers(files: &[ParsedFile], root: &Path) -> Vec<Container> {
    ContainerBuilder::build(files, root)
        .tree
        .containers()
        .to_vec()
}

/// Interns containers (file/folder/domain/package/package group) for the files.
pub(super) struct ContainerBuilder {
    /// The interned container tree.
    tree: ContainerTree,
    /// File path -> its file container id.
    files: HashMap<SmolStr, ContainerId>,
}

impl ContainerBuilder {
    /// Returns the file container id for a file path.
    pub(super) fn file_of(&self, path: &SmolStr) -> ContainerId {
        self.files.get(path).copied().unwrap_or(ContainerId(0))
    }

    /// Builds the laminar container tree for `files` under `root`.
    ///
    /// Levels follow the fixed five-level mapping: the repository is the package
    /// group, the first path segment a package, the next two directory levels
    /// domain and folder, and the file itself a leaf. Shallow paths reuse a
    /// single implicit container at each missing level so every file still hangs
    /// off a valid, strictly-ascending chain.
    pub(super) fn build(files: &[ParsedFile], root: &Path) -> Self {
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
