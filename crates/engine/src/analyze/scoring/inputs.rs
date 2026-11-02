//! Scorer inputs derived from a placement: naming cohesion groups, path cohesion and container sizes.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::score::{CohesionGroup, ContainerSizes};
use strata_ir::{ContainerId, ContainerTree, Polarity, ScopeLevel, Snapshot};

use super::distance::folder_key_of_files;
use crate::narrate::tokenize;

/// Derives the naming-cohesion groups and the path-cohesion fraction of a
/// placement (the α and β scoring inputs, previously stubbed).
///
/// Every parent container that directly holds files forms one group carrying
/// its production SLOC and the basename token set of each member file. Path
/// cohesion is the production-SLOC-weighted fraction of files placed under the
/// same folder key their own directory already resolves to in the snapshot's
/// laminar tree — an unchanged layout scores 1.0 and every relocation dilutes
/// it.
pub(super) fn cohesion_inputs(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
) -> (Vec<CohesionGroup>, f64) {
    let ir = snapshot.ir();
    // production SLOC landing in each file container under this placement.
    let mut file_sloc: BTreeMap<u32, u32> = BTreeMap::new();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(container) = placement(node.id.0) {
            let slot = file_sloc.entry(container.0).or_default();
            *slot = slot.saturating_add(node.effective_size);
        }
    }

    let name_of: BTreeMap<u32, &SmolStr> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, &container.name))
        .collect();

    // the real folder key each file currently lives under, read from the
    // snapshot's own laminar tree; both trees key a file container by its raw
    // repo path, so the path joins a placed file back to its real directory.
    let real_folder_of = folder_key_of_files(&ir.containers);

    let mut groups: BTreeMap<u32, CohesionGroup> = BTreeMap::new();
    let mut matched = 0_u64;
    let mut total = 0_u64;
    for container in tree.containers() {
        if container.level != ScopeLevel::File {
            continue;
        }
        let Some(parent) = container.parent else {
            continue;
        };
        let sloc = file_sloc.get(&container.id.0).copied().unwrap_or(0);
        let group = groups.entry(parent.0).or_insert_with(|| CohesionGroup {
            production_sloc: 0,
            members: Vec::new(),
        });
        group.production_sloc = group.production_sloc.saturating_add(sloc);
        group.members.push(tokenize(&container.name));

        let placed = name_of.get(&parent.0).map_or("", |name| name.as_str());
        let real = real_folder_of
            .get(container.name.as_str())
            .copied()
            .unwrap_or("");
        total = total.saturating_add(u64::from(sloc));
        if !real.is_empty() && placed == real {
            matched = matched.saturating_add(u64::from(sloc));
        }
    }

    let path_cohesion = if total == 0 {
        // No production SLOC exists under this placement (an all-test fixture,
        // say), so no file could demonstrably have left its folder: the
        // weighted fraction has an empty denominator, and the documented
        // invariant credits such a layout in full instead of collapsing the
        // empty ratio to a signed-zero term that reads as zero cohesion.
        1.0
    } else {
        // reason: sloc totals fit u32 sums; the f64 mantissa loses nothing material
        #[allow(clippy::cast_precision_loss)]
        let ratio = matched as f64 / total as f64;
        ratio
    };
    (groups.into_values().collect(), path_cohesion)
}

/// Computes the per-container child subtree sizes (in production SLOC) for the
/// imbalance term.
pub(super) fn container_sizes(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
) -> Vec<ContainerSizes> {
    let ir = snapshot.ir();
    // each file's production SLOC is the sum of effective_size over the production
    // nodes placed in it (test-zoned nodes already carry zero size).
    let mut file_sloc: BTreeMap<u32, u32> = BTreeMap::new();
    for node in &ir.nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(container) = placement(node.id.0) {
            let entry = file_sloc.entry(container.0).or_default();
            *entry = entry.saturating_add(node.effective_size);
        }
    }

    // bottom-up subtree size of each container.
    let mut subtree: BTreeMap<u32, u32> = file_sloc.clone();
    let mut children_by_parent: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for container in tree.containers() {
        if let Some(parent) = container.parent {
            children_by_parent
                .entry(parent.0)
                .or_default()
                .push(container.id.0);
        }
    }

    // accumulate sizes bottom-up by repeatedly summing known children; the
    // laminar tree is at most five levels deep, so five passes reach fixpoint.
    for _ in 0..5 {
        for container in tree.containers() {
            let total: u32 = children_by_parent
                .get(&container.id.0)
                .into_iter()
                .flatten()
                .filter_map(|child| subtree.get(child).copied())
                .sum();
            if total > 0 {
                subtree.insert(container.id.0, total);
            }
        }
    }

    children_by_parent
        .into_values()
        .map(|children| ContainerSizes {
            child_sizes: children
                .iter()
                .map(|child| subtree.get(child).copied().unwrap_or(0))
                .collect(),
        })
        .collect()
}
