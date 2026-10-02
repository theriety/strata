//! Physical-folder entry counts and the capacity findings drawn from them.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_ir::{Container, ContainerId, ContainerTree, ScopeLevel};

use crate::analyze::BORDERLINE_CAPACITY_MARGIN;
use crate::narrate::{
    drain_dataset, path_segments, physical_directory, project_package_rooted_path,
    project_physical_path,
};
use crate::result::{CapacityBreach, Severity, Violation, ViolationKind};

/// Counts immediate entries in each physical directory without changing the
/// source-root-transparent laminar tree. A directory binds its direct files
/// plus its distinct direct child directories; deeper descendants do not add
/// to an ancestor's count.
#[cfg(test)]
pub(super) fn physical_folder_entries(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
) -> BTreeMap<Vec<String>, u32> {
    physical_folder_entries_with_rootedness(tree, namespaces, true)
}

pub(in crate::analyze) fn physical_folder_entries_repository_relative(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
) -> BTreeMap<Vec<String>, u32> {
    physical_folder_entries_with_rootedness(tree, namespaces, false)
}

fn physical_folder_entries_with_rootedness(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
    package_rooted: bool,
) -> BTreeMap<Vec<String>, u32> {
    let by_id: BTreeMap<u32, &Container> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let mut direct_files: BTreeMap<Vec<String>, u32> = BTreeMap::new();
    let mut child_folders: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();

    for file in tree
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
    {
        let mut ancestor = Some(file);
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
        let folder = file.parent.and_then(|parent| by_id.get(&parent.0).copied());
        let mut logical = path_segments(folder.map_or("", |folder| folder.name.as_str()));
        let package = drain_dataset(path_segments(package.unwrap_or("")), &dataset_segments);
        // the synthetic `workspace` bucket names no real directory: it stands
        // for the package itself.
        if folder.is_some_and(|folder| folder.synthetic) {
            logical.clone_from(&package);
        }
        if logical.starts_with(&dataset_segments) {
            logical.drain(..dataset_segments.len());
        }
        let directory = if let Some(namespace) = namespaces.and_then(|values| values.get(&file.id))
        {
            // restore the package-relative namespace between the package and
            // the folder's scope (ADR-18): `crates/core` + `src` + `cluster`.
            let relative = physical_directory(&package, &path_segments(namespace), &logical);
            if package_rooted {
                project_package_rooted_path(dataset, &relative)
            } else {
                project_physical_path(dataset, &relative)
            }
        } else {
            let raw_directory: Vec<String> = file
                .name
                .rsplit_once('/')
                .map_or("", |(directory, _)| directory)
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect();
            if package_rooted {
                project_package_rooted_path(dataset, &raw_directory)
            } else {
                project_physical_path(dataset, &raw_directory)
            }
        };
        *direct_files.entry(directory.clone()).or_default() += 1;

        let root_depth = dataset_segments.len();
        for depth in root_depth.max(1)..directory.len() {
            let (Some(parent), Some(child)) = (directory.get(..depth), directory.get(depth)) else {
                continue;
            };
            child_folders
                .entry(parent.to_vec())
                .or_default()
                .insert(child.clone());
        }
    }

    let mut entries = direct_files;
    for (folder, children) in child_folders {
        let child_count = u32::try_from(children.len()).unwrap_or(u32::MAX);
        *entries.entry(folder).or_default() += child_count;
    }
    entries
}

pub(in crate::analyze) fn physical_folder_findings(
    entries: &BTreeMap<Vec<String>, u32>,
    cap: u32,
) -> Vec<Violation> {
    entries
        .iter()
        .filter_map(|(path, &measure)| {
            if measure <= cap {
                return None;
            }
            let severity =
                if f64::from(measure) > f64::from(cap) * (1.0 + BORDERLINE_CAPACITY_MARGIN) {
                    Severity::Violation
                } else {
                    Severity::Borderline
                };
            let qualified = path.join("/");
            Some(Violation {
                kind: ViolationKind::Capacity,
                severity,
                location: path.clone(),
                detail: format!("folder `{qualified}` holds {measure} against a cap of {cap}"),
                break_suggestions: None,
                capacity: Some(CapacityBreach {
                    measured: measure,
                    cap,
                    path: None,
                }),
            })
        })
        .collect()
}
