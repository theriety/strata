//! Capacity pressure of a placement: how far files, folders and upper levels overrun their caps.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use unlace_ir::{Container, ContainerId, ContainerTree, ScopeLevel};
#[cfg(test)]
use unlace_ir::{Polarity, Snapshot};

use crate::analyze::findings::{RealRoots, physical_folder_entries_repository_relative};
use crate::config::CapacityConfig;

/// The capacity pressure of one tree, apart from file sizes: everything the
/// physical folder entries and the structural levels contribute is fixed by the
/// tree, so only the per-file term varies with the placement.
pub(super) struct TreePressure {
    file_cap: u32,
    /// Overage of the physical folder entries.
    folder: f64,
    /// Non-zero overage of each structural container, in tree order.
    structural: Vec<f64>,
}

impl TreePressure {
    /// Prices the tree-dependent terms of `tree` once.
    pub(super) fn new(
        tree: &ContainerTree,
        namespaces: &BTreeMap<ContainerId, SmolStr>,
        key_by_id: &BTreeMap<u32, SmolStr>,
        capacity: &CapacityConfig,
    ) -> Self {
        let folder = physical_folder_entries_repository_relative(tree, Some(namespaces))
            .values()
            .map(|&measure| normalized_overage(measure, capacity.folder))
            .sum::<f64>();
        let children = children_by_container(tree);
        let domain_counts = RealRoots::from_tree_keyed(tree, key_by_id).domain_subfolder_counts();
        let mut structural = Vec::new();
        for container in tree.containers() {
            let Some((measure, cap)) =
                structural_measure(tree, &children, &domain_counts, container, capacity)
            else {
                continue;
            };
            let overage = normalized_overage(measure, cap);
            // a zero overage leaves the running sum untouched.
            if overage != 0.0 {
                structural.push(overage);
            }
        }
        Self {
            file_cap: capacity.file,
            folder,
            structural,
        }
    }

    /// Prices the tree under a placement whose files hold `file_sloc`
    /// production SLOC, indexed by container id.
    pub(super) fn total(&self, file_sloc: &[u32]) -> f64 {
        let mut pressure = 0.0;
        pressure += file_sloc
            .iter()
            .filter(|&&measure| measure > self.file_cap)
            .map(|&measure| normalized_overage(measure, self.file_cap))
            .sum::<f64>();
        pressure += self.folder;
        for overage in &self.structural {
            pressure += overage;
        }
        pressure
    }
}

/// Prices every configured capacity level with the same measures used by
/// capacity findings.
///
/// Test oracle for `TreePressure`, which production scores through.
#[cfg(test)]
pub(super) fn capacity_pressure(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
    namespaces: &BTreeMap<ContainerId, SmolStr>,
    key_by_id: &BTreeMap<u32, SmolStr>,
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
    let domain_counts = RealRoots::from_tree_keyed(tree, key_by_id).domain_subfolder_counts();
    for container in tree.containers() {
        let Some((measure, cap)) =
            structural_measure(tree, &children, &domain_counts, container, capacity)
        else {
            continue;
        };
        pressure += normalized_overage(measure, cap);
    }
    pressure
}

fn normalized_overage(measure: u32, cap: u32) -> f64 {
    if measure <= cap {
        0.0
    } else if cap == 0 {
        f64::from(measure)
    } else {
        f64::from(measure - cap) / f64::from(cap)
    }
}

fn children_by_container(tree: &ContainerTree) -> BTreeMap<ContainerId, Vec<ContainerId>> {
    let mut children: BTreeMap<ContainerId, Vec<ContainerId>> = BTreeMap::new();
    for container in tree.containers() {
        if let Some(parent) = container.parent {
            children.entry(parent).or_default().push(container.id);
        }
    }
    children
}

