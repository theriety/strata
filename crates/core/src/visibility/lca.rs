//! Binary-lifting lowest-common-ancestor tables over the container forest.

use strata_ir::{ContainerId, ContainerTree, ScopeLevel};

/// Binary-lifting ancestor tables over a [`ContainerTree`] forest.
///
/// `up[k][v]` is the `2^k`-th ancestor of container `v`, saturating at the root
/// (a root is its own ancestor). `depth[v]` is the root-relative depth used to
/// align two nodes before the lift. A forest is handled by giving every root a
/// self-loop, so two containers in different trees resolve their LCA to one of
/// the roots — the widest scope, which is the correct conservative answer.
pub(super) struct LcaTable {
    /// `up[k][v]`: the `2^k`-th ancestor of `v`.
    up: Vec<Vec<u32>>,
    /// Root-relative depth of each container.
    depth: Vec<u32>,
    /// Level of each container, indexed by raw id.
    level: Vec<ScopeLevel>,
    /// Number of binary-lifting tiers (`ceil(log2(max_depth)) + 1`).
    tiers: usize,
}

impl LcaTable {
    /// Builds the ancestor tables over `tree`.
    pub(super) fn build(tree: &ContainerTree) -> Self {
        let containers = tree.containers();
        let count = containers.len();

        let mut parent = vec![0_u32; count];
        let mut level = vec![ScopeLevel::File; count];
        for container in containers {
            let id = container.id.0 as usize;
            if let Some(slot) = parent.get_mut(id) {
                *slot = container.parent.map_or(container.id.0, |link| link.0);
            }
            if let Some(slot) = level.get_mut(id) {
                *slot = container.level;
            }
        }

        let depth = Self::depths(&parent);
        let max_depth = depth.iter().copied().max().unwrap_or(0);
        let tiers = usize::try_from(u32::BITS - max_depth.leading_zeros())
            .unwrap_or(1)
            .max(1);

        let mut up = vec![parent.clone()];
        for tier in 1..tiers {
            let previous = up.get(tier - 1).cloned().unwrap_or_default();
            let lifted = previous
                .iter()
                .map(|&ancestor| previous.get(ancestor as usize).copied().unwrap_or(ancestor))
                .collect();
            up.push(lifted);
        }

        Self {
            up,
            depth,
            level,
            tiers,
        }
    }

    /// Computes each container's depth from its tree root by walking parents,
    /// memoizing as it goes.
    fn depths(parent: &[u32]) -> Vec<u32> {
        let count = parent.len();
        let mut depth = vec![u32::MAX; count];

        for start in 0..count {
            // walk to a resolved ancestor or the root, recording the chain.
            let mut chain = Vec::new();
            let mut cursor = start;
            loop {
                let resolved = depth.get(cursor).copied().unwrap_or(u32::MAX);
                if resolved != u32::MAX {
                    break;
                }
                chain.push(cursor);
                let next = parent
                    .get(cursor)
                    .copied()
                    .unwrap_or_else(|| u32::try_from(cursor).unwrap_or(0))
                    as usize;
                if next == cursor {
                    // a root: depth zero, then unwind the chain above it.
                    if let Some(slot) = depth.get_mut(cursor) {
                        *slot = 0;
                    }
                    chain.pop();
                    break;
                }
                cursor = next;
            }

            let mut base = depth.get(cursor).copied().unwrap_or(0);
            while let Some(node) = chain.pop() {
                base = base.saturating_add(1);
                if let Some(slot) = depth.get_mut(node) {
                    *slot = base;
                }
            }
        }

        depth
    }

    /// Returns the level of container `id`, defaulting to [`ScopeLevel::File`]
    /// when out of range.
    pub(super) fn level(&self, id: ContainerId) -> ScopeLevel {
        self.level
            .get(id.0 as usize)
            .copied()
            .unwrap_or(ScopeLevel::File)
    }

    /// Returns the lowest common ancestor of two containers.
    ///
    /// The deeper container is lifted to the shallower's depth, then both rise in
    /// lockstep until they meet. Containers in different trees of the forest
    /// resolve to a root, the widest (most conservative) scope.
    pub(super) fn lca(&self, left: ContainerId, right: ContainerId) -> ContainerId {
        let mut a = left.0;
        let mut b = right.0;
        let depth_a = self.depth.get(a as usize).copied().unwrap_or(0);
        let depth_b = self.depth.get(b as usize).copied().unwrap_or(0);

        if depth_a < depth_b {
            b = self.lift(b, depth_b.saturating_sub(depth_a));
        } else {
            a = self.lift(a, depth_a.saturating_sub(depth_b));
        }

        if a == b {
            return ContainerId(a);
        }

        for tier in (0..self.tiers).rev() {
            let up_a = self.ancestor(a, tier);
            let up_b = self.ancestor(b, tier);
            if up_a != up_b {
                a = up_a;
                b = up_b;
            }
        }

        ContainerId(self.ancestor(a, 0))
    }

    /// Lifts container `node` up by `steps` levels via the binary-lifting tables.
    fn lift(&self, node: u32, steps: u32) -> u32 {
        let mut current = node;
        let mut remaining = steps;
        let mut tier = 0;
        while remaining > 0 {
            if remaining & 1 == 1 {
                current = self.ancestor(current, tier);
            }
            remaining >>= 1;
            tier += 1;
        }
        current
    }

    /// Returns the `2^tier`-th ancestor of `node`, saturating at `node` when the
    /// tier or id is out of range.
    fn ancestor(&self, node: u32, tier: usize) -> u32 {
        self.up
            .get(tier)
            .and_then(|table| table.get(node as usize))
            .copied()
            .unwrap_or(node)
    }
}
