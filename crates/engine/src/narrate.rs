//! File-identity narration: explaining a candidate's moves against the current
//! tree.
//!
//! Narration diffs the two trees at *file* granularity — a file container's full
//! path is its stable identity across both trees, so no cross-tree container-id
//! matching is ever attempted. A file has moved exactly when its folded parent
//! path differs between the trees. Moved files are grouped by their destination
//! folder (and, for spec files, by the subject they follow), each group is
//! classified as a move, split, or merge at folder granularity, and stamped with
//! a computed reason: following a subject, relieving an over-cap folder, being
//! pulled by a dependency partner, naming cohesion with the destination, or the
//! clustering fallback.
//!
//! A file's destination is the key of its folder container — a package-qualified
//! real directory path — read straight from the tree. The domain, package, and
//! group labels above the folder are display groupings, never path components, so
//! a move target only ever names a real, `mv`-able directory.

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{ContainerTree, ScopeLevel};

use crate::result::{FileMove, Move, MoveKind, MoveReason};

/// Per-file facts narration consults when explaining a move.
///
/// File paths are the file containers' full names — the same identity the
/// narration diffs on. Weights are the config-priced edge weights summed per
/// directed file pair.
pub(crate) struct FileFacts {
    /// Summed edge weight per directed file pair, keyed `(source, target)`.
    pub(crate) edge_weights: BTreeMap<(String, String), f64>,
    /// Files whose symbols are exclusively test cases (spec files).
    pub(crate) test_case_files: BTreeSet<String>,
    /// The rest of the tie-cut zone — `[tests]`-pattern matches and
    /// test-support helpers beyond the case-only set. Their moves narrate as
    /// following a subject exactly like spec files do.
    pub(crate) shadow_test_files: BTreeSet<String>,
    /// The folder member cap, for the cap-relief reason.
    pub(crate) folder_cap: u32,
}

/// Splits a path's basename into lowercase naming tokens.
///
/// The final extension is dropped, then the stem splits on every
/// non-alphanumeric separator and every lower-to-upper camel-case boundary:
/// `user-service.ts` and `UserService.spec.ts` share `{user, service}`. This is
/// the one tokenizer both the naming-cohesion objective term and the narration
/// reason consult, so the two never disagree about similarity.
pub(crate) fn tokenize(name: &str) -> BTreeSet<String> {
    let base = basename(name);
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);

    let mut tokens = BTreeSet::new();
    let mut current = String::new();
    let mut previous_was_lower = false;
    for ch in stem.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && previous_was_lower && !current.is_empty() {
                tokens.insert(current.to_lowercase());
                current = String::new();
            }
            current.push(ch);
            previous_was_lower = ch.is_lowercase() || ch.is_numeric();
        } else {
            if !current.is_empty() {
                tokens.insert(current.to_lowercase());
                current = String::new();
            }
            previous_was_lower = false;
        }
    }
    if !current.is_empty() {
        tokens.insert(current.to_lowercase());
    }
    tokens
}

/// Narrates the moves of `candidate` against `current`, grouped and explained.
///
/// Files present in both trees whose folded parent paths differ are the moved
/// set. Groups key on `(destination folder, followed subject)` so specs
/// trailing different subjects into one folder narrate separately; groups (and
/// the files inside them) come out in deterministic lexicographic order.
/// Each `FileMove` carries a moved *file path* and its source folder — symbol
/// granularity arrives with the pack phase.
pub(crate) fn narrate(
    current: &ContainerTree,
    candidate: &ContainerTree,
    facts: &FileFacts,
) -> Vec<Move> {
    let before = index_files(current);
    let mut after = index_files(candidate);
    for (file, logical) in &after.logical_parent_of {
        let Some(namespace) = before.namespace_of.get(file) else {
            continue;
        };
        let mut physical = namespace.clone();
        physical.extend(logical.iter().cloned());
        after.parent_of.insert(file.clone(), physical);
    }
    after.members_of.clear();
    for (file, parent) in &after.parent_of {
        after
            .members_of
            .entry(parent.clone())
            .or_default()
            .push(file.clone());
    }
    for members in after.members_of.values_mut() {
        members.sort();
    }
    after.entry_count = folder_entry_counts(&after.parent_of);

    // the moved set: (file, origin, destination) with a changed folded parent.
    let mut moved: Vec<(&String, &Vec<String>, &Vec<String>)> = Vec::new();
    for (file, destination) in &after.parent_of {
        let Some(origin) = before.parent_of.get(file) else {
            continue;
        };
        if origin != destination {
            moved.push((file, origin, destination));
        }
    }

    // folder-granular scatter: which destinations each source folder feeds.
    let mut dests_of: BTreeMap<&Vec<String>, BTreeSet<&Vec<String>>> = BTreeMap::new();
    for &(_, origin, destination) in &moved {
        dests_of.entry(origin).or_default().insert(destination);
    }

    // group by (destination, followed subject); the BTreeMap orders groups by
    // folded destination path, then subject.
    let mut groups: BTreeMap<(&Vec<String>, Option<String>), GroupAccumulator> = BTreeMap::new();
    for &(file, origin, destination) in &moved {
        let follows = followed_subject(file, destination, &after, facts);
        let group = groups.entry((destination, follows)).or_default();
        group.files.push((file, origin));
        group.origins.insert(origin);
    }

    groups
        .into_iter()
        .map(|((destination, follows), group)| {
            let file_refs: Vec<&String> = group.files.iter().map(|&(file, _)| file).collect();
            let kind = group_kind(&group.origins, &dests_of);
            let reason = group_reason(&GroupContext {
                files: &file_refs,
                origins: &group.origins,
                destination,
                follows: follows.as_deref(),
                before: &before,
                after: &after,
                facts,
            });
            let mut files: Vec<FileMove> = group
                .files
                .iter()
                .map(|&(file, origin)| FileMove {
                    path: file.clone(),
                    from: origin.join("/"),
                })
                .collect();
            files.sort_by(|left, right| left.path.cmp(&right.path));
            Move {
                kind,
                files,
                to: destination.join("/"),
                reason,
            }
        })
        .collect()
}

