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
//! Container names in a tree are *cumulative* path prefixes (`src`,
//! `src/__tests__`, …); [`fold_segments`] collapses a root-to-node chain of them
//! into clean display segments so no rendered path ever repeats a segment.

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{ContainerTree, ScopeLevel};

use crate::result::{Move, MoveKind};

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
    /// The folder member cap, for the cap-relief reason.
    pub(crate) folder_cap: u32,
}

/// Folds a root-to-node chain of cumulative container names into clean display
/// segments.
///
/// Interned container names accumulate their path (`nested-ts`, `src`,
/// `src/__tests__`), and adjacent levels can share one name outright. Each
/// chain entry therefore contributes only what it adds over its predecessor:
/// an exact repeat contributes nothing, a chain entry extending the previous
/// one contributes its new suffix segments, and an unrelated entry contributes
/// all of its own segments.
pub(crate) fn fold_segments(chain: &[String]) -> Vec<String> {
    let mut folded: Vec<String> = Vec::new();
    let mut previous = "";
    for name in chain {
        if name == previous {
            continue;
        }
        let addition = name
            .strip_prefix(previous)
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|_| !previous.is_empty())
            .unwrap_or(name);
        folded.extend(
            addition
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned),
        );
        previous = name;
    }
    folded
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
/// `Move.symbols` carries the moved *file paths* — symbol granularity arrives
/// with the pack phase.
pub(crate) fn narrate(
    current: &ContainerTree,
    candidate: &ContainerTree,
    facts: &FileFacts,
) -> Vec<Move> {
    let before = index_files(current);
    let after = index_files(candidate);

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
        group.files.push(file);
        group.origins.insert(origin);
    }

    groups
        .into_iter()
        .map(|((destination, follows), group)| {
            let kind = group_kind(&group.origins, &dests_of);
            let reason = group_reason(&GroupContext {
                files: &group.files,
                origins: &group.origins,
                destination,
                follows: follows.as_deref(),
                before: &before,
                after: &after,
                facts,
            });
            let mut symbols: Vec<String> = group.files.iter().map(|file| (*file).clone()).collect();
            symbols.sort();
            Move {
                kind,
                symbols,
                from: group
                    .origins
                    .iter()
                    .map(|origin| origin.join("/"))
                    .collect(),
                to: vec![destination.join("/")],
                reason,
                follows_subject: follows,
            }
        })
        .collect()
}

/// The moved files and source folders accumulated for one narration group.
#[derive(Default)]
struct GroupAccumulator<'a> {
    /// The moved file paths in this group.
    files: Vec<&'a String>,
    /// The distinct folded source folders the files left.
    origins: BTreeSet<&'a Vec<String>>,
}

/// Every file's placement within one tree, folded for display.
struct FilePlacements {
    /// Each file path's folded parent path.
    parent_of: BTreeMap<String, Vec<String>>,
    /// The (sorted) member file paths of each folded folder path.
    members_of: BTreeMap<Vec<String>, Vec<String>>,
}

/// Indexes a tree's file containers by their folded parent paths.
fn index_files(tree: &ContainerTree) -> FilePlacements {
    let containers = tree.containers();
    let by_id: BTreeMap<u32, &strata_ir::Container> = containers
        .iter()
        .map(|container| (container.id.0, container))
        .collect();

    let mut parent_of = BTreeMap::new();
    let mut members_of: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
    for container in containers {
        if container.level != ScopeLevel::File {
            continue;
        }
        // ancestor cumulative-name chain, root first; the strict level ascent
        // the tree validates guarantees the walk terminates.
        let mut chain: Vec<String> = Vec::new();
        let mut cursor = container.parent;
        while let Some(id) = cursor {
            let Some(node) = by_id.get(&id.0) else {
                break;
            };
            chain.push(node.name.to_string());
            cursor = node.parent;
        }
        chain.reverse();
        let folded = fold_segments(&chain);
        parent_of.insert(container.name.to_string(), folded.clone());
        members_of
            .entry(folded)
            .or_default()
            .push(container.name.to_string());
    }
    for members in members_of.values_mut() {
        members.sort();
    }
    FilePlacements {
        parent_of,
        members_of,
    }
}

