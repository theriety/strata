//! Per-folder member views the candidate assembly reads.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::Partition;
use strata_core::cluster::seed::SeedLevel;
use strata_core::graph::csr::Csr;
use strata_ir::{Container, ContainerId, ContainerTree, ScopeLevel};

use crate::analyze::layout::{NameTally, cluster_level, home_affinity, plurality, vote};
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::partition_keys::split_by_key;
use crate::analyze::relocation::{CandidateTree, FileInfo, PipelineSolver};

impl PipelineSolver<'_> {
    /// Groups the file-graph vertices by the folder cluster their SCC lands in,
    /// members sorted by file path for deterministic emission.
    pub(super) fn folder_members(&self, parts: &Partition) -> BTreeMap<u32, Vec<u32>> {
        let mut members_of: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for vertex in 0..self.files.len() {
            let Some(scc) = self.condensation.membership.get(vertex) else {
                continue;
            };
            let Some(cluster) = parts.cluster_of(scc.0) else {
                debug_assert!(false, "the folder partition must cover every file scc");
                continue;
            };
            members_of
                .entry(cluster.0)
                .or_default()
                .push(u32::try_from(vertex).unwrap_or(u32::MAX));
        }
        for members in members_of.values_mut() {
            members.sort_by(|&left, &right| {
                let left_name = self
                    .files
                    .get(left as usize)
                    .map_or("", |file| file.name.as_str());
                let right_name = self
                    .files
                    .get(right as usize)
                    .map_or("", |file| file.name.as_str());
                left_name.cmp(right_name).then(left.cmp(&right))
            });
        }
        members_of
    }

    /// Each folder cluster's members drawn in the cluster's own folder.
    ///
    /// [`Self::projected_folder_members`] draws a cross-package cycle member
    /// whose retained home is another folder in that home, under its own
    /// package, so it must not key the cluster it is solved in. A cluster none
    /// of whose members stay keeps them all, so no cluster goes unkeyed.
    pub(super) fn drawn_members(
        &self,
        parts: &Partition,
        members_of: &BTreeMap<u32, Vec<u32>>,
        evidence: &PolishEvidence,
    ) -> BTreeMap<u32, Vec<u32>> {
        members_of
            .iter()
            .map(|(&folder, members)| {
                let key = self
                    .real_folder_names
                    .get(folder as usize)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                let drawn = self
                    .projected_folder_members(parts, members, evidence, &key)
                    .remove(&key)
                    .filter(|drawn| !drawn.is_empty())
                    .unwrap_or_else(|| members.clone());
                (folder, drawn)
            })
            .collect()
    }

    /// Dominant laminar home key (via `key`, weighted by production SLOC) per
    /// base vertex of an upper clustering level.
    ///
    /// `cluster_of` maps a folder cluster to the base vertex it contributes to at
    /// this level — the identity for the domain level (each folder is a vertex),
    /// the domain partition for the package level (each domain is a vertex). An
    /// absent (empty) vertex falls back to `workspace`. The result seeds and gains
    /// [`cluster_level`] so containers group by home directory.
    pub(super) fn home_keys(
        &self,
        members_of: &BTreeMap<u32, Vec<u32>>,
        vertex_count: usize,
        cluster_of: impl Fn(u32) -> u32,
        key: impl Fn(&FileInfo) -> &SmolStr,
    ) -> Vec<SmolStr> {
        let mut tally: NameTally = BTreeMap::new();
        for (&folder, members) in members_of {
            let cluster = cluster_of(folder);
            for &vertex in members {
                if let Some(file) = self.files.get(vertex as usize) {
                    vote(&mut tally, cluster, key(file).clone(), file.production_sloc);
                }
            }
        }
        (0..vertex_count)
            .map(|vertex| {
                tally
                    .get(&u32::try_from(vertex).unwrap_or(u32::MAX))
                    .map_or_else(|| SmolStr::new("workspace"), plurality)
            })
            .collect()
    }

    /// Clusters domains into packages with the wall lifted, keeping every
    /// domain that holds a folded file (ADR-21) apart from the others by its
    /// pin.
    pub(super) fn cluster_packages(
        &self,
        domain_quotient: &Csr,
        package_homes: &[SmolStr],
        domain_pins: Option<&[SmolStr]>,
    ) -> Partition {
        let clustered = cluster_level(
            domain_quotient,
            &self.caps,
            SeedLevel::Package,
            &home_affinity(package_homes),
        );
        match domain_pins {
            Some(domain_pins) => split_by_key(&clustered, domain_pins),
            None => clustered,
        }
    }

    /// The candidate of a repository with no files: its root package group
    /// alone.
    pub(super) fn empty_candidate(&self) -> CandidateTree {
        let root = Container {
            id: ContainerId(0),
            name: self.root_name.clone(),
            level: ScopeLevel::PackageGroup,
            parent: None,
            synthetic: false,
        };
        CandidateTree {
            tree: ContainerTree::new(vec![root]),
            placement: BTreeMap::new(),
            pass_start_file_by_candidate: BTreeMap::new(),
            zone_by_file: BTreeMap::new(),
            namespace_by_file: BTreeMap::new(),
            package_by_file: BTreeMap::new(),
            key_by_id: BTreeMap::new(),
        }
    }

    /// Keys each folder cluster by the pass-start package of a folded file it
    /// holds (ADR-21), or the empty key when it holds none; `None` when the
    /// restart folded nothing.
    pub(super) fn folded_folder_pins(
        &self,
        members_of: &BTreeMap<u32, Vec<u32>>,
        folder_count: usize,
        evidence: &PolishEvidence,
    ) -> Option<Vec<SmolStr>> {
        if evidence.folds.is_empty() {
            return None;
        }
        let folded: BTreeSet<u32> = evidence.folds.iter().map(|fold| fold.file).collect();
        let mut pins = vec![SmolStr::default(); folder_count];
        for (&folder, members) in members_of {
            let pinned = members
                .iter()
                .filter_map(|&vertex| self.files.get(vertex as usize))
                .find(|file| folded.contains(&file.container));
            if let (Some(file), Some(slot)) = (pinned, pins.get_mut(folder as usize)) {
                slot.clone_from(&file.home.package);
            }
        }
        Some(pins)
    }
}
