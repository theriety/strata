//! The engine-defined total order over findings and the identity used to deduplicate them.

use crate::result::{Severity, Violation, ViolationKind};

/// Sorts findings by their complete serialized identity and removes exact
/// duplicates before shared/profile-specific partitioning.
pub(super) fn sort_and_dedup_violations(violations: &mut Vec<Violation>) {
    sort_violations(violations);
    violations.dedup_by(|left, right| violation_identity(left) == violation_identity(right));
}

/// Returns the stable schema identity used to order and deduplicate findings.
pub(super) fn violation_identity(violation: &Violation) -> String {
    serde_json::to_string(violation).unwrap_or_else(|_| format!("{violation:?}"))
}

/// Sorts violations into the engine-defined total order — hard violations
/// before borderline, then kind (cycle, polarity, capacity, visibility), then
/// location, then detail — so every face, including JSON, shares one order.
pub(super) fn sort_violations(violations: &mut [Violation]) {
    violations.sort_by(|left, right| {
        severity_rank(left.severity)
            .cmp(&severity_rank(right.severity))
            .then_with(|| kind_rank(left.kind).cmp(&kind_rank(right.kind)))
            .then_with(|| left.location.cmp(&right.location))
            .then_with(|| left.detail.cmp(&right.detail))
            .then_with(|| violation_identity(left).cmp(&violation_identity(right)))
    });
}

/// Ranks a severity for the violation ordering: hard violations first.
fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Violation => 0,
        Severity::Borderline => 1,
    }
}

/// Ranks a kind for the violation ordering, mirroring the emission order.
fn kind_rank(kind: ViolationKind) -> u8 {
    match kind {
        ViolationKind::Cycle => 0,
        ViolationKind::Polarity => 1,
        ViolationKind::Capacity => 2,
        ViolationKind::Visibility => 3,
    }
}