/// The moved files and source folders accumulated for one narration group.
#[derive(Default)]
struct GroupAccumulator<'a> {
    /// The moved (file path, folded source folder) pairs in this group.
    files: Vec<(&'a String, &'a Vec<String>)>,
    /// The distinct folded source folders the files left.
    origins: BTreeSet<&'a Vec<String>>,
}

/// Every file's placement within one tree, folded for display.
struct FilePlacements {
    /// Each file path's folded parent path.
    parent_of: BTreeMap<String, Vec<String>>,
    /// Each file's logical destination before physical namespace qualification.
    logical_parent_of: BTreeMap<String, Vec<String>>,
    /// Dataset and transparent physical-root segments retained across a move.
    namespace_of: BTreeMap<String, Vec<String>>,
    /// The (sorted) member file paths of each folded folder path.
    members_of: BTreeMap<Vec<String>, Vec<String>>,
    /// Direct files plus distinct direct child folders at each physical path.
    entry_count: BTreeMap<Vec<String>, u32>,
}

fn folder_entry_counts(parent_of: &BTreeMap<String, Vec<String>>) -> BTreeMap<Vec<String>, u32> {
    let mut counts = BTreeMap::new();
    let mut children: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    for folder in parent_of.values() {
        *counts.entry(folder.clone()).or_default() += 1;
        for depth in 1..folder.len() {
            let (Some(parent), Some(child)) = (folder.get(..depth), folder.get(depth)) else {
                continue;
            };
            children
                .entry(parent.to_vec())
                .or_default()
                .insert(child.clone());
        }
    }
    for (folder, names) in children {
        *counts.entry(folder).or_default() += u32::try_from(names.len()).unwrap_or(u32::MAX);
    }
    counts
}

/// Projects a repository-relative path beneath its package-group root.
///
/// Nested package roots can be present at the end of `root` and the beginning
/// of `relative`. The longest boundary overlap is emitted once; every other
/// segment keeps its original order.
pub(crate) fn project_physical_path(root: &str, relative: &[String]) -> Vec<String> {
    let root: Vec<String> = root
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect();
    let overlap = (0..=root.len().min(relative.len()))
        .rev()
        .find(|&length| root.get(root.len().saturating_sub(length)..) == relative.get(..length))
        .unwrap_or(0);
    let mut projected = root;
    projected.extend(relative.iter().skip(overlap).cloned());
    projected
}

/// Indexes a tree's file containers by their real folder key.
///
/// A file's destination is the key of the folder container holding it — the real
/// directory the file lands in, and the path a move would target. Folder keys are
/// package-qualified real paths (`cts/core`, `nested-ts/geometry`), so they mv
/// cleanly. The domain/package/group above the folder are display groupings, not
/// path components: a suggested domain that merges several real directories has
/// no directory of its own to name, so it never contributes a destination
/// segment (and its injective display label never leaks into a move target).
fn index_files(tree: &ContainerTree) -> FilePlacements {
    let containers = tree.containers();
    let by_id: BTreeMap<u32, &strata_ir::Container> = containers
        .iter()
        .map(|container| (container.id.0, container))
        .collect();

    let mut parent_of = BTreeMap::new();
    let mut logical_parent_of = BTreeMap::new();
    let mut namespace_of = BTreeMap::new();
    let mut members_of: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
    for container in containers {
        if container.level != ScopeLevel::File {
            continue;
        }
        let folder = container.parent.and_then(|parent| by_id.get(&parent.0));
        let folder_key = folder.map_or("", |folder| folder.name.as_str());
        let mut logical_segments: Vec<String> = folder_key
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        // the synthetic `workspace` bucket names no real directory, so a
        // root-level file's move target is its package, not an invented
        // `.../workspace` path: drop the trailing synthetic segment.
        if folder.is_some_and(|folder| folder.synthetic) {
            logical_segments.pop();
        }
        let mut ancestor = Some(container);
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
        let dataset_segments: Vec<&str> = dataset
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        if logical_segments.len() >= dataset_segments.len()
            && logical_segments
                .iter()
                .map(String::as_str)
                .zip(dataset_segments.iter().copied())
                .all(|(left, right)| left == right)
        {
            logical_segments.drain(..dataset_segments.len());
        }
        let raw_segments: Vec<String> = container
            .name
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        let physical_file_segments = project_physical_path(dataset, &raw_segments);
        let raw_directory = physical_file_segments
            .get(..physical_file_segments.len().saturating_sub(1))
            .unwrap_or_default();
        let common_suffix = raw_directory
            .iter()
            .rev()
            .zip(logical_segments.iter().rev())
            .take_while(|(left, right)| **left == right.as_str())
            .count();
        let namespace_end = raw_directory.len().saturating_sub(common_suffix);
        let namespace = raw_directory
            .get(..namespace_end)
            .unwrap_or_default()
            .to_vec();
        let mut segments = namespace.clone();
        segments.extend(logical_segments);
        let physical_file = physical_file_segments.join("/");
        parent_of.insert(physical_file.clone(), segments.clone());
        logical_parent_of.insert(
            physical_file.clone(),
            segments.get(namespace.len()..).unwrap_or_default().to_vec(),
        );
        namespace_of.insert(physical_file.clone(), namespace);
        members_of.entry(segments).or_default().push(physical_file);
    }
    for members in members_of.values_mut() {
        members.sort();
    }
    let entry_count = folder_entry_counts(&parent_of);
    FilePlacements {
        parent_of,
        logical_parent_of,
        namespace_of,
        members_of,
        entry_count,
    }
}

