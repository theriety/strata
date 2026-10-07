//! The container hierarchy of one candidate tree: depths, child lists, common
//! ancestors and subtree sizes.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use strata_core::score::ContainerSizes;
use strata_ir::{ContainerId, ContainerTree, ScopeLevel};

use super::ScoringContext;

/// The children of one container, for the subtree-size term.
pub(super) struct ChildGroup {
    parent: usize,
    children: Vec<usize>,
}

impl ScoringContext<'_> {
    /// Counts the parent hops from container `id` to the top of its chain.
    pub(super) fn depth_of(parent: &[Option<ContainerId>], id: usize) -> u32 {
        let mut hops = 0_u32;
        let mut cursor = parent.get(id).copied().flatten();
        // a validated tree is a forest, so the chain ends; the bound keeps a
        // malformed one from looping.
        while let Some(next) = cursor {
            hops += 1;
            if hops as usize > parent.len() {
                break;
            }
            cursor = parent.get(next.0 as usize).copied().flatten();
        }
        hops
    }

    /// Groups every container under its parent, ascending by parent id.
    pub(super) fn child_groups(tree: &ContainerTree) -> (Vec<ChildGroup>, Vec<usize>) {
        let mut children_by_parent: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for container in tree.containers() {
            if let Some(parent) = container.parent {
                children_by_parent
                    .entry(parent.0 as usize)
                    .or_default()
                    .push(container.id.0 as usize);
            }
        }
        let groups: Vec<ChildGroup> = children_by_parent
            .into_iter()
            .map(|(parent, children)| ChildGroup { parent, children })
            .collect();
        let group_of_parent: BTreeMap<usize, usize> = groups
            .iter()
            .enumerate()
            .map(|(index, group)| (group.parent, index))
            .collect();
        let group_order = tree
            .containers()
            .iter()
            .filter_map(|container| group_of_parent.get(&(container.id.0 as usize)).copied())
            .collect();
        (groups, group_order)
    }

    /// Level of the lowest common ancestor of two containers, or the coarsest
    /// level when their chains never meet.
    pub(super) fn lca_level(&self, left: ContainerId, right: ContainerId) -> ScopeLevel {
        let step = |id: usize| self.parent.get(id).copied().flatten().map(|p| p.0 as usize);
        let depth = |id: usize| self.depth.get(id).copied().unwrap_or(0);
        let (mut up_left, mut up_right) = (left.0 as usize, right.0 as usize);
        let (mut depth_left, mut depth_right) = (depth(up_left), depth(up_right));
        let mut guard = self.slots.saturating_add(2);
        while up_left != up_right {
            guard -= 1;
            if guard == 0 {
                return ScopeLevel::PackageGroup;
            }
            match depth_left.cmp(&depth_right) {
                Ordering::Greater => {
                    let Some(next) = step(up_left) else {
                        return ScopeLevel::PackageGroup;
                    };
                    up_left = next;
                    depth_left -= 1;
                }
                Ordering::Less => {
                    let Some(next) = step(up_right) else {
                        return ScopeLevel::PackageGroup;
                    };
                    up_right = next;
                    depth_right -= 1;
                }
                Ordering::Equal => {
                    let (Some(next_left), Some(next_right)) = (step(up_left), step(up_right))
                    else {
                        return ScopeLevel::PackageGroup;
                    };
                    up_left = next_left;
                    up_right = next_right;
                    depth_left = depth_left.saturating_sub(1);
                    depth_right = depth_right.saturating_sub(1);
                }
            }
        }
        self.level
            .get(up_left)
            .copied()
            .flatten()
            .unwrap_or(ScopeLevel::PackageGroup)
    }

    /// Child subtree sizes of every container that has children.
    pub(super) fn container_sizes(&self, file_sloc: &[u32]) -> Vec<ContainerSizes> {
        let mut subtree: Vec<u32> = file_sloc.iter().copied().take(self.slots).collect();
        subtree.resize(self.slots, 0);
        // the laminar tree is at most five levels deep, so five passes reach
        // the fixpoint.
        for _ in 0..5 {
            for &index in &self.group_order {
                let Some(group) = self.groups.get(index) else {
                    continue;
                };
                let total: u32 = group
                    .children
                    .iter()
                    .map(|child| subtree.get(*child).copied().unwrap_or(0))
                    .sum();
                if total > 0
                    && let Some(cell) = subtree.get_mut(group.parent)
                {
                    *cell = total;
                }
            }
        }
        self.groups
            .iter()
            .map(|group| ContainerSizes {
                child_sizes: group
                    .children
                    .iter()
                    .map(|child| subtree.get(*child).copied().unwrap_or(0))
                    .collect(),
            })
            .collect()
    }
}
