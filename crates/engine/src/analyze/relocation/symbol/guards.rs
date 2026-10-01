use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_ir::{ContainerId, Edge, Node, ScopeLevel, Snapshot};

use crate::analyze::relocation::CandidateTree;

/// preserves dependency evidence from the layout a symbol pass started with
pub(in crate::analyze) struct PassStartGuard {
    claimant_files: BTreeMap<u32, BTreeSet<ContainerId>>,
    reachable: BTreeSet<(ContainerId, ContainerId)>,
}

impl PassStartGuard {
    pub(super) fn new(base: &BTreeMap<u32, ContainerId>, edges: &[Edge]) -> Self {
        let mut claimant_files: BTreeMap<u32, BTreeSet<ContainerId>> = BTreeMap::new();
        let mut successors: BTreeMap<ContainerId, BTreeSet<ContainerId>> = BTreeMap::new();
        let files: BTreeSet<ContainerId> = base.values().copied().collect();

        for edge in edges {
            let (Some(source), Some(target)) = (base.get(&edge.source.0), base.get(&edge.target.0))
            else {
                continue;
            };
            if source == target {
                if edge.source != edge.target {
                    claimant_files
                        .entry(edge.target.0)
                        .or_default()
                        .insert(*source);
                }
                continue;
            }
            successors.entry(*source).or_default().insert(*target);
        }

        let mut reachable = BTreeSet::new();
        for start in files {
            let mut pending: Vec<ContainerId> = successors
                .get(&start)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            while let Some(file) = pending.pop() {
                if !reachable.insert((start, file)) {
                    continue;
                }
                pending.extend(successors.get(&file).into_iter().flatten().copied());
            }
        }

        Self {
            claimant_files,
            reachable,
        }
    }

    pub(super) fn blocks(&self, node: u32, destination: ContainerId) -> bool {
        self.claimant_files
            .get(&node)
            .into_iter()
            .flatten()
            .any(|claimant| self.reachable.contains(&(destination, *claimant)))
    }
}

/// Protects a declaration jointly consumed by sibling physical-folder
/// branches from being buried inside just one of those branches. Ownership is
/// derived once from pass-start paths and raw structural edges; unrelated
/// folder reach therefore cannot manufacture permission later in the pass.
pub(in crate::analyze) struct ConsumerBranchGuard {
    ownership_by_node: BTreeMap<u32, ConsumerBranchOwnership>,
    folder_by_candidate_file: BTreeMap<ContainerId, Vec<SmolStr>>,
}

pub(in crate::analyze) struct ConsumerBranchOwnership {
    lca: Vec<SmolStr>,
    occupied_branches: BTreeSet<SmolStr>,
}

impl ConsumerBranchGuard {
    pub(super) fn new(assembled: &CandidateTree, snapshot: &Snapshot, edges: &[Edge]) -> Self {
        let pass_start_folder_by_file: BTreeMap<ContainerId, Vec<SmolStr>> = snapshot
            .ir()
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id, physical_folder_segments(&container.name)))
            .collect();
        let folder_by_candidate_file = assembled
            .pass_start_file_by_candidate
            .iter()
            .filter_map(|(&candidate, original)| {
                Some((candidate, pass_start_folder_by_file.get(original)?.clone()))
            })
            .collect();
        let nodes: BTreeMap<u32, &Node> = snapshot
            .ir()
            .nodes
            .iter()
            .map(|node| (node.id.0, node))
            .collect();
        let mut consumer_folders: BTreeMap<u32, BTreeSet<Vec<SmolStr>>> = BTreeMap::new();
        for edge in edges {
            if edge.source == edge.target {
                continue;
            }
            let Some(source) = nodes.get(&edge.source.0) else {
                continue;
            };
            let Some(folder) = pass_start_folder_by_file.get(&source.container) else {
                continue;
            };
            consumer_folders
                .entry(edge.target.0)
                .or_default()
                .insert(folder.clone());
        }

        let ownership_by_node = consumer_folders
            .into_iter()
            .filter_map(|(node, folders)| {
                let folders: Vec<Vec<SmolStr>> = folders.into_iter().collect();
                let first = folders.first()?;
                let lca_len = first
                    .iter()
                    .enumerate()
                    .take_while(|(index, segment)| {
                        folders
                            .iter()
                            .all(|folder| folder.get(*index) == Some(*segment))
                    })
                    .count();
                let occupied_branches: BTreeSet<SmolStr> = folders
                    .iter()
                    .filter_map(|folder| folder.get(lca_len).cloned())
                    .collect();
                (occupied_branches.len() >= 2).then_some((
                    node,
                    ConsumerBranchOwnership {
                        lca: first.get(..lca_len)?.to_vec(),
                        occupied_branches,
                    },
                ))
            })
            .collect();

        Self {
            ownership_by_node,
            folder_by_candidate_file,
        }
    }

    pub(super) fn blocks(&self, node: u32, destination: ContainerId) -> bool {
        let Some(ownership) = self.ownership_by_node.get(&node) else {
            return false;
        };
        let Some(folder) = self.folder_by_candidate_file.get(&destination) else {
            return false;
        };
        folder.starts_with(&ownership.lca)
            && folder
                .get(ownership.lca.len())
                .is_some_and(|branch| ownership.occupied_branches.contains(branch))
    }

    /// Reports a destination inside the consumers' shared LCA but outside
    /// every occupied branch. Such a sibling is neutral shared territory, so
    /// the older pairwise reach guard must not mistake its absent branch edges
    /// for one consumer taking ownership from another.
    pub(super) fn is_neutral_shared_destination(
        &self,
        node: u32,
        destination: ContainerId,
    ) -> bool {
        let Some(ownership) = self.ownership_by_node.get(&node) else {
            return false;
        };
        let Some(folder) = self.folder_by_candidate_file.get(&destination) else {
            return false;
        };
        folder.starts_with(&ownership.lca)
            && folder
                .get(ownership.lca.len())
                .is_some_and(|branch| !ownership.occupied_branches.contains(branch))
    }
}

pub(in crate::analyze) fn physical_folder_segments(file: &str) -> Vec<SmolStr> {
    file.rsplit_once('/').map_or_else(Vec::new, |(folder, _)| {
        folder
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(SmolStr::new)
            .collect()
    })
}
