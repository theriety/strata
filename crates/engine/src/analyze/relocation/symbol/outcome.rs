use std::collections::{BTreeMap, BTreeSet};

use strata_ir::ContainerId;

use crate::analyze::relocation::CandidateTree;

/// One symbol relocation the FIX08 symbol polish accepted, in candidate-tree
/// file ids. `delta` is the strict J improvement it earned at acceptance time.
pub(in crate::analyze) struct SymbolRelocation {
    /// The relocated node's id.
    pub(super) node: u32,
    /// The candidate file container the symbol leaves.
    pub(super) from_file: ContainerId,
    /// The candidate file container the symbol joins.
    pub(super) to_file: ContainerId,
    /// The objective improvement contributed (J drops by this much). Positive
    /// for an ordinary relocation; within a fold it is the marginal change in
    /// acceptance order, and only the fold's sum is guaranteed positive.
    pub(super) delta: f64,
    /// The collision fold this relocation belongs to (ADR-21), as the
    /// pass-start id of the file it drains; `None` for an ordinary relocation.
    /// A fold is accepted or refused as one unit.
    pub(super) fold: Option<ContainerId>,
}

/// The outcome of one symbol polish pass: the effective placement overlay, the
/// accepted relocations in acceptance order, and the final objective total.
pub(in crate::analyze) struct SymbolOutcome {
    /// Node id → candidate file container for relocated symbols only.
    pub(in crate::analyze::relocation) overlay: BTreeMap<u32, ContainerId>,
    /// Accepted relocations in acceptance order.
    pub(super) relocations: Vec<SymbolRelocation>,
    /// The objective total after all accepted relocations.
    pub(in crate::analyze::relocation) total: f64,
}

impl SymbolOutcome {
    /// Pass-start ids of the files whose collision fold (ADR-21) the pass
    /// accepted.
    pub(in crate::analyze::relocation) fn accepted_folds(&self) -> BTreeSet<u32> {
        self.relocations
            .iter()
            .filter_map(|relocation| relocation.fold.map(|file| file.0))
            .collect()
    }

    pub(in crate::analyze::relocation) fn preserves_namespaces(
        &self,
        assembled: &CandidateTree,
    ) -> bool {
        self.relocations.iter().all(|relocation| {
            assembled.shares_namespace(relocation.from_file, relocation.to_file)
                && self.overlay.get(&relocation.node) == Some(&relocation.to_file)
        })
    }
}
