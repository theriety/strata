//! Relieves over-capacity physical namespaces with evidence-backed child groups.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};
use strata_core::condense::Condensation;
use strata_core::graph::csr::Csr;

use crate::analyze::layout::{connected_piles, elect_split_token};
use crate::analyze::relocation::FileInfo;

/// Relieves every over-capacity physical namespace with evidence-backed child
/// groups while preserving whole-SCC movement.
///
/// The split runs on priced connectivity between the folder's SCCs: two SCCs
/// end up in the same pile only when a positively-priced file edge joins them
/// ([`connected_piles`]), so each child group stays internally connected and the
/// quotient DAG never gains a cycle (whole SCCs always move together). Only
/// evidence-backed groups split off: a pile qualifies when its members are
/// actually joined (at least two SCCs or two files); unrelated singletons stay
/// glued to the original cluster, because inventing groups for unconnected
/// files would erase the merge pressure that cross-folder coupling otherwise
/// exerts on the search. A connected pile that alone exceeds the budget is
/// chunked contiguously along its own connectivity; a single oversized SCC
/// cannot be split at all and is left whole — the objective still prices it,
/// which is honest.
///
/// Pile 0 of each folder keeps the folder's original cluster id; every later
/// pile gets a fresh cluster id, a `{folder}/{token}` name that path-extends
/// its base folder at render time, and a `false` synthetic marker. Members'
/// `home.domain` keys stay untouched: the groups already sit in one domain —
/// their base folder's — and the slash in the label is what nests the group
/// under it, so no home rewriting can weld or split anything here. Returns the
/// files plus the relieved partition and its extended tables; the
/// identity entry must be cloned before this runs (the caller does).
pub(in crate::analyze) fn relieve_over_capacity(
    files: Vec<FileInfo>,
    condensation: &Condensation,
    graph: &Csr,
    base: &Partition,
    base_names: &[SmolStr],
    base_synthetic: &[bool],
    cap: u32,
) -> (Vec<FileInfo>, Partition, Vec<SmolStr>, Vec<bool>) {
    let mut assignment = base.assignment().to_vec();
    let mut names = base_names.to_vec();
    let mut synthetic = base_synthetic.to_vec();
    if cap == 0 {
        // a disabled budget binds nothing, so there is nothing to relieve.
        return (files, base.clone(), names, synthetic);
    }
    let base_count = base.cluster_count();
    let mut next_cluster = u64::from(u32::try_from(base_count).unwrap_or(u32::MAX));
    let mut used_labels: BTreeSet<SmolStr> = names.iter().cloned().collect();
    let direct_children = relief_child_entries(base, base_names, condensation, &files);
    for cluster_index in 0..base_count {
        if next_cluster > u64::from(u32::MAX) {
            break;
        }
        let cluster = ClusterId(u32::try_from(cluster_index).unwrap_or(u32::MAX));
        let sccs: Vec<u32> = (0..u32::try_from(condensation.members.len()).unwrap_or(u32::MAX))
            .filter(|scc| base.cluster_of(*scc) == Some(cluster))
            .collect();
        let base_name = base_names
            .get(cluster_index)
            .cloned()
            .unwrap_or_else(|| SmolStr::new("workspace"));
        let by_namespace = relief_sccs_by_namespace(&sccs, condensation, &files);
        let mut planned_groups = Vec::new();
        for (namespace, file_count_by_scc) in &by_namespace {
            let bound: u32 = file_count_by_scc.values().copied().sum();
            let child_entries = direct_children
                .get(&(cluster_index, namespace.clone()))
                .copied()
                .unwrap_or(0);
            if bound.saturating_add(child_entries) <= cap {
                continue;
            }
            let file_budget = cap.saturating_sub(child_entries).max(1);
            let namespace_sccs: Vec<u32> = file_count_by_scc.keys().copied().collect();
            let piles = connected_relief_piles(
                &namespace_sccs,
                condensation,
                graph,
                file_budget,
                file_count_by_scc,
            );
            planned_groups.extend(relief_move_groups(
                &piles,
                file_count_by_scc,
                bound,
                child_entries,
                cap,
            ));
        }
        for (ordinal, group) in merge_overlapping_relief_groups(planned_groups)
            .iter()
            .enumerate()
        {
            let token = elect_split_token(group, condensation, &files);
            let mut label = token.map_or_else(
                || format!("{base_name}/{}", ordinal + 1),
                |stem| format!("{base_name}/{stem}"),
            );
            if used_labels.contains(label.as_str()) {
                label = format!("{base_name}/{next_cluster}");
            }
            used_labels.insert(SmolStr::from(label.clone()));
            for &scc in group {
                if let Some(slot) = assignment.get_mut(scc as usize) {
                    *slot = ClusterId(u32::try_from(next_cluster).unwrap_or(u32::MAX));
                }
            }
            names.push(SmolStr::from(label));
            synthetic.push(false);
            next_cluster += 1;
        }
    }
    (
        files,
        Partition::from_assignment(
            assignment,
            usize::try_from(next_cluster).unwrap_or(base_count),
        ),
        names,
        synthetic,
    )
}

