//! Strata's solver core: graph algorithms over the IR (Tarjan, layering, MFAS,
//! clustering, packing, scoring).
//!
//! Commit 2 lays the graph foundation: identifier interning and CSR adjacency
//! ([`graph`]), iterative Tarjan SCC condensation ([`condense`]), and
//! longest-path layering ([`layer`]). Commit 3 adds per-SCC cycle shattering —
//! exact MFAS via ILP with lazy cycle constraints and an Eades–Lin–Smyth
//! fallback ([`shatter`]). Commit 4 layers dagP-style multilevel acyclic
//! clustering on top — coarsen, seed, refine — where acyclicity is an inviolable
//! veto and capacity a hard cap ([`cluster`]). It re-exports the IR contract so
//! downstream crates have a single solver-facing entry point.

pub mod cluster;
pub mod condense;
pub mod graph;
pub mod layer;
pub mod shatter;

pub use strata_ir as ir;