/// Measures one structural container, returning its measure and its cap, or
/// `None` for a folder or file.
///
/// A domain counts only its direct sub-folders, read from the real roots of the
/// tree as the findings do (`RealRoots::domain_subfolder_counts` here,
/// `RealRoots::domain_measures` there), so
/// a finding and its price cannot disagree. A package and a package group count
/// their binding members at any depth.
fn structural_measure(
    tree: &ContainerTree,
    children: &BTreeMap<ContainerId, Vec<ContainerId>>,
    domain_counts: &BTreeMap<u32, u32>,
    container: &Container,
    capacity: &CapacityConfig,
) -> Option<(u32, u32)> {
    match container.level {
        ScopeLevel::Domain => Some((
            domain_counts.get(&container.id.0).copied().unwrap_or(0),
            capacity.domain,
        )),
        ScopeLevel::Package => Some((
            count_bound_structural_members(tree, children, container.id, ScopeLevel::Domain),
            capacity.package,
        )),
        ScopeLevel::PackageGroup => Some((
            count_bound_structural_members(tree, children, container.id, ScopeLevel::Package),
            capacity.package_group,
        )),
        ScopeLevel::Folder | ScopeLevel::File => None,
    }
}

fn count_bound_structural_members(
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
        if container.level == member_level && binds_file {
            count = count.saturating_add(1);
        }
        pending.extend(children.get(&id).into_iter().flatten().copied());
    }
    count
}

