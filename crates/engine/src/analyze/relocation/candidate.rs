//! Builds the reported candidate for a solved partition.

use std::collections::BTreeMap;

use strata_core::cluster::Partition;
use strata_core::diversify::SolvedCandidate;
use strata_core::score::score;
use strata_ir::{ContainerId, ContainerTree, Node};

use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::{CandidateTree, PipelineSolver, symbol};
use crate::analyze::rendering::render_tree;
use crate::analyze::scoring::{
    move_distance, score_candidate, score_current_with_affinity, score_current_with_overlay,
};
use crate::error::StrataError;
use crate::narrate::narrate_repository_relative;
use crate::result::{Candidate, ConditionalSplit, ScoreBreakdown};

impl PipelineSolver<'_> {
    /// Attaches solver-owned follower outcomes to their primary source moves.
    pub(super) fn finish(
        &self,
        parts: Partition,
        total: f64,
        evidence: PolishEvidence,
    ) -> SolvedCandidate<PolishEvidence> {
        if let Some(identity) = &self.identity
            && identity == &parts
        {
            // A faithful partition is priced on the current tree with its
            // symbol overlay applied (`symbol_polish_with_polish_evidence`), so
            // an improving symbol polish survives as an offer; without one it
            // collapses onto the "change nothing" identity entry.
            let entry = self.identity_entry(identity);
            if total >= entry.score {
                return entry;
            }
        }
        SolvedCandidate {
            partition: parts,
            score: total,
            evidence,
        }
    }

    /// The identity pool entry: the "change nothing" layout scored on the actual
    /// current tree, so the anchored pool always contains the current score and
    /// a suggested candidate can never silently lose to it.
    pub(super) fn identity_entry(&self, identity: &Partition) -> SolvedCandidate<PolishEvidence> {
        SolvedCandidate {
            partition: identity.clone(),
            score: score_current_with_affinity(
                self.snapshot,
                &self.coefficients,
                &self.weights,
                &self.capacity,
                self.same_file_symbol,
                self.same_file_type,
            )
            .total,
            evidence: PolishEvidence::default(),
        }
    }

    /// Whether a solved partition is the current layout itself (FIX05).
    ///
    /// The faithful exit keys on the layout being reality, not on the analysis
    /// mode. Gating it on `identity` alone (anchored-only) meant a greenfield
    /// candidate whose partition is byte-for-byte the real directory layout
    /// still went through assemble() — which re-elects upper-level containers
    /// and nests them differently from the current tree — so "change nothing"
    /// rendered as structural moves for every file whose fabricated chain
    /// differed. When relief split an over-capacity folder, the search's start
    /// is no longer reality, so the assemble path stays (the split is exactly
    /// what the proposal must show).
    pub(super) fn is_faithful(&self, partition: &Partition) -> bool {
        self.identity.as_ref() == Some(partition)
            || (self.real_is_identity && &self.real_partition == partition)
    }

    /// The current tree as a [`CandidateTree`]: every file keeps its current
    /// container id and every declaration its current file. A faithful layout
    /// renders as exactly this tree, so its symbol pass runs here, with every
    /// structural veto and every price read from the tree the report shows.
    pub(in crate::analyze) fn current_candidate_tree(&self) -> CandidateTree {
        let ir = self.snapshot.ir();
        let mut pass_start_file_by_candidate = BTreeMap::new();
        let mut zone_by_file = BTreeMap::new();
        let mut namespace_by_file = BTreeMap::new();
        let mut package_by_file = BTreeMap::new();
        for (vertex, file) in self.files.iter().enumerate() {
            let id = ContainerId(file.container);
            pass_start_file_by_candidate.insert(id, id);
            zone_by_file.insert(id, self.test_zone.get(vertex).copied().unwrap_or(false));
            namespace_by_file.insert(id, file.namespace.clone());
            package_by_file.insert(id, file.home.package.clone());
        }
        let placement = ir
            .nodes
            .iter()
            .filter(|node| pass_start_file_by_candidate.contains_key(&node.container))
            .map(|node| (node.id.0, node.container))
            .collect();
        CandidateTree {
            tree: ir.containers.clone(),
            placement,
            pass_start_file_by_candidate,
            zone_by_file,
            namespace_by_file,
            package_by_file,
            key_by_id: BTreeMap::new(),
        }
    }

    /// Prices the current tree with `overlay` (current file ids) applied.
    pub(super) fn score_faithful(
        &self,
        overlay: &BTreeMap<u32, ContainerId>,
    ) -> strata_core::score::ScoreBreakdown {
        score_current_with_overlay(
            self.snapshot,
            &self.coefficients,
            &self.weights,
            &self.capacity,
            self.same_file_symbol,
            self.same_file_type,
            overlay,
        )
    }

    /// Builds the candidate for a faithful layout: the current tree verbatim,
    /// no file moves, and whatever symbol relocations the symbol polish
    /// accepted on it, priced on the current tree. With no accepted symbol
    /// move this is "change nothing" at exactly the current score.
    fn build_faithful_candidate(
        &self,
        current_tree: &ContainerTree,
        solved: &SolvedCandidate<PolishEvidence>,
        index: u32,
        splits: &[ConditionalSplit],
    ) -> Result<Candidate, StrataError> {
        let nodes = &self.snapshot.ir().nodes;
        let current = self.current_candidate_tree();
        let symbols = self.symbol_polish_with_polish_evidence(&solved.partition, &solved.evidence);
        ensure_namespaces_preserved(&symbols, &current)?;
        let overlay = &symbols.overlay;
        let breakdown = self.score_faithful(overlay);
        let node = render_tree(
            current_tree,
            nodes,
            &|node: &Node| Some(overlay.get(&node.id.0).copied().unwrap_or(node.container)),
            &BTreeMap::new(),
        )?;
        Ok(Candidate {
            index,
            score: breakdown.total,
            score_breakdown: ScoreBreakdown::from(breakdown),
            improvement: 0.0,
            tree: node,
            conditional_splits: splits.to_vec(),
            delta_narration: Vec::new(),
            symbol_moves: self.symbol_narrate(&current, &symbols),
            capacity_remainder: None,
        })
    }

    /// Builds one DTO [`Candidate`] from a solved partition.
    ///
    /// A faithful partition keeps the current tree verbatim — zero file moves,
    /// never re-derived through assembly — and carries only its accepted
    /// symbol relocations, priced on that tree exactly as ranking priced them.
    /// Every other partition is assembled, rescored under this mode's
    /// coefficients, and narrated against the current layout.
    ///
    /// # Errors
    ///
    /// Returns [`StrataError::SnapshotInvalid`] if the tree cannot be rendered.
    pub(in crate::analyze) fn build_candidate(
        &self,
        current_tree: &ContainerTree,
        solved: &SolvedCandidate<PolishEvidence>,
        index: u32,
        splits: &[ConditionalSplit],
    ) -> Result<Candidate, StrataError> {
        let nodes = &self.snapshot.ir().nodes;
        if self.is_faithful(&solved.partition) {
            return self.build_faithful_candidate(current_tree, solved, index, splits);
        }

        let assembled = self.assemble_with_polish_evidence(&solved.partition, &solved.evidence);
        // FIX08: re-run the deterministic symbol pass on this exact partition.
        // `solve` already priced its result into the ranking score, so the DTO
        // score here matches what ranked this candidate by construction.
        let symbols = self.symbol_polish_with_polish_evidence(&solved.partition, &solved.evidence);
        ensure_namespaces_preserved(&symbols, &assembled)?;
        let merged = |id: u32| {
            symbols
                .overlay
                .get(&id)
                .copied()
                .or_else(|| assembled.placement.get(&id).copied())
        };
        let distance = move_distance(self.snapshot, &assembled.tree, &merged);
        let breakdown = score(
            &score_candidate(
                self.snapshot,
                &merged,
                &assembled.pass_start_file_by_candidate,
                &assembled.tree,
                &assembled.namespace_by_file,
                distance,
                &self.capacity,
                self.same_file_symbol,
                self.same_file_type,
            ),
            &self.coefficients,
            &self.weights,
        );
        let placement_of = |node: &Node| merged(node.id.0);
        let node = render_tree(&assembled.tree, nodes, &placement_of, &assembled.key_by_id)?;
        let mut delta = narrate_repository_relative(current_tree, &assembled.tree, &self.facts);
        Self::attach_test_mirrors(&mut delta, &solved.evidence);
        let symbol_moves = self.symbol_narrate(&assembled, &symbols);

        Ok(Candidate {
            index,
            score: breakdown.total,
            score_breakdown: ScoreBreakdown::from(breakdown),
            improvement: 0.0,
            tree: node,
            conditional_splits: splits.to_vec(),
            delta_narration: delta,
            symbol_moves,
            capacity_remainder: None,
        })
    }
}

/// Fails when the symbol pass moved a declaration across a pass-start render
/// namespace of `tree`.
fn ensure_namespaces_preserved(
    symbols: &symbol::SymbolOutcome,
    tree: &CandidateTree,
) -> Result<(), StrataError> {
    if symbols.preserves_namespaces(tree) {
        return Ok(());
    }
    Err(StrataError::SnapshotInvalid {
        source: strata_ir::SnapshotError::Serialization {
            reason: "symbol relocation crossed a pass-start render namespace".to_owned(),
        },
    })
}
