//! The collision-fold entry of the symbol pass: moving a retired file's
//! declarations into the file that collided with it as one unit.

use std::collections::BTreeMap;

use strata_ir::{ContainerId, Node, NodeKind, Polarity};

use super::SymbolPass;
use crate::analyze::relocation::SYMBOL_MIN_IMPROVEMENT;
use crate::analyze::relocation::collision::CollisionFold;
use crate::analyze::scoring::CycleCounts;

impl SymbolPass<'_> {
    /// Offers each collision fold (ADR-21) before the ordinary sweep. A fold
    /// the collision pass marked as not offered (its file is a module root,
    /// or an earlier round refused it) only pins its file and is skipped here.
    pub(super) fn fold(&mut self, folds: &[CollisionFold]) {
        if folds.is_empty() {
            return;
        }
        let candidate_of: BTreeMap<u32, ContainerId> = self
            .assembled
            .pass_start_file_by_candidate
            .iter()
            .map(|(candidate, pass_start)| (pass_start.0, *candidate))
            .collect();
        for fold in folds.iter().filter(|fold| fold.offered) {
            if let (Some(&source), Some(&destination)) =
                (candidate_of.get(&fold.file), candidate_of.get(&fold.into))
            {
                self.try_fold(ContainerId(fold.file), source, destination);
            }
        }
    }

    /// Moves every movable declaration the assembly placed in `source` into
    /// `destination` as one unit, or none of them; `file` is the source's
    /// pass-start id, recorded on each relocation so the solver can tell an
    /// accepted fold from a refused one.
    ///
    /// A fold replaces a file move that would have overwritten `destination`.
    /// Its members are the file's declarations other than the executable file
    /// body and, under the polarity pin, its test-polarity declarations: those
    /// stay in the source file and do not refuse the unit. Each member must
    /// pass every veto an ordinary relocation obeys, the reach guards included
    /// ([`Admission::admits_nominated`]); the unit must fit the destination's
    /// SLOC cap, raise neither cycle dimension nor the visibility finding
    /// count, and improve the objective by more than
    /// [`SYMBOL_MIN_IMPROVEMENT`]. A fold may drain the source of every
    /// production declaration: the file is being retired, and the collision
    /// pass never offers a module root.
    fn try_fold(
        &mut self,
        file: ContainerId,
        source: ContainerId,
        destination: ContainerId,
    ) -> bool {
        let nodes = self.nodes;
        let members: Vec<&Node> = nodes
            .iter()
            .filter(|node| node.kind != NodeKind::FileBody)
            .filter(|node| self.base.get(&node.id.0) == Some(&source))
            .filter(|node| {
                !(self.admission.policy.pin_test_polarity && node.polarity != Polarity::Production)
            })
            .collect();
        let admissible = !members.is_empty()
            && !self.admission.policy.forbidden_sources.contains(&source)
            && members.iter().all(|node| {
                let taken = self.ledger.moved.contains(&node.id.0)
                    || self.ledger.overlay.contains_key(&node.id.0);
                !taken
                    && self
                        .admission
                        .admits_nominated(node, source, destination, &self.ledger)
            });
        let arriving: u32 = members
            .iter()
            .filter(|node| node.polarity == Polarity::Production)
            .map(|node| node.effective_size)
            .sum();
        if !admissible
            || self
                .ledger
                .sloc
                .get(&destination)
                .copied()
                .unwrap_or(0)
                .saturating_add(arriving)
                > self.capacity.file
        {
            return false;
        }

        let before = self.ledger.best;
        let mut running = before;
        let mut priced = Vec::with_capacity(members.len());
        for node in &members {
            self.place_tentatively(node.id.0, destination);
            let total = self.pricing.score_with(&self.ledger.overlay);
            priced.push((*node, running - total));
            running = total;
        }
        let cyclic_now = CycleCounts::from_graph(&self.pricing.crossing_csr(&self.ledger));
        let vis_now = self.visibility.count_findings();
        if cyclic_now.exceeds(self.cyclic_base)
            || vis_now > self.visibility.vis_base
            || before - running <= SYMBOL_MIN_IMPROVEMENT
        {
            for node in &members {
                self.revert_relocation(node.id.0, None, source);
            }
            return false;
        }

        self.ledger.best = running;
        self.cyclic_base = cyclic_now;
        self.visibility.vis_base = vis_now;
        for (node, delta) in priced {
            self.ledger
                .record_relocation(node, source, destination, delta, Some(file));
        }
        true
    }
}
