//! Strata's orchestration engine and public library surface.
//!
//! This crate is the only one that sees both the solver ([`strata_core`]) and the
//! language adapters; it is also the embeddable library `strata-engine` (AD-5).
//! Everything the CLI can do is a plain Rust call with typed inputs and outputs:
//!
//! - [`load_config`] parses and validates a `strata.toml` into an [`AnalyzeConfig`];
//! - [`snapshot_from_root`] discovers sources, runs adapters, merges fragments,
//!   flattens re-exports, and assembles a validated, hashed [`Snapshot`];
//! - [`analyze`] is a pure, deterministic function of a snapshot and config that
//!   returns an owned [`AnalyzeResult`] — no I/O, no global state.
//!
//! All seven failure modes collapse into the single [`StrataError`] enum. The
//! crate re-exports the `ir` contract so consumers can assemble snapshots from a
//! custom adapter and call [`analyze`] directly.
//!
//! [`Snapshot`]: strata_ir::Snapshot

pub mod analyze;
pub mod config;
pub mod error;
mod narrate;
pub mod result;
pub mod snapshot;

pub use strata_core as core;
pub use strata_ir as ir;

pub use crate::analyze::{BORDERLINE_CAPACITY_MARGIN, analyze};
pub use crate::config::{
    AdaptersConfig, AnalysisConfig, AnalyzeConfig, CapacityConfig, DiversityConfig, Mode,
    ObjectiveConfig, SolverConfig, TestsConfig, WeightsConfig, load_config,
};
pub use crate::error::StrataError;
pub use crate::result::{
    AnalyzeResult, Candidate, CapacityBreach, CapacityRemainder, ConditionalSplit, ContainerNode,
    CurrentStanding, CurrentTree, EdgeBreak, Level, ModeResult, Modes, Move, MoveKind, MoveReason,
    RESULT_SCHEMA_VERSION, ScoreBreakdown, Severity, Summary, SymbolPlacement, Violation,
    ViolationKind,
};
pub use crate::snapshot::snapshot_from_root;
