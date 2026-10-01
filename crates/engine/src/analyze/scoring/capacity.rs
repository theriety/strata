//! Capacity pressure of a placement: how far files, folders and upper levels overrun their caps.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_ir::{Container, ContainerId, ContainerTree, Polarity, ScopeLevel, Snapshot};

use crate::analyze::findings::physical_folder_entries_repository_relative;
use crate::config::CapacityConfig;

/// Prices every configured capacity level with the same measures used by
/// capacity findings.
pub(super) fn capacity_pressure(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
    namespaces: &BTreeMap<ContainerId, SmolStr>,
    capacity: &CapacityConfig,
) -> f64 {
    let mut pressure = 0.0;
    let mut file_sloc: BTreeMap<ContainerId, u32> = BTreeMap::new();
    for node in &snapshot.ir().nodes {
        if node.polarity != Polarity::Production {
            continue;
        }
        if let Some(file) = placement(node.id.0) {
            let measure = file_sloc.entry(file).or_default();
            *measure = measure.saturating_add(node.effective_size);
        }
    }
    pressure += file_sloc
        .values()
        .map(|&measure| normalized_overage(measure, capacity.file))
        .sum::<f64>();
    pressure += physical_folder_entries_repository_relative(tree, Some(namespaces))
        .values()
        .map(|&measure| normalized_overage(measure, capacity.folder))
        .sum::<f64>();

    let children = children_by_container(tree);
    for container in tree.containers() {
        let (member_level, cap) = match container.level {
            ScopeLevel::Domain => (ScopeLevel::Folder, capacity.domain),
            ScopeLevel::Package => (ScopeLevel::Domain, capacity.package),
            ScopeLevel::PackageGroup => (ScopeLevel::Package, capacity.package_group),
            ScopeLevel::Folder | ScopeLevel::File => continue,
        };
        let measure = count_bound_structural_members(tree, &children, container.id, member_level);
        pressure += normalized_overage(measure, cap);
    }
    pressure
}

pub(super) fn normalized_overage(measure: u32, cap: u32) -> f64 {
    if measure <= cap {
        0.0
    } else if cap == 0 {
        f64::from(measure)
    } else {
        f64::from(measure - cap) / f64::from(cap)
    }
}

pub(super) fn children_by_container(
    tree: &ContainerTree,
) -> BTreeMap<ContainerId, Vec<ContainerId>> {
    let mut children: BTreeMap<ContainerId, Vec<ContainerId>> = BTreeMap::new();
    for container in tree.containers() {
        if let Some(parent) = container.parent {
            children.entry(parent).or_default().push(container.id);
        }
    }
    children
}

pub(super) fn count_bound_structural_members(
    tree: &ContainerTree,
    children: &BTreeMap<ContainerId, Vec<ContainerId>>,
    root: ContainerId,
    member_level: ScopeLevel,
) -> u32 {
    let by_id: BTreeMap<ContainerId, &Container> = tree
        .containers()
        .iter()
        .map(|container| (container.id, container))
        .collect();
    let mut pending = children.get(&root).cloned().unwrap_or_default();
    let mut count = 0_u32;
    while let Some(id) = pending.pop() {
        let Some(container) = by_id.get(&id).copied() else {
            continue;
        };
        let binds_file = subtree_binds_file(children, &by_id, id);
        let binds_member = if member_level == ScopeLevel::Folder {
            children.get(&id).into_iter().flatten().any(|child| {
                by_id
                    .get(child)
                    .is_some_and(|entry| entry.level == ScopeLevel::File)
            })
        } else {
            binds_file
        };
        if container.level == member_level && binds_member {
            count = count.saturating_add(1);
        }
        pending.extend(children.get(&id).into_iter().flatten().copied());
    }
    count
}

pub(super) fn subtree_binds_file(
    children: &BTreeMap<ContainerId, Vec<ContainerId>>,
    by_id: &BTreeMap<ContainerId, &Container>,
    root: ContainerId,
) -> bool {
    let mut pending = children.get(&root).cloned().unwrap_or_default();
    while let Some(id) = pending.pop() {
        let Some(container) = by_id.get(&id).copied() else {
            continue;
        };
        if container.level == ScopeLevel::File {
            return true;
        }
        pending.extend(children.get(&id).into_iter().flatten().copied());
    }
    false
}

#[cfg(test)]
pub(in crate::analyze) fn physical_binding_pressure(
    tree: &ContainerTree,
    namespaces: &BTreeMap<ContainerId, SmolStr>,
    folder_budget: u32,
) -> f64 {
    physical_folder_entries_repository_relative(tree, Some(namespaces))
        .values()
        .map(|&measure| normalized_overage(measure, folder_budget))
        .sum()
}

/// Sums the scoped over-capacity binding pressure of a rendered tree: over
/// every folder and domain container, the share its transitively-bound file
/// count exceeds `folder_budget` — `Σ max(0, bound − budget) / budget` (FIX03).
///
/// Ancestors bind, so nesting cannot dodge the budget: a domain whose folders
/// together hold more than the budget pays for the whole binding even when
/// each folder sits within it. The rendered tree is the flat IR form, so the
/// count accumulates upward: each container folds its subtree total into its
/// parent, charging folders and domains on the way. This is the priced
/// counterpart of the eval's ancestor-binding rule; the configured per-level
/// caps stay findings-only semantics (`walk_capacity`). A zero budget disables
/// the term.
#[cfg(test)]
pub(in crate::analyze) fn binding_pressure(tree: &ContainerTree, folder_budget: u32) -> f64 {
    if folder_budget == 0 {
        return 0.0;
    }
    // Subtree file totals remain the domain-grain measure. Folders instead
    // bind only their immediate entries: direct files and direct child
    // folders.
    let mut totals: BTreeMap<u32, u32> = BTreeMap::new();
    let mut entries: BTreeMap<u32, u32> = BTreeMap::new();
    let mut pressure = 0.0_f64;
    for container in tree.containers().iter().rev() {
        if let Some(parent) = container.parent
            && matches!(container.level, ScopeLevel::File | ScopeLevel::Folder)
        {
            *entries.entry(parent.0).or_insert(0) += 1;
        }
        match container.level {
            ScopeLevel::File => {
                if let Some(parent) = container.parent {
                    *totals.entry(parent.0).or_insert(0) += 1;
                }
            }
            level => {
                let bound = totals.remove(&container.id.0).unwrap_or(0);
                if matches!(level, ScopeLevel::Folder | ScopeLevel::Domain) {
                    let measured = if level == ScopeLevel::Folder {
                        entries.remove(&container.id.0).unwrap_or(0)
                    } else {
                        bound
                    };
                    let over = measured.saturating_sub(folder_budget);
                    pressure += f64::from(over) / f64::from(folder_budget);
                }
                if let Some(parent) = container.parent {
                    *totals.entry(parent.0).or_insert(0) += bound;
                }
            }
        }
    }
    pressure
}
