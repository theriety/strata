//! Strata's solver core: graph algorithms over the IR (Tarjan, layering, MFAS,
//! clustering, scoring).
//!
//! Commit 2 lays the graph foundation: CSR adjacency ([`graph`]), iterative
//! Tarjan SCC condensation ([`condense`]), and longest-path layering
//! ([`layer`]). Commit 3 adds per-SCC cycle shattering — exact MFAS via ILP
//! with lazy cycle constraints and an Eades–Lin–Smyth fallback ([`shatter`]).
//! Commit 4 layers dagP-style multilevel acyclic clustering on top — coarsen,
//! seed, refine — where acyclicity is an inviolable veto and capacity a hard
//! cap ([`cluster`]). Commit 6 derives visibility from consumption — LCA over
//! each symbol's consumers widened by the transitive type-exposure fixpoint
//! ([`visibility`]). Commit 7 closes the pipeline: the objective J(T) scores a
//! candidate tree with a per-term breakdown ([`score`]), and multi-start
//! diversification returns k genuinely different candidates by max-min variation
//! of information ([`diversify`]). It re-exports the IR contract so downstream
//! crates have a single solver-facing entry point.

pub mod cluster;
pub mod condense;
pub mod diversify;
pub mod graph;
pub mod layer;
pub mod score;
pub mod shatter;
pub mod visibility;

pub use strata_ir as ir;