/// Returns the subject a moved spec file follows, when it follows one.
///
/// A spec file's subject is the production file receiving its largest summed
/// outgoing edge weight (ties to the lexicographically smaller path, which the
/// ascending map order yields for free). The move "follows" the subject when
/// both land in the same candidate folder, or when the spec's destination
/// folder hangs directly inside the container holding the subject — the
/// candidate regrouped the spec's real folder to sit beside its subject, which
/// is the same intent one level up.
fn followed_subject(
    file: &str,
    destination: &[String],
    after: &FilePlacements,
    facts: &FileFacts,
) -> Option<String> {
    if !facts.test_case_files.contains(file) && !facts.shadow_test_files.contains(file) {
        return None;
    }
    let is_test =
        |path: &str| facts.test_case_files.contains(path) || facts.shadow_test_files.contains(path);
    let mut best: Option<(&String, f64)> = None;
    for ((source, target), weight) in &facts.edge_weights {
        if source.as_str() != file || is_test(target) {
            continue;
        }
        // strictly-greater keeps the earlier (lexicographically smaller) target
        // on a weight tie.
        let replace = best.as_ref().is_none_or(|(_, top)| *weight > *top);
        if replace && *weight > 0.0 {
            best = Some((target, *weight));
        }
    }
    let (subject, _) = best?;
    let home = after.parent_of.get(subject).map(Vec::as_slice)?;
    let same_folder = home == destination;
    let beside_subject = destination
        .split_last()
        .is_some_and(|(_, container)| home == container);
    (same_folder || beside_subject).then(|| subject.clone())
}

/// Classifies a group at folder granularity: two or more sources converging on
/// one destination merge; one source scattering to two or more destinations
/// splits; anything else is a plain move.
fn group_kind(
    origins: &BTreeSet<&Vec<String>>,
    dests_of: &BTreeMap<&Vec<String>, BTreeSet<&Vec<String>>>,
) -> MoveKind {
    if origins.len() >= 2 {
        return MoveKind::Merge;
    }
    let scatter = origins
        .iter()
        .next()
        .and_then(|origin| dests_of.get(*origin))
        .map_or(0, BTreeSet::len);
    if scatter >= 2 {
        MoveKind::Split
    } else {
        MoveKind::Move
    }
}

/// Everything a group's reason computation reads.
struct GroupContext<'a> {
    /// The moved file paths in the group.
    files: &'a [&'a String],
    /// The distinct folded source folders.
    origins: &'a BTreeSet<&'a Vec<String>>,
    /// The folded destination folder.
    destination: &'a Vec<String>,
    /// The followed subject path, when the group follows one.
    follows: Option<&'a str>,
    /// File placements in the current tree.
    before: &'a FilePlacements,
    /// File placements in the candidate tree.
    after: &'a FilePlacements,
    /// The per-file facts.
    facts: &'a FileFacts,
}

/// Threshold on the mean pairwise basename-token Jaccard above which a group's
/// move narrates as naming cohesion with its destination.
const NAMING_COHESION_THRESHOLD: f64 = 0.5;

