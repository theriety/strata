//! Unlace's orchestration engine and public library surface.
//!
//! This crate is the only one that sees both the solver ([`unlace_core`]) and the
//! language adapters; it is also the embeddable library `unlace-engine` (AD-5).
//! Everything the CLI can do is a plain Rust call with typed inputs and outputs:
//!
//! - [`load_config`] parses and validates a `unlace.toml` into an [`AnalyzeConfig`];
//! - [`snapshot_from_root`] discovers sources, runs adapters, merges fragments,
//!   flattens re-exports, and assembles a validated, hashed [`Snapshot`];
//! - [`analyze`](fn@analyze) is a pure, deterministic function of a snapshot and config that
//!   returns an owned [`AnalyzeResult`] — no I/O, no global state.
//!
//! All seven failure modes collapse into the single [`UnlaceError`] enum. The
//! crate re-exports the `ir` contract so consumers can assemble snapshots from a
//! custom adapter and call [`analyze`](fn@analyze) directly.
//!
//! [`Snapshot`]: unlace_ir::Snapshot

pub mod analyze;
pub mod config;
pub mod error;
mod narrate;
pub mod result;
pub mod snapshot;

pub use unlace_core as core;
pub use unlace_ir as ir;

pub use crate::analyze::{BORDERLINE_CAPACITY_MARGIN, analyze};
pub use crate::config::{
    AdaptersConfig, AnalysisConfig, AnalyzeConfig, CapacityConfig, DiversityConfig, Mode,
    ObjectiveConfig, ProfileConfig, ProfileName, ProfilesConfig, QualificationConfig,
    QualificationWeightsConfig, RelocationConfig, SolverConfig, TestMirrorRule,
    TestMirroringConfig, TestsConfig, WeightsConfig, load_config,
};
pub use crate::error::UnlaceError;
pub use crate::result::{
    Advice, AnalyzeResult, Candidate, CapacityBreach, CapacityRemainder, CapacityUnit,
    ConditionalSplit, ContainerNode, CurrentStanding, CurrentTree, EdgeBreak, EvidenceSignals,
    FileMove, Level, MirrorMove, ModeResult, Modes, Move, MoveKind, MoveReason, ProfileAssessment,
    ProfileConflict, ProfileCurrent, ProfileResult, Profiles, QualificationThresholds,
    RESULT_SCHEMA_VERSION, RelocationAdvice, RelocationProposal, ReviewReason, ScoreBreakdown,
    Severity, Summary, SymbolKind, SymbolMove, SymbolPlacement, TestSettings, Violation,
    ViolationKind,
};
pub use crate::snapshot::snapshot_from_root;
