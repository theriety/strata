//! Capacity findings over the container tree against the configured per-level caps.

use unlace_ir::Snapshot;

use crate::analyze::BORDERLINE_CAPACITY_MARGIN;
use crate::analyze::findings::physical::{
    physical_folder_findings_with_real, physical_folders_repository_relative,
};
use crate::analyze::findings::real_roots::RealRoots;
use crate::analyze::scoring::CapacitySource;
#[cfg(test)]
use crate::config::AnalyzeConfig;
use crate::config::CapacityConfig;
use crate::result::{
    CapacityBreach, CapacityUnit, ContainerNode, Level, Severity, Violation, ViolationKind,
};

/// Derives capacity violations from the current tree against the configured caps.
///
/// A file over its production-SLOC cap and an interior container over its
/// member-count cap each yield a finding; a finding within ±10% of its cap is
/// `borderline` and never gates. Each container is checked against the cap of
/// its own level. A physical folder is measured by its immediate files plus its
/// distinct immediate subfolders; domains and above retain their structural
/// member measures.
pub(in crate::analyze) fn snapshot_capacity_violations(
    snapshot: &Snapshot,
    tree: &ContainerNode,
    capacity: &CapacityConfig,
) -> Vec<Violation> {
    let roots = RealRoots::from_tree(&snapshot.ir().containers);
    let mut findings: Vec<Violation> = walk_capacity_from(tree, capacity, &roots)
        .into_iter()
        .filter(|(level, _)| *level != Level::Folder)
        .map(|(_, violation)| violation)
        .collect();
    let (entries, real_folders) = physical_folders_repository_relative(&snapshot.ir().containers);
    findings.extend(physical_folder_findings_with_real(
        &entries,
        &real_folders,
        capacity.folder,
    ));
    findings
}

/// DTO-only capacity walk retained for focused structural tests that read no
/// domain: it knows no real roots. Production analysis augments it with the
/// snapshot's physical folder projection above.
#[cfg(test)]
pub(super) fn capacity_violations(tree: &ContainerNode, config: &AnalyzeConfig) -> Vec<Violation> {
    walk_all_capacity(
        tree,
        &config.profiles.anchored.capacity,
        &RealRoots::empty(),
    )
    .into_iter()
    .map(|(_, violation)| violation)
    .collect()
}

/// Walks the DTO tree, returning each capacity finding with the level it hit.
///
/// `roots` are the real roots of the container tree `tree` was rendered from.
/// The domain cap is measured over that container tree, by domain, through
/// `roots` — the very measure candidate pricing uses — never over the rendered
/// domain nodes, since the renderer drops a domain that only echoes its package.
pub(in crate::analyze) fn walk_all_capacity(
    tree: &ContainerNode,
    source: &impl CapacitySource,
    roots: &RealRoots,
) -> Vec<(Level, Violation)> {
    walk_capacity_from(tree, source.capacity(), roots)
}

/// Walks the DTO tree; `roots` names the real directories of domains and above.
fn walk_capacity_from(
    tree: &ContainerNode,
    capacity: &CapacityConfig,
    roots: &RealRoots,
) -> Vec<(Level, Violation)> {
    let mut findings = Vec::new();
    let mut path = Vec::new();
    append_display_segments(&mut path, tree);
    walk_capacity(tree, &path, capacity, roots, &mut findings);
    for (measure, real_paths) in roots.domain_measures() {
        if let Some(finding) = breach_violation(
            Level::Domain,
            measure,
            capacity.domain,
            real_paths,
            None,
            &[],
        ) {
            findings.push((Level::Domain, finding));
        }
    }
    findings
}

/// Recursively checks `node` and its descendants against their level caps.
fn walk_capacity(
    node: &ContainerNode,
    path: &[String],
    capacity: &CapacityConfig,
    roots: &RealRoots,
    findings: &mut Vec<(Level, Violation)>,
) {
    // QUAL-P3-2: a package counts every binding domain beneath it and a package
    // group every binding package, deeply, so nesting through an intermediate
    // level cannot launder binding out of the finding. A domain is measured over
    // the container tree instead (D-44, 2026-11-05), so its node is skipped
    // here. The folder arm is used only by DTO-focused tests; production folder
    // findings come from the package-aware physical projection.
    let measured = match node.level {
        Level::File => Some((node.production_sloc.unwrap_or(0), capacity.file)),
        Level::Folder => Some((file_child_count(node), capacity.folder)),
        Level::Domain => None,
        Level::Package => Some((count_bound_members(node, Level::Domain).0, capacity.package)),
        Level::PackageGroup => Some((
            count_bound_members(node, Level::Package).0,
            capacity.package_group,
        )),
    };

    if let Some((measure, cap)) = measured
        && let Some(finding) = capacity_finding(node, path, measure, cap, roots)
    {
        findings.push((node.level, finding));
    }

    if let Some(children) = &node.children {
        for child in children {
            let mut child_path = path.to_vec();
            append_display_segments(&mut child_path, child);
            walk_capacity(child, &child_path, capacity, roots, findings);
        }
    }
}

