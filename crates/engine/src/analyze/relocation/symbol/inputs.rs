use std::collections::BTreeSet;

use strata_core::score::{Coefficients, KindWeights};
use strata_ir::{ContainerId, Edge, Node, Snapshot};

use crate::analyze::relocation::CandidateTree;
use crate::config::CapacityConfig;

/// The read-only analysis inputs a [`SymbolPass`] prices placements against.
#[derive(Clone, Copy)]
pub(super) struct PassInputs<'a> {
    pub(super) snapshot: &'a Snapshot,
    pub(super) coefficients: &'a Coefficients,
    pub(super) weights: &'a KindWeights,
    pub(super) same_file_symbol: f64,
    pub(super) same_file_type: f64,
    pub(super) capacity: CapacityConfig,
    pub(super) assembled: &'a CandidateTree,
    pub(super) nodes: &'a [Node],
    pub(super) edges: &'a [Edge],
}

/// The admission rules of a [`SymbolPass`]: which files may not give or
/// receive a symbol, whether test-polarity declarations are pinned, and
/// whether the package wall is lifted.
#[derive(Default)]
pub(super) struct RelocationPolicy {
    /// Files whose symbols may not leave.
    pub(super) forbidden_sources: BTreeSet<ContainerId>,
    /// Files that may not receive a symbol.
    pub(super) forbidden_destinations: BTreeSet<ContainerId>,
    /// Refuse any non-production-polarity declaration, wherever it lives.
    pub(super) pin_test_polarity: bool,
    /// Allow a symbol to land in a file of another package.
    pub(super) allow_cross_package: bool,
}