/// Returns the subject a moved spec file follows, when it follows one.
///
/// A spec file's subject is the production file receiving its largest summed
/// outgoing edge weight (ties to the lexicographically smaller path, which the
/// ascending map order yields for free). The move "follows" the subject only
/// when both land in the same candidate folder.
fn followed_subject(
    file: &str,
    destination: &[String],
    after: &FilePlacements,
    facts: &FileFacts,
) -> Option<String> {
    if !facts.test_case_files.contains(file) {
        return None;
    }
    let mut best: Option<(&String, f64)> = None;
    for ((source, target), weight) in &facts.edge_weights {
        if source.as_str() != file || facts.test_case_files.contains(target) {
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
    (after.parent_of.get(subject).map(Vec::as_slice) == Some(destination)).then(|| subject.clone())
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
fn group_reason(ctx: &GroupContext<'_>) -> String {
    if let Some(subject) = ctx.follows {
        return format!("follows {}", basename(subject));
    }

    // cap relief: the first (lexicographically smallest) over-cap source folder.
    for origin in ctx.origins {
        let count = ctx.before.members_of.get(*origin).map_or(0, Vec::len);
        if u32::try_from(count).unwrap_or(u32::MAX) > ctx.facts.folder_cap {
            return format!(
                "relieves over-cap folder {} ({count}/{} files)",
                origin.join("/"),
                ctx.facts.folder_cap
            );
        }
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
        return format!("pulled by {} (w {total:.1})", basename(partner));
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
            return format!("naming cohesion {mean:.2} with destination");
        }
    }

    "regrouped by clustering".to_owned()
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
fn basename(path: &str) -> &str {
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
            folder_cap: 15,
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn should_fold_cumulative_chains_into_clean_segments() {
        let cases: Vec<(Vec<String>, Vec<String>)> = vec![
            (
                strings(&["nested-ts", "src", "src/__tests__", "src/__tests__"]),
                strings(&["nested-ts", "src", "__tests__"]),
            ),
            (strings(&["a"]), strings(&["a"])),
            (Vec::new(), Vec::new()),
            (strings(&["src", "src"]), strings(&["src"])),
            (
                strings(&["repo", "lib/util"]),
                strings(&["repo", "lib", "util"]),
            ),
            (
                strings(&["src", "src/a", "src/a/b"]),
                strings(&["src", "a", "b"]),
            ),
        ];

        for (chain, expected) in cases {
            assert_eq!(fold_segments(&chain), expected, "chain {chain:?}");
        }
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
    fn should_group_files_sharing_a_destination_into_one_move() {
        let current = two_folder_tree(&["src/core/a.ts", "src/core/b.ts"], &[]);
        let candidate = two_folder_tree(&[], &["src/core/a.ts", "src/core/b.ts"]);

        let moves = narrate(&current, &candidate, &plain_facts());

        assert_eq!(moves.len(), 1);
        let entry = moves.first();
        assert_eq!(
            entry.map(|m| m.symbols.clone()),
            Some(strings(&["src/core/a.ts", "src/core/b.ts"]))
        );
        assert_eq!(
            entry.map(|m| m.from.clone()),
            Some(strings(&["app/src/core"]))
        );
        assert_eq!(entry.map(|m| m.to.clone()), Some(strings(&["app/src/io"])));
        assert_eq!(entry.map(|m| m.kind), Some(MoveKind::Move));
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
        assert_eq!(
            entry.map(|m| m.from.clone()),
            Some(strings(&["app/src/core", "app/src/io"]))
        );
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
            folder_cap: 15,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert_eq!(moves.len(), 1);
        let entry = moves.first();
        assert_eq!(
            entry.and_then(|m| m.follows_subject.clone()),
            Some("src/core/app.ts".to_owned())
        );
        assert_eq!(
            entry.map(|m| m.reason.clone()),
            Some("follows app.ts".to_owned())
        );
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
            folder_cap: 3,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert_eq!(
            moves.first().map(|m| m.reason.clone()),
            Some("relieves over-cap folder app/src/core (4/3 files)".to_owned())
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
            folder_cap: 15,
        };

        let moves = narrate(&current, &candidate, &facts);

        assert_eq!(
            moves.first().map(|m| m.reason.clone()),
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

        assert_eq!(
            moves.first().map(|m| m.reason.clone()),
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
