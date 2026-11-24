//! A symbol's priced pull toward the files around it, and the
//! minority-consumer rule that reads it (ADR-34).

use std::collections::BTreeMap;

use unlace_ir::ContainerId;

use crate::analyze::relocation::SYMBOL_MAJORITY_SHARE;

/// Relative tolerance under which two pulls count as equal: pulls are sums of
/// priced edge weights, so float rounding must not decide a tie.
const PULL_TIE_TOLERANCE: f64 = 1e-9;

/// The summed two-way priced pull on one symbol from the base-placed residents
/// of each file, split into its home file and every other file.
///
/// Pull counts edges, so parallel edges between the same two declarations add
/// up: an adapter that states one edge per call site gets call-site pull. An
/// adapter that collapses a class's member calls into one edge per class gives
/// the class one unit of pull, which is why a tie among the top files keeps
/// the symbol home (ADR-34).
#[derive(Debug, Default)]
pub(super) struct Pull {
    /// Pull from residents of each file other than the symbol's home.
    pub(super) by_file: BTreeMap<ContainerId, f64>,
    /// Pull from residents of the symbol's own file.
    pub(super) home: f64,
}

impl Pull {
    /// Pull from every file other than the symbol's home.
    pub(super) fn external(&self) -> f64 {
        self.by_file.values().sum()
    }

    /// The pull `destination`'s residents exert, `0` for a file that exerts
    /// none.
    pub(super) fn of(&self, destination: ContainerId) -> f64 {
        self.by_file.get(&destination).copied().unwrap_or(0.0)
    }

    /// Whether `destination` may receive the symbol under the minority-consumer
    /// rule (ADR-34): it must hold at least [`SYMBOL_MAJORITY_SHARE`] of the
    /// external pull, and no other file may pull as hard. A file holding less
    /// is a minority consumer, and a file tied with another has no better
    /// claim than the symbol's current home, so both keep the symbol home.
    pub(super) fn admits(&self, destination: ContainerId) -> bool {
        let held = self.of(destination);
        let external = self.external();
        if held <= 0.0 || external <= 0.0 {
            return false;
        }
        let tolerance = PULL_TIE_TOLERANCE * external;
        if held + tolerance < SYMBOL_MAJORITY_SHARE * external {
            return false;
        }
        !self
            .by_file
            .iter()
            .any(|(file, pull)| *file != destination && *pull + tolerance >= held)
    }
}
