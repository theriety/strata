//! Acyclic group splitting for packing.
//!
//! Splits an oversized acyclic group at the min-weight topological cut, found over
//! the group's topological order and applied recursively until every file fits.

use std::collections::BTreeSet;

use super::{FileGroup, FolderView, file_group};

/// Splits an oversized acyclic group at the min-weight topological cut.
///
/// The group's atoms are ordered topologically (by the folder's inter-atom
/// edges); a dynamic program over that order finds the prefix whose cut weight to
/// the suffix is minimal among prefixes that keep each side within `cap`. The
/// split recurses on each side until every file fits.
pub(super) fn split_acyclic(folder: &FolderView, cap: u32, group: &[usize]) -> Vec<FileGroup> {
    if group.is_empty() {
        return Vec::new();
    }

    let size = group
        .iter()
        .filter_map(|&atom| folder.atoms.get(atom))
        .map(|atom| atom.production_sloc)
        .sum::<u32>();
    if size <= cap || group.len() == 1 {
        return vec![file_group(folder, group, size > cap)];
    }

    let order = topological_atoms(folder, group);
    let Some((left, right)) = min_weight_cut(folder, &order, cap) else {
        // no balanced legal cut exists; keep the group whole but mark it exempt.
        return vec![file_group(folder, group, true)];
    };

    let mut files = split_acyclic(folder, cap, &left);
    files.extend(split_acyclic(folder, cap, &right));
    files
}

/// Orders the atoms of `group` topologically over the folder's dependency edges
/// (dependent before dependency), breaking ties by ascending atom index.
fn topological_atoms(folder: &FolderView, group: &[usize]) -> Vec<usize> {
    let members: BTreeSet<usize> = group.iter().copied().collect();
    let mut indegree: std::collections::BTreeMap<usize, usize> =
        group.iter().map(|&atom| (atom, 0)).collect();
    let mut successors: std::collections::BTreeMap<usize, BTreeSet<usize>> =
        group.iter().map(|&atom| (atom, BTreeSet::new())).collect();

    for edge in &folder.edges {
        if !members.contains(&edge.source) || !members.contains(&edge.target) {
            continue;
        }
        if edge.source == edge.target {
            continue;
        }
        if let Some(list) = successors.get_mut(&edge.source)
            && list.insert(edge.target)
            && let Some(slot) = indegree.get_mut(&edge.target)
        {
            *slot += 1;
        }
    }

    let mut ready: BTreeSet<usize> = indegree
        .iter()
        .filter_map(|(&atom, &degree)| (degree == 0).then_some(atom))
        .collect();
    let mut order = Vec::with_capacity(group.len());
    while let Some(&atom) = ready.iter().next() {
        ready.remove(&atom);
        order.push(atom);
        if let Some(list) = successors.get(&atom) {
            for &next in list {
                if let Some(slot) = indegree.get_mut(&next) {
                    *slot = slot.saturating_sub(1);
                    if *slot == 0 {
                        ready.insert(next);
                    }
                }
            }
        }
    }

    // any atoms left out (should not happen for an acyclic group) append in order.
    for &atom in group {
        if !order.contains(&atom) {
            order.push(atom);
        }
    }
    order
}

/// Finds the prefix/suffix split of a topological `order` whose crossing weight is
/// minimal among splits where both sides fit `cap`.
///
/// Returns `None` when no split point leaves both sides within the cap (e.g. a
/// single atom already exceeds it).
fn min_weight_cut(
    folder: &FolderView,
    order: &[usize],
    cap: u32,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let total: u32 = order
        .iter()
        .filter_map(|&atom| folder.atoms.get(atom))
        .map(|atom| atom.production_sloc)
        .sum();

    let mut prefix_sloc = 0_u32;
    let mut best: Option<(f64, usize)> = None;
    for split in 1..order.len() {
        if let Some(&atom) = order.get(split - 1) {
            prefix_sloc =
                prefix_sloc.saturating_add(folder.atoms.get(atom).map_or(0, |a| a.production_sloc));
        }
        let suffix_sloc = total.saturating_sub(prefix_sloc);
        if prefix_sloc > cap || suffix_sloc > cap {
            continue;
        }
        let weight = crossing_weight(folder, order, split);
        if best.is_none_or(|(best_weight, _)| weight < best_weight) {
            best = Some((weight, split));
        }
    }

    best.map(|(_, split)| {
        let left = order
            .get(..split)
            .map(<[usize]>::to_vec)
            .unwrap_or_default();
        let right = order
            .get(split..)
            .map(<[usize]>::to_vec)
            .unwrap_or_default();
        (left, right)
    })
}

/// Sums the weight of edges crossing the `split` boundary of `order` (between the
/// first `split` atoms and the rest), in either direction.
fn crossing_weight(folder: &FolderView, order: &[usize], split: usize) -> f64 {
    let prefix: BTreeSet<usize> = order.iter().take(split).copied().collect();
    let suffix: BTreeSet<usize> = order.iter().skip(split).copied().collect();
    folder
        .edges
        .iter()
        .filter(|edge| {
            (prefix.contains(&edge.source) && suffix.contains(&edge.target))
                || (suffix.contains(&edge.source) && prefix.contains(&edge.target))
        })
        .map(|edge| edge.weight)
        .sum()
}
