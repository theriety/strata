//! Strata's solver core: graph algorithms over the IR (Tarjan, layering, MFAS,
//! clustering, packing, scoring).
//!
//! This crate is a placeholder in commit 1 and gains its algorithms in later
//! commits. It re-exports the IR contract so downstream crates have a single
//! solver-facing entry point.

pub use strata_ir as ir;
