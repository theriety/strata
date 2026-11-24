//! The findings of the current tree: cycles, polarity breaches, visibility
//! over-exports, and capacity breaches, plus the snapshot census.
//!
//! Each child owns one kind of finding or the order they share; this root
//! assembles them into the engine's single violation list.

use std::collections::BTreeMap;

use unlace_ir::{ScopeLevel, Snapshot};

use crate::analyze::test_policy::TestPolicy;
use crate::config::{CapacityConfig, ProfileConfig};
use crate::result::{ContainerNode, Severity, Summary, Violation, ViolationKind};
use crate::snapshot::Language;

mod capacity;
mod cycles;
mod import_cycles;
mod ordering;
mod physical;
mod polarity;
mod real_roots;
mod test_only;
mod visibility;

#[cfg(test)]
mod capacity_wording_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
use capacity::capacity_violations;
pub(in crate::analyze) use capacity::{snapshot_capacity_violations, walk_all_capacity};
pub(in crate::analyze) use cycles::conditional_splits;
pub(in crate::analyze) use import_cycles::cycle_coverage;
use import_cycles::import_cycle_violations;
use ordering::sort_and_dedup_violations;
#[cfg(test)]
use ordering::{sort_violations, violation_identity};
#[cfg(test)]
use physical::physical_folder_entries;
pub(in crate::analyze) use physical::{
    physical_folder_entries_repository_relative, physical_folder_findings,
};
use polarity::polarity_violations;
pub(in crate::analyze) use real_roots::RealRoots;
use test_only::test_only_violations;
use visibility::visibility_violations;

/// The findings of the current tree under `profile`.
///
/// Cycle findings need no symbol break set: the import graph is cut on its
/// own, and the hard symbol components serve only as unavailable witnesses,
/// so no MFAS is solved here. `solve_cycles` runs only for the conditional
/// splits of a profile result.
pub(in crate::analyze) fn profile_findings(
    snapshot: &Snapshot,
    tree: &ContainerNode,
    profile: &ProfileConfig,
    tests: &TestPolicy,
) -> Vec<Violation> {
    collect_violations(snapshot, &profile.capacity, tree, tests)
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

/// Collects the violations of the current tree: file-import cycles (ADR-35),
/// with the symbol cycles as witnesses where imports are unavailable, polarity
/// breaches, visibility over-exports, and capacity findings against the
/// configured caps.
fn collect_violations(
    snapshot: &Snapshot,
    capacity: &CapacityConfig,
    tree: &ContainerNode,
    tests: &TestPolicy,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    violations.extend(import_cycle_violations(snapshot));
    violations.extend(polarity_violations(snapshot));
    violations.extend(visibility_violations(snapshot, tests));
    violations.extend(test_only_violations(snapshot, tests));
    violations.extend(snapshot_capacity_violations(snapshot, tree, capacity));
    sort_and_dedup_violations(&mut violations);
    violations
}
