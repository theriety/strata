//! Disjoint-set structure backing packing's agglomeration.

/// A union-find (disjoint-set) over atom indices with path compression and
/// union-by-lower-index, the latter keeping group roots deterministic.
pub(super) struct UnionFind {
    /// Parent pointer per element; a root points to itself.
    parent: Vec<usize>,
}

impl UnionFind {
    /// Allocates `count` singleton sets.
    pub(super) fn new(count: usize) -> Self {
        Self {
            parent: (0..count).collect(),
        }
    }

    /// Returns the representative of `element`, compressing the path walked.
    pub(super) fn find(&mut self, element: usize) -> usize {
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
    pub(super) fn union(&mut self, a: usize, b: usize) -> usize {
        let (keep, drop) = if a <= b { (a, b) } else { (b, a) };
        if let Some(slot) = self.parent.get_mut(drop) {
            *slot = keep;
        }
        keep
    }
}
