//! Structural polish vetoes: bridge absorption, comparable-anchor stranding and
//! flight into the synthetic bucket.

use strata_core::cluster::{ClusterId, Partition};

use crate::analyze::relocation::PipelineSolver;

impl PipelineSolver<'_> {
    /// Whether relocating `scc` from `source` into `target` would leave part of
    /// its priced neighborhood behind in some third folder — the bridge-absorption
    /// shape the polish veto bars (see the FIX05 comment at the call site). An
    /// SCC every one of whose priced edges terminates inside {source, target} is
    /// consolidating; one that also reaches elsewhere is orchestrating.
    pub(in crate::analyze) fn absorbs_a_foreign_anchor(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                if weights.get(slot).copied().unwrap_or(0.0) <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement,
                    // so it cannot anchor a bridge either.
                    continue;
                }
                let Some(cluster) = parts.cluster_of(neighbour) else {
                    continue;
                };
                if cluster != source && cluster != target {
                    return true;
                }
            }
        }
        false
    }

    /// Whether relocating `scc` from `source` into `target` would strand priced
    /// pull in `source` at least equal to what awaits in `target` — the
    /// tearing-side veto (see the FIX05 companion comment at the call site).
    /// Only strictly stronger destinations justify leaving.
    pub(in crate::analyze) fn strands_a_comparable_anchor(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        let mut stranded = 0.0_f64;
        let mut awaiting = 0.0_f64;
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                let weight = f64::from(weights.get(slot).copied().unwrap_or(0.0));
                if weight <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                match parts.cluster_of(neighbour) {
                    Some(cluster) if cluster == source => stranded += weight,
                    Some(cluster) if cluster == target => awaiting += weight,
                    _ => {}
                }
            }
        }
        awaiting <= stranded
    }

    /// Whether `target` is the synthetic `workspace` bucket and `scc` would have
    /// to abandon priced company in its own folder to get there — the
    /// bucket-flight veto (see the FIX05 third-veto comment at the call site).
    pub(in crate::analyze) fn flees_into_the_synthetic_bucket(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
        target: ClusterId,
    ) -> bool {
        if !self
            .real_folder_synthetic
            .get(target.0 as usize)
            .copied()
            .unwrap_or(false)
        {
            return false;
        }
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                if weights.get(slot).copied().unwrap_or(0.0) <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                if parts.cluster_of(neighbour) == Some(source) {
                    return true;
                }
            }
        }
        false
    }
}
