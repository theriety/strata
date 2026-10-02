//! Builds the per-file facts narration consults when explaining moves.

use std::collections::{BTreeMap, BTreeSet};

use strata_core::score::KindWeights;
use strata_ir::{Container, Polarity, ScopeLevel, Snapshot};

use crate::analyze::relocation::FileInfo;
use crate::narrate::{FileFacts, project_package_rooted_path, project_physical_path};

#[cfg(test)]
pub(super) fn file_facts(
    snapshot: &Snapshot,
    weights: &KindWeights,
    folder_cap: u32,
    files: &[FileInfo],
    test_zone: &[bool],
) -> FileFacts {
    file_facts_with_rootedness(snapshot, weights, folder_cap, files, test_zone, true)
}

/// Builds the per-file facts narration consults from priced dependencies,
/// test-zone membership, and the configured folder capacity.
pub(super) fn file_facts_repository_relative(
    snapshot: &Snapshot,
    weights: &KindWeights,
    folder_cap: u32,
    files: &[FileInfo],
    test_zone: &[bool],
) -> FileFacts {
    file_facts_with_rootedness(snapshot, weights, folder_cap, files, test_zone, false)
}

fn file_facts_with_rootedness(
    snapshot: &Snapshot,
    weights: &KindWeights,
    folder_cap: u32,
    files: &[FileInfo],
    test_zone: &[bool],
    package_rooted: bool,
) -> FileFacts {
    let ir = snapshot.ir();
    let by_id: BTreeMap<u32, &Container> = ir
        .containers
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let file_of: BTreeMap<u32, String> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| {
            let mut ancestor = Some(container);
            let mut dataset = None;
            while let Some(current) = ancestor {
                if current.level == ScopeLevel::PackageGroup {
                    dataset = Some(current.name.as_str());
                }
                ancestor = current
                    .parent
                    .and_then(|parent| by_id.get(&parent.0).copied());
            }
            let relative: Vec<String> = container
                .name
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect();
            let path = dataset.map_or_else(
                || relative.join("/"),
                |dataset| {
                    if package_rooted {
                        project_package_rooted_path(dataset, &relative).join("/")
                    } else {
                        project_physical_path(dataset, &relative).join("/")
                    }
                },
            );
            (container.id.0, path)
        })
        .collect();
    let container_of: BTreeMap<u32, u32> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.container.0))
        .collect();

    let mut edge_weights: BTreeMap<(String, String), f64> = BTreeMap::new();
    for edge in &ir.edges {
        let (Some(source), Some(target)) = (
            container_of
                .get(&edge.source.0)
                .and_then(|container| file_of.get(container)),
            container_of
                .get(&edge.target.0)
                .and_then(|container| file_of.get(container)),
        ) else {
            continue;
        };
        if source == target {
            continue;
        }
        *edge_weights
            .entry((source.clone(), target.clone()))
            .or_insert(0.0) += weights.edge_weight(edge.kind, edge.confidence);
    }

    // a spec file holds at least one symbol and nothing but test cases.
    let mut case_only: BTreeMap<u32, bool> = BTreeMap::new();
    for node in &ir.nodes {
        let entry = case_only.entry(node.container.0).or_insert(true);
        *entry &= node.polarity == Polarity::TestCase;
    }
    let test_case_files = case_only
        .iter()
        .filter(|&(_, &only_cases)| only_cases)
        .filter_map(|(container, _)| file_of.get(container).cloned())
        .collect();

    // the tie-cut zone beyond the case-only set: pattern-marked paths and
    // support-polarity helpers narrate their moves as following a subject
    // exactly like spec files do.
    let mut shadow_test_files = BTreeSet::new();
    for (index, file) in files.iter().enumerate() {
        let zone = test_zone.get(index).copied().unwrap_or(false);
        let only_cases = case_only.get(&file.container).copied().unwrap_or(false);
        if zone
            && !only_cases
            && let Some(path) = file_of.get(&file.container)
        {
            shadow_test_files.insert(path.clone());
        }
    }

    FileFacts {
        edge_weights,
        test_case_files,
        shadow_test_files,
        folder_cap,
    }
}
