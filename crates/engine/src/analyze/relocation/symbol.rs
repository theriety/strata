use std::collections::BTreeMap;

use unlace_core::score::ScoreBreakdown;
#[cfg(test)]
use unlace_core::visibility::derive_visibility;
use unlace_ir::{ContainerId, Node, NodeKind, Polarity};

use crate::analyze::relocation::{CandidateTree, SYMBOL_MIN_IMPROVEMENT, SYMBOL_SWEEPS};
use crate::analyze::scoring::CycleCounts;
use crate::config::CapacityConfig;

mod admission;
mod exports;
mod fold;
mod guards;
mod inputs;
mod ledger;
mod narrate;
mod outcome;
mod polish;
mod pricing;
mod pull;
mod reach;
mod visibility;

#[cfg(test)]
mod tests;

use admission::Admission;
use inputs::{PassInputs, RelocationPolicy};
use ledger::Ledger;
pub(in crate::analyze) use outcome::SymbolOutcome;
#[cfg(test)]
use outcome::SymbolRelocation;
use pricing::Pricing;
use visibility::VisibilityTracker;

/// One deterministic run of the FIX08 symbol polish over an assembled tree.
///
/// A thin coordinator over four parts, each with one job: the [`Ledger`]
/// owns the trial placement and its bookkeeping, [`Pricing`] nominates and
/// prices, [`Admission`] vetoes, and the [`VisibilityTracker`] holds the
/// visibility floor. The pass itself keeps only the cycle baseline and the
/// methods that must consult several parts in one fixed order. Determinism is
/// structural: symbols sweep in ascending id order, destinations rank by
/// summed two-way priced pull with ties broken toward the lower path, the
/// best-scoring admitted destination wins, and every tie elsewhere resolves to
/// staying (ADR-34).
pub(in crate::analyze) struct SymbolPass<'a> {
    assembled: &'a CandidateTree,
    base: &'a BTreeMap<u32, ContainerId>,
    nodes: &'a [Node],
    capacity: CapacityConfig,
    /// Placement overlay, running-best score and the SLOC, occupancy and
    /// relocation ledgers.
    ledger: Ledger<'a>,
    /// Nomination, the objective and the crossing graph.
    pricing: Pricing<'a>,
    /// The structural veto family.
    admission: Admission<'a>,
    /// The working visibility copy and its baseline.
    visibility: VisibilityTracker,
    /// Cycle baseline: relocation may raise neither cyclic dimension.
    cyclic_base: CycleCounts,
}

impl<'a> SymbolPass<'a> {
    fn new_with_policy(inputs: PassInputs<'a>, policy: RelocationPolicy) -> Self {
        let PassInputs {
            assembled,
            nodes,
            capacity,
            ..
        } = inputs;
        let base = &assembled.placement;
        // FIX11: the test-zone tie-cut rides placement into symbol grain. A
        // node's zone is its placed file's mark.
        let touches_zone = |node: u32| -> bool {
            base.get(&node)
                .and_then(|file| assembled.zone_by_file.get(file))
                .copied()
                .unwrap_or(false)
        };
        let pricing = Pricing::new(inputs, &touches_zone);
        let start = pricing.score_with(&BTreeMap::new());
        let mut pass = Self {
            assembled,
            base,
            nodes,
            capacity,
            ledger: Ledger::new(inputs, start),
            pricing,
            admission: Admission::new(inputs, policy, &touches_zone),
            visibility: VisibilityTracker::new(inputs),
            cyclic_base: CycleCounts::default(),
        };
        let crossing = pass.pricing.crossing_csr(&pass.ledger);
        pass.cyclic_base = CycleCounts::from_graph(&crossing);
        pass.visibility.vis_base = pass
            .visibility
            .refresh_visibility(nodes, &pass.ledger.overlay);
        pass
    }

    #[cfg(test)]
    fn new(inputs: PassInputs<'a>) -> Self {
        Self::new_with_policy(inputs, RelocationPolicy::default())
    }

    /// Sweeps every symbol in ascending id order, at most
    /// [`SYMBOL_SWEEPS`] times, stopping early once a sweep relocates
    /// nothing.
    fn run(&mut self) {
        for _ in 0..SYMBOL_SWEEPS {
            let mut improved = false;
            for node in self.nodes {
                improved |= self.try_relocate(node);
            }
            if !improved {
                break;
            }
        }
    }

    /// Offers one symbol its strongest-pulling destinations under the full
    /// veto family and the ADR-34 gates, then moves it to the best-scoring
    /// admitted destination if that move earns its place; records an accepted
    /// relocation and returns whether the sweep made progress.
    fn try_relocate(&mut self, node: &Node) -> bool {
        let Some(source_file) = self.movable_source(node) else {
            return false;
        };
        let pull = self.pricing.pull(node, source_file);
        let mut best: Option<Trial> = None;
        for destination in self.pricing.nominate(node, &pull) {
            let Some(trial) = self.trial(node, source_file, destination, &pull) else {
                continue;
            };
            best = match best {
                None => Some(trial),
                Some(held) => Some(Trial::better(held, trial)),
            };
        }
        // A tie between the best destinations keeps the symbol home: the
        // evidence does not say where it belongs (ADR-34).
        let Some(Trial::Single {
            destination,
            terms,
            cyclic,
            visible,
        }) = best
        else {
            return false;
        };
        let before = self.ledger.best;
        if before.total - terms.total <= self.required_gain() {
            return false;
        }
        self.place_tentatively(node.id.0, destination);
        self.ledger.best = terms;
        self.cyclic_base = cyclic;
        self.visibility.vis_base = visible;
        self.ledger
            .record_relocation(node, (source_file, destination), (before, terms), None);
        true
    }

