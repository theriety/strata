//! Withdrawal of one colliding file move (ADR-21).

use strata_core::cluster::{ClusterId, Partition};

use crate::analyze::relocation::PipelineSolver;

impl PipelineSolver<'_> {
    /// Withdraws one colliding move (see [`Self::fold_path_collisions`]);
    /// `before` is the round's partition. Returns the newcomers evicted, each
    /// with the cluster it left.
    pub(super) fn withdraw(
        &self,
        before: &Partition,
        parts: &mut Partition,
        mover: usize,
        occupant: usize,
        offered: bool,
    ) -> Vec<(u32, ClusterId)> {
        let scc_of = |vertex: usize| self.condensation.membership.get(vertex).map(|scc| scc.0);
        let (Some(scc), Some(occupant_scc)) = (scc_of(mover), scc_of(occupant)) else {
            return Vec::new();
        };
        let (Some(home), Some(current), Some(landing)) = (
            self.pass_start_partition.cluster_of(scc),
            before.cluster_of(scc),
            before.cluster_of(occupant_scc),
        ) else {
            return Vec::new();
        };
        if current != home {
            let _moved = parts.move_node(scc, home);
            return Vec::new();
        }
        if !offered {
            return Vec::new();
        }
        self.evict_newcomers(before, parts, home, landing)
    }
}