pub(super) fn relief_child_entries(
    base: &Partition,
    names: &[SmolStr],
    condensation: &Condensation,
    files: &[FileInfo],
) -> BTreeMap<(usize, SmolStr), u32> {
    let mut entries = BTreeMap::new();
    for (child, child_name) in names.iter().enumerate().take(base.cluster_count()) {
        let Some((parent_name, _)) = child_name.rsplit_once('/') else {
            continue;
        };
        let Some(parent) = names.iter().position(|name| name.as_str() == parent_name) else {
            continue;
        };
        let child_cluster = ClusterId(u32::try_from(child).unwrap_or(u32::MAX));
        let namespaces: BTreeSet<SmolStr> = condensation
            .members
            .iter()
            .enumerate()
            .filter(|(scc, _)| {
                base.cluster_of(u32::try_from(*scc).unwrap_or(u32::MAX)) == Some(child_cluster)
            })
            .flat_map(|(_, members)| members)
            .filter_map(|member| files.get(member.0 as usize))
            .map(|file| file.namespace.clone())
            .collect();
        for namespace in namespaces {
            *entries.entry((parent, namespace)).or_default() += 1;
        }
    }
    entries
}

pub(super) fn relief_sccs_by_namespace(
    sccs: &[u32],
    condensation: &Condensation,
    files: &[FileInfo],
) -> BTreeMap<SmolStr, BTreeMap<u32, u32>> {
    let mut grouped = BTreeMap::new();
    for &scc in sccs {
        for file in condensation
            .members
            .get(scc as usize)
            .into_iter()
            .flatten()
            .filter_map(|member| files.get(member.0 as usize))
        {
            let by_scc: &mut BTreeMap<u32, u32> =
                grouped.entry(file.namespace.clone()).or_default();
            let count = by_scc.entry(scc).or_default();
            *count = count.saturating_add(1);
        }
    }
    grouped
}

fn connected_relief_piles(
    sccs: &[u32],
    condensation: &Condensation,
    graph: &Csr,
    file_budget: u32,
    file_count_by_scc: &BTreeMap<u32, u32>,
) -> Vec<Vec<u32>> {
    connected_piles(sccs, condensation, graph, file_budget, file_count_by_scc)
        .into_iter()
        .filter(|pile| {
            pile.len() >= 2
                || pile
                    .first()
                    .and_then(|scc| file_count_by_scc.get(scc))
                    .is_some_and(|&count| count >= 2)
        })
        .collect()
}

fn relief_move_groups(
    piles: &[Vec<u32>],
    file_count_by_scc: &BTreeMap<u32, u32>,
    bound: u32,
    child_entries: u32,
    cap: u32,
) -> Vec<Vec<u32>> {
    if piles.is_empty() {
        return Vec::new();
    }
    let grouped_files: u32 = piles
        .iter()
        .flatten()
        .filter_map(|scc| file_count_by_scc.get(scc))
        .copied()
        .sum();
    let stationary = bound.saturating_sub(grouped_files);
    let available = usize::try_from(cap.saturating_sub(child_entries).saturating_sub(stationary))
        .unwrap_or(usize::MAX);
    if available == 0 {
        return Vec::new();
    }
    let first_files: u32 = piles
        .first()
        .into_iter()
        .flatten()
        .filter_map(|scc| file_count_by_scc.get(scc))
        .copied()
        .sum();
    let keep_first = stationary
        .saturating_add(first_files)
        .saturating_add(child_entries)
        .saturating_add(u32::try_from((piles.len() - 1).min(available)).unwrap_or(u32::MAX))
        <= cap;
    let moving = if keep_first {
        piles.get(1..).unwrap_or_default()
    } else {
        piles
    };
    let group_count = moving.len().min(available);
    let mut groups = vec![Vec::new(); group_count];
    for (index, pile) in moving.iter().enumerate() {
        let group = index.saturating_mul(group_count) / moving.len();
        if let Some(target) = groups.get_mut(group) {
            target.extend(pile.iter().copied());
        }
    }
    groups
}

fn merge_overlapping_relief_groups(groups: Vec<Vec<u32>>) -> Vec<Vec<u32>> {
    let mut merged: Vec<BTreeSet<u32>> = Vec::new();
    for group in groups {
        let mut current: BTreeSet<u32> = group.into_iter().collect();
        if current.is_empty() {
            continue;
        }
        let mut untouched = Vec::new();
        for existing in std::mem::take(&mut merged) {
            if current.is_disjoint(&existing) {
                untouched.push(existing);
            } else {
                current.extend(existing);
            }
        }
        merged = untouched;
        merged.push(current);
    }
    let mut result: Vec<Vec<u32>> = merged
        .into_iter()
        .map(|group| group.into_iter().collect())
        .collect();
    result.sort();
    result
}