/// Appends `node`'s display segments to `path`.
///
/// Interior DTO names are already incremental, so they split directly into
/// segments; a file contributes only its basename because the finding's
/// `detail` line carries the full path. Adjacent levels sharing one name (the
/// synthetic `workspace` chain over a root-level file) contribute it once.
fn append_display_segments(path: &mut Vec<String>, node: &ContainerNode) {
    let name = if node.level == Level::File {
        node.name.rsplit('/').next().unwrap_or(node.name.as_str())
    } else {
        node.name.as_str()
    };
    for segment in name.split('/').filter(|segment| !segment.is_empty()) {
        if path.last().map(String::as_str) != Some(segment) {
            path.push(segment.to_owned());
        }
    }
}

/// Counts, in one pass, the descendants of `node` at `member_level` whose
/// subtree binds at least one file — the real places beneath it — plus
/// whether `node` itself binds one (QUAL-P3-2). An intermediate level can no
/// longer launder binding out of the finding. Used for the package and package
/// group caps, whose members (domains, packages) have no folder-style chaining,
/// so their transitive binding stands.
fn count_bound_members(node: &ContainerNode, member_level: Level) -> (u32, bool) {
    if node.level == Level::File {
        return (0, true);
    }
    let mut members = 0_u32;
    let mut holds = false;
    for child in node.children.iter().flatten() {
        let (child_members, child_holds) = count_bound_members(child, member_level);
        members = members.saturating_add(child_members);
        holds |= child_holds;
    }
    if node.level == member_level && holds {
        members = members.saturating_add(1);
    }
    (members, holds)
}

/// Returns the number of immediate entries a folder holds directly.
///
/// Files and distinct child folders each consume one entry. Descendants below
/// those child folders do not.
fn file_child_count(node: &ContainerNode) -> u32 {
    let entries = node.children.as_ref().map_or(0, Vec::len);
    u32::try_from(entries).unwrap_or(u32::MAX)
}

/// Builds the capacity finding of a rendered node, if it breaches `cap`.
fn capacity_finding(
    node: &ContainerNode,
    path: &[String],
    measure: u32,
    cap: u32,
    roots: &RealRoots,
) -> Option<Violation> {
    let real_paths = match node.level {
        Level::File => vec![node.name.clone()],
        Level::Folder => Vec::new(),
        level => roots.package_paths(node, level),
    };
    // for files the container name IS the full repo-relative path; folder
    // and higher paths are carried by `real_paths`.
    let file_path = (node.level == Level::File).then(|| node.name.clone());
    breach_violation(node.level, measure, cap, real_paths, file_path, path)
}

/// Builds a capacity finding if `measure` is over `cap`, classifying borderline
/// (within ±10%) versus a hard breach. `fallback` names the place when no real
/// path is known.
fn breach_violation(
    level: Level,
    measure: u32,
    cap: u32,
    real_paths: Vec<String>,
    file_path: Option<String>,
    fallback: &[String],
) -> Option<Violation> {
    if measure <= cap {
        return None;
    }
    let cap_f = f64::from(cap);
    let measure_f = f64::from(measure);
    let upper = cap_f * (1.0 + BORDERLINE_CAPACITY_MARGIN);
    let severity = if measure_f > upper {
        Severity::Violation
    } else {
        Severity::Borderline
    };

    let (unit, config_key) = unit_and_key(level);
    let breach = CapacityBreach {
        measured: measure,
        cap,
        unit,
        config_key: config_key.to_owned(),
        real_paths,
        path: file_path,
    };
    Some(Violation {
        kind: ViolationKind::Capacity,
        severity,
        location: breach
            .real_location()
            .unwrap_or_else(|| vec![fallback.join("/")]),
        detail: breach.describe(fallback),
        break_suggestions: None,
        capacity: Some(breach),
    })
}

/// Returns what a level's measure counts and the config key of its cap.
fn unit_and_key(level: Level) -> (CapacityUnit, &'static str) {
    match level {
        Level::File => (CapacityUnit::Lines, "capacity.file"),
        Level::Folder => (CapacityUnit::Entries, "capacity.folder"),
        Level::Domain => (CapacityUnit::Folders, "capacity.domain"),
        Level::Package => (CapacityUnit::Domains, "capacity.package"),
        Level::PackageGroup => (CapacityUnit::Packages, "capacity.package-group"),
    }
}