/// Computes a group's dominant reason, first match wins: followed subject >
/// cap relief > dependency pull > naming cohesion > clustering fallback.
fn group_reason(ctx: &GroupContext<'_>) -> MoveReason {
    if let Some(subject) = ctx.follows {
        return MoveReason::Follows {
            subject: subject.to_owned(),
        };
    }

    // cap relief: among the over-cap source folders, attribute to the one that
    // contributes the most files to *this* group — the dominant contributor —
    // not the lexicographically-first origin, which may barely feature in the
    // move. Ties keep the first (lexicographic) origin, since `origins` iterates
    // in sorted order.
    let mut dominant: Option<(&Vec<String>, usize, u32)> = None;
    for origin in ctx.origins {
        let Some(members) = ctx.before.members_of.get(*origin) else {
            continue;
        };
        let size = ctx
            .before
            .entry_count
            .get(*origin)
            .copied()
            .unwrap_or_else(|| u32::try_from(members.len()).unwrap_or(u32::MAX));
        if size <= ctx.facts.folder_cap {
            continue;
        }
        let contributed = ctx
            .files
            .iter()
            .filter(|&&file| members.contains(file))
            .count();
        if dominant.is_none_or(|(_, top, _)| contributed > top) {
            dominant = Some((*origin, contributed, size));
        }
    }
    if let Some((origin, _, size)) = dominant {
        return MoveReason::RelievesOverCap {
            container: origin.join("/"),
            count: size,
            cap: ctx.facts.folder_cap,
        };
    }

    // residents: destination files that are not part of this group.
    let residents: Vec<&String> =
        ctx.after
            .members_of
            .get(ctx.destination)
            .map_or_else(Vec::new, |members| {
                members
                    .iter()
                    .filter(|member| ctx.files.iter().all(|file| *file != *member))
                    .collect()
            });

    // dependency pull: the resident with the strongest two-way tie to the group.
    let mut best: Option<(&String, f64)> = None;
    for resident in &residents {
        let mut total = 0.0;
        for file in ctx.files {
            let outgoing = ((*file).clone(), (*resident).clone());
            let incoming = ((*resident).clone(), (*file).clone());
            total += ctx
                .facts
                .edge_weights
                .get(&outgoing)
                .copied()
                .unwrap_or(0.0);
            total += ctx
                .facts
                .edge_weights
                .get(&incoming)
                .copied()
                .unwrap_or(0.0);
        }
        let replace = best.as_ref().is_none_or(|(_, top)| total > *top);
        if replace && total > 0.0 {
            best = Some((*resident, total));
        }
    }
    if let Some((partner, total)) = best {
        return MoveReason::PulledBy {
            partner: partner.clone(),
            weight: total,
        };
    }

    // naming cohesion: mean pairwise Jaccard between moved and resident stems.
    if !residents.is_empty() {
        let mut sum = 0.0;
        let mut pairs = 0_u32;
        for file in ctx.files {
            let file_tokens = tokenize(file);
            for resident in &residents {
                sum += jaccard(&file_tokens, &tokenize(resident));
                pairs = pairs.saturating_add(1);
            }
        }
        let mean = if pairs == 0 {
            0.0
        } else {
            sum / f64::from(pairs)
        };
        if mean >= NAMING_COHESION_THRESHOLD {
            return MoveReason::NamingCohesion { cohesion: mean };
        }
    }

    MoveReason::Clustering
}

/// Returns the Jaccard similarity of two token sets (`0.0` when both are empty).
fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    let intersection = left.intersection(right).count();
    let union = left.union(right).count();
    if union == 0 {
        return 0.0;
    }
    let intersection = f64::from(u32::try_from(intersection).unwrap_or(u32::MAX));
    let union = f64::from(u32::try_from(union).unwrap_or(u32::MAX));
    intersection / union
}

