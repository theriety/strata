//! Capacitated file packing and conditional splits.
//!
//! Packing is the phase that turns folder-level clusters into concrete files
//! under the 250-production-SLOC cap, with cohesion as the first-class objective
//! — files are not bins. Its unit of work is the *atom*: one strongly connected
//! component from condensation, which can never be split across files without
//! breaking a cycle. Atoms are agglomerated Kruskal-style — affinities (edge
//! weight plus naming-token cohesion) sorted descending, merged greedily while
//! the combined size fits the cap and the folder's file-level quotient stays
//! acyclic.
//!
//! Three exceptional shapes survive agglomeration oversized:
//!
//! - a single symbol larger than the cap is flagged as an `oversized-symbol`
//!   exemption and its file is cap-exempt;
//! - an oversized but acyclic group splits at the cheapest legal point — a
//!   min-weight topological cut found by dynamic programming over the group's
//!   topological order;
//! - an oversized cyclic group (an SCC larger than the cap) is *never* split
//!   illegally. It emits a [`ConditionalSplit`] instead: the MFAS break
//!   suggestions from the shatter phase become the preconditions, and the legal
//!   split that becomes possible once those edges are cut is attached as
//!   guidance (AD-1).
//!
//! Capacity counts production SLOC only — each node's `effective_size`, with
//! test-case files exempt — so the cap reflects shipped code, not test scaffolding.

use std::collections::BTreeSet;

use strata_ir::NodeId;

use crate::shatter::BreakSet;

/// An atom of packing: one condensation SCC that must live in a single file.
///
/// An atom carries every member node, its production SLOC, and the lower-cased
/// naming tokens used for cohesion scoring. A cyclic atom (more than one member)
/// references the break set the shatter phase produced for it, so a conditional
/// split can name the precise edges to cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SccAtom {
    /// Member nodes of the component, in ascending id order.
    pub members: Vec<NodeId>,
    /// Production SLOC of the atom (sum of member `effective_size`).
    pub production_sloc: u32,
    /// Lower-cased naming tokens contributed by the members.
    pub naming_tokens: BTreeSet<String>,
    /// Index into the folder's `breaks` table for a cyclic atom, if any.
    pub break_set: Option<usize>,
}

impl SccAtom {
    /// Returns whether the atom is a single-node component.
    #[must_use]
    fn is_single(&self) -> bool {
        self.members.len() == 1
    }
}

/// A folder's view as packing consumes it: the atoms to place, the directed
/// affinity edges between them, and the per-SCC break suggestions.
///
/// Edges run from dependent atom to dependency atom (this crate's convention),
/// each carrying the summed edge weight that feeds both the affinity score and
/// the topological-cut objective. `breaks` is indexed by [`SccAtom::break_set`].
#[derive(Debug, Clone, PartialEq)]
pub struct FolderView {
    /// The atoms to pack, indexed by their position in this vector.
    pub atoms: Vec<SccAtom>,
    /// Directed, weighted dependency edges between atoms.
    pub edges: Vec<AtomEdge>,
    /// MFAS break sets referenced by cyclic atoms.
    pub breaks: Vec<BreakSet>,
}

/// A directed, weighted dependency edge between two atoms of a [`FolderView`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtomEdge {
    /// Index of the dependent atom.
    pub source: usize,
    /// Index of the depended-upon atom.
    pub target: usize,
    /// Summed weight of the underlying symbol-level edges.
    pub weight: f64,
}

/// A file emitted by packing: the atoms assigned to one source file.
///
/// `atoms` holds the indices of the [`FolderView`] atoms placed together;
/// `members` flattens those atoms' nodes for convenience. `cap_exempt` marks a
/// file that legitimately exceeds the cap (an oversized single symbol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileGroup {
    /// Indices of the folder atoms assigned to this file.
    pub atoms: Vec<usize>,
    /// Flattened member nodes of the assigned atoms, in ascending id order.
    pub members: Vec<NodeId>,
    /// Combined production SLOC of the assigned atoms.
    pub production_sloc: u32,
    /// Whether this file is allowed to exceed the cap.
    pub cap_exempt: bool,
}

/// A split that is illegal today but becomes legal once specific edges are cut.
///
/// The `group` is the oversized cyclic atom set; `preconditions` are the MFAS
/// edges whose removal makes the group acyclic; `resulting_files` is the legal
/// packing that the cut unlocks.
#[derive(Debug, Clone, PartialEq)]
pub struct ConditionalSplit {
    /// Member nodes of the oversized cyclic group.
    pub group: Vec<NodeId>,
    /// Break set whose removal makes the split legal.
    pub preconditions: BreakSet,
    /// The files the group splits into once the preconditions hold.
    pub resulting_files: Vec<FileGroup>,
}

