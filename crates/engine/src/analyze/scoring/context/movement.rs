//! How far one candidate tree and placement sit from the current layout.

use std::collections::BTreeMap;

use strata_ir::{ContainerTree, ScopeLevel};

use super::ScoringContext;
use super::placed::Placed;
use super::snapshot::SnapshotScoring;
use crate::analyze::scoring::distance::folder_key_of_files;

/// The tree-dependent half of move distance.
pub(super) struct DistanceFacts {
    total: usize,
    /// Declarations whose folder key differs from the current layout in the
    /// candidate tree, whatever file the placement picks.
    always_moved: usize,
    /// The rest: `(node id, current file path key)`, moved only when the
    /// placement puts them in a file with another path.
    probes: Vec<(u32, u32)>,
    /// Path key of each file container, by container id.
    file_key: Vec<Option<u32>>,
}

impl ScoringContext<'_> {
    /// Resolves how far `tree` sits from the snapshot's current layout, apart
    /// from where the placement puts each declaration.
    pub(super) fn distance_facts(
        lookups: &SnapshotScoring,
        tree: &ContainerTree,
        slots: usize,
    ) -> DistanceFacts {
        fn intern<'s>(keys: &mut BTreeMap<&'s str, u32>, name: &'s str) -> u32 {
            let next = u32::try_from(keys.len()).unwrap_or(u32::MAX);
            *keys.entry(name).or_insert(next)
        }

        let candidate_folder = folder_key_of_files(tree);
        let mut keys: BTreeMap<&str, u32> = BTreeMap::new();

        let mut file_key: Vec<Option<u32>> = vec![None; slots];
        for container in tree.containers() {
            if container.level != ScopeLevel::File {
                continue;
            }
            let key = intern(&mut keys, container.name.as_str());
            if let Some(cell) = file_key.get_mut(container.id.0 as usize) {
                *cell = Some(key);
            }
        }

        let home_key: Vec<u32> = lookups
            .homes
            .iter()
            .map(|home| intern(&mut keys, home.name.as_str()))
            .collect();
        let home_always_moved: Vec<bool> = lookups
            .homes
            .iter()
            .map(|home| {
                candidate_folder
                    .get(home.name.as_str())
                    .is_none_or(|placed| *placed != home.folder.as_str())
            })
            .collect();

        let mut always_moved = 0;
        let mut probes = Vec::new();
        for &(id, home) in &lookups.node_home {
            let Some(home) = home else {
                continue;
            };
            if home_always_moved.get(home).copied().unwrap_or(false) {
                always_moved += 1;
            } else if let Some(key) = home_key.get(home).copied() {
                probes.push((id, key));
            }
        }
        DistanceFacts {
            total: lookups.node_home.len(),
            always_moved,
            probes,
            file_key,
        }
    }

    /// Fraction of declarations whose file or folder differs from the snapshot's
    /// current layout under this placement.
    pub(in crate::analyze) fn move_distance(&self, placed: &Placed) -> f64 {
        let facts = &self.distance;
        if facts.total == 0 {
            return 0.0;
        }
        let left_file = facts
            .probes
            .iter()
            .filter(|&&(node, home_key)| {
                placed
                    .file(node)
                    .and_then(|file| facts.file_key.get(file.0 as usize).copied().flatten())
                    .is_some_and(|key| key != home_key)
            })
            .count();
        let moved = facts.always_moved + left_file;
        f64::from(u32::try_from(moved).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(facts.total).unwrap_or(u32::MAX))
    }
}
