//! Interns the package-group, package, and domain naming ladder.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use unlace_ir::{ContainerId, ScopeLevel};

use crate::analyze::layout::{
    ContainerArena, NameTally, anchor_min, elect, qualify_elected, record_undecorated_key,
};
use crate::analyze::relocation::PipelineSolver;
use crate::analyze::scoring::ContainerSpec;

impl PipelineSolver<'_> {
    /// Interns the upper naming ladder — package groups over packages over
    /// domains — into `arena`, parent before child, and returns each domain
    /// cluster's [`ContainerId`] so [`emit`](Self::emit) can hang folders and
    /// files beneath it.
    ///
    /// Every level's sibling names are elected through the never-mixed,
    /// never-numeric [`elect`] ladder and then made injective by
    /// [`qualify_elected`], disambiguating a shared elected name with the
    /// cluster's lexicographically smallest real folder — its *anchor* — the
    /// same real-location qualifier folder twins use, so no reachable elected
    /// path ever falls back to the arena's numeric backstop.
    pub(super) fn intern_upper_levels(
        &self,
        arena: &mut ContainerArena,
        key_by_id: &mut BTreeMap<u32, SmolStr>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        package_tally: &NameTally,
    ) -> BTreeMap<u32, ContainerId> {
        // folders are reality: each upper cluster anchors on the smallest real
        // directory name it holds, so two siblings that elect one name split
        // apart by their true locations rather than a synthetic `-N` twin.
        let mut group_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let mut package_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        let mut domain_anchor: BTreeMap<u32, SmolStr> = BTreeMap::new();
        for (&folder, &(domain, package, group)) in chain_of {
            let name = self
                .real_folder_names
                .get(folder as usize)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            anchor_min(&mut group_anchor, group, &name);
            anchor_min(&mut package_anchor, package, &name);
            anchor_min(&mut domain_anchor, domain, &name);
        }
        let package_label_keyed = anchored_label_keyed(
            chain_of,
            &self.real_folder_names,
            &self.real_folder_label_keyed,
            |(_, package, _)| package,
        );
        let domain_label_keyed = anchored_label_keyed(
            chain_of,
            &self.real_folder_names,
            &self.real_folder_label_keyed,
            |(domain, _, _)| domain,
        );

        let groups: BTreeSet<u32> = chain_of.values().map(|&(_, _, group)| group).collect();
        let group_raw: BTreeMap<u32, (u32, SmolStr)> = groups
            .iter()
            .map(|&group| (group, (0, self.root_name.clone())))
            .collect();
        let group_names = qualify_elected(&group_raw, &group_anchor);
        let mut group_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&group, name) in &group_names {
            let id = arena.push(ContainerSpec {
                name,
                level: ScopeLevel::PackageGroup,
                parent: None,
                synthetic: false,
                label_keyed: false,
            });
            if let Some((_, raw)) = group_raw.get(&group) {
                record_undecorated_key(key_by_id, id, raw, name);
            }
            group_ids.insert(group, id);
        }

        let packages: BTreeMap<u32, u32> = chain_of
            .values()
            .map(|&(_, package, group)| (package, group))
            .collect();
        let package_raw: BTreeMap<u32, (u32, SmolStr)> = packages
            .iter()
            .map(|(&package, &group)| {
                // the fallback is unreachable: `packages` and `package_anchor`
                // are both built from `chain_of`, so every key holds an anchor.
                let anchor = package_anchor
                    .get(&package)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                let name = package_tally
                    .get(&package)
                    .map_or_else(|| SmolStr::new("workspace"), |tally| elect(tally, &anchor));
                (package, (group, name))
            })
            .collect();
        let package_names = qualify_elected(&package_raw, &package_anchor);
        let mut package_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&package, &group) in &packages {
            let name = package_names
                .get(&package)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let id = arena.push(ContainerSpec {
                name: &name,
                level: ScopeLevel::Package,
                parent: group_ids.get(&group).copied(),
                synthetic: false,
                label_keyed: package_label_keyed.get(&package).copied().unwrap_or(false),
            });
            if let Some((_, raw)) = package_raw.get(&package) {
                record_undecorated_key(key_by_id, id, raw, &name);
            }
            package_ids.insert(package, id);
        }

        Self::intern_domains(
            arena,
            key_by_id,
            chain_of,
            domain_tally,
            &domain_anchor,
            &package_ids,
            &domain_label_keyed,
        )
    }

    /// Interns the domain level beneath already-interned packages.
    ///
    /// Each domain elects its name through the [`elect`] ladder, disambiguates
    /// with its cluster anchor via [`qualify_elected`], and hangs off its parent
    /// package. The elected key lands verbatim — a name foreign to its parent
    /// renders whole, the honest display of a suggested grouping that spans real
    /// locations (folders set the precedent). Returns each cluster's domain id.
    fn intern_domains(
        arena: &mut ContainerArena,
        key_by_id: &mut BTreeMap<u32, SmolStr>,
        chain_of: &BTreeMap<u32, (u32, u32, u32)>,
        domain_tally: &NameTally,
        domain_anchor: &BTreeMap<u32, SmolStr>,
        package_ids: &BTreeMap<u32, ContainerId>,
        domain_label_keyed: &BTreeMap<u32, bool>,
    ) -> BTreeMap<u32, ContainerId> {
        let domains: BTreeMap<u32, u32> = chain_of
            .values()
            .map(|&(domain, package, _)| (domain, package))
            .collect();
        let domain_raw: BTreeMap<u32, (u32, SmolStr)> = domains
            .iter()
            .map(|(&domain, &package)| {
                // the fallback is unreachable: `domains` and `domain_anchor`
                // are both built from `chain_of`, so every key holds an anchor.
                let anchor = domain_anchor
                    .get(&domain)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::new("workspace"));
                let name = domain_tally
                    .get(&domain)
                    .map_or_else(|| SmolStr::new("workspace"), |tally| elect(tally, &anchor));
                (domain, (package, name))
            })
            .collect();
        let domain_names = qualify_elected(&domain_raw, domain_anchor);
        let mut domain_ids: BTreeMap<u32, ContainerId> = BTreeMap::new();
        for (&domain, &package) in &domains {
            let name = domain_names
                .get(&domain)
                .cloned()
                .unwrap_or_else(|| SmolStr::new("workspace"));
            let id = arena.push(ContainerSpec {
                name: &name,
                level: ScopeLevel::Domain,
                parent: package_ids.get(&package).copied(),
                synthetic: false,
                label_keyed: domain_label_keyed.get(&domain).copied().unwrap_or(false),
            });
            if let Some((_, raw)) = domain_raw.get(&domain) {
                record_undecorated_key(key_by_id, id, raw, &name);
            }
            domain_ids.insert(domain, id);
        }

        domain_ids
    }
}

