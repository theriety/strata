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

mod namespaces;
mod paths;
mod placements;
mod reason;
mod tokens;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::ContainerTree;

use self::namespaces::preserve_pass_start_namespaces;
use self::placements::{FilePlacements, folder_entry_counts, index_files};
use self::reason::{GroupContext, group_reason};
use crate::result::{FileMove, Move, MoveKind};

pub(crate) use self::namespaces::physical_relocation_folders;
pub(crate) use self::paths::{
    drain_dataset, normalize_physical_path, path_segments, physical_directory, physical_namespace,
    project_package_rooted_path, project_physical_path,
};
#[cfg(test)]
pub(crate) use self::placements::physical_file_folders;
pub(crate) use self::tokens::{basename, tokenize};

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

/// Narrates the moves of `candidate` against `current`, grouped and explained.
///
/// Files present in both trees whose folded parent paths differ are the moved
/// set. Groups key on `(destination folder, followed subject)` so specs
/// trailing different subjects into one folder narrate separately; groups (and
/// the files inside them) come out in deterministic lexicographic order.
/// Each `FileMove` carries a moved *file path* and its source folder — symbol
/// granularity arrives with the pack phase.
#[cfg(test)]
pub(crate) fn narrate(
    current: &ContainerTree,
    candidate: &ContainerTree,
    facts: &FileFacts,
) -> Vec<Move> {
    narrate_with_rootedness(current, candidate, facts, true)
}

pub(crate) fn narrate_repository_relative(
    current: &ContainerTree,
    candidate: &ContainerTree,
    facts: &FileFacts,
) -> Vec<Move> {
    narrate_with_rootedness(current, candidate, facts, false)
}

fn narrate_with_rootedness(
    current: &ContainerTree,
    candidate: &ContainerTree,
    facts: &FileFacts,
    package_rooted: bool,
) -> Vec<Move> {
    let before = index_files(current, package_rooted);
    let mut after = index_files(candidate, package_rooted);
    preserve_pass_start_namespaces(&before, &mut after);
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
                mirrors: Vec::new(),
                blocked_mirrors: Vec::new(),
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
