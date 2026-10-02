//! Pass-start permissions that keep relocations inside their package and namespace.

use std::collections::BTreeSet;

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};
use strata_core::condense::Condensation;

use crate::analyze::layout::dominant_member;
use crate::analyze::relocation::FileInfo;

mod permits;

/// Preserves pass-start render namespaces and namespace-scoped leaf identity,
/// and keeps every file inside its manifest package (ADR-17).
///
/// Render namespaces are package-relative (every crate's `src` renders alike),
/// so they cannot tell two packages apart; the package key from the laminar
/// home can. Both permissions are frozen at pass start: a cluster admits only
/// the namespaces its pass-start members already had, and a file may join a
/// cluster only when every file physically in that cluster at pass start is in
/// the file's package. So no relocation (search move, mirror follower, or roof rebuild)
/// carries a file across a package boundary unless the profile lifts the
/// package wall.
///
/// An SCC whose files span packages (a cross-package import cycle) belongs to
/// no single package. With the wall up it stays in its pass-start cluster: it
/// never moves and never joins a new group folder. The cluster it sits in is
/// the folder of its dominant member; members whose retained home is another
/// folder do not count toward that folder's packages, so a file of the
/// folder's own package may still join it.
pub(in crate::analyze) struct RelocationIdentityGuard {
    identities_by_scc: Vec<Vec<RenderIdentity>>,
    allowed_namespaces: Vec<BTreeSet<SmolStr>>,
    packages_by_scc: Vec<BTreeSet<SmolStr>>,
    allowed_packages: Vec<BTreeSet<SmolStr>>,
    homes_by_scc: Vec<Vec<ClusterId>>,
    package_wall: bool,
}

type RenderIdentity = (SmolStr, SmolStr);
type IdentitySubgroup = (Vec<u32>, BTreeSet<RenderIdentity>);

impl RelocationIdentityGuard {
    /// Builds a guard with the package wall up; lift it with
    /// [`Self::lift_package_wall`].
    pub(in crate::analyze) fn new(
        files: &[FileInfo],
        condensation: &Condensation,
        identity: &Partition,
        relieved: &Partition,
        roof_rebuild: Option<&Partition>,
    ) -> Self {
        let identities_by_scc: Vec<Vec<RenderIdentity>> = condensation
            .members
            .iter()
            .map(|members| {
                members
                    .iter()
                    .filter_map(|member| files.get(member.0 as usize))
                    .map(|file| {
                        (
                            file.namespace.clone(),
                            SmolStr::new(
                                file.name.rsplit('/').next().unwrap_or(file.name.as_str()),
                            ),
                        )
                    })
                    .collect()
            })
            .collect();
        let namespaces_by_scc: Vec<BTreeSet<SmolStr>> = identities_by_scc
            .iter()
            .map(|identities| {
                identities
                    .iter()
                    .map(|(namespace, _)| namespace.clone())
                    .collect()
            })
            .collect();
        let packages_by_scc: Vec<BTreeSet<SmolStr>> = condensation
            .members
            .iter()
            .map(|members| {
                members
                    .iter()
                    .filter_map(|member| files.get(member.0 as usize))
                    .map(|file| file.home.package.clone())
                    .collect()
            })
            .collect();
        // a cluster's packages are those of the files physically in it: a
        // cross-package cycle's members whose retained home is another folder
        // do not widen the folder the cycle is placed in.
        let resident_packages_by_scc: Vec<BTreeSet<SmolStr>> = condensation
            .members
            .iter()
            .map(|members| {
                let placed = dominant_member(files, members).map(|file| &file.home);
                members
                    .iter()
                    .filter_map(|member| files.get(member.0 as usize))
                    .filter(|file| placed == Some(&file.home))
                    .map(|file| file.home.package.clone())
                    .collect()
            })
            .collect();
        let allowed_namespaces =
            Self::pass_start_values(identity, relieved, roof_rebuild, &namespaces_by_scc);
        let allowed_packages =
            Self::pass_start_values(identity, relieved, roof_rebuild, &resident_packages_by_scc);
        let homes_by_scc =
            Self::pass_start_homes(identity, relieved, roof_rebuild, packages_by_scc.len());
        Self {
            identities_by_scc,
            allowed_namespaces,
            packages_by_scc,
            allowed_packages,
            homes_by_scc,
            package_wall: true,
        }
    }

