//! Node, edge, affinity, re-export, and polarity emission.

mod affinity;
mod edges;
mod polarity;
mod re_export;

pub(super) use affinity::emit_companion_affinities;
pub(super) use edges::emit_edges;
pub(super) use polarity::{apply_polarity, classify_polarity};
pub(super) use re_export::{assign_re_export_nodes, emit_re_exports};

/// Confidence assigned to a statically resolved edge.
pub(super) const CONFIDENCE_STATIC: f64 = 1.0;