/// Returns the final path segment.
pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use strata_ir::{Container, ContainerId};

    use super::*;

    /// Builds a container at a level with an optional parent.
    fn container(id: u32, name: &str, level: ScopeLevel, parent: Option<u32>) -> Container {
        Container {
            id: ContainerId(id),
            name: SmolStr::new(name),
            level,
            parent: parent.map(ContainerId),
            synthetic: false,
        }
    }

    /// A domain root `app` over two folders `src/core` and `src/io` holding the
    /// given files (cumulative names, as interning produces them).
    fn two_folder_tree(core_files: &[&str], io_files: &[&str]) -> ContainerTree {
        let mut containers = vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/core", ScopeLevel::Folder, Some(0)),
            container(2, "src/io", ScopeLevel::Folder, Some(0)),
        ];
        let mut next = 3;
        for file in core_files {
            containers.push(container(next, file, ScopeLevel::File, Some(1)));
            next += 1;
        }
        for file in io_files {
            containers.push(container(next, file, ScopeLevel::File, Some(2)));
            next += 1;
        }
        ContainerTree::new(containers)
    }

    /// Facts with no edges, no specs, and a roomy folder cap.
    fn plain_facts() -> FileFacts {
        FileFacts {
            edge_weights: BTreeMap::new(),
            test_case_files: BTreeSet::new(),
            shadow_test_files: BTreeSet::new(),
            folder_cap: 15,
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn should_tokenize_basenames_on_separators_and_camel_case() {
        let kebab = tokenize("src/core/user-service.ts");
        let camel = tokenize("UserService.spec.ts");

        assert_eq!(kebab, strings(&["user", "service"]).into_iter().collect());
        assert_eq!(
            camel,
            strings(&["user", "service", "spec"]).into_iter().collect()
        );
    }

    #[test]
    fn should_compose_a_destination_from_the_folder_key_not_a_decorated_domain_label() {
        // when clustering merges several real domains into one suggested domain,
        // the elected domain label decorates to `cts (cts.core)` to stay
        // injective. That label is a display overlay, never a path component:
        // the move destination is the folder's own key, the real directory the
        // file lands in. The old fold treated the decorated label as a path
        // ancestor and re-embedded the whole folder key beneath it.
        let current = ContainerTree::new(vec![
            container(0, "cts", ScopeLevel::PackageGroup, None),
            container(1, "cts", ScopeLevel::Package, Some(0)),
            container(2, "cts/util", ScopeLevel::Domain, Some(1)),
            container(3, "cts/util", ScopeLevel::Folder, Some(2)),
            container(4, "src/util/clamp.ts", ScopeLevel::File, Some(3)),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "cts", ScopeLevel::PackageGroup, None),
            container(1, "cts", ScopeLevel::Package, Some(0)),
            container(2, "cts (cts.core)", ScopeLevel::Domain, Some(1)),
            container(3, "cts/core", ScopeLevel::Folder, Some(2)),
            container(4, "src/util/clamp.ts", ScopeLevel::File, Some(3)),
        ]);

        let moves = narrate(&current, &candidate, &plain_facts());

        assert_eq!(moves.len(), 1);
        let entry = moves.first();
        assert_eq!(entry.map(|m| m.to.clone()), Some("cts/src/core".to_owned()));
        assert_eq!(
            entry.map(|m| m.files.iter().map(|f| f.from.clone()).collect::<Vec<_>>()),
            Some(strings(&["cts/src/util"]))
        );
    }

    #[test]
    fn should_group_files_sharing_a_destination_into_one_move() {
        let current = two_folder_tree(&["src/core/a.ts", "src/core/b.ts"], &[]);
        let candidate = two_folder_tree(&[], &["src/core/a.ts", "src/core/b.ts"]);

        let moves = narrate(&current, &candidate, &plain_facts());

        assert_eq!(moves.len(), 1);
        let entry = moves.first();
        assert_eq!(
            entry.map(|m| m.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()),
            Some(strings(&["src/core/a.ts", "src/core/b.ts"]))
        );
        assert_eq!(
            entry.map(|m| m.files.iter().map(|f| f.from.clone()).collect::<Vec<_>>()),
            Some(strings(&["src/core", "src/core"]))
        );
        assert_eq!(entry.map(|m| m.to.clone()), Some("src/io".to_owned()));
        assert_eq!(entry.map(|m| m.kind), Some(MoveKind::Move));
    }

    #[test]
    fn should_preserve_physical_namespaces_in_dataset_qualified_moves() {
        let current = ContainerTree::new(vec![
            container(0, "sample", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Domain, Some(0)),
            container(2, "area/origin", ScopeLevel::Folder, Some(1)),
            container(3, "area/destination", ScopeLevel::Folder, Some(1)),
            container(4, "src/area/origin/item.ts", ScopeLevel::File, Some(2)),
            container(
                5,
                "spec/area/origin/item.spec.ts",
                ScopeLevel::File,
                Some(2),
            ),
            container(
                6,
                "src/area/destination/anchor.ts",
                ScopeLevel::File,
                Some(3),
            ),
            container(
                7,
                "spec/area/destination/anchor.spec.ts",
                ScopeLevel::File,
                Some(3),
            ),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "sample", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Domain, Some(0)),
            container(2, "area/origin", ScopeLevel::Folder, Some(1)),
            container(3, "area/destination", ScopeLevel::Folder, Some(1)),
            container(4, "src/area/origin/item.ts", ScopeLevel::File, Some(3)),
            container(
                5,
                "spec/area/origin/item.spec.ts",
                ScopeLevel::File,
                Some(3),
            ),
            container(
                6,
                "src/area/destination/anchor.ts",
                ScopeLevel::File,
                Some(3),
            ),
            container(
                7,
                "spec/area/destination/anchor.spec.ts",
                ScopeLevel::File,
                Some(3),
            ),
        ]);

        let moves = narrate(&current, &candidate, &plain_facts());

        assert_eq!(moves.len(), 2, "physical namespaces narrate separately");
        let physical_moves: Vec<(String, String, String)> = moves
            .iter()
            .filter_map(|entry| {
                entry
                    .files
                    .first()
                    .map(|file| (file.path.clone(), file.from.clone(), entry.to.clone()))
            })
            .collect();
        assert_eq!(
            physical_moves,
            vec![
                (
                    "sample/spec/area/origin/item.spec.ts".to_owned(),
                    "sample/spec/area/origin".to_owned(),
                    "sample/spec/area/destination".to_owned(),
                ),
                (
                    "sample/src/area/origin/item.ts".to_owned(),
                    "sample/src/area/origin".to_owned(),
                    "sample/src/area/destination".to_owned(),
                ),
            ]
        );
    }

    #[test]
    fn should_order_nested_package_and_transparent_roots_once_in_narration() {
        let current = ContainerTree::new(vec![
            container(0, "workspace/packages/unit", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "target", ScopeLevel::Folder, Some(0)),
            container(
                3,
                "packages/unit/left/area/item.ts",
                ScopeLevel::File,
                Some(1),
            ),
            container(
                4,
                "packages/unit/left/target/anchor.ts",
                ScopeLevel::File,
                Some(2),
            ),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "workspace/packages/unit", ScopeLevel::PackageGroup, None),
            container(1, "area", ScopeLevel::Folder, Some(0)),
            container(2, "target", ScopeLevel::Folder, Some(0)),
            container(
                3,
                "packages/unit/left/area/item.ts",
                ScopeLevel::File,
                Some(2),
            ),
            container(
                4,
                "packages/unit/left/target/anchor.ts",
                ScopeLevel::File,
                Some(2),
            ),
        ]);

        let moves = narrate(&current, &candidate, &plain_facts());
        let moved = moves.first().and_then(|entry| {
            entry
                .files
                .first()
                .map(|file| (file.path.as_str(), file.from.as_str(), entry.to.as_str()))
        });

        assert_eq!(
            moved,
            Some((
                "workspace/packages/unit/left/area/item.ts",
                "workspace/packages/unit/left/area",
                "workspace/packages/unit/left/target",
            ))
        );
    }

    #[test]
    fn should_classify_a_two_source_destination_as_a_merge() {
        // one file from each folder converges on a third folder.
        let current = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/core", ScopeLevel::Folder, Some(0)),
            container(2, "src/io", ScopeLevel::Folder, Some(0)),
            container(3, "src/merged", ScopeLevel::Folder, Some(0)),
            container(4, "src/core/a.ts", ScopeLevel::File, Some(1)),
            container(5, "src/io/b.ts", ScopeLevel::File, Some(2)),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/core", ScopeLevel::Folder, Some(0)),
            container(2, "src/io", ScopeLevel::Folder, Some(0)),
            container(3, "src/merged", ScopeLevel::Folder, Some(0)),
            container(4, "src/core/a.ts", ScopeLevel::File, Some(3)),
            container(5, "src/io/b.ts", ScopeLevel::File, Some(3)),
        ]);

        let moves = narrate(&current, &candidate, &plain_facts());

        assert_eq!(moves.len(), 1);
        let entry = moves.first();
        assert_eq!(entry.map(|m| m.kind), Some(MoveKind::Merge));
        // per-file sources are retained: a.ts leaves core, b.ts leaves io.
        let mut origins = entry
            .map(|m| m.files.iter().map(|f| f.from.clone()).collect::<Vec<_>>())
            .unwrap_or_default();
        origins.sort();
        assert_eq!(origins, strings(&["src/core", "src/io"]));
    }

    #[test]
    fn should_classify_a_scattering_source_as_a_split() {
        // both files leave `src/core` for two different folders.
        let current = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/core", ScopeLevel::Folder, Some(0)),
            container(2, "src/io", ScopeLevel::Folder, Some(0)),
            container(3, "src/net", ScopeLevel::Folder, Some(0)),
            container(4, "src/core/a.ts", ScopeLevel::File, Some(1)),
            container(5, "src/core/b.ts", ScopeLevel::File, Some(1)),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/core", ScopeLevel::Folder, Some(0)),
            container(2, "src/io", ScopeLevel::Folder, Some(0)),
            container(3, "src/net", ScopeLevel::Folder, Some(0)),
            container(4, "src/core/a.ts", ScopeLevel::File, Some(2)),
            container(5, "src/core/b.ts", ScopeLevel::File, Some(3)),
        ]);

        let moves = narrate(&current, &candidate, &plain_facts());

        assert_eq!(moves.len(), 2);
        assert!(moves.iter().all(|entry| entry.kind == MoveKind::Split));
    }

    #[test]
    fn should_follow_a_spec_files_subject_into_its_folder() {
        // the spec starts in io, its subject lives in core; the candidate brings
        // the spec to the subject.
        let current = two_folder_tree(&["src/core/app.ts"], &["src/io/app.spec.ts"]);
        let candidate = two_folder_tree(&["src/core/app.ts", "src/io/app.spec.ts"], &[]);
        let facts = FileFacts {
            edge_weights: [(
                (
                    "src/io/app.spec.ts".to_owned(),
                    "src/core/app.ts".to_owned(),
                ),
                2.0,
            )]
            .into_iter()
            .collect(),
            test_case_files: ["src/io/app.spec.ts".to_owned()].into_iter().collect(),
            shadow_test_files: BTreeSet::new(),
            folder_cap: 15,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert_eq!(moves.len(), 1);
        let entry = moves.first();
        assert_eq!(
            entry.map(|m| m.reason.clone()),
            Some(MoveReason::Follows {
                subject: "src/core/app.ts".to_owned(),
            })
        );
        assert_eq!(
            entry.map(|m| m.reason.to_string()),
            Some("follows app.ts".to_owned())
        );
    }

    #[test]
    fn should_narrate_a_co_moved_spec_as_following_its_twin() {
        // Subject and spec start together in `src/core`; the candidate moves
        // both into the new nested folder `src/core/app`. Each role narrates on
        // its own: the twin is pulled by its spec, the spec follows back.
        let start = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/core", ScopeLevel::Folder, Some(0)),
            container(2, "src/io", ScopeLevel::Folder, Some(0)),
            container(3, "src/core/app.ts", ScopeLevel::File, Some(1)),
            container(4, "src/core/app.spec.ts", ScopeLevel::File, Some(1)),
            container(5, "src/io/keep.ts", ScopeLevel::File, Some(2)),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/core", ScopeLevel::Folder, Some(0)),
            container(2, "src/io", ScopeLevel::Folder, Some(0)),
            container(3, "src/core/app", ScopeLevel::Folder, Some(1)),
            container(4, "src/core/app.ts", ScopeLevel::File, Some(3)),
            container(5, "src/core/app.spec.ts", ScopeLevel::File, Some(3)),
            container(6, "src/io/keep.ts", ScopeLevel::File, Some(2)),
        ]);
        let facts = FileFacts {
            edge_weights: [(
                (
                    "src/core/app.spec.ts".to_owned(),
                    "src/core/app.ts".to_owned(),
                ),
                2.0,
            )]
            .into_iter()
            .collect(),
            test_case_files: ["src/core/app.spec.ts".to_owned()].into_iter().collect(),
            shadow_test_files: BTreeSet::new(),
            folder_cap: 15,
        };

        let moves = narrate(&start, &candidate, &facts);

        assert_eq!(moves.len(), 2, "each role narrates as its own entry");
        let followed = moves.iter().find(|entry| {
            entry
                .files
                .iter()
                .any(|file| file.path.ends_with(".spec.ts"))
        });
        assert_eq!(
            followed.map(|entry| entry.reason.to_string()),
            Some("follows app.ts".to_owned())
        );
    }

    #[test]
    fn should_narrate_a_shadow_test_file_as_following_its_subject() {
        // a `[tests]`-pattern match that is not case-only polarity — a test
        // support file — narrates exactly like a spec: it follows its subject.
        let current = two_folder_tree(&["src/core/app.ts"], &["src/io/app.test-utils.ts"]);
        let candidate = two_folder_tree(&["src/core/app.ts", "src/io/app.test-utils.ts"], &[]);
        let facts = FileFacts {
            // narration reads the uncut IR weights, not the tie-cut graph.
            edge_weights: [(
                (
                    "src/io/app.test-utils.ts".to_owned(),
                    "src/core/app.ts".to_owned(),
                ),
                2.0,
            )]
            .into_iter()
            .collect(),
            test_case_files: BTreeSet::new(),
            shadow_test_files: ["src/io/app.test-utils.ts".to_owned()]
                .into_iter()
                .collect(),
            folder_cap: 15,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert_eq!(moves.len(), 1);
        assert_eq!(
            moves.first().map(|m| m.reason.clone()),
            Some(MoveReason::Follows {
                subject: "src/core/app.ts".to_owned(),
            })
        );
    }

    #[test]
    fn should_not_move_a_spec_when_only_its_domain_label_is_regrouped() {
        // the spec's real folder (`nested-ts/__tests__`) is unchanged; only the
        // domain node above it is relabelled as the clusterer regroups the folder
        // to display beside its subject's domain. A domain label is a display
        // overlay, never a path component, so the spec does not move on disk —
        // the regrouping shows as a heading, it is not narrated as a file move.
        let current = ContainerTree::new(vec![
            container(0, "nested-ts", ScopeLevel::Domain, None),
            container(1, "nested-ts/geometry", ScopeLevel::Folder, Some(0)),
            container(2, "nested-ts/__tests__", ScopeLevel::Folder, Some(0)),
            container(3, "app.ts", ScopeLevel::File, Some(1)),
            container(4, "app.spec.ts", ScopeLevel::File, Some(2)),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "nested-ts/geometry", ScopeLevel::Domain, None),
            container(1, "nested-ts/geometry", ScopeLevel::Folder, Some(0)),
            container(2, "nested-ts/__tests__", ScopeLevel::Folder, Some(0)),
            container(3, "app.ts", ScopeLevel::File, Some(1)),
            container(4, "app.spec.ts", ScopeLevel::File, Some(2)),
        ]);
        let facts = FileFacts {
            edge_weights: [(("app.spec.ts".to_owned(), "app.ts".to_owned()), 2.0)]
                .into_iter()
                .collect(),
            test_case_files: ["app.spec.ts".to_owned()].into_iter().collect(),
            shadow_test_files: BTreeSet::new(),
            folder_cap: 15,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert!(moves.is_empty());
    }

    #[test]
    fn should_explain_a_move_out_of_an_over_cap_folder() {
        let core_files: Vec<String> = (0..4).map(|i| format!("src/core/f{i}.ts")).collect();
        let core_refs: Vec<&str> = core_files.iter().map(String::as_str).collect();
        let current = two_folder_tree(&core_refs, &[]);
        // the first file relocates to io; core held 4 files against a cap of 3.
        let mut moved_core = core_refs.clone();
        let mover = moved_core.remove(0);
        let candidate = two_folder_tree(&moved_core, &[mover]);
        let facts = FileFacts {
            edge_weights: BTreeMap::new(),
            test_case_files: BTreeSet::new(),
            shadow_test_files: BTreeSet::new(),
            folder_cap: 3,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert_eq!(
            moves.first().map(|m| m.reason.clone()),
            Some(MoveReason::RelievesOverCap {
                container: "src/core".to_owned(),
                count: 4,
                cap: 3,
            })
        );
        assert_eq!(
            moves.first().map(|m| m.reason.to_string()),
            Some("relieves over-cap folder src/core (4/3 entries)".to_owned())
        );
    }

    #[test]
    fn should_attribute_cap_relief_to_the_dominant_over_cap_origin() {
        // two over-cap folders merge into a third; `src/zebra` sheds three files
        // while `src/alpha` sheds one. Attribution must name zebra — the dominant
        // contributor — not alpha, which sorts first but barely features.
        let current = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/alpha", ScopeLevel::Folder, Some(0)),
            container(2, "src/zebra", ScopeLevel::Folder, Some(0)),
            container(3, "src/dest", ScopeLevel::Folder, Some(0)),
            container(4, "src/alpha/a0.ts", ScopeLevel::File, Some(1)),
            container(5, "src/alpha/a1.ts", ScopeLevel::File, Some(1)),
            container(6, "src/alpha/a2.ts", ScopeLevel::File, Some(1)),
            container(7, "src/alpha/a3.ts", ScopeLevel::File, Some(1)),
            container(8, "src/zebra/z0.ts", ScopeLevel::File, Some(2)),
            container(9, "src/zebra/z1.ts", ScopeLevel::File, Some(2)),
            container(10, "src/zebra/z2.ts", ScopeLevel::File, Some(2)),
            container(11, "src/zebra/z3.ts", ScopeLevel::File, Some(2)),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "app", ScopeLevel::Domain, None),
            container(1, "src/alpha", ScopeLevel::Folder, Some(0)),
            container(2, "src/zebra", ScopeLevel::Folder, Some(0)),
            container(3, "src/dest", ScopeLevel::Folder, Some(0)),
            container(4, "src/alpha/a0.ts", ScopeLevel::File, Some(3)),
            container(5, "src/alpha/a1.ts", ScopeLevel::File, Some(1)),
            container(6, "src/alpha/a2.ts", ScopeLevel::File, Some(1)),
            container(7, "src/alpha/a3.ts", ScopeLevel::File, Some(1)),
            container(8, "src/zebra/z0.ts", ScopeLevel::File, Some(3)),
            container(9, "src/zebra/z1.ts", ScopeLevel::File, Some(3)),
            container(10, "src/zebra/z2.ts", ScopeLevel::File, Some(3)),
            container(11, "src/zebra/z3.ts", ScopeLevel::File, Some(2)),
        ]);
        let facts = FileFacts {
            edge_weights: BTreeMap::new(),
            test_case_files: BTreeSet::new(),
            shadow_test_files: BTreeSet::new(),
            folder_cap: 3,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert_eq!(
            moves.first().map(|m| m.reason.clone()),
            Some(MoveReason::RelievesOverCap {
                container: "src/zebra".to_owned(),
                count: 4,
                cap: 3,
            })
        );
    }

    #[test]
    fn should_explain_a_move_by_its_strongest_dependency_pull() {
        let current = two_folder_tree(&["src/core/engine.ts"], &["src/io/mover.ts"]);
        let candidate = two_folder_tree(&["src/core/engine.ts", "src/io/mover.ts"], &[]);
        let facts = FileFacts {
            edge_weights: [
                (
                    (
                        "src/io/mover.ts".to_owned(),
                        "src/core/engine.ts".to_owned(),
                    ),
                    1.5,
                ),
                (
                    (
                        "src/core/engine.ts".to_owned(),
                        "src/io/mover.ts".to_owned(),
                    ),
                    1.0,
                ),
            ]
            .into_iter()
            .collect(),
            test_case_files: BTreeSet::new(),
            shadow_test_files: BTreeSet::new(),
            folder_cap: 15,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert!(matches!(
            moves.first().map(|m| &m.reason),
            Some(MoveReason::PulledBy { partner, .. }) if partner == "src/core/engine.ts"
        ));
        assert_eq!(
            moves.first().map(|m| m.reason.to_string()),
            Some("pulled by engine.ts (w 2.5)".to_owned())
        );
    }

    #[test]
    fn should_explain_a_move_by_naming_cohesion_with_the_destination() {
        // no edges; {user, service} vs {user, service, api} → Jaccard 2/3.
        let current = two_folder_tree(
            &["src/core/user-service-api.ts"],
            &["src/io/user-service.ts"],
        );
        let candidate = two_folder_tree(
            &["src/core/user-service-api.ts", "src/io/user-service.ts"],
            &[],
        );

        let moves = narrate(&current, &candidate, &plain_facts());

        assert!(matches!(
            moves.first().map(|m| &m.reason),
            Some(MoveReason::NamingCohesion { .. })
        ));
        assert_eq!(
            moves.first().map(|m| m.reason.to_string()),
            Some("naming cohesion 0.67 with destination".to_owned())
        );
    }

    #[test]
    fn should_fall_back_to_the_clustering_reason() {
        let current = two_folder_tree(&["src/core/alpha.ts"], &["src/io/zulu.ts"]);
        let candidate = two_folder_tree(&["src/core/alpha.ts", "src/io/zulu.ts"], &[]);

        let moves = narrate(&current, &candidate, &plain_facts());

        assert_eq!(
            moves.first().map(|m| m.reason.clone()),
            Some(MoveReason::Clustering)
        );
        assert_eq!(
            moves.first().map(|m| m.reason.to_string()),
            Some("regrouped by clustering".to_owned())
        );
    }

    #[test]
    fn should_narrate_nothing_for_identical_trees() {
        let tree = two_folder_tree(&["src/core/a.ts"], &["src/io/b.ts"]);

        let moves = narrate(&tree, &tree, &plain_facts());

        assert!(moves.is_empty());
    }
}
