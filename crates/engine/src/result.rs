//! The `AnalyzeResult` DTO.
//!
//! This is the contract both faces of Strata share: the library returns an owned
//! [`AnalyzeResult`], and the CLI serializes the very same value as JSON for
//! `--format json`. Serialization is camelCase so the JSON matches the reference
//! `AnalyzeResult` interface byte-for-byte; every nested type carries the same
//! field names the reference documents.

mod advice;
mod candidate;
mod finding;
mod movement;
mod profile;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

pub use self::advice::{
    Advice, EvidenceSignals, ProfileAssessment, ProfileConflict, QualificationThresholds,
    RelocationAdvice, RelocationProposal, ReviewReason,
};
pub use self::candidate::{Candidate, ContainerNode, Level, ScoreBreakdown, SymbolPlacement};
pub use self::finding::{
    CapacityBreach, ConditionalSplit, EdgeBreak, Severity, Violation, ViolationKind,
};
pub use self::movement::{
    BlockedMirror, BlockedMirrorReason, FileMove, MirrorMove, Move, MoveKind, MoveReason,
    SymbolKind, SymbolMove,
};
pub use self::profile::{
    CapacityRemainder, CurrentStanding, CurrentTree, ModeResult, Modes, ProfileCurrent,
    ProfileResult, Profiles, Summary,
};

/// The schema version stamped into every [`AnalyzeResult`] this crate produces.
///
/// Readers of a saved result reject any other version rather than misreading a
/// future shape.
pub const RESULT_SCHEMA_VERSION: u32 = 9;

/// The top-level analysis result: the snapshot hash, the current tree with its
/// violations, and the per-profile candidate sets.
///
/// `snapshot_hash` keys a deterministic cache — an identical snapshot and config
/// always produce an identical result. `profiles` carries up to one
/// [`ProfileResult`] per requested parameter profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    /// The result schema version ([`RESULT_SCHEMA_VERSION`]); a saved result
    /// with any other version is rejected on read. Defaults to `0` (unsupported)
    /// when absent, so pre-versioned files fail with a clear message.
    #[serde(default)]
    pub schema_version: u32,
    /// The hex blake3 hash of the analyzed snapshot.
    pub snapshot_hash: String,
    /// A coarse census of the analyzed graph.
    pub summary: Summary,
    /// The shared current layout and findings identical across executed profiles.
    pub current: CurrentTree,
    /// The independently configured parameter-profile results.
    pub profiles: Profiles,
    /// Evidence-qualified consensus over the selected profiles' first candidates.
    pub advice: Advice,
}
