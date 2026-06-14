//! Multilevel acyclic clustering (dagP-style) over the layered condensation.
//!
//! Clustering groups condensation nodes into the nested container levels
//! (folders → domains → packages → package groups) while every level stays a
//! DAG and respects its member cap. The scheme has three phases, each in its own
//! submodule:
//!
//! - [`coarsen`] shrinks the DAG through acyclicity-preserving heavy-edge
//!   matching, producing a chain of progressively smaller graphs;
//! - [`seed`] grows an initial partition at the top of the chain by walking nodes
//!   in topological order, so the seed quotient is acyclic by construction;
//! - [`refine`] applies Fiduccia–Mattheyses single-node moves while uncoarsening,
//!   vetoing any move that would make the quotient cyclic or overflow a cap.
//!
//! Acyclicity is an inviolable veto, never a penalty: no phase ever produces a
//! cyclic quotient. Cohesion bonuses (naming-token Jaccard, path similarity) live
//! inside the [`refine`] gain function, mode-dependent via its α and β
//! coefficients.

pub mod coarsen;
pub mod refine;
pub mod seed;

/// Identifier of a cluster within a [`Partition`], dense over `0..cluster_count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClusterId(pub u32);

/// Per-level member caps, indexed by the container level a clustering pass
/// targets (folder → domain → package → package group).
///
/// A cap bounds how many child members a single container at that level may
/// hold; refinement vetoes any move that would push a target cluster over its
/// cap. The defaults mirror FR-1 / the `[caps]` config block (15 / 12 / 10 / 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelCaps {
    /// Maximum members per folder.
    pub folder: u32,
    /// Maximum members per domain.
    pub domain: u32,
    /// Maximum members per package.
    pub package: u32,
    /// Maximum members per package group.
    pub package_group: u32,
}

impl LevelCaps {
    /// The default caps (15 folder / 12 domain / 10 package / 12 package group),
    /// matching a config-less run.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            folder: 15,
            domain: 12,
            package: 10,
            package_group: 12,
        }
    }
}

impl Default for LevelCaps {
    fn default() -> Self {
        Self::defaults()
    }
}

/// An assignment of every graph node to a cluster, plus the per-cluster member
/// counts kept in step with it.
///
/// Indices are dense: `assignment[v]` is node `v`'s cluster, and `sizes[c]` is
/// the live member count of cluster `c`. The two stay synchronised through
/// [`Partition::move_node`], so a capacity veto is a single `sizes` lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// Owning cluster of each node, indexed by node id.
    assignment: Vec<ClusterId>,
    /// Live member count of each cluster, indexed by cluster id.
    sizes: Vec<u32>,
}

impl Partition {
    /// Builds a partition from a per-node cluster assignment.
    ///
    /// `cluster_count` is the number of clusters the assignment spans; member
    /// counts are derived in one pass. Assignments that name a cluster at or
    /// above `cluster_count` are ignored for sizing (they cannot pass the
    /// bounds check in [`Self::cluster_of`]) so a malformed input degrades
    /// gracefully rather than panicking.
    #[must_use]
    pub fn from_assignment(assignment: Vec<ClusterId>, cluster_count: usize) -> Self {
        let mut sizes = vec![0_u32; cluster_count];
        for cluster in &assignment {
            if let Some(slot) = sizes.get_mut(cluster.0 as usize) {
                *slot += 1;
            }
        }
        Self { assignment, sizes }
    }

    /// Returns the number of nodes the partition assigns.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.assignment.len()
    }

    /// Returns the number of clusters.
    #[must_use]
    pub fn cluster_count(&self) -> usize {
        self.sizes.len()
    }

    /// Returns the cluster owning `node`, or `None` when `node` is out of range.
    #[must_use]
    pub fn cluster_of(&self, node: u32) -> Option<ClusterId> {
        self.assignment.get(node as usize).copied()
    }

    /// Returns the live member count of cluster `cluster`, or `0` when out of
    /// range.
    #[must_use]
    pub fn size_of(&self, cluster: ClusterId) -> u32 {
        self.sizes.get(cluster.0 as usize).copied().unwrap_or(0)
    }

    /// Returns the per-node cluster assignment.
    #[must_use]
    pub fn assignment(&self) -> &[ClusterId] {
        &self.assignment
    }

    /// Moves `node` into `target`, updating both source and target member counts.
    ///
    /// A no-op when `node` is out of range or already in `target`. Returns `true`
    /// when the assignment changed.
    pub fn move_node(&mut self, node: u32, target: ClusterId) -> bool {
        let Some(current) = self.assignment.get(node as usize).copied() else {
            return false;
        };
        if current == target {
            return false;
        }
        if let Some(slot) = self.sizes.get_mut(current.0 as usize) {
            *slot = slot.saturating_sub(1);
        }
        if let Some(slot) = self.sizes.get_mut(target.0 as usize) {
            *slot += 1;
        }
        if let Some(slot) = self.assignment.get_mut(node as usize) {
            *slot = target;
        }
        true
    }
}
