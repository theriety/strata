//! File placements: indexes a tree's file containers by their real folder keys.

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{ContainerTree, ScopeLevel};

use super::paths::{
    drain_dataset, path_segments, physical_directory, physical_namespace,
    project_package_rooted_path, project_physical_path,
};

/// Every file's placement within one tree, folded for display.
pub(super) struct FilePlacements {
    /// Each file path's folded parent path.
    pub(super) parent_of: BTreeMap<String, Vec<String>>,
    /// Each file's logical folder key, relative to the package group, before
    /// its namespace is restored.
    pub(super) logical_parent_of: BTreeMap<String, Vec<String>>,
    /// Each file's package-relative namespace (ADR-18): the transparent
    /// source-root segments retained across a move.
    pub(super) namespace_of: BTreeMap<String, Vec<String>>,
    /// Each file's manifest package key, relative to the package group.
    pub(super) package_of: BTreeMap<String, Vec<String>>,
    /// Each file's projected package-group root segments.
    pub(super) root_of: BTreeMap<String, Vec<String>>,
    /// Each file's parent container id: files sharing one are one folder
    /// cluster of the candidate.
    pub(super) cluster_of: BTreeMap<String, u32>,
    /// The (sorted) member file paths of each folded folder path.
    pub(super) members_of: BTreeMap<Vec<String>, Vec<String>>,
    /// Direct files plus distinct direct child folders at each physical path.
    pub(super) entry_count: BTreeMap<Vec<String>, u32>,
}

pub(super) fn folder_entry_counts(
    parent_of: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<Vec<String>, u32> {
    let mut counts = BTreeMap::new();
    let mut children: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    for folder in parent_of.values() {
        *counts.entry(folder.clone()).or_default() += 1;
        for depth in 1..folder.len() {
            let (Some(parent), Some(child)) = (folder.get(..depth), folder.get(depth)) else {
                continue;
            };
            children
                .entry(parent.to_vec())
                .or_default()
                .insert(child.clone());
        }
    }
    for (folder, names) in children {
        *counts.entry(folder).or_default() += u32::try_from(names.len()).unwrap_or(u32::MAX);
    }
    counts
}

/// Indexes a tree's file containers by their real folder key.
///
/// A file's destination is the key of the folder container holding it — the real
/// directory the file lands in, and the path a move would target. Folder keys are
/// package-qualified real paths (`cts/core`, `nested-ts/geometry`), so they mv
/// cleanly. The domain/package/group above the folder are display groupings, not
/// path components: a suggested domain that merges several real directories has
/// no directory of its own to name, so it never contributes a destination
/// segment (and its injective display label never leaks into a move target).
pub(super) fn index_files(tree: &ContainerTree, package_rooted: bool) -> FilePlacements {
    let containers = tree.containers();
    let by_id: BTreeMap<u32, &strata_ir::Container> = containers
        .iter()
        .map(|container| (container.id.0, container))
        .collect();

    let mut parent_of = BTreeMap::new();
    let mut logical_parent_of = BTreeMap::new();
    let mut namespace_of = BTreeMap::new();
    let mut package_of = BTreeMap::new();
    let mut root_of = BTreeMap::new();
    let mut cluster_of = BTreeMap::new();
    let mut members_of: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
    for container in containers {
        if container.level != ScopeLevel::File {
            continue;
        }
        let folder = container.parent.and_then(|parent| by_id.get(&parent.0));
        let mut ancestor = Some(container);
        let (mut dataset, mut package) = (None, None);
        while let Some(current) = ancestor {
            match current.level {
                ScopeLevel::PackageGroup => dataset = Some(current.name.as_str()),
                ScopeLevel::Package if package.is_none() => package = Some(current.name.as_str()),
                _ => {}
            }
            ancestor = current
                .parent
                .and_then(|parent| by_id.get(&parent.0).copied());
        }
        let dataset = dataset.unwrap_or("");
        let dataset_segments = path_segments(dataset);
        let package = drain_dataset(path_segments(package.unwrap_or("")), &dataset_segments);
        // the synthetic `workspace` bucket names no real directory, so a
        // root-level file's move target is its package, not an invented
        // `.../workspace` path: the bucket stands for the package itself.
        let logical = if folder.is_some_and(|folder| folder.synthetic) {
            package.clone()
        } else {
            drain_dataset(
                path_segments(folder.map_or("", |folder| folder.name.as_str())),
                &dataset_segments,
            )
        };
        let raw_segments = path_segments(&container.name);
        let physical_file_segments = if package_rooted {
            project_package_rooted_path(dataset, &raw_segments)
        } else {
            project_physical_path(dataset, &raw_segments)
        };
        let physical_directory_segments = physical_file_segments
            .get(..physical_file_segments.len().saturating_sub(1))
            .unwrap_or_default();
        // both projections prefix exactly the package-group root segments.
        let root_length = dataset_segments
            .len()
            .min(physical_directory_segments.len());
        let (root, directory) = physical_directory_segments.split_at(root_length);
        let namespace = physical_namespace(directory, &package, &logical);
        let mut segments = root.to_vec();
        segments.extend(physical_directory(&package, &namespace, &logical));
        let physical_file = physical_file_segments.join("/");
        parent_of.insert(physical_file.clone(), segments.clone());
        logical_parent_of.insert(physical_file.clone(), logical);
        namespace_of.insert(physical_file.clone(), namespace);
        package_of.insert(physical_file.clone(), package);
        root_of.insert(physical_file.clone(), root.to_vec());
        if let Some(parent) = container.parent {
            cluster_of.insert(physical_file.clone(), parent.0);
        }
        members_of.entry(segments).or_default().push(physical_file);
    }
    for members in members_of.values_mut() {
        members.sort();
    }
    let entry_count = folder_entry_counts(&parent_of);
    FilePlacements {
        parent_of,
        logical_parent_of,
        namespace_of,
        package_of,
        root_of,
        cluster_of,
        members_of,
        entry_count,
    }
}

/// Projects immutable file identities onto their candidate physical folders.
#[cfg(test)]
pub(crate) fn physical_file_folders(tree: &ContainerTree) -> BTreeMap<String, Vec<String>> {
    index_files(tree, false).parent_of
}
