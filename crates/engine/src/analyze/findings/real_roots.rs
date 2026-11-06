//! The real directories behind a domain, package or package group (ADR-18).

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use unlace_ir::{Container, ContainerTree, ScopeLevel};

use super::physical::real_label;
use crate::narrate::{path_segments, physical_directory, physical_namespace, real_key_named};
use crate::result::{ContainerNode, Level};

#[cfg(test)]
mod tests;

/// One file's real domain and package directories, repo-relative segments.
struct FileRoots {
    domain: Vec<String>,
    package: Vec<String>,
    /// The file's nearest domain container.
    domain_id: Option<u32>,
    /// The direct sub-folder of that domain the file sits in, or `None` when it
    /// sits in the domain's own directory.
    below_domain: Option<SubFolder>,
}

/// One entry a domain's cap counts: a direct sub-folder of the domain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SubFolder {
    /// The first segment beneath the domain's own key: `a` for `a`, `a/b/c`.
    Inside(String),
    /// A real folder whose key does not extend the domain's key; each distinct
    /// one is its own entry.
    Foreign(Vec<String>),
}

/// Names the direct sub-folder of a domain that a folder falls under.
///
/// Reads the keys as real segments (`real_key_named`, which knows whether the
/// label was minted into the key, over the undecorated key), so a real
/// `gen/gen/` beneath a domain `gen` is the sub-folder `gen`, while the folder
/// keyed exactly like the domain is the domain's own directory and names none.
fn sub_folder_of(folder: &[String], domain: &[String]) -> Option<SubFolder> {
    match folder.strip_prefix(domain) {
        Some(rest) => rest.first().cloned().map(SubFolder::Inside),
        None => Some(SubFolder::Foreign(folder.to_vec())),
    }
}

/// Maps every file of a container tree to the real directories its domain and
/// package occupy, restoring the see-through source roots the laminar keys
/// omit (`src`, `spec`).
pub(in crate::analyze) struct RealRoots {
    by_file: BTreeMap<String, FileRoots>,
}

impl RealRoots {
    /// Derives each file's domain and package directories from its own path.
    pub(in crate::analyze) fn from_tree(tree: &ContainerTree) -> Self {
        Self::from_tree_keyed(tree, &BTreeMap::new())
    }

    /// [`Self::from_tree`] over a candidate tree, whose upper containers may
    /// carry a decorated display name: `key_by_id` supplies the undecorated key
    /// of each one, and every real path and sub-folder is read from it. A name
    /// decorated by the election fallback (`2024 (2024.x)`) is not in
    /// `key_by_id`, so its folders count as foreign sub-folders: consistent
    /// between findings and pricing, but an overcount.
    pub(in crate::analyze) fn from_tree_keyed(
        tree: &ContainerTree,
        key_by_id: &BTreeMap<u32, SmolStr>,
    ) -> Self {
        let key_name = |container: &Container| -> String {
            key_by_id
                .get(&container.id.0)
                .map_or_else(|| container.name.to_string(), ToString::to_string)
        };
        let by_id: BTreeMap<u32, &Container> = tree
            .containers()
            .iter()
            .map(|container| (container.id.0, container))
            .collect();
        let mut by_file = BTreeMap::new();
        for file in tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
        {
            let (mut dataset, mut package, mut domain) = (None, None, None);
            let mut ancestor = Some(file);
            while let Some(current) = ancestor {
                match current.level {
                    ScopeLevel::PackageGroup => dataset = Some(key_name(current)),
                    ScopeLevel::Package if package.is_none() => {
                        package = Some(current);
                    }
                    ScopeLevel::Domain if domain.is_none() => domain = Some(current),
                    _ => {}
                }
                ancestor = current
                    .parent
                    .and_then(|parent| by_id.get(&parent.0).copied());
            }
            let dataset = dataset.unwrap_or_default();
            let package = package.map_or_else(Vec::new, |package| {
                real_key_named(package, &key_name(package), &dataset)
            });
            let folder = file.parent.and_then(|parent| by_id.get(&parent.0).copied());
            // the synthetic `workspace` bucket names no real directory: it
            // stands for the package itself.
            let scope_of = |container: Option<&Container>| match container {
                Some(container) if !container.synthetic => {
                    real_key_named(container, &key_name(container), &dataset)
                }
                _ => package.clone(),
            };
            let directory = path_segments(
                file.name
                    .rsplit_once('/')
                    .map_or("", |(directory, _)| directory),
            );
            let namespace = physical_namespace(&directory, &package, &scope_of(folder));
            let domain_directory = physical_directory(&package, &namespace, &scope_of(domain));
            let below_domain = if folder.is_some_and(|folder| folder.synthetic) {
                None
            } else {
                sub_folder_of(&scope_of(folder), &scope_of(domain))
            };
            by_file.insert(
                file.name.to_string(),
                FileRoots {
                    domain: domain_directory,
                    package,
                    domain_id: domain.map(|domain| domain.id.0),
                    below_domain,
                },
            );
        }
        Self { by_file }
    }

