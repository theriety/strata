//! Interns the candidate containers and records every symbol placement.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::cluster::Partition;
use strata_ir::{ContainerId, ContainerTree, ScopeLevel};

use crate::analyze::layout::{ContainerArena, NameTally};
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::{CandidateTree, PipelineSolver};
use crate::analyze::scoring::ContainerSpec;

impl PipelineSolver<'_> {
    /// Interns the candidate containers parent-before-child — package groups,
    /// packages, domains, then each folder with its files — and records every
    /// symbol's file placement.
    pub(super) fn emit(
        &self,
        parts: &Partition,
        members_of: &BTreeMap<u32, Vec<u32>>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        package_tally: &NameTally,
        polish_evidence: &PolishEvidence,
    ) -> CandidateTree {
        let mut arena = ContainerArena::default();
        let mut key_by_id: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let domain_ids = self.intern_upper_levels(
            &mut arena,
            &mut key_by_id,
            chain_of,
            domain_tally,
            package_tally,
        );

        let mut file_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        let mut folder_ids = BTreeMap::new();
        let mut restored_ids = BTreeMap::new();
        let mut zone_by_file: BTreeMap<ContainerId, bool> = BTreeMap::new();
        let mut namespace_by_file: BTreeMap<ContainerId, SmolStr> = BTreeMap::new();
        let mut package_by_file: BTreeMap<ContainerId, SmolStr> = BTreeMap::new();
        let mut pass_start_file_by_candidate: BTreeMap<ContainerId, ContainerId> = BTreeMap::new();
        let cluster_by_folder: BTreeMap<&SmolStr, u32> = self
            .real_folder_names
            .iter()
            .enumerate()
            .map(|(cluster, name)| (name, u32::try_from(cluster).unwrap_or(u32::MAX)))
            .collect();
        for (&folder, members) in members_of {
            let Some(&(domain, _, _)) = chain_of.get(&folder) else {
                continue;
            };
            // folders are reality: the cluster keeps its full real key — one
            // container per distinct real location with an injective name
            // (`qualify_folder_names`), so sibling folders never collide and no
            // synthetic `-N` twin can arise. A key that doesn't path-extend its
            // elected domain displays relative to its enclosing package at the
            // render boundary (`folder_increment`), the honest display of a
            // foreign real directory folded into the suggested domain.
            let key = self
                .real_folder_names
                .get(folder as usize)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let members_by_folder =
                self.projected_folder_members(parts, members, polish_evidence, &key);
            for (projected_key, namespace_members) in members_by_folder {
                let projected_cluster = cluster_by_folder.get(&projected_key).copied();
                // A cycle can be the only resident of its nondominant home.
                // Such a home has no solver cluster: restore its original
                // ancestor chain, including transparent/synthetic containers.
                let restored_folder = namespace_members.first().and_then(|&vertex| {
                    (projected_cluster.is_none()
                        && self.retained_cycle_home(parts, vertex, polish_evidence)
                            == Some(&projected_key))
                    .then(|| self.restore_cycle_folder(vertex, &mut arena, &mut restored_ids))
                    .flatten()
                });
                let projected_domain = projected_cluster
                    .and_then(|cluster| chain_of.get(&cluster))
                    .map_or(domain, |&(projected_domain, _, _)| projected_domain);
                let parent = domain_ids.get(&projected_domain).copied();
                let folder_id = restored_folder.unwrap_or_else(|| {
                    *folder_ids
                        .entry((parent, projected_key.clone()))
                        .or_insert_with(|| {
                            arena.push(ContainerSpec {
                                name: &projected_key,
                                level: ScopeLevel::Folder,
                                parent,
                                synthetic: self
                                    .real_folder_synthetic
                                    .get(projected_cluster.unwrap_or(folder) as usize)
                                    .copied()
                                    .unwrap_or(false),
                            })
                        })
                });
                for vertex in namespace_members {
                    let Some(file) = self.files.get(vertex as usize) else {
                        continue;
                    };
                    let id = arena.push(ContainerSpec {
                        name: &file.name,
                        level: ScopeLevel::File,
                        parent: Some(folder_id),
                        synthetic: false,
                    });
                    file_ids.insert(vertex, id);
                    pass_start_file_by_candidate.insert(id, ContainerId(file.container));
                    namespace_by_file.insert(id, file.namespace.clone());
                    package_by_file.insert(id, file.home.package.clone());
                    zone_by_file.insert(
                        id,
                        self.test_zone
                            .get(vertex as usize)
                            .copied()
                            .unwrap_or(false),
                    );
                }
            }
        }

        CandidateTree {
            tree: ContainerTree::new(arena.containers),
            placement: self.placements(&file_ids),
            pass_start_file_by_candidate,
            zone_by_file,
            namespace_by_file,
            package_by_file,
            key_by_id,
        }
    }

    /// Maps every symbol node to the candidate file container holding it, by the
    /// file vertex the node's current container indexes to.
    fn placements(&self, file_ids: &BTreeMap<u32, ContainerId>) -> BTreeMap<u32, ContainerId> {
        let mut placement = BTreeMap::new();
        for node in &self.snapshot.ir().nodes {
            let Some(&vertex) = self.index_of.get(&node.container.0) else {
                continue;
            };
            if let Some(&file_id) = file_ids.get(&vertex) {
                placement.insert(node.id.0, file_id);
            }
        }
        placement
    }
}
