//! Capacity findings over the container tree against the configured per-level caps.

use strata_ir::Snapshot;

use crate::analyze::BORDERLINE_CAPACITY_MARGIN;
use crate::analyze::findings::physical::{
    physical_folder_entries_repository_relative, physical_folder_findings,
};
use crate::analyze::scoring::CapacitySource;
#[cfg(test)]
use crate::config::AnalyzeConfig;
use crate::config::CapacityConfig;
use crate::result::{CapacityBreach, ContainerNode, Level, Severity, Violation, ViolationKind};

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
    let mut findings: Vec<Violation> = walk_all_capacity(tree, capacity)
        .into_iter()
        .filter(|(level, _)| *level != Level::Folder)
        .map(|(_, violation)| violation)
        .collect();
    findings.extend(physical_folder_findings(
        &physical_folder_entries_repository_relative(&snapshot.ir().containers, None),
        capacity.folder,
    ));
    findings
}

/// DTO-only capacity walk retained for focused structural tests. Production
/// analysis augments it with the snapshot's physical folder projection above.
#[cfg(test)]
pub(super) fn capacity_violations(tree: &ContainerNode, config: &AnalyzeConfig) -> Vec<Violation> {
    walk_all_capacity(tree, &config.profiles.anchored.capacity)
        .into_iter()
        .map(|(_, violation)| violation)
        .collect()
}

/// Walks the DTO tree, returning each capacity finding with the level it hit.
pub(in crate::analyze) fn walk_all_capacity(
    tree: &ContainerNode,
    source: &impl CapacitySource,
) -> Vec<(Level, Violation)> {
    let capacity = source.capacity();
    let mut findings = Vec::new();
    let mut path = Vec::new();
    append_display_segments(&mut path, tree);
    walk_capacity(tree, &path, capacity, &mut findings);
    findings
}

/// Recursively checks `node` and its descendants against their level caps.
fn walk_capacity(
    node: &ContainerNode,
    path: &[String],
    capacity: &CapacityConfig,
    findings: &mut Vec<(Level, Violation)>,
) {
    let (measure, cap) = match node.level {
        Level::File => (node.production_sloc.unwrap_or(0), capacity.file),
        Level::Folder => (file_child_count(node), capacity.folder),
        // QUAL-P3-2: upper levels count their real structural members deeply —
        // a domain every file-binding folder beneath it, a package every
        // binding domain, a package group every binding package — so nesting
        // through an intermediate level cannot launder binding out of the
        // finding. Only members that actually bind a file count: an interior
        // directory chain (`a/b/c`) is one real place, never three. The folder
        // arm is used only by DTO-focused tests; production folder findings
        // come from the package-aware physical projection.
        Level::Domain => (count_bound_members(node, Level::Folder).0, capacity.domain),
        Level::Package => (count_bound_members(node, Level::Domain).0, capacity.package),
        Level::PackageGroup => (
            count_bound_members(node, Level::Package).0,
            capacity.package_group,
        ),
    };

    if let Some(finding) = capacity_finding(node, path, measure, cap) {
        findings.push((node.level, finding));
    }

    if let Some(children) = &node.children {
        for child in children {
            let mut child_path = path.to_vec();
            append_display_segments(&mut child_path, child);
            walk_capacity(child, &child_path, capacity, findings);
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
/// longer launder binding out of the finding. A rendered directory chain
/// interleaves one Folder node per path segment, so a folder counts as a real
/// place only when it *directly* holds a file — otherwise every ancestor
/// segment of `a/b/c` would price the single directory three times. Upper
/// member levels (domain, package) have no such chaining, so their transitive
/// binding stands.
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
    if node.level == member_level {
        let binds = if member_level == Level::Folder {
            node.children
                .as_ref()
                .is_some_and(|children| children.iter().any(|child| child.level == Level::File))
        } else {
            holds
        };
        if binds {
            members = members.saturating_add(1);
        }
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

/// Builds a capacity finding if `measure` is at or over the borderline band of
/// `cap`, classifying borderline (within ±10%) versus a hard breach.
fn capacity_finding(
    node: &ContainerNode,
    path: &[String],
    measure: u32,
    cap: u32,
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

    Some(Violation {
        kind: ViolationKind::Capacity,
        severity,
        location: path.to_vec(),
        detail: format!(
            "{} `{}` holds {measure} against a cap of {cap}",
            level_word(node.level),
            node.name
        ),
        break_suggestions: None,
        // for files the container name IS the full repo-relative path; folder
        // and higher paths are already carried by `location`.
        capacity: Some(CapacityBreach {
            measured: measure,
            cap,
            path: (node.level == Level::File).then(|| node.name.clone()),
        }),
    })
}

/// Returns the noun for a container level used in a capacity message.
fn level_word(level: Level) -> &'static str {
    match level {
        Level::File => "file",
        Level::Folder => "folder",
        Level::Domain => "domain",
        Level::Package => "package",
        Level::PackageGroup => "package group",
    }
}