    /// A roots table that knows no file, for tests of levels that never read it.
    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self {
            by_file: BTreeMap::new(),
        }
    }

    /// Every domain container's cap measure with its sorted real directories,
    /// in container-id order: the domain findings' inputs, taken from the
    /// container tree so a domain the renderer drops (one echoing its package)
    /// is still measured. A domain whose files are all unknown is omitted.
    pub(super) fn domain_measures(&self) -> Vec<(u32, Vec<String>)> {
        let mut by_domain: BTreeMap<u32, (BTreeSet<&SubFolder>, BTreeSet<String>)> =
            BTreeMap::new();
        for roots in self.by_file.values() {
            let Some(id) = roots.domain_id else { continue };
            let entry = by_domain.entry(id).or_default();
            if let Some(below) = roots.below_domain.as_ref() {
                entry.0.insert(below);
            }
            entry.1.insert(real_label(&roots.domain));
        }
        by_domain
            .into_values()
            .map(|(names, dirs)| {
                (
                    u32::try_from(names.len()).unwrap_or(u32::MAX),
                    dirs.into_iter().collect(),
                )
            })
            .collect()
    }

    /// The same measure for every domain container of the tree, by container
    /// id: what candidate pricing charges against the domain cap.
    pub(in crate::analyze) fn domain_subfolder_counts(&self) -> BTreeMap<u32, u32> {
        let mut names: BTreeMap<u32, BTreeSet<&SubFolder>> = BTreeMap::new();
        for roots in self.by_file.values() {
            if let (Some(id), Some(below)) = (roots.domain_id, roots.below_domain.as_ref()) {
                names.entry(id).or_default().insert(below);
            }
        }
        names
            .into_iter()
            .map(|(id, set)| (id, u32::try_from(set.len()).unwrap_or(u32::MAX)))
            .collect()
    }

    /// The sorted real package directories `node` covers, or none when its
    /// files are unknown to the tree: a package group is the repository root.
    /// A domain's real directories come from [`Self::domain_measures`].
    pub(super) fn package_paths(&self, node: &ContainerNode, level: Level) -> Vec<String> {
        if level == Level::PackageGroup {
            return vec![".".to_owned()];
        }
        let mut files = Vec::new();
        collect_files(node, &mut files);
        let roots: BTreeSet<String> = files
            .into_iter()
            .filter_map(|name| self.by_file.get(name))
            .map(|roots| real_label(&roots.package))
            .collect();
        roots.into_iter().collect()
    }
}

/// Collects the names of every file beneath `node`.
fn collect_files<'a>(node: &'a ContainerNode, files: &mut Vec<&'a str>) {
    if node.level == Level::File {
        files.push(node.name.as_str());
    }
    for child in node.children.iter().flatten() {
        collect_files(child, files);
    }
}
