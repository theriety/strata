//! The naming-cohesion facts of one candidate tree.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::score::group_cohesion;
use strata_ir::{ContainerTree, ScopeLevel};

use super::ScoringContext;
use super::snapshot::SnapshotScoring;
use crate::narrate::tokenize;

/// A file container of the tree and what its folder says about naming cohesion.
pub(super) struct CohesionFile {
    pub(super) container: usize,
    pub(super) group: usize,
    /// Whether the folder holding it already is the folder its own path names.
    pub(super) matches_real: bool,
}

impl ScoringContext<'_> {
    /// Resolves the naming groups of `tree`, each file's membership in one, and
    /// each group's cohesion.
    pub(super) fn cohesion(
        lookups: &SnapshotScoring,
        tree: &ContainerTree,
    ) -> (Vec<CohesionFile>, Vec<f64>) {
        let name_of: BTreeMap<u32, &SmolStr> = tree
            .containers()
            .iter()
            .map(|container| (container.id.0, &container.name))
            .collect();

        let mut members_by_parent: BTreeMap<u32, Vec<BTreeSet<String>>> = BTreeMap::new();
        let mut file_parents = Vec::new();
        for container in tree.containers() {
            if container.level != ScopeLevel::File {
                continue;
            }
            let Some(parent) = container.parent else {
                continue;
            };
            members_by_parent
                .entry(parent.0)
                .or_default()
                .push(tokenize(&container.name));

            let placed = name_of.get(&parent.0).map_or("", |name| name.as_str());
            let real = lookups
                .real_folder_of
                .get(container.name.as_str())
                .map_or("", SmolStr::as_str);
            file_parents.push((
                container.id.0 as usize,
                parent.0,
                !real.is_empty() && placed == real,
            ));
        }

        let group_index: BTreeMap<u32, usize> = members_by_parent
            .keys()
            .enumerate()
            .map(|(index, parent)| (*parent, index))
            .collect();
        let files = file_parents
            .into_iter()
            .filter_map(|(container, parent, matches_real)| {
                Some(CohesionFile {
                    container,
                    group: group_index.get(&parent).copied()?,
                    matches_real,
                })
            })
            .collect();
        let cohesions = members_by_parent
            .values()
            .map(|members| group_cohesion(members))
            .collect();
        (files, cohesions)
    }
}
