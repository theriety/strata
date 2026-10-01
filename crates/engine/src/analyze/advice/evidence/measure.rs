//! Pure arithmetic of the evidence model: dependency weights, token overlap, and
//! the weighted and structural evidence scores.

use std::collections::BTreeSet;

use strata_ir::EdgeKind;

use crate::config::ProfileConfig;
use crate::result::EvidenceSignals;

pub(super) fn dependency_weight(kind: EdgeKind, config: &ProfileConfig) -> f64 {
    match kind {
        EdgeKind::ValueImport => config.weights.value_import,
        EdgeKind::TypeReference => config.weights.type_reference,
        EdgeKind::Inheritance => config.weights.inheritance,
        EdgeKind::Call => config.weights.call,
        EdgeKind::ReExport => config.weights.re_export,
    }
}
pub(super) fn jaccard_tokens(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    let union = left.union(right).count();
    if union == 0 {
        0.0
    } else {
        usize_ratio(left.intersection(right).count(), union)
    }
}

#[allow(clippy::cast_precision_loss)]
pub(super) fn usize_as_f64(value: usize) -> f64 {
    value as f64
}

pub(super) fn usize_ratio(numerator: usize, denominator: usize) -> f64 {
    usize_as_f64(numerator) / usize_as_f64(denominator)
}
pub(super) fn weighted_evidence(e: EvidenceSignals, config: &ProfileConfig) -> f64 {
    let w = config.qualification.weights;
    let total = w.unique_owner
        + w.role_affinity
        + w.source_cohesion
        + w.destination_cohesion
        + w.producer_evidence
        + w.architectural_reach;
    (e.unique_owner * w.unique_owner
        + e.role_affinity * w.role_affinity
        + e.source_cohesion * w.source_cohesion
        + e.destination_cohesion * w.destination_cohesion
        + e.producer_evidence * w.producer_evidence
        + e.architectural_reach * w.architectural_reach)
        / total
}
pub(super) fn structural_evidence(e: EvidenceSignals, config: &ProfileConfig) -> f64 {
    let w = config.qualification.weights;
    let total = w.unique_owner
        + w.source_cohesion
        + w.destination_cohesion
        + w.producer_evidence
        + w.architectural_reach;
    if total == 0.0 {
        0.0
    } else {
        (e.unique_owner * w.unique_owner
            + e.source_cohesion * w.source_cohesion
            + e.destination_cohesion * w.destination_cohesion
            + e.producer_evidence * w.producer_evidence
            + e.architectural_reach * w.architectural_reach)
            / total
    }
}
