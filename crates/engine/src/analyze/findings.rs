use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use smol_str::SmolStr;
use strata_core::visibility::derive_visibility;
use strata_ir::{Container, ContainerId, ContainerTree, Node, Polarity, ScopeLevel, Snapshot};

use crate::analyze::BORDERLINE_CAPACITY_MARGIN;
use crate::analyze::scoring::CapacitySource;
use crate::analyze::search::{SccSolution, node_names, solve_cycles};
#[cfg(test)]
use crate::config::AnalyzeConfig;
use crate::config::{CapacityConfig, ProfileConfig};
use crate::narrate::{project_package_rooted_path, project_physical_path};
use crate::result::{
    CapacityBreach, ConditionalSplit, ContainerNode, EdgeBreak, Level, Severity, Summary,
    Violation, ViolationKind,
};
use crate::snapshot::Language;

#[cfg(test)]
mod tests;

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
pub(in crate::analyze) fn collect_violations(
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

/// Sorts findings by their complete serialized identity and removes exact
/// duplicates before shared/profile-specific partitioning.
pub(in crate::analyze) fn sort_and_dedup_violations(violations: &mut Vec<Violation>) {
    sort_violations(violations);
    violations.dedup_by(|left, right| violation_identity(left) == violation_identity(right));
}

/// Returns the stable schema identity used to order and deduplicate findings.
pub(in crate::analyze) fn violation_identity(violation: &Violation) -> String {
    serde_json::to_string(violation).unwrap_or_else(|_| format!("{violation:?}"))
}

/// Sorts violations into the engine-defined total order — hard violations
/// before borderline, then kind (cycle, polarity, capacity, visibility), then
/// location, then detail — so every face, including JSON, shares one order.
pub(in crate::analyze) fn sort_violations(violations: &mut [Violation]) {
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
pub(in crate::analyze) fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Violation => 0,
        Severity::Borderline => 1,
    }
}

/// Ranks a kind for the violation ordering, mirroring the emission order.
pub(in crate::analyze) fn kind_rank(kind: ViolationKind) -> u8 {
    match kind {
        ViolationKind::Cycle => 0,
        ViolationKind::Polarity => 1,
        ViolationKind::Capacity => 2,
        ViolationKind::Visibility => 3,
    }
}

/// Reports every multi-node strongly connected component of the hard-edge graph
/// as a cycle violation, carrying the MFAS break set as suggestions.
pub(in crate::analyze) fn cycle_violations(
    snapshot: &Snapshot,
    cycles: &[SccSolution],
) -> Vec<Violation> {
    let names = node_names(snapshot);
    let nodes: BTreeMap<u32, &Node> = snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id.0, node))
        .collect();
    let files: BTreeMap<u32, String> = snapshot
        .ir()
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container.name.to_string()))
        .collect();

    cycles
        .iter()
        .map(|solution| {
            let location: Vec<String> = solution
                .members
                .iter()
                .filter_map(|node| nodes.get(&node.0))
                .filter_map(|node| files.get(&node.container.0).cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let breaks = edge_breaks(solution, &names);
            Violation {
                kind: ViolationKind::Cycle,
                severity: Severity::Violation,
                detail: cycle_detail(solution.members.len(), &breaks),
                location,
                break_suggestions: Some(breaks),
                capacity: None,
            }
        })
        .collect()
}

/// Maps one SCC's MFAS break set onto named [`EdgeBreak`]s, in the break set's
/// ascending `(source, target)` order.
pub(in crate::analyze) fn edge_breaks(
    solution: &SccSolution,
    names: &BTreeMap<u32, String>,
) -> Vec<EdgeBreak> {
    let name_of = |local: u32| {
        solution
            .members
            .get(local as usize)
            .and_then(|node| names.get(&node.0))
            .cloned()
            .unwrap_or_default()
    };
    solution
        .break_set
        .edges
        .iter()
        .map(|edge| EdgeBreak {
            source: name_of(edge.source),
            target: name_of(edge.target),
            weight: solution
                .pair_weights
                .get(&(edge.source, edge.target))
                .copied()
                .unwrap_or(0.0),
            exact: solution.break_set.exact,
        })
        .collect()
}

