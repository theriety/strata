//! The pipeline solver that selects relocation candidates.

use strata_core::diversify::{SolvedCandidate, Solver};

use crate::analyze::relocation::PipelineSolver;
use crate::analyze::relocation::collision::Withdrawals;
use crate::analyze::relocation::mirror::PolishEvidence;

mod capacity;
mod construct;
mod file_graph;
mod polish;

pub(in crate::analyze) use file_graph::{TestPolicy, build_file_graph, test_zone_marks};

impl Solver for PipelineSolver<'_> {
    type Evidence = PolishEvidence;

    fn solve(&self, seed: u64) -> SolvedCandidate<PolishEvidence> {
        let offset = seed.wrapping_sub(self.base_seed);
        if offset == 0
            && let Some(identity) = &self.identity
        {
            return self.identity_entry(identity);
        }
        // lean: every non-identity seed converges on the same polished layout —
        // folders are reality, so the folder-level seed perturbation that used
        // to differentiate restarts is gone and the pool collapses toward
        // identity plus one improvement candidate. FIX09 re-sources diversity at
        // exactly one point: when the naming-incoherence signature fired at
        // construction, offset 1 starts from the synthesized roof rebuild and
        // runs the identical polish/score path on it, so the pool carries a
        // genuinely different shape that the objective ratifies or rejects like
        // any other. Every other offset polishes reality unchanged.
        let start = match (&self.roof_rebuild, offset) {
            (Some(rebuild), 1) => rebuild,
            _ => &self.real_partition,
        };
        let mut parts = start.clone();
        self.polish(&mut parts);
        // ADR-21: a move onto a path another file holds is withdrawn, and the
        // test followers are placed against the layout that survives; both
        // repeat until a round withdraws nothing, because a follower can change
        // a display-level election and carry another file onto an occupied
        // path. The shadow pass runs on the polished layout so a test file
        // follows the placement its subject actually earned, not the one
        // reality suggested; production placements are never moved by it.
        let mut folds = Vec::new();
        let mut withdrawals = Withdrawals::default();
        let mut polish_evidence = self.settle_collisions(&mut parts, &mut folds, &mut withdrawals);
        // FIX08: the file polish's layout is refined by the symbol-grain pass
        // before scoring, so pool ranking prices symbol relocation too. The
        // outcome itself is not threaded out — `build_candidate` re-runs this
        // pure, deterministic pass on the identical partition and gets the
        // identical overlay, so ranking score and DTO score agree by
        // construction. (The polish's own total is subsumed: the symbol pass
        // re-prices the identical layout before improving on it.)
        let mut symbols = self.symbol_polish_with_polish_evidence(&parts, &polish_evidence);
        (polish_evidence, symbols) = self.settle_refusals(
            &mut parts,
            &mut folds,
            &mut withdrawals,
            polish_evidence,
            symbols,
        );
        self.finish(parts, symbols.total, polish_evidence)
    }
}
