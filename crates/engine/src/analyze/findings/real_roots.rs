//! The real directories behind a domain, package or package group (ADR-18).

use std::collections::{BTreeMap, BTreeSet};

use unlace_ir::{Container, ContainerTree, ScopeLevel};

use super::physical::real_label;
use crate::narrate::{path_segments, physical_directory, physical_namespace, real_key};
use crate::result::{ContainerNode, Level};

#[cfg(test)]
mod tests;

/// One file's real domain and package directories, repo-relative segments.
struct FileRoots {
    domain: Vec<String>,
    package: Vec<String>,
}

/// Maps every file of a container tree to the real directories its domain and
/// package occupy, restoring the see-through source roots the laminar keys
/// omit (`src`, `spec`).
pub(super) struct RealRoots {
    by_file: BTreeMap<String, FileRoots>,
}

impl RealRoots {
    /// Derives each file's domain and package directories from its own path.
    pub(super) fn from_tree(tree: &ContainerTree) -> Self {
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
                    ScopeLevel::PackageGroup => dataset = Some(current.name.as_str()),
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
            let dataset = dataset.unwrap_or("");
            let package = package.map_or_else(Vec::new, |package| real_key(package, dataset));
            let folder = file.parent.and_then(|parent| by_id.get(&parent.0).copied());
            // the synthetic `workspace` bucket names no real directory: it
            // stands for the package itself.
            let scope_of = |container: Option<&Container>| match container {
                Some(container) if !container.synthetic => real_key(container, dataset),
                _ => package.clone(),
            };
            let directory = path_segments(
                file.name
                    .rsplit_once('/')
                    .map_or("", |(directory, _)| directory),
            );
            let namespace = physical_namespace(&directory, &package, &scope_of(folder));
            let domain_directory = physical_directory(&package, &namespace, &scope_of(domain));
            by_file.insert(
                file.name.to_string(),
                FileRoots {
                    domain: domain_directory,
                    package,
                },
            );
        }
        Self { by_file }
    }

    /// The sorted real directories `node` covers, or none when its files are
    /// unknown to the tree: a package group is the repository root.
    pub(super) fn paths(&self, node: &ContainerNode, level: Level) -> Vec<String> {
        if level == Level::PackageGroup {
            return vec![".".to_owned()];
        }
        let mut files = Vec::new();
        collect_files(node, &mut files);
        let roots: BTreeSet<String> = files
            .into_iter()
            .filter_map(|name| self.by_file.get(name))
            .map(|roots| match level {
                Level::Domain => real_label(&roots.domain),
                _ => real_label(&roots.package),
            })
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