    /// The file `node` may leave this sweep, or `None` when a pin keeps it:
    /// a file body, an already-moved symbol, a pinned test or start symbol, a
    /// forbidden source, or its origin's last native resident.
    fn movable_source(&self, node: &Node) -> Option<ContainerId> {
        if node.kind == NodeKind::FileBody {
            return None;
        }
        // One move per symbol per candidate (FIX12-C): a second relocation
        // would narrate the same name twice with contradictory destinations.
        if self.ledger.moved.contains(&node.id.0) {
            return None;
        }
        // A detected test declaration stays pinned by its own polarity, not
        // only by its file's zone: a `#[cfg(test)]` helper or `#[test]` case
        // living in a production file would otherwise be free to move.
        if self.admission.policy.pin_test_polarity && node.polarity != Polarity::Production {
            return None;
        }
        if self.admission.policy.start_symbols.contains(&node.id.0) {
            return None;
        }
        let source_file = self.ledger.effective(node.id.0)?;
        if self
            .admission
            .policy
            .forbidden_sources
            .contains(&source_file)
        {
            return None;
        }
        // No empty shells: the origin keeps at least one of the production
        // symbols the assembly placed there. Counted over `native`, so an
        // arrival cannot unlock the drain (FIX12-A).
        if self.ledger.native.get(&source_file).copied().unwrap_or(0) <= 1 {
            return None;
        }
        Some(source_file)
    }

    /// Prices moving `node` to `destination` behind every veto, then undoes
    /// the trial; `None` when a veto refuses it. The static vetoes run first:
    /// structure and reach, the minority-consumer rule, no new exports and
    /// capacity. Then the trial placement must raise neither cyclic dimension
    /// nor the visibility finding count.
    fn trial(
        &mut self,
        node: &Node,
        source_file: ContainerId,
        destination: ContainerId,
        pull: &pull::Pull,
    ) -> Option<Trial> {
        if !self
            .admission
            .admits_nominated(node, source_file, destination, &self.ledger)
            || !pull.admits(destination)
            || self
                .admission
                .exports
                .exposes(node.id.0, source_file, &self.ledger)
        {
            return None;
        }
        // SLOC cap on the destination, priced in production SLOC.
        let destination_sloc = self.ledger.sloc.get(&destination).copied().unwrap_or(0);
        let moving_sloc = (node.polarity == Polarity::Production).then_some(node.effective_size);
        if let Some(size) = moving_sloc
            && destination_sloc.saturating_add(size) > self.capacity.file
        {
            return None;
        }
        let previous = self.place_tentatively(node.id.0, destination);
        let cyclic = CycleCounts::from_graph(&self.pricing.crossing_csr(&self.ledger));
        let visible = self.visibility.count_findings();
        let admitted = !cyclic.exceeds(self.cyclic_base) && visible <= self.visibility.vis_base;
        let terms = admitted.then(|| self.pricing.score_with(&self.ledger.overlay));
        self.revert_relocation(node.id.0, previous, source_file);
        Some(Trial::Single {
            destination,
            terms: terms?,
            cyclic,
            visible,
        })
    }

    /// The gain a move must beat: θ of the score it starts from (ADR-34), and
    /// never less than the float-dust floor (see [`SYMBOL_MIN_IMPROVEMENT`]).
    /// θ is relative because the mean imbalance term rescales every term with
    /// the tree (ADR-32). An unscored start (`best` not finite) keeps only the
    /// floor.
    fn required_gain(&self) -> f64 {
        let threshold = self.admission.policy.gain_threshold;
        let best = self.ledger.best.total;
        if threshold > 0.0 && best.is_finite() {
            (threshold * best.abs()).max(SYMBOL_MIN_IMPROVEMENT)
        } else {
            SYMBOL_MIN_IMPROVEMENT
        }
    }

    /// Tentatively places one symbol in `destination`: overlays it and moves its
    /// visibility home there. Returns the overlay entry it had before, for
    /// [`Self::revert_relocation`].
    fn place_tentatively(&mut self, node: u32, destination: ContainerId) -> Option<ContainerId> {
        let previous = self.ledger.overlay.insert(node, destination);
        self.visibility.set_visible(node, destination);
        previous
    }

    /// Undoes one tentative relocation: restores the overlay entry the symbol
    /// had before (`previous`) and its visibility home in `source`.
    fn revert_relocation(&mut self, node: u32, previous: Option<ContainerId>, source: ContainerId) {
        self.ledger.undo(node, previous);
        self.visibility.set_visible(node, source);
    }
}

/// One admitted destination priced for a symbol, or a tie between the best
/// ones.
enum Trial {
    /// The best admitted destination so far, with the objective breakdown,
    /// cycle counts and visibility finding count the move would leave.
    Single {
        destination: ContainerId,
        terms: ScoreBreakdown,
        cyclic: CycleCounts,
        visible: usize,
    },
    /// At least two admitted destinations score the same best total.
    Tied { total: f64 },
}

impl Trial {
    /// The objective value this trial would leave.
    fn total(&self) -> f64 {
        match self {
            Self::Single { terms, .. } => terms.total,
            Self::Tied { total } => *total,
        }
    }

    /// The better of two trials: the lower total wins; equal totals tie,
    /// within the float-dust floor.
    fn better(held: Self, challenger: Self) -> Self {
        let (held_total, challenger_total) = (held.total(), challenger.total());
        if (held_total - challenger_total).abs() <= SYMBOL_MIN_IMPROVEMENT {
            Self::Tied {
                total: held_total.min(challenger_total),
            }
        } else if challenger_total < held_total {
            challenger
        } else {
            held
        }
    }
}