fn subtree_binds_file(
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

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use unlace_ir::{Layout, build_laminar_tree};

    use super::*;
    use crate::analyze::findings::walk_all_capacity;
    use crate::analyze::rendering::render_tree;
    use crate::analyze::test_support::container;
    use crate::result::Level;

    fn domain_cap(domain: u32) -> CapacityConfig {
        CapacityConfig {
            file: 1000,
            folder: 1000,
            domain,
            package: 1000,
            package_group: 1000,
        }
    }

    /// Each domain finding's first real path and measure, sorted; a path
    /// reported twice shows up twice.
    fn reported_by_path(
        tree: &ContainerTree,
        keys: &BTreeMap<u32, SmolStr>,
        capacity: &CapacityConfig,
    ) -> Vec<(String, u32)> {
        let roots = RealRoots::from_tree_keyed(tree, keys);
        let Ok(dto) = render_tree(tree, &[], &|_| None, keys) else {
            return Vec::new();
        };
        let mut reported: Vec<(String, u32)> = walk_all_capacity(&dto, capacity, &roots)
            .into_iter()
            .filter(|(level, _)| *level == Level::Domain)
            .filter_map(|(_, finding)| finding.capacity)
            .filter_map(|breach| {
                breach
                    .real_paths
                    .first()
                    .map(|path| (path.clone(), breach.measured))
            })
            .collect();
        reported.sort();
        reported
    }

    /// Each domain's priced measure keyed by its container name.
    fn priced_by_name(
        tree: &ContainerTree,
        keys: &BTreeMap<u32, SmolStr>,
    ) -> BTreeMap<String, u32> {
        let counts = RealRoots::from_tree_keyed(tree, keys).domain_subfolder_counts();
        let children = children_by_container(tree);
        tree.containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::Domain)
            .filter_map(|container| {
                structural_measure(tree, &children, &counts, container, &domain_cap(0))
                    .filter(|&(measure, _)| measure > 0)
                    .map(|(measure, _)| (container.name.to_string(), measure))
            })
            .collect()
    }

    fn pressure_of(
        tree: &ContainerTree,
        keys: &BTreeMap<u32, SmolStr>,
        capacity: &CapacityConfig,
    ) -> f64 {
        TreePressure::new(tree, &BTreeMap::new(), keys, capacity).total(&[])
    }

    /// Findings and pricing measure each domain identically: the breach a
    /// finding reports is the very count the objective prices.
    #[test]
    fn should_measure_each_domain_by_direct_subfolders_in_findings_and_pricing() {
        let paths: Vec<SmolStr> = [
            "src/gen/analysts/a.ts",
            "spec/gen/analysts/b.ts",
            "src/gen/embedders/deep/er/c.ts",
            "src/gen/embedders/d.ts",
            "src/gen/x/y/z/e.ts",
            "src/gen/top.ts",
            "src/gen/gen/g.ts",
            "spec/gen/gen/h.ts",
            "src/other/one/f.ts",
        ]
        .iter()
        .map(|path| SmolStr::new(*path))
        .collect();
        let layout = Layout {
            package_roots: Vec::new(),
            source_roots: vec![SmolStr::new("src"), SmolStr::new("spec")],
        };
        let built = build_laminar_tree(&paths, "workspace", &layout);
        let keys = BTreeMap::new();

        let reported = reported_by_path(&built.tree, &keys, &domain_cap(0));
        let priced = priced_by_name(&built.tree, &keys);

        // `gen` (src and spec merged): analysts, embedders, x and the real
        // `gen/gen`; its own files in `gen/` are no sub-folder.
        assert_eq!(
            reported,
            [("spec/gen".to_owned(), 4), ("src/other".to_owned(), 1)]
        );
        assert_eq!(
            priced,
            BTreeMap::from([
                ("workspace/gen".to_owned(), 4),
                ("workspace/other".to_owned(), 1),
            ])
        );
    }

    /// Two sibling domains of one raw name carry decorated display names; the
    /// measure reads their undecorated keys, so a 4/1 split measures 4 and 1.
    #[test]
    fn should_measure_decorated_sibling_domains_from_their_undecorated_keys() {
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "pa", ScopeLevel::Package, Some(0)),
            container(2, "pa/app (pa.app.x1)", ScopeLevel::Domain, Some(1)),
            container(3, "pa/app (pa.app.x5)", ScopeLevel::Domain, Some(1)),
            container(4, "pa/app/x1", ScopeLevel::Folder, Some(2)),
            container(5, "pa/app/x1/f.ts", ScopeLevel::File, Some(4)),
            container(6, "pa/app/x2", ScopeLevel::Folder, Some(2)),
            container(7, "pa/app/x2/f.ts", ScopeLevel::File, Some(6)),
            container(8, "pa/app/x3", ScopeLevel::Folder, Some(2)),
            container(9, "pa/app/x3/f.ts", ScopeLevel::File, Some(8)),
            container(10, "pa/app/x4", ScopeLevel::Folder, Some(2)),
            container(11, "pa/app/x4/f.ts", ScopeLevel::File, Some(10)),
            container(12, "pa/app/x5", ScopeLevel::Folder, Some(3)),
            container(13, "pa/app/x5/f.ts", ScopeLevel::File, Some(12)),
            // a nested folder beneath `x1` is still the sub-folder `x1`; read
            // against the decorated display name it would be a foreign folder.
            container(14, "pa/app/x1/deep", ScopeLevel::Folder, Some(2)),
            container(15, "pa/app/x1/deep/f.ts", ScopeLevel::File, Some(14)),
        ]);
        let keys: BTreeMap<u32, SmolStr> =
            [(2, SmolStr::new("pa/app")), (3, SmolStr::new("pa/app"))]
                .into_iter()
                .collect();

        let counts = RealRoots::from_tree_keyed(&tree, &keys).domain_subfolder_counts();
        assert_eq!(counts, BTreeMap::from([(2, 4), (3, 1)]));

        // cap 3: only the four-folder half overruns, by one third.
        assert_eq!(
            reported_by_path(&tree, &keys, &domain_cap(3)),
            [("pa/app".to_owned(), 4)]
        );
        assert!((pressure_of(&tree, &keys, &domain_cap(3)) - 1.0 / 3.0).abs() < 1e-12);
        assert!(
            (capacity_pressure_of_tree(&tree, &keys, &domain_cap(3)) - 1.0 / 3.0).abs() < 1e-12
        );
    }

    /// A domain whose undecorated key repeats its package's key is dropped from
    /// the rendered tree, yet candidate pricing charges it. Findings measure the
    /// container tree, so they report it too, at the package's real directory.
    #[test]
    fn should_report_and_price_a_domain_that_echoes_its_package() {
        let tree = echoing_package_tree();
        let keys: BTreeMap<u32, SmolStr> = [(2, SmolStr::new("cts")), (3, SmolStr::new("cts"))]
            .into_iter()
            .collect();

        // the renderer drops both echoing domains.
        let rendered = render_tree(&tree, &[], &|_| None, &keys).ok();
        assert!(
            rendered
                .as_ref()
                .is_some_and(|dto| !contains_domain_node(dto)),
            "{rendered:?}"
        );
        // findings see the one domain over cap 1, reported at the package dir.
        assert_eq!(
            reported_by_path(&tree, &keys, &domain_cap(1)),
            [("cts".to_owned(), 2)]
        );
        // pricing charges the very same overage.
        assert!((pressure_of(&tree, &keys, &domain_cap(1)) - 1.0).abs() < 1e-12);
        assert_eq!(
            priced_by_name(&tree, &keys),
            BTreeMap::from([
                ("cts (cts.core)".to_owned(), 2),
                ("cts (cts.zz)".to_owned(), 1)
            ])
        );
    }

    fn contains_domain_node(node: &crate::result::ContainerNode) -> bool {
        node.level == Level::Domain || node.children.iter().flatten().any(contains_domain_node)
    }

    /// Package `cts` with two sibling domains of the key `cts`, as a candidate
    /// tree carries them: decorated display names, folders keyed by real path.
    fn echoing_package_tree() -> ContainerTree {
        ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "cts", ScopeLevel::Package, Some(0)),
            container(2, "cts (cts.core)", ScopeLevel::Domain, Some(1)),
            container(3, "cts (cts.zz)", ScopeLevel::Domain, Some(1)),
            container(4, "cts/core/a", ScopeLevel::Folder, Some(2)),
            container(5, "cts/core/a/f.ts", ScopeLevel::File, Some(4)),
            container(6, "cts/io/b", ScopeLevel::Folder, Some(2)),
            container(7, "cts/io/b/f.ts", ScopeLevel::File, Some(6)),
            container(8, "cts/zz", ScopeLevel::Folder, Some(3)),
            container(9, "cts/zz/f.ts", ScopeLevel::File, Some(8)),
        ])
    }

    /// A folder whose key does not extend its domain's key is its own entry,
    /// one per distinct real folder.
    #[test]
    fn should_count_each_foreign_folder_of_a_candidate_domain_as_its_own_entry() {
        let tree = ContainerTree::new(vec![
            container(0, "ws", ScopeLevel::PackageGroup, None),
            container(1, "pa", ScopeLevel::Package, Some(0)),
            container(2, "pa/app", ScopeLevel::Domain, Some(1)),
            container(3, "pa/app/x", ScopeLevel::Folder, Some(2)),
            container(4, "pa/app/x/f.ts", ScopeLevel::File, Some(3)),
            container(5, "pa/other/a", ScopeLevel::Folder, Some(2)),
            container(6, "pa/other/a/f.ts", ScopeLevel::File, Some(5)),
            container(7, "pa/other/a/g.ts", ScopeLevel::File, Some(5)),
            container(8, "pa/other/b", ScopeLevel::Folder, Some(2)),
            container(9, "pa/other/b/f.ts", ScopeLevel::File, Some(8)),
        ]);
        let keys = BTreeMap::new();

        // `x`, plus the two foreign folders `pa/other/a` and `pa/other/b`.
        assert_eq!(
            RealRoots::from_tree_keyed(&tree, &keys).domain_subfolder_counts(),
            BTreeMap::from([(2, 3)])
        );
        assert_eq!(
            reported_by_path(&tree, &keys, &domain_cap(0))
                .iter()
                .map(|(_, measured)| *measured)
                .collect::<Vec<_>>(),
            [3]
        );
    }

    fn capacity_pressure_of_tree(
        tree: &ContainerTree,
        keys: &BTreeMap<u32, SmolStr>,
        capacity: &CapacityConfig,
    ) -> f64 {
        let snapshot = crate::analyze::test_support::snapshot(
            Vec::new(),
            Vec::new(),
            tree.containers().to_vec(),
        );
        capacity_pressure(&snapshot, &|_| None, tree, &BTreeMap::new(), keys, capacity)
    }
}