/// Whether each upper cluster `level` picks out of `chain_of` is keyed under the
/// package-group label, as the folder it anchors on is.
///
/// A cluster anchors on the smallest real folder name it holds (ties to the
/// lowest folder cluster id), the same folder that qualifies its name, so the
/// provenance and the name come from one place. A cluster holding folders of
/// both provenances, such as loose files beside a same-named package, takes the
/// anchor's rather than a blend of the two.
fn anchored_label_keyed(
    chain_of: &BTreeMap<u32, (u32, u32, u32)>,
    folder_names: &[SmolStr],
    folder_label_keyed: &[bool],
    level: fn((u32, u32, u32)) -> u32,
) -> BTreeMap<u32, bool> {
    let mut anchored: BTreeMap<u32, (&SmolStr, bool)> = BTreeMap::new();
    for (&folder, &chain) in chain_of {
        let Some(name) = folder_names.get(folder as usize) else {
            continue;
        };
        let keyed = folder_label_keyed
            .get(folder as usize)
            .copied()
            .unwrap_or(false);
        anchored
            .entry(level(chain))
            .and_modify(|held| {
                if name < held.0 {
                    *held = (name, keyed);
                }
            })
            .or_insert((name, keyed));
    }
    anchored
        .into_iter()
        .map(|(cluster, (_, keyed))| (cluster, keyed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<SmolStr> {
        values.iter().map(|value| SmolStr::new(*value)).collect()
    }

    /// Folders 0 and 1 share package 7; folder 2 sits alone in package 8.
    fn chain() -> BTreeMap<u32, (u32, u32, u32)> {
        BTreeMap::from([(0, (10, 7, 1)), (1, (11, 7, 1)), (2, (12, 8, 1))])
    }

    #[test]
    fn should_take_a_packages_provenance_from_its_anchor_folder_not_a_blend() {
        // the package holds a real folder `ws/core` and a loose `ws/aaa`; the
        // loose one is the smallest name, so it anchors and the package follows it.
        let flags = anchored_label_keyed(
            &chain(),
            &names(&["ws/core", "ws/aaa", "ws/other"]),
            &[false, true, false],
            |(_, package, _)| package,
        );

        assert_eq!(flags.get(&7), Some(&true));
        assert_eq!(flags.get(&8), Some(&false));
    }

    #[test]
    fn should_follow_a_real_anchor_when_a_loose_folder_sorts_after_it() {
        let flags = anchored_label_keyed(
            &chain(),
            &names(&["ws/core", "ws/zzz", "ws/other"]),
            &[false, true, false],
            |(_, package, _)| package,
        );

        assert_eq!(flags.get(&7), Some(&false));
    }

    #[test]
    fn should_anchor_a_tie_on_the_lowest_folder_cluster() {
        let flags = anchored_label_keyed(
            &chain(),
            &names(&["ws/same", "ws/same", "ws/other"]),
            &[true, false, false],
            |(_, package, _)| package,
        );

        assert_eq!(flags.get(&7), Some(&true));
    }

    #[test]
    fn should_key_each_domain_by_its_own_anchor() {
        let flags = anchored_label_keyed(
            &chain(),
            &names(&["ws/core", "ws/aaa", "ws/other"]),
            &[false, true, false],
            |(domain, _, _)| domain,
        );

        assert_eq!(flags.get(&10), Some(&false));
        assert_eq!(flags.get(&11), Some(&true));
    }
}
