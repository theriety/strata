//! Kruskal-style atom agglomeration for packing.
//!
//! Scores atom pairs by edge weight plus naming-token cohesion and merges them
//! greedily while the group fits the cap and the file-level quotient stays acyclic.

use std::collections::BTreeSet;

use super::FolderView;
use super::union_find::UnionFind;

/// Runs the Kruskal-style agglomeration, mutating `state` so each root identifies
/// a file group.
pub(super) fn agglomerate(folder: &FolderView, cap: u32, state: &mut UnionFind) {
    let mut group_sloc: Vec<u32> = folder
        .atoms
        .iter()
        .map(|atom| atom.production_sloc)
        .collect();

    for pair in affinity_pairs(folder) {
        let root_a = state.find(pair.left);
        let root_b = state.find(pair.right);
        if root_a == root_b {
            continue;
        }

        let combined = group_sloc
            .get(root_a)
            .copied()
            .unwrap_or(0)
            .saturating_add(group_sloc.get(root_b).copied().unwrap_or(0));
        if combined > cap {
            continue;
        }
        if !merge_keeps_acyclic(folder, state, root_a, root_b) {
            continue;
        }

        let merged = state.union(root_a, root_b);
        if let Some(slot) = group_sloc.get_mut(merged) {
            *slot = combined;
        }
    }
}

/// A symbol-pair affinity: two atoms and the score that orders their merge.
struct AffinityPair {
    /// The lower-indexed atom of the pair.
    left: usize,
    /// The higher-indexed atom of the pair.
    right: usize,
    /// Edge weight plus naming-token cohesion.
    score: f64,
}

/// Builds the affinity pairs for `folder`, sorted by descending score with ties
/// broken by ascending `(left, right)` so the merge order is deterministic.
///
/// Affinity sums every directed edge weight between the two atoms (in either
/// orientation) and the naming-token Jaccard cohesion of their token sets.
fn affinity_pairs(folder: &FolderView) -> Vec<AffinityPair> {
    let atom_count = folder.atoms.len();
    let mut edge_weight: Vec<f64> = vec![0.0; atom_count.saturating_mul(atom_count)];

    for edge in &folder.edges {
        if edge.source == edge.target {
            continue;
        }
        let (left, right) = order_pair(edge.source, edge.target);
        if let Some(slot) =
            edge_weight.get_mut(left.saturating_mul(atom_count).saturating_add(right))
        {
            *slot += edge.weight;
        }
    }

    let mut pairs = Vec::new();
    for left in 0..atom_count {
        for right in (left + 1)..atom_count {
            let weight = edge_weight
                .get(left.saturating_mul(atom_count).saturating_add(right))
                .copied()
                .unwrap_or(0.0);
            let cohesion = naming_cohesion(folder, left, right);
            let score = weight + cohesion;
            if score > 0.0 {
                pairs.push(AffinityPair { left, right, score });
            }
        }
    }

    pairs.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.left.cmp(&b.left))
            .then(a.right.cmp(&b.right))
    });
    pairs
}

/// Returns the Jaccard similarity of two atoms' naming-token sets, or zero when
/// either atom is missing or both token sets are empty.
fn naming_cohesion(folder: &FolderView, left: usize, right: usize) -> f64 {
    let (Some(left), Some(right)) = (folder.atoms.get(left), folder.atoms.get(right)) else {
        return 0.0;
    };
    let intersection = left
        .naming_tokens
        .intersection(&right.naming_tokens)
        .count();
    let union = left.naming_tokens.union(&right.naming_tokens).count();
    if union == 0 {
        0.0
    } else {
        f64::from(u32::try_from(intersection).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(union).unwrap_or(u32::MAX))
    }
}

/// Orders two indices into an ascending `(low, high)` pair.
fn order_pair(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Tests whether merging the groups rooted at `root_a` and `root_b` leaves the
/// folder's file-level quotient acyclic.
///
/// The quotient places each atom in its current group root; merging two roots is
/// legal iff the resulting group graph has no directed cycle. The check builds
/// the post-merge quotient and runs a depth-first cycle search over it.
fn merge_keeps_acyclic(
    folder: &FolderView,
    state: &mut UnionFind,
    root_a: usize,
    root_b: usize,
) -> bool {
    let atom_count = folder.atoms.len();
    // map every atom to its group representative after the hypothetical merge.
    let mut representative: Vec<usize> = (0..atom_count).map(|atom| state.find(atom)).collect();
    for slot in &mut representative {
        if *slot == root_b {
            *slot = root_a;
        }
    }

    let mut adjacency: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); atom_count];
    for edge in &folder.edges {
        let (Some(&from), Some(&to)) = (
            representative.get(edge.source),
            representative.get(edge.target),
        ) else {
            continue;
        };
        if from != to
            && let Some(list) = adjacency.get_mut(from)
        {
            list.insert(to);
        }
    }

    !has_cycle(&adjacency)
}

/// Returns whether the directed graph in `adjacency` contains a cycle, via an
/// iterative three-colour depth-first search.
fn has_cycle(adjacency: &[BTreeSet<usize>]) -> bool {
    let count = adjacency.len();
    let mut color = vec![0_u8; count];

    for root in 0..count {
        if color.get(root).copied().unwrap_or(2) != 0 {
            continue;
        }
        let mut stack: Vec<(usize, Vec<usize>)> = Vec::new();
        let neighbours = adjacency
            .get(root)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        stack.push((root, neighbours));
        if let Some(slot) = color.get_mut(root) {
            *slot = 1;
        }

        while let Some((vertex, pending)) = stack.last_mut() {
            let Some(next) = pending.pop() else {
                if let Some(slot) = color.get_mut(*vertex) {
                    *slot = 2;
                }
                stack.pop();
                continue;
            };
            match color.get(next).copied().unwrap_or(2) {
                1 => return true,
                0 => {
                    let onward = adjacency
                        .get(next)
                        .map(|set| set.iter().copied().collect())
                        .unwrap_or_default();
                    if let Some(slot) = color.get_mut(next) {
                        *slot = 1;
                    }
                    stack.push((next, onward));
                }
                _ => {}
            }
        }
    }
    false
}