/// Renders a cycle violation's detail line, leading with the cheapest break.
pub(in crate::analyze) fn cycle_detail(size: usize, breaks: &[EdgeBreak]) -> String {
    const CONSEQUENCE: &str = "symbols form one placement unit and must remain in one file unless a suggested dependency edge is broken";
    let Some(first) = breaks.first() else {
        return format!("{size}-symbol cycle; {CONSEQUENCE}");
    };
    let method = if first.exact { "exact" } else { "heuristic" };
    let mut detail = format!(
        "{size}-symbol cycle; {CONSEQUENCE}; break {} -> {} (w={:.1}, {method})",
        first.source, first.target, first.weight
    );
    if breaks.len() > 1 {
        let _ = write!(detail, ", +{} more", breaks.len() - 1);
    }
    detail
}

/// Derives the shared conditional splits: one per solved SCC whose production
/// SLOC exceeds the file cap, with the MFAS break set as preconditions and a
/// ceil-packed file-count estimate.
pub(in crate::analyze) fn conditional_splits(
    cycles: &[SccSolution],
    snapshot: &Snapshot,
    file_cap: u32,
) -> Vec<ConditionalSplit> {
    let names = node_names(snapshot);
    let cap = u64::from(file_cap.max(1));
    cycles
        .iter()
        .filter(|solution| solution.production_sloc > cap)
        .map(|solution| ConditionalSplit {
            scc: solution
                .members
                .iter()
                .filter_map(|node| names.get(&node.0).cloned())
                .collect(),
            preconditions: edge_breaks(solution, &names),
            resulting_files: u32::try_from(solution.production_sloc.div_ceil(cap))
                .unwrap_or(u32::MAX),
        })
        .collect()
}

/// Reports polarity-matrix breaches: production code depending on test code,
/// and test support depending on a test case.
pub(in crate::analyze) fn polarity_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let polarity_by_id: BTreeMap<u32, Polarity> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.polarity))
        .collect();
    let names = node_names(snapshot);

    ir.edges
        .iter()
        .filter_map(|edge| {
            let source_polarity = polarity_by_id.get(&edge.source.0)?;
            let target_polarity = polarity_by_id.get(&edge.target.0)?;
            let (source_word, target_word) = match (source_polarity, target_polarity) {
                (Polarity::Production, Polarity::TestCase | Polarity::TestSupport) => {
                    ("production symbol", "test code")
                }
                (Polarity::TestSupport, Polarity::TestCase) => ("test support", "test case"),
                _ => return None,
            };
            let source = names.get(&edge.source.0).cloned().unwrap_or_default();
            let target = names.get(&edge.target.0).cloned().unwrap_or_default();
            Some(Violation {
                kind: ViolationKind::Polarity,
                severity: Severity::Violation,
                detail: format!("{source_word} `{source}` depends on {target_word} `{target}`"),
                location: vec![source, target],
                break_suggestions: None,
                capacity: None,
            })
        })
        .collect()
}

/// Reports symbols whose declared visibility is wider than their derived scope.
pub(in crate::analyze) fn visibility_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let result = derive_visibility(&ir.containers, &ir.nodes, &ir.edges);
    let names = node_names(snapshot);

    result
        .findings
        .iter()
        .map(|finding| {
            let name = names.get(&finding.node.0).cloned().unwrap_or_default();
            Violation {
                kind: ViolationKind::Visibility,
                severity: Severity::Violation,
                detail: format!(
                    "`{name}` is exported at {:?} but needed only at {:?}",
                    finding.declared, finding.derived
                ),
                location: vec![name],
                break_suggestions: None,
                capacity: None,
            }
        })
        .collect()
}

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
pub(in crate::analyze) fn capacity_violations(
    tree: &ContainerNode,
    config: &AnalyzeConfig,
) -> Vec<Violation> {
    walk_all_capacity(tree, &config.profiles.anchored.capacity)
        .into_iter()
        .map(|(_, violation)| violation)
        .collect()
}

/// Counts immediate entries in each physical directory without changing the
/// source-root-transparent laminar tree. A directory binds its direct files
/// plus its distinct direct child directories; deeper descendants do not add
/// to an ancestor's count.
#[cfg(test)]
pub(in crate::analyze) fn physical_folder_entries(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
) -> BTreeMap<Vec<String>, u32> {
    physical_folder_entries_with_rootedness(tree, namespaces, true)
}

pub(in crate::analyze) fn physical_folder_entries_repository_relative(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
) -> BTreeMap<Vec<String>, u32> {
    physical_folder_entries_with_rootedness(tree, namespaces, false)
}

