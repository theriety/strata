//! Strata's solver core: graph algorithms over the IR (Tarjan, layering, MFAS,
//! clustering, packing, scoring).
//!
//! Commit 2 lays the graph foundation: identifier interning and CSR adjacency
//! ([`graph`]), iterative Tarjan SCC condensation ([`condense`]), and
//! longest-path layering ([`layer`]). It re-exports the IR contract so
//! downstream crates have a single solver-facing entry point.

pub mod condense;
pub mod graph;
pub mod layer;

pub use strata_ir as ir;
