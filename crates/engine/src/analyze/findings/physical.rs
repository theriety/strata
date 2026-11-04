//! Physical-folder entry counts and the capacity findings drawn from them.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use unlace_ir::{Container, ContainerId, ContainerTree, ScopeLevel};

use crate::analyze::BORDERLINE_CAPACITY_MARGIN;
use crate::narrate::{
    path_segments, physical_directory, project_package_rooted_path, project_physical_path, real_key,
};
use crate::result::{CapacityBreach, CapacityUnit, Severity, Violation, ViolationKind};

/// Each projected physical folder's repo-relative real directory (`.` for the
/// repository root), keyed by the projected path the entry counts use.
pub(in crate::analyze) type RealFolders = BTreeMap<Vec<String>, String>;

/// Counts immediate entries in each physical directory without changing the
/// source-root-transparent laminar tree. A directory binds its direct files
/// plus its distinct direct child directories; deeper descendants do not add
/// to an ancestor's count.
#[cfg(test)]
pub(super) fn physical_folder_entries(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
) -> BTreeMap<Vec<String>, u32> {
    physical_folder_entries_with_rootedness(tree, namespaces, true).0
}

pub(in crate::analyze) fn physical_folder_entries_repository_relative(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
) -> BTreeMap<Vec<String>, u32> {
    physical_folder_entries_with_rootedness(tree, namespaces, false).0
}

/// The repository-relative entry counts together with each folder's real
/// directory, so a finding can name the folder as it exists on disk.
pub(in crate::analyze) fn physical_folders_repository_relative(
    tree: &ContainerTree,
) -> (BTreeMap<Vec<String>, u32>, RealFolders) {
    physical_folder_entries_with_rootedness(tree, None, false)
}

fn physical_folder_entries_with_rootedness(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
    package_rooted: bool,
) -> (BTreeMap<Vec<String>, u32>, RealFolders) {
    let by_id: BTreeMap<u32, &Container> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let mut direct_files: BTreeMap<Vec<String>, u32> = BTreeMap::new();
    let mut child_folders: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    let mut real_folders: RealFolders = BTreeMap::new();

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
                ScopeLevel::Package if package.is_none() => package = Some(current),
                _ => {}
            }
            ancestor = current
                .parent
                .and_then(|parent| by_id.get(&parent.0).copied());
        }
        let dataset = dataset.unwrap_or("");
        let dataset_segments = path_segments(dataset);
        let folder = file.parent.and_then(|parent| by_id.get(&parent.0).copied());
        let package = package.map_or_else(Vec::new, |package| real_key(package, dataset));
        // the synthetic `workspace` bucket names no real directory: it stands
        // for the package itself.
        let logical = if folder.is_some_and(|folder| folder.synthetic) {
            package.clone()
        } else {
            folder.map_or_else(Vec::new, |folder| real_key(folder, dataset))
        };
        // the file's own path is repo-relative: its directory is the real one,
        // whatever root name the projection prefixes.
        let real_directory: Vec<String> = path_segments(
            file.name
                .rsplit_once('/')
                .map_or("", |(directory, _)| directory),
        );
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
        } else if package_rooted {
            project_package_rooted_path(dataset, &real_directory)
        } else {
            project_physical_path(dataset, &real_directory)
        };
        *direct_files.entry(directory.clone()).or_default() += 1;
        // the projection prefixes the real directory, so each ancestor of the
        // projected path maps to the matching prefix of the real one.
        let offset = directory.len().saturating_sub(real_directory.len());
        for depth in offset..=directory.len() {
            if let Some(projected) = directory.get(..depth)
                && let Some(real) = real_directory.get(..depth - offset)
            {
                real_folders
                    .entry(projected.to_vec())
                    .or_insert_with(|| real_label(real));
            }
        }

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
    (entries, real_folders)
}

/// Joins real directory segments, naming the repository root `.`.
pub(super) fn real_label(segments: &[String]) -> String {
    if segments.is_empty() {
        ".".to_owned()
    } else {
        segments.join("/")
    }
}

pub(in crate::analyze) fn physical_folder_findings(
    entries: &BTreeMap<Vec<String>, u32>,
    cap: u32,
) -> Vec<Violation> {
    physical_folder_findings_with_real(entries, &BTreeMap::new(), cap)
}

/// Builds the folder findings, naming each folder by its real directory when
/// `real` knows it.
pub(in crate::analyze) fn physical_folder_findings_with_real(
    entries: &BTreeMap<Vec<String>, u32>,
    real: &RealFolders,
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
            let breach = CapacityBreach {
                measured: measure,
                cap,
                unit: CapacityUnit::Entries,
                config_key: "capacity.folder".to_owned(),
                real_paths: real.get(path).cloned().into_iter().collect(),
                path: None,
            };
            Some(Violation {
                kind: ViolationKind::Capacity,
                severity,
                location: breach
                    .real_location()
                    .unwrap_or_else(|| vec![path.join("/")]),
                detail: breach.describe(path),
                break_suggestions: None,
                capacity: Some(breach),
            })
        })
        .collect()
}
