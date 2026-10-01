//! Objective evaluation and the J(T)-polish pass.

use std::collections::BTreeMap;

use strata_core::cluster::{ClusterId, Partition};
use strata_core::score::score;

#[cfg(test)]
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::{CandidateTree, POLISH_SWEEPS, POLISH_TARGETS, PipelineSolver};
use crate::analyze::scoring::{CycleCounts, move_distance, score_candidate};

impl PipelineSolver<'_> {
    /// Scores the five-level layout `parts` induces under this mode's
    /// coefficients.
    pub(in crate::analyze) fn evaluate(&self, parts: &Partition) -> f64 {
        let assembled = self.assemble(parts);
        self.evaluate_assembled(&assembled)
    }

    #[cfg(test)]
    pub(in crate::analyze) fn evaluate_with_polish_evidence(
        &self,
        parts: &Partition,
        evidence: &PolishEvidence,
    ) -> f64 {
        let assembled = self.assemble_with_polish_evidence(parts, evidence);
        self.evaluate_assembled(&assembled)
    }

    fn evaluate_assembled(&self, assembled: &CandidateTree) -> f64 {
        let placement = |id: u32| assembled.placement.get(&id).copied();
        // the assembly's placement maps every node to its current file's
        // candidate id, so this distance is pure file-grain movement.
        let distance = move_distance(self.snapshot, &assembled.tree, &placement);
        let candidate = score_candidate(
            self.snapshot,
            &placement,
            &assembled.pass_start_file_by_candidate,
            &assembled.tree,
            &assembled.namespace_by_file,
            distance,
            &self.capacity,
            self.same_file_symbol,
            self.same_file_type,
        );
        score(&candidate, &self.coefficients, &self.weights).total
    }

    /// The J(T)-polish pass: sweeps every file SCC in deterministic order and
    /// greedily relocates it to the strongest-pulling folder whenever the move
    /// strictly lowers the full objective. Physical immediate-entry capacity
    /// (direct files plus direct child directories) and quotient cyclicity stay
    /// hard vetoes, never penalties — but the cyclicity veto is relative, not
    /// absolute: a move is barred when it *grows* either the number of folders
    /// caught in quotient cycles or the edges held inside those cycles, never
    /// for cyclicity the current layout already has. Misplaced files routinely
    /// entangle real folder graphs in cycles no single move can dissolve; an
    /// absolute veto would price every move at infinity on such a base and
    /// freeze the pass wholesale. On an acyclic base the two vetoes agree.
    /// At most [`POLISH_SWEEPS`] passes, stopping early once a sweep applies
    /// no move. Returns the final score so `solve` never re-evaluates.
    pub(in crate::analyze) fn polish(&self, parts: &mut Partition) -> f64 {
        let mut best = self.evaluate(parts);
        let initial_quotient = parts.quotient(&self.condensation.dag);
        let mut cyclic_base = CycleCounts::from_graph(&initial_quotient);
        for _ in 0..POLISH_SWEEPS {
            let mut improved = false;
            for scc in 0..self.condensation.members.len() {
                let scc32 = u32::try_from(scc).unwrap_or(u32::MAX);
                if self.pinned_scc.get(scc).copied().unwrap_or(false) {
                    continue;
                }
                let Some(source) = parts.cluster_of(scc32) else {
                    continue;
                };
                for target in self.pull_targets(parts, scc32, source) {
                    if !self.relocation_identity.permits_join(parts, scc32, target) {
                        continue;
                    }
                    // FIX05 (WS-D anchored-inversion): a bridge is not a member of
                    // the thing it bridges. When an SCC's priced edges reach a
                    // folder besides the pair (current, target) — main.py importing
                    // three features, pipeline.py bridging billing and telemetry —
                    // absorbing it into one side strands the rest of its boundary,
                    // yet every locally-scored statistic of the absorber improves:
                    // the adopted edges drop to folder height while the abandoned
                    // ones keep whatever height they already had. The objective
                    // alone therefore ratifies the absorption and greenfield
                    // out-churns anchored, inverting the product promise (the
                    // eval corpus's inversion witness). The veto is structural,
                    // not scored: it fires before evaluation, needs no reference
                    // to the current layout, and so binds both modes equally —
                    // the FIX04 pattern of enforcing contract intent where
                    // admission cannot see it. Zero-priced edges nominate nothing
                    // here either: they never bind placement.
                    if self.absorbs_a_foreign_anchor(parts, scc32, source, target) {
                        continue;
                    }
                    // FIX05 (companion veto): the mirror image of bridge
                    // absorption. An SCC whose current folder pulls at least as
                    // hard as the destination is being torn from measured
                    // company for speculative proximity — the channel that
                    // survived the bridge veto: with the facade unabsorbable,
                    // greedy polish instead walked the feature members out of
                    // their real directories toward it, one transiently cheap
                    // step at a time. Relocation is honest only when the
                    // destination out-pulls what would be stranded (the
                    // satellite joining its sole anchor); a tie resolves to
                    // staying, because folders are reality until priced
                    // evidence says otherwise.
                    if self.strands_a_comparable_anchor(parts, scc32, source, target) {
                        continue;
                    }
                    // FIX05 (third veto): the synthetic bucket is not a place.
                    // `workspace` is the fallback name for files whose real
                    // directory is the project root — an absence of structure,
                    // not a structure. Once the first two vetoes sealed the
                    // feature folders, greedy polish found the remaining exit:
                    // feature members fleeing their real directories INTO the
                    // bucket, because sitting beside the unabsorbable hub
                    // cheapens their hub edges while the bucket prices nothing
                    // back. That flight is the collapse defect itself (real
                    // directories swallowed by an invented container). A file
                    // with priced company in its own folder therefore may not
                    // relocate into the bucket at all; only files reality left
                    // loose belong there, and they are already home.
                    if self.flees_into_the_synthetic_bucket(parts, scc32, source, target) {
                        continue;
                    }
                    if !self.permits_physical_capacity(parts, scc32, target) {
                        continue;
                    }
                    if !parts.move_node(scc32, target) {
                        continue;
                    }
                    let tentative_quotient = parts.quotient(&self.condensation.dag);
                    let cyclic_now = CycleCounts::from_graph(&tentative_quotient);
                    let total = if cyclic_now.exceeds(cyclic_base) {
                        f64::INFINITY
                    } else {
                        self.evaluate(parts)
                    };
                    if total < best {
                        best = total;
                        cyclic_base = cyclic_now;
                        improved = true;
                        break;
                    }
                    parts.move_node(scc32, source);
                }
            }
            if !improved {
                break;
            }
        }
        best
    }

    /// Ranks the folders pulling hardest on `scc` — summed edge weight over both
    /// directions — and returns up to [`POLISH_TARGETS`] of them, strongest
    /// first, ties broken by the lower cluster id. Zero-priced edges nominate
    /// nothing: they never bind placement (the FIX04 doctrine), so a folder
    /// connected only through re-exports is never offered as a move target.
    pub(in crate::analyze) fn pull_targets(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
    ) -> Vec<ClusterId> {
        let mut pull: BTreeMap<ClusterId, f64> = BTreeMap::new();
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                let Some(cluster) = parts.cluster_of(neighbour) else {
                    continue;
                };
                if cluster == source {
                    continue;
                }
                let weight = weights.get(slot).copied().unwrap_or(0.0);
                if weight <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                *pull.entry(cluster).or_insert(0.0) += f64::from(weight);
            }
        }
        let mut ranked: Vec<(ClusterId, f64)> = pull.into_iter().collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.cmp(&right.0))
        });
        ranked
            .into_iter()
            .take(POLISH_TARGETS)
            .map(|(cluster, _)| cluster)
            .collect()
    }
}