    /// Lifts the package wall (the profile's `allow-cross-package-moves`);
    /// every other rule still applies.
    #[must_use]
    pub(in crate::analyze) const fn lift_package_wall(mut self) -> Self {
        self.package_wall = false;
        self
    }

    /// Whether the package wall is lifted; the single source of the setting.
    pub(in crate::analyze) const fn allows_cross_package_moves(&self) -> bool {
        !self.package_wall
    }

    /// Returns `true` when the package wall is down, when `target` is one of
    /// `moving`'s own pass-start clusters, or when `moving` lies in a single
    /// package and every pass-start member of `target` lies in that package.
    pub(in crate::analyze) fn permits_package_join(&self, moving: u32, target: ClusterId) -> bool {
        !self.package_wall
            || self.is_pass_start_home(moving, target)
            || self.within_pass_start_package(moving, target)
    }

    fn is_pass_start_home(&self, scc: u32, cluster: ClusterId) -> bool {
        self.homes_by_scc
            .get(scc as usize)
            .is_some_and(|homes| homes.contains(&cluster))
    }

    /// The single-package join rule: the packages of the files physically in
    /// the cluster at pass start must equal the mover's one package. A cluster
    /// holding files of two packages admits no newcomer, and a mover spanning
    /// packages matches no cluster (it may only stay home).
    fn within_pass_start_package(&self, scc: u32, cluster: ClusterId) -> bool {
        match (
            self.packages_by_scc.get(scc as usize),
            self.allowed_packages.get(cluster.0 as usize),
        ) {
            (Some(packages), Some(allowed)) => packages.len() == 1 && packages == allowed,
            _ => false,
        }
    }

    /// Whether `scc`'s files all lie in one package.
    fn is_single_package(&self, scc: u32) -> bool {
        self.packages_by_scc
            .get(scc as usize)
            .is_some_and(|packages| packages.len() == 1)
    }

    /// Freezes one per-SCC value set per cluster at pass start: the identity
    /// clusters, then the fresh clusters that capacity relief and the roof
    /// rebuild introduce.
    fn pass_start_values(
        identity: &Partition,
        relieved: &Partition,
        roof_rebuild: Option<&Partition>,
        values_by_scc: &[BTreeSet<SmolStr>],
    ) -> Vec<BTreeSet<SmolStr>> {
        let mut values = vec![BTreeSet::new(); identity.cluster_count()];
        Self::merge_cluster_values(&mut values, identity, values_by_scc, 0);
        for fresh in std::iter::once(relieved).chain(roof_rebuild) {
            let first_fresh = values.len();
            values.resize_with(fresh.cluster_count().max(first_fresh), BTreeSet::new);
            Self::merge_cluster_values(&mut values, fresh, values_by_scc, first_fresh);
        }
        values
    }

    /// Records, per SCC, the clusters it occupies at pass start, under the
    /// same freshness rule as [`Self::pass_start_values`]: its identity
    /// cluster, then any fresh cluster capacity relief or the roof rebuild
    /// places it in.
    fn pass_start_homes(
        identity: &Partition,
        relieved: &Partition,
        roof_rebuild: Option<&Partition>,
        scc_count: usize,
    ) -> Vec<Vec<ClusterId>> {
        let mut homes = vec![Vec::new(); scc_count];
        let mut first_fresh = 0;
        for parts in [identity, relieved].into_iter().chain(roof_rebuild) {
            for (scc, scc_homes) in homes.iter_mut().enumerate() {
                if let Some(cluster) = parts.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX))
                    && cluster.0 as usize >= first_fresh
                    && !scc_homes.contains(&cluster)
                {
                    scc_homes.push(cluster);
                }
            }
            first_fresh = first_fresh.max(parts.cluster_count());
        }
        homes
    }

    fn merge_cluster_values(
        values: &mut [BTreeSet<SmolStr>],
        parts: &Partition,
        values_by_scc: &[BTreeSet<SmolStr>],
        first_cluster: usize,
    ) {
        for (scc, scc_values) in values_by_scc.iter().enumerate() {
            let Some(cluster) = parts.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX)) else {
                continue;
            };
            if cluster.0 as usize >= first_cluster
                && let Some(allowed) = values.get_mut(cluster.0 as usize)
            {
                allowed.extend(scc_values.iter().cloned());
            }
        }
    }
}