pub(in crate::analyze) fn physical_folder_entries_with_rootedness(
    tree: &ContainerTree,
    namespaces: Option<&BTreeMap<ContainerId, SmolStr>>,
    package_rooted: bool,
) -> BTreeMap<Vec<String>, u32> {
    let by_id: BTreeMap<u32, &Container> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let mut direct_files: BTreeMap<Vec<String>, u32> = BTreeMap::new();
    let mut child_folders: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();

    for file in tree
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
    {
        let mut ancestor = Some(file);
        let mut dataset = None;
        while let Some(current) = ancestor {
            if current.level == ScopeLevel::PackageGroup {
                dataset = Some(current.name.as_str());
            }
            ancestor = current
                .parent
                .and_then(|parent| by_id.get(&parent.0).copied());
        }
        let dataset = dataset.unwrap_or("");
        let dataset_segments: Vec<String> = dataset
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        let folder = file.parent.and_then(|parent| by_id.get(&parent.0).copied());
        let mut logical: Vec<String> = folder
            .into_iter()
            .flat_map(|folder| folder.name.split('/'))
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        if folder.is_some_and(|folder| folder.synthetic) {
            logical.clear();
        }
        if logical.starts_with(&dataset_segments) {
            logical.drain(..dataset_segments.len());
        }
        let directory = if let Some(namespace) = namespaces.and_then(|values| values.get(&file.id))
        {
            let mut relative: Vec<String> = namespace
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect();
            relative.extend(logical);
            if package_rooted {
                project_package_rooted_path(dataset, &relative)
            } else {
                project_physical_path(dataset, &relative)
            }
        } else {
            let raw_directory: Vec<String> = file
                .name
                .rsplit_once('/')
                .map_or("", |(directory, _)| directory)
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect();
            if package_rooted {
                project_package_rooted_path(dataset, &raw_directory)
            } else {
                project_physical_path(dataset, &raw_directory)
            }
        };
        *direct_files.entry(directory.clone()).or_default() += 1;

        let root_depth = dataset_segments.len();
        for depth in root_depth.max(1)..directory.len() {
            let (Some(parent), Some(child)) = (directory.get(..depth), directory.get(depth)) else {
                continue;
            };
            child_folders
                .entry(parent.to_vec())
                .or_default()
                .insert(child.clone());
        }
    }

    let mut entries = direct_files;
    for (folder, children) in child_folders {
        let child_count = u32::try_from(children.len()).unwrap_or(u32::MAX);
        *entries.entry(folder).or_default() += child_count;
    }
    entries
}

pub(in crate::analyze) fn physical_folder_findings(
    entries: &BTreeMap<Vec<String>, u32>,
    cap: u32,
) -> Vec<Violation> {
    entries
        .iter()
        .filter_map(|(path, &measure)| {
            if measure <= cap {
                return None;
            }
            let severity =
                if f64::from(measure) > f64::from(cap) * (1.0 + BORDERLINE_CAPACITY_MARGIN) {
                    Severity::Violation
                } else {
                    Severity::Borderline
                };
            let qualified = path.join("/");
            Some(Violation {
                kind: ViolationKind::Capacity,
                severity,
                location: path.clone(),
                detail: format!("folder `{qualified}` holds {measure} against a cap of {cap}"),
                break_suggestions: None,
                capacity: Some(CapacityBreach {
                    measured: measure,
                    cap,
                    path: None,
                }),
            })
        })
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
pub(in crate::analyze) fn walk_capacity(
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
pub(in crate::analyze) fn append_display_segments(path: &mut Vec<String>, node: &ContainerNode) {
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
pub(in crate::analyze) fn count_bound_members(
    node: &ContainerNode,
    member_level: Level,
) -> (u32, bool) {
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
pub(in crate::analyze) fn file_child_count(node: &ContainerNode) -> u32 {
    let entries = node.children.as_ref().map_or(0, Vec::len);
    u32::try_from(entries).unwrap_or(u32::MAX)
}

/// Builds a capacity finding if `measure` is at or over the borderline band of
/// `cap`, classifying borderline (within ±10%) versus a hard breach.
pub(in crate::analyze) fn capacity_finding(
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
pub(in crate::analyze) fn level_word(level: Level) -> &'static str {
    match level {
        Level::File => "file",
        Level::Folder => "folder",
        Level::Domain => "domain",
        Level::Package => "package",
        Level::PackageGroup => "package group",
    }
}
