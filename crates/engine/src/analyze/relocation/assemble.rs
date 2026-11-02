//! Assembles the five-level candidate tree a folder partition induces.

use std::collections::BTreeMap;

use strata_core::cluster::Partition;
use strata_core::cluster::seed::SeedLevel;

use crate::analyze::layout::{NameTally, cluster_level, home_affinity, vote};
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::partition_keys::{
    group_by_key, lift_pins, name_pinned_packages, split_by_key,
};
use crate::analyze::relocation::{CandidateTree, PipelineSolver};

impl PipelineSolver<'_> {
    /// Assembles the five-level candidate tree a folder partition induces.
    ///
    /// Folders are the partition's non-empty clusters and keep their real
    /// directory names ([`real_dir_partition`]) — folders are reality, so no
    /// election happens at that level. The upper levels come from clustering
    /// each level's weighted quotient in turn (folders → domains → packages →
    /// package groups) and are named from the files they transitively hold —
    /// each file votes its laminar domain and package name keys
    /// (source-root-transparent, package-root-resolved), weighted by production
    /// SLOC then file count, and [`elect`] names the cluster through its
    /// never-mixed, never-numeric ladder — strict-majority home, shared home
    /// prefix, top-two join, dominant token, then an anchored non-numeric last
    /// resort; the group takes the current root's name — and file leaves keep
    /// their full current paths so file identity stays stable across trees.
    pub(super) fn assemble(&self, parts: &Partition) -> CandidateTree {
        self.assemble_with_polish_evidence(parts, &PolishEvidence::default())
    }

    pub(super) fn assemble_with_polish_evidence(
        &self,
        parts: &Partition,
        evidence: &PolishEvidence,
    ) -> CandidateTree {
        if self.files.is_empty() {
            return self.empty_candidate();
        }

        let members_of = self.folder_members(parts);

        // one clustering pass per upper level, each over the previous level's
        // weighted quotient graph. Each pass carries a home-directory seed
        // affinity keyed by the dominant laminar home of the containers below it,
        // so folders group by home directory into named domains instead of pooling
        // by index order into a cut-minimal grab-bag no single home could honestly
        // name. The package-group level has no home key, so it keeps the neutral
        // descending-layer order.
        let folder_quotient = parts.quotient(&self.condensation.dag);
        let domain_homes = self.home_keys(
            &members_of,
            folder_quotient.vertex_count(),
            |folder| folder,
            |file| &file.home.domain,
        );
        let mut domain_parts = cluster_level(
            &folder_quotient,
            &self.caps,
            SeedLevel::Domain,
            &home_affinity(&domain_homes),
        );
        // with the package wall up the display levels obey it too (ADR-17):
        // no domain spans packages and each package container holds exactly
        // one real package, so the tree never draws a file under a package
        // its move list keeps it out of.
        //
        // A folder is keyed by the files drawn in it, not every member of its
        // cluster: a cross-package cycle member whose retained home is another
        // folder is drawn there, under its own package.
        let drawn_of = (!self.relocation_identity.allows_cross_package_moves())
            .then(|| self.drawn_members(parts, &members_of, evidence));
        let package_members = drawn_of.as_ref().unwrap_or(&members_of);
        let folder_packages = drawn_of.as_ref().map(|drawn_of| {
            self.home_keys(
                drawn_of,
                folder_quotient.vertex_count(),
                |folder| folder,
                |file| &file.home.package,
            )
        });
        // a folder holding a folded file (ADR-21) stays in that file's
        // pass-start package, so no level can re-propose the withdrawn move.
        let folder_pins =
            self.folded_folder_pins(&members_of, folder_quotient.vertex_count(), evidence);
        if let Some(folder_keys) = folder_packages.as_ref().or(folder_pins.as_ref()) {
            domain_parts = split_by_key(&domain_parts, folder_keys);
        }
        let domain_quotient = domain_parts.quotient(&folder_quotient);
        let package_homes = self.home_keys(
            package_members,
            domain_quotient.vertex_count(),
            |folder| {
                domain_parts
                    .cluster_of(folder)
                    .map_or(0, |cluster| cluster.0)
            },
            |file| &file.home.package,
        );
        // with the wall up a package level is an uncapped mirror of the
        // manifests (ADR-17): its containers are the real packages, which
        // the capacity caps never split, so it is grouped by key, not clustered.
        let domain_pins = folder_pins
            .as_ref()
            .map(|pins| lift_pins(pins, &domain_parts, domain_quotient.vertex_count()));
        let package_parts = if folder_packages.is_some() {
            group_by_key(&package_homes)
        } else {
            self.cluster_packages(&domain_quotient, &package_homes, domain_pins.as_deref())
        };
        let package_quotient = package_parts.quotient(&domain_quotient);
        let group_parts =
            cluster_level(&package_quotient, &self.caps, SeedLevel::PackageGroup, &[]);

        // ancestry of every non-empty folder cluster, plus the directory tallies
        // each level's containers are named from.
        let mut chain_of: BTreeMap<u32, (u32, u32, u32)> = BTreeMap::new();
        let mut domain_tally: NameTally = BTreeMap::new();
        let mut package_tally: NameTally = BTreeMap::new();
        for (&folder, members) in &members_of {
            let domain = domain_parts
                .cluster_of(folder)
                .map_or(0, |cluster| cluster.0);
            let package = package_parts
                .cluster_of(domain)
                .map_or(0, |cluster| cluster.0);
            let group = group_parts
                .cluster_of(package)
                .map_or(0, |cluster| cluster.0);
            chain_of.insert(folder, (domain, package, group));
            for &vertex in members {
                let Some(file) = self.files.get(vertex as usize) else {
                    continue;
                };
                let sloc = file.production_sloc;
                // vote with the laminar tree's resolved name keys, not raw path
                // prefixes, so source roots stay transparent and the package
                // resolves to its manifest root (never a bare `src`).
                vote(&mut domain_tally, domain, file.home.domain.clone(), sloc);
            }
            for &vertex in package_members.get(&folder).map_or(&[][..], Vec::as_slice) {
                if let Some(file) = self.files.get(vertex as usize) {
                    vote(
                        &mut package_tally,
                        package,
                        file.home.package.clone(),
                        file.production_sloc,
                    );
                }
            }
        }
        if let Some(domain_pins) = &domain_pins {
            name_pinned_packages(&mut package_tally, domain_pins, &package_parts);
        }

        self.emit(
            parts,
            &members_of,
            &chain_of,
            &domain_tally,
            &package_tally,
            evidence,
        )
    }
}