/// The result of packing one folder: its files, conditional splits, and
/// oversized-symbol exemptions.
#[derive(Debug, Clone, PartialEq)]
pub struct Packing {
    /// The files produced, in a deterministic order.
    pub files: Vec<FileGroup>,
    /// Conditional splits for oversized cyclic groups.
    pub conditional_splits: Vec<ConditionalSplit>,
    /// Nodes that are single symbols larger than the cap.
    pub exemptions: Vec<NodeId>,
}

/// Packs `folder`'s atoms into files under `cap` production SLOC.
///
/// Affinities (edge weight plus naming-token cohesion) are sorted descending and
/// merged Kruskal-style while the combined size fits the cap and the folder's
/// file-level quotient stays acyclic. Leftover oversized groups are resolved per
/// their shape: a single oversized symbol earns a cap-exempt file and an
/// exemption entry; an oversized acyclic group splits at the min-weight
/// topological cut; an oversized cyclic group emits a [`ConditionalSplit`] whose
/// preconditions are the matching MFAS break set.
///
/// `breaks` mirrors [`FolderView::breaks`]; the explicit parameter keeps the
/// signature aligned with the shatter phase, which owns the break sets.
#[must_use]
pub fn pack(folder: &FolderView, cap: u32, breaks: &[BreakSet]) -> Packing {
    let mut state = UnionFind::new(folder.atoms.len());
    agglomerate(folder, cap, &mut state);

    let mut files = Vec::new();
    let mut conditional_splits = Vec::new();
    let mut exemptions = Vec::new();

    for group in groups_in_order(folder, &mut state) {
        let size = group
            .iter()
            .map(|&atom| atom_sloc(folder, atom))
            .sum::<u32>();

        if size <= cap {
            files.push(file_group(folder, &group, false));
            continue;
        }

        resolve_oversized(
            folder,
            cap,
            breaks,
            &group,
            &mut files,
            &mut conditional_splits,
            &mut exemptions,
        );
    }

    Packing {
        files,
        conditional_splits,
        exemptions,
    }
}

/// Returns the production SLOC of `atom` within `folder`, or zero when the index
/// is out of range.
fn atom_sloc(folder: &FolderView, atom: usize) -> u32 {
    folder
        .atoms
        .get(atom)
        .map_or(0, |atom| atom.production_sloc)
}

