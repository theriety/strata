//! Placement-independent scoring data, resolved once and reused across evaluations.
//!
//! A search prices thousands of placements. What a placement changes is small —
//! which file each declaration occupies — while almost everything the scorer
//! reads around it stays put: the snapshot's edges and their same-file
//! affinity, who consumes whom, the current folder of every file, and, for one
//! candidate tree, its parent links, child lists, token sets and capacity
//! pressure. Two layers hold those facts so each is built as rarely as it can
//! be:
//!
//! - [`SnapshotScoring`] depends on the snapshot and the profile's same-file
//!   multipliers only, so one solver builds it once for every tree it scores.
//! - [`ScoringContext`] adds the facts of one candidate tree, so one symbol
//!   pass builds it once for every trial placement it prices.
//!
//! A [`Placed`] is one evaluation's dense view of the placement function, and
//! [`ScoringContext::score`] turns it into the objective. The arithmetic is the
//! free-standing per-term functions' arithmetic in the same order, so a score
//! is identical to the one they produce.

mod cohesion;
mod evaluate;
mod hierarchy;
mod movement;
mod placed;
mod snapshot;

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_ir::{ContainerId, ContainerTree, ScopeLevel};

use super::capacity::TreePressure;
use crate::config::CapacityConfig;

pub(in crate::analyze) use placed::Placed;
pub(in crate::analyze) use snapshot::SnapshotScoring;

use cohesion::CohesionFile;
use hierarchy::ChildGroup;
use movement::DistanceFacts;

/// Everything about one candidate tree that no placement changes, on top of the
/// snapshot's own placement-independent facts.
pub(in crate::analyze) struct ScoringContext<'a> {
    lookups: &'a SnapshotScoring,
    /// Number of container ids the tree spans.
    slots: usize,
    /// Parent of each container, by id.
    parent: Vec<Option<ContainerId>>,
    /// Level of each container, by id.
    level: Vec<Option<ScopeLevel>>,
    /// Number of parent hops from each container to its root, by id.
    depth: Vec<u32>,
    /// Pass-start file of each candidate file, by candidate id.
    pass_start: Vec<Option<ContainerId>>,
    /// Containers with children, ascending by parent id.
    groups: Vec<ChildGroup>,
    /// Index into `groups` of each parent in tree order; the subtree sizes
    /// accumulate in this order.
    group_order: Vec<usize>,
    cohesion_files: Vec<CohesionFile>,
    /// Naming cohesion of each parent's files, ascending by parent id.
    group_cohesions: Vec<f64>,
    distance: DistanceFacts,
    pressure: TreePressure,
}

impl<'a> ScoringContext<'a> {
    /// Resolves the facts of `tree` on top of `lookups`.
    ///
    /// `namespaces` and `pass_start_file_by_candidate` describe the candidate
    /// files of `tree`; `capacity` carries the caps the pressure term reads.
    pub(in crate::analyze) fn new(
        lookups: &'a SnapshotScoring,
        tree: &ContainerTree,
        namespaces: &BTreeMap<ContainerId, SmolStr>,
        pass_start_file_by_candidate: &BTreeMap<ContainerId, ContainerId>,
        capacity: &CapacityConfig,
    ) -> Self {
        let containers = tree.containers();
        let slots = containers
            .iter()
            .map(|container| container.id.0 as usize + 1)
            .max()
            .unwrap_or(0);
        let mut parent = vec![None; slots];
        let mut level = vec![None; slots];
        for container in containers {
            if let Some(cell) = parent.get_mut(container.id.0 as usize) {
                *cell = container.parent;
            }
            if let Some(cell) = level.get_mut(container.id.0 as usize) {
                *cell = Some(container.level);
            }
        }
        let depth = (0..slots)
            .map(|id| Self::depth_of(&parent, id))
            .collect::<Vec<_>>();

        let pass_start_len = pass_start_file_by_candidate
            .keys()
            .map(|id| id.0 as usize + 1)
            .max()
            .unwrap_or(0);
        let mut pass_start = vec![None; pass_start_len];
        for (candidate, start) in pass_start_file_by_candidate {
            if let Some(cell) = pass_start.get_mut(candidate.0 as usize) {
                *cell = Some(*start);
            }
        }

        let (groups, group_order) = Self::child_groups(tree);
        let (cohesion_files, group_cohesions) = Self::cohesion(lookups, tree);

        Self {
            lookups,
            slots,
            parent,
            level,
            depth,
            pass_start,
            groups,
            group_order,
            cohesion_files,
            group_cohesions,
            distance: Self::distance_facts(lookups, tree, slots),
            pressure: TreePressure::new(tree, namespaces, capacity),
        }
    }
}
