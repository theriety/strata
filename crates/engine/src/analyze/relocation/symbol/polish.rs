use std::collections::{BTreeMap, BTreeSet};

use strata_core::cluster::Partition;
use strata_core::score::score;
use strata_ir::{ContainerId, ScopeLevel};

use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::{CandidateTree, PipelineSolver};
use crate::analyze::scoring::{move_distance, score_candidate};

use super::SymbolPass;
use super::inputs::{PassInputs, RelocationPolicy};
use super::outcome::SymbolOutcome;

impl PipelineSolver<'_> {
    /// Sweeps symbols over existing files while preserving every structural
    /// veto and accepting only strict objective improvement.
    #[cfg(test)]
    pub(super) fn symbol_polish(&self, parts: &Partition) -> SymbolOutcome {
        self.symbol_polish_with_polish_evidence(parts, &PolishEvidence::default())
    }

    pub(in crate::analyze::relocation) fn symbol_polish_with_polish_evidence(
        &self,
        parts: &Partition,
        evidence: &PolishEvidence,
    ) -> SymbolOutcome {
        let ir = self.snapshot.ir();
        // A faithful layout renders as the current tree (FIX05), so its symbol
        // pass runs on that tree: every structural veto, the accept test and
        // every reported delta read the tree the report shows, and the deltas
        // sum to the candidate's score minus the current score (FIX08).
        let faithful = self.is_faithful(parts);
        let assembled = self.symbol_tree(faithful, parts, evidence);
        let mut pass = SymbolPass::new_with_policy(
            PassInputs {
                snapshot: self.snapshot,
                coefficients: &self.coefficients,
                weights: &self.weights,
                same_file_symbol: self.same_file_symbol,
                same_file_type: self.same_file_type,
                capacity: self.capacity,
                assembled: &assembled,
                nodes: &ir.nodes,
                edges: &ir.edges,
            },
            RelocationPolicy {
                forbidden_sources: self.symbol_source_blocks(&assembled),
                forbidden_destinations: self.symbol_destination_blocks(&assembled),
                pin_test_polarity: self.pin_detected_test_symbols(),
                allow_cross_package: self.relocation_identity.allows_cross_package_moves(),
            },
        );
        pass.fold(&evidence.folds);
        pass.run();
        let overlay = pass.ledger.overlay;
        let relocations = pass.ledger.relocations;
        if faithful {
            return SymbolOutcome {
                total: self.score_faithful(&overlay).total,
                overlay,
                relocations,
            };
        }
        let placement = |id: u32| {
            overlay
                .get(&id)
                .copied()
                .or_else(|| assembled.placement.get(&id).copied())
        };
        let distance = move_distance(self.snapshot, &assembled.tree, &placement);
        let total = score(
            &score_candidate(
                self.snapshot,
                &placement,
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
        )
        .total;
        SymbolOutcome {
            overlay,
            relocations,
            total,
        }
    }

    /// The tree the symbol pass runs on for `parts`: the current tree for a
    /// faithful layout, otherwise the assembled candidate tree.
    pub(super) fn symbol_tree(
        &self,
        faithful: bool,
        parts: &Partition,
        evidence: &PolishEvidence,
    ) -> CandidateTree {
        if faithful {
            self.current_candidate_tree()
        } else {
            self.assemble_with_polish_evidence(parts, evidence)
        }
    }

    pub(super) fn symbol_source_blocks(&self, assembled: &CandidateTree) -> BTreeSet<ContainerId> {
        let pass_start_names: BTreeMap<ContainerId, &str> = self
            .snapshot
            .ir()
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id, container.name.as_str()))
            .collect();
        assembled
            .pass_start_file_by_candidate
            .iter()
            .filter_map(|(candidate, pass_start)| {
                let test_pinned = self.pin_detected_test_symbols()
                    && assembled
                        .zone_by_file
                        .get(candidate)
                        .copied()
                        .unwrap_or(false);
                let path_pinned = pass_start_names
                    .get(pass_start)
                    .is_some_and(|path| self.forbidden_symbol_path(path));
                (test_pinned || path_pinned).then_some(*candidate)
            })
            .collect()
    }

    fn symbol_destination_blocks(&self, assembled: &CandidateTree) -> BTreeSet<ContainerId> {
        assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .filter(|container| self.forbidden_symbol_path(&container.name))
            .map(|container| container.id)
            .collect()
    }

    fn forbidden_symbol_path(&self, path: &str) -> bool {
        self.forbidden_symbol_files
            .iter()
            .any(|pattern| pattern.matches_path(std::path::Path::new(path)))
    }

    fn pin_detected_test_symbols(&self) -> bool {
        // `pin-detected-test-symbols` pins two things: declarations inside a
        // detected test zone, and test-polarity declarations anywhere, even
        // in a production file (ADR-22). Turning it off releases both.
        self.pin_test_symbols
    }
}