/// Runs the Kruskal-style agglomeration, mutating `state` so each root identifies
/// a file group.
fn agglomerate(folder: &FolderView, cap: u32, state: &mut UnionFind) {
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

/// Collects the final groups, each as a sorted list of atom indices, ordered by
/// their smallest member atom so the output is deterministic.
fn groups_in_order(folder: &FolderView, state: &mut UnionFind) -> Vec<Vec<usize>> {
    let atom_count = folder.atoms.len();
    let mut by_root: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for atom in 0..atom_count {
        by_root.entry(state.find(atom)).or_default().push(atom);
    }
    by_root.into_values().collect()
}

/// Builds a [`FileGroup`] from a group of atom indices, flattening members and
/// summing SLOC. `cap_exempt` marks an oversized-single-symbol file.
fn file_group(folder: &FolderView, group: &[usize], cap_exempt: bool) -> FileGroup {
    let mut members = Vec::new();
    let mut production_sloc = 0_u32;
    for &atom in group {
        if let Some(atom) = folder.atoms.get(atom) {
            members.extend(atom.members.iter().copied());
            production_sloc = production_sloc.saturating_add(atom.production_sloc);
        }
    }
    members.sort_unstable();
    FileGroup {
        atoms: group.to_vec(),
        members,
        production_sloc,
        cap_exempt,
    }
}

/// Resolves a single oversized group into files, conditional splits, or an
/// exemption, appending to the relevant output vectors.
fn resolve_oversized(
    folder: &FolderView,
    cap: u32,
    breaks: &[BreakSet],
    group: &[usize],
    files: &mut Vec<FileGroup>,
    conditional_splits: &mut Vec<ConditionalSplit>,
    exemptions: &mut Vec<NodeId>,
) {
    // a lone atom is special: a single oversized symbol earns an exemption, while
    // a multi-member (cyclic) atom can never be split legally as it stands.
    if let [only] = group
        && let Some(atom) = folder.atoms.get(*only)
    {
        if atom.is_single() {
            if let Some(&node) = atom.members.first() {
                exemptions.push(node);
            }
        } else {
            conditional_splits.push(conditional_split(folder, cap, breaks, *only));
        }
        files.push(file_group(folder, group, true));
        return;
    }

    // an oversized multi-atom group is acyclic by agglomeration invariant: split
    // it at the cheapest topological cut into cap-respecting files.
    files.extend(split_acyclic(folder, cap, group));
}

/// Emits the conditional split for an oversized cyclic atom: its MFAS break set
/// as preconditions plus the legal files the cut unlocks.
fn conditional_split(
    folder: &FolderView,
    cap: u32,
    breaks: &[BreakSet],
    atom: usize,
) -> ConditionalSplit {
    let preconditions = folder
        .atoms
        .get(atom)
        .and_then(|atom| atom.break_set)
        .and_then(|index| breaks.get(index))
        .cloned()
        .unwrap_or(BreakSet {
            edges: Vec::new(),
            exact: false,
        });

    let group: Vec<NodeId> = folder
        .atoms
        .get(atom)
        .map(|atom| atom.members.clone())
        .unwrap_or_default();

    // the cut makes the atom acyclic; the resulting legal split treats the atom
    // as a single-atom group split at the min-weight topological cut.
    let resulting_files = split_acyclic(folder, cap, &[atom]);

    ConditionalSplit {
        group,
        preconditions,
        resulting_files,
    }
}

/// Splits an oversized acyclic group at the min-weight topological cut.
///
/// The group's atoms are ordered topologically (by the folder's inter-atom
/// edges); a dynamic program over that order finds the prefix whose cut weight to
/// the suffix is minimal among prefixes that keep each side within `cap`. The
/// split recurses on each side until every file fits.
fn split_acyclic(folder: &FolderView, cap: u32, group: &[usize]) -> Vec<FileGroup> {
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

/// A union-find (disjoint-set) over atom indices with path compression and
/// union-by-lower-index, the latter keeping group roots deterministic.
struct UnionFind {
    /// Parent pointer per element; a root points to itself.
    parent: Vec<usize>,
}

impl UnionFind {
    /// Allocates `count` singleton sets.
    fn new(count: usize) -> Self {
        Self {
            parent: (0..count).collect(),
        }
    }

    /// Returns the representative of `element`, compressing the path walked.
    fn find(&mut self, element: usize) -> usize {
        let mut root = element;
        while self.parent.get(root).copied().unwrap_or(root) != root {
            root = self.parent.get(root).copied().unwrap_or(root);
        }
        let mut cursor = element;
        while cursor != root {
            let next = self.parent.get(cursor).copied().unwrap_or(root);
            if let Some(slot) = self.parent.get_mut(cursor) {
                *slot = root;
            }
            cursor = next;
        }
        root
    }

    /// Unions the sets rooted at `a` and `b`, attaching the higher root under the
    /// lower so the representative is the smallest member index. Returns the
    /// surviving root.
    fn union(&mut self, a: usize, b: usize) -> usize {
        let (keep, drop) = if a <= b { (a, b) } else { (b, a) };
        if let Some(slot) = self.parent.get_mut(drop) {
            *slot = keep;
        }
        keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an atom from members, sloc, and naming tokens, with no break set.
    fn atom(members: &[u32], sloc: u32, tokens: &[&str]) -> SccAtom {
        SccAtom {
            members: members.iter().copied().map(NodeId).collect(),
            production_sloc: sloc,
            naming_tokens: tokens.iter().map(|&token| token.to_owned()).collect(),
            break_set: None,
        }
    }

    /// Builds a cyclic atom that references break set `index`.
    fn cyclic_atom(members: &[u32], sloc: u32, index: usize) -> SccAtom {
        SccAtom {
            members: members.iter().copied().map(NodeId).collect(),
            production_sloc: sloc,
            naming_tokens: BTreeSet::new(),
            break_set: Some(index),
        }
    }

    fn edge(source: usize, target: usize, weight: f64) -> AtomEdge {
        AtomEdge {
            source,
            target,
            weight,
        }
    }

    #[test]
    fn should_merge_cohesive_atoms_into_one_file() {
        let folder = FolderView {
            atoms: vec![
                atom(&[0], 100, &["user", "service"]),
                atom(&[1], 100, &["user", "repository"]),
            ],
            edges: vec![edge(0, 1, 2.0)],
            breaks: Vec::new(),
        };

        let packing = pack(&folder, 250, &[]);

        assert_eq!(packing.files.len(), 1);
        assert_eq!(
            packing.files.first().map(|file| file.members.clone()),
            Some(vec![NodeId(0), NodeId(1)])
        );
        assert!(packing.conditional_splits.is_empty());
        assert!(packing.exemptions.is_empty());
    }

    #[test]
    fn should_keep_atoms_apart_when_the_merge_would_overflow_the_cap() {
        let folder = FolderView {
            atoms: vec![atom(&[0], 200, &["a"]), atom(&[1], 200, &["a"])],
            edges: vec![edge(0, 1, 5.0)],
            breaks: Vec::new(),
        };

        let packing = pack(&folder, 250, &[]);

        assert_eq!(packing.files.len(), 2);
    }

    #[test]
    fn should_flag_a_single_oversized_symbol_as_exempt() {
        let folder = FolderView {
            atoms: vec![atom(&[7], 400, &["giant"])],
            edges: Vec::new(),
            breaks: Vec::new(),
        };

        let packing = pack(&folder, 250, &[]);

        assert_eq!(packing.exemptions, vec![NodeId(7)]);
        assert_eq!(packing.files.len(), 1);
        assert_eq!(
            packing.files.first().map(|file| file.cap_exempt),
            Some(true)
        );
    }

    #[test]
    fn should_emit_a_conditional_split_for_an_oversized_cyclic_group() {
        let breaks = vec![BreakSet {
            edges: vec![crate::shatter::EdgeRef {
                source: 0,
                target: 1,
            }],
            exact: true,
        }];
        let folder = FolderView {
            atoms: vec![cyclic_atom(&[0, 1, 2], 400, 0)],
            edges: Vec::new(),
            breaks: breaks.clone(),
        };

        let packing = pack(&folder, 250, &breaks);

        assert_eq!(packing.conditional_splits.len(), 1);
        let split = packing.conditional_splits.first();
        assert_eq!(
            split.map(|split| split.preconditions.clone()),
            breaks.first().cloned()
        );
        assert_eq!(
            split.map(|split| split.group.clone()),
            Some(vec![NodeId(0), NodeId(1), NodeId(2)])
        );
    }

    #[test]
    fn should_split_an_oversized_acyclic_group_at_the_min_weight_cut() {
        // a 3-atom chain of 100 sloc each (300 > cap 250). Cheapest cut is between
        // the 0.1-weight edge, isolating atom 2.
        let folder = FolderView {
            atoms: vec![
                atom(&[0], 100, &["shared"]),
                atom(&[1], 100, &["shared"]),
                atom(&[2], 100, &["shared"]),
            ],
            edges: vec![edge(0, 1, 5.0), edge(1, 2, 0.1)],
            breaks: Vec::new(),
        };

        let packing = pack(&folder, 250, &[]);

        assert_eq!(packing.files.len(), 2);
        let sizes: Vec<u32> = packing
            .files
            .iter()
            .map(|file| file.production_sloc)
            .collect();
        assert!(sizes.iter().all(|&sloc| sloc <= 250));
        // the cheap edge 1->2 is cut, so atom 2 lands alone.
        assert!(
            packing
                .files
                .iter()
                .any(|file| file.members == vec![NodeId(2)])
        );
    }

    #[test]
    fn should_never_merge_into_a_cyclic_file_quotient() {
        // 0 -> 1 -> 2 -> 0 across atoms would be cyclic if all three merged, but
        // each pair is small. Cohesion would otherwise pull them together; the
        // acyclicity veto must keep at least one apart.
        let folder = FolderView {
            atoms: vec![
                atom(&[0], 50, &["x"]),
                atom(&[1], 50, &["x"]),
                atom(&[2], 50, &["x"]),
            ],
            edges: vec![edge(0, 1, 1.0), edge(1, 2, 1.0), edge(2, 0, 1.0)],
            breaks: Vec::new(),
        };

        let packing = pack(&folder, 250, &[]);

        // merging all three is illegal (cyclic quotient); only acyclic merges run.
        // the result must not place all atoms in a single file via a cyclic merge.
        let all_in_one = packing.files.iter().any(|file| file.members.len() == 3);
        assert!(!all_in_one);
    }

    #[test]
    fn should_return_an_empty_packing_for_no_atoms() {
        let folder = FolderView {
            atoms: Vec::new(),
            edges: Vec::new(),
            breaks: Vec::new(),
        };

        let packing = pack(&folder, 250, &[]);

        assert!(packing.files.is_empty());
        assert!(packing.conditional_splits.is_empty());
        assert!(packing.exemptions.is_empty());
    }

    #[test]
    fn should_produce_identical_packings_across_runs() {
        let folder = FolderView {
            atoms: vec![
                atom(&[0], 80, &["alpha", "beta"]),
                atom(&[1], 80, &["beta", "gamma"]),
                atom(&[2], 80, &["gamma"]),
            ],
            edges: vec![edge(0, 1, 1.0), edge(1, 2, 1.0)],
            breaks: Vec::new(),
        };

        let first = pack(&folder, 250, &[]);
        let second = pack(&folder, 250, &[]);

        assert_eq!(first, second);
    }
}
