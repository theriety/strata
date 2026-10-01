//! The findings of the current tree: cycles, polarity breaches, visibility
//! over-exports, and capacity breaches, plus the snapshot census.
//!
//! Each child owns one kind of finding or the order they share; this root
//! assembles them into the engine's single violation list.

use std::collections::BTreeMap;

use strata_ir::{ScopeLevel, Snapshot};

use crate::analyze::search::{SccSolution, solve_cycles};
use crate::config::{CapacityConfig, ProfileConfig};
use crate::result::{ContainerNode, Severity, Summary, Violation, ViolationKind};
use crate::snapshot::Language;

mod capacity;
mod cycles;
mod ordering;
mod physical;
mod polarity;
mod visibility;

#[cfg(test)]
mod tests;

#[cfg(test)]
use capacity::capacity_violations;
pub(in crate::analyze) use capacity::{snapshot_capacity_violations, walk_all_capacity};
pub(in crate::analyze) use cycles::conditional_splits;
use cycles::cycle_violations;
use ordering::sort_and_dedup_violations;
#[cfg(test)]
use ordering::{sort_violations, violation_identity};
#[cfg(test)]
use physical::physical_folder_entries;
pub(in crate::analyze) use physical::{
    physical_folder_entries_repository_relative, physical_folder_findings,
};
use polarity::polarity_violations;
use visibility::visibility_violations;

pub(in crate::analyze) fn profile_findings(
    snapshot: &Snapshot,
    tree: &ContainerNode,
    profile: &ProfileConfig,
) -> Vec<Violation> {
    let weights = profile.weights.kind_weights();
    let cycles = solve_cycles(snapshot, profile, &weights);
    collect_violations(snapshot, &profile.capacity, tree, &cycles)
}

/// Counts the capacity findings that hard-breach their caps: `Severity::Violation`
/// only. Borderline observations sit within the tolerance band, never gate a
/// standing, and never count as breaks; they remain listed in `violations`.
pub(in crate::analyze) fn hard_capacity_breaks(violations: &[Violation]) -> u32 {
    let count = violations
        .iter()
        .filter(|violation| {
            violation.kind == ViolationKind::Capacity && violation.severity == Severity::Violation
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Builds the coarse census of the snapshot.
///
/// Files are classified by the same extension rule the snapshotter routes them
/// with, so `files_by_language` mirrors the adapter dispatch; a file no adapter
/// claims (possible only in a hand-assembled snapshot) counts toward `files` but
/// no language.
pub(in crate::analyze) fn summarize(snapshot: &Snapshot) -> Summary {
    let ir = snapshot.ir();
    let mut files = 0u32;
    let mut files_by_language: BTreeMap<String, u32> = BTreeMap::new();
    for container in ir.containers.containers() {
        if container.level != ScopeLevel::File {
            continue;
        }
        files = files.saturating_add(1);
        if let Some(language) = Language::ALL
            .iter()
            .find(|language| language.matches_extension(&container.name))
        {
            *files_by_language
                .entry(language.name().to_owned())
                .or_default() += 1;
        }
    }

    Summary {
        symbols: u32::try_from(ir.nodes.len()).unwrap_or(u32::MAX),
        edges: u32::try_from(ir.edges.len()).unwrap_or(u32::MAX),
        files,
        files_by_language,
    }
}

/// Collects the violations of the current tree: dependency cycles, polarity
/// breaches, visibility over-exports, and capacity findings against the
/// configured caps.
fn collect_violations(
    snapshot: &Snapshot,
    capacity: &CapacityConfig,
    tree: &ContainerNode,
    cycles: &[SccSolution],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    violations.extend(cycle_violations(snapshot, cycles));
    violations.extend(polarity_violations(snapshot));
    violations.extend(visibility_violations(snapshot));
    violations.extend(snapshot_capacity_violations(snapshot, tree, capacity));
    sort_and_dedup_violations(&mut violations);
    violations
}
