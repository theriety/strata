//! Pure metric functions over [`ContainerNode`] trees.
//!
//! Everything here is total: no panics, no indexing, no engine calls. The
//! harness composes these into verdicts; the unit tests at the bottom pin each
//! rule CONTRACT.md states so a semantic drift in the harness itself is caught
//! independently of strata's behavior.

use std::collections::{BTreeMap, BTreeSet};

use strata_engine::result::{ContainerNode, Level};

/// Splits a container key or path into its `/`-separated segments.
///
/// The last segment of a full-prefix key is what rendering collapses to, so
/// bucket-name checks and `container =` selectors compare against this.
#[must_use]
pub fn last_segment(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Tokenizes per CONTRACT.md: lowercase; split on `/`, `_`, `-`, `.` and
/// camelCase humps; drop empty and all-digit tokens. A hump splits before an
/// uppercase letter that follows a lowercase letter or digit; acronym runs stay
/// one token (`JSONBlob` → `jsonblob`).
#[must_use]
pub fn tokenize(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut previous_kind = CharKind::Start;
    for character in raw.chars() {
        let kind = CharKind::of(character);
        if kind == CharKind::Separator {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            previous_kind = kind;
            continue;
        }
        // A hump boundary: uppercase (or any case change upward from a digit)
        // following a lowercase letter or digit starts a new token.
        let hump = matches!(kind, CharKind::Upper)
            && matches!(previous_kind, CharKind::Lower | CharKind::Digit);
        if hump && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        current.push(character.to_ascii_lowercase());
        previous_kind = kind;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
        .into_iter()
        .filter(|token| !(token.is_empty() || token.chars().all(|c| c.is_ascii_digit())))
        .collect()
}

/// The character classes [`tokenize] splits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Start,
    Lower,
    Upper,
    Digit,
    Separator,
}

impl CharKind {
    fn of(character: char) -> Self {
        if character.is_ascii_lowercase() || !character.is_ascii() && character.is_lowercase() {
            Self::Lower
        } else if character.is_ascii_uppercase()
            || !character.is_ascii() && character.is_uppercase()
        {
            Self::Upper
        } else if character.is_ascii_digit() {
            Self::Digit
        } else if matches!(character, '/' | '_' | '-' | '.') {
            Self::Separator
        } else {
            // Any other byte (rare in identifiers) rides along inside a token.
            Self::Lower
        }
    }
}

/// Collects the names of every file-level descendant, transitively.
///
/// This is the census form: universes and placement initialization need every
/// file in a subtree regardless of where it nests. Structural predicates never
/// count with this — they use [`direct_members`], because a folder's own
/// children are its membership and nested descendants belong to their own
/// places.
#[must_use]
pub fn members(node: &ContainerNode) -> Vec<String> {
    let mut files = Vec::new();
    collect_members(node, &mut files);
    files
}

fn collect_members(node: &ContainerNode, files: &mut Vec<String>) {
    if node.level == Level::File {
        files.push(node.name.clone());
        return;
    }
    for child in node.children.iter().flatten() {
        collect_members(child, files);
    }
}

/// Collects the names of a container's DIRECT file children only.
///
/// This is CONTRACT.md's counting rule: when tallying what a folder holds,
/// only its first level counts — nested sub-containers contribute nothing
/// because their files already belong to their own place. Splitting an
/// over-cap folder into halves that nest under it therefore reads as two
/// within-band places, not one still-over-cap umbrella.
#[must_use]
pub fn direct_members(node: &ContainerNode) -> Vec<String> {
    node.children
        .iter()
        .flatten()
        .filter(|child| child.level == Level::File)
        .map(|child| child.name.clone())
        .collect()
}

/// Whether a node participates in structural assertions: folder/domain only.
/// Package and packageGroup envelopes are exempt per the Container scope
/// principle.
#[must_use]
pub fn is_scoped_container(node: &ContainerNode) -> bool {
    matches!(node.level, Level::Folder | Level::Domain)
}

/// Walks `root`, applying `visit` to every non-file node with its chain of
/// scoped-container names (folder/domain ancestors within the current package,
/// `/`-joined). Package/packageGroup nodes reset nothing but are never visited.
pub fn walk_containers<'a>(
    root: &'a ContainerNode,
    visit: &mut dyn FnMut(&'a ContainerNode, &str),
) {
    walk_inner(root, &Vec::new(), visit);
}

/// Walks `root`, applying `visit` to every file node. The complement of
/// [`walk_containers`] — that walker deliberately skips files so structural
/// assertions see only scoped containers; symbol-grain assertions need the
/// files themselves, wherever the tree nests them.
pub fn walk_files<'a>(root: &'a ContainerNode, visit: &mut dyn FnMut(&'a ContainerNode)) {
    if root.level == Level::File {
        visit(root);
    }
    for child in root.children.iter().flatten() {
        walk_files(child, visit);
    }
}

fn walk_inner<'a>(
    node: &'a ContainerNode,
    chain: &[String],
    visit: &mut dyn FnMut(&'a ContainerNode, &str),
) {
    match node.level {
        Level::File => {}
        Level::Folder | Level::Domain => {
            let mut next = chain.to_vec();
            next.push(node.name.clone());
            let joined = next.join("/");
            visit(node, &joined);
            for child in node.children.iter().flatten() {
                walk_inner(child, &next, visit);
            }
        }
        Level::Package | Level::PackageGroup => {
            // Envelopes reset the scoped chain: a package is not a folder-level
            // ancestor of its members' structural container paths.
            for child in node.children.iter().flatten() {
                walk_inner(child, &[], visit);
            }
        }
    }
}

/// Maps every file under `root` to its folder/domain container path.
///
/// A loose file (directly under a package or the group root) has the empty
/// path. This is the structural placement move counting reads; narration never
/// feeds it.
#[must_use]
pub fn structural_placement(root: &ContainerNode) -> BTreeMap<String, String> {
    // Total over the census: every file starts loose (empty path) and gains
    // its container chain when its immediate folder/domain claims it.
    let mut placements: BTreeMap<String, String> = members(root)
        .into_iter()
        .map(|file| (file, String::new()))
        .collect();
    let mut visit = |node: &ContainerNode, path: &str| {
        for member in direct_members(node) {
            placements.insert(member, path.to_owned());
        }
    };
    walk_containers(root, &mut visit);
    placements
}

/// Counts moved FILES structurally: files whose folder/domain container path
/// differs between the two trees. Files present in only one tree count as
/// moved; a loose file's placement is the empty string.
#[must_use]
pub fn moved_files(
    current: &BTreeMap<String, String>,
    candidate: &BTreeMap<String, String>,
) -> Vec<String> {
    let keys: BTreeSet<&String> = current.keys().chain(candidate.keys()).collect();
    keys.into_iter()
        .filter(|file| current.get(*file) != candidate.get(*file))
        .cloned()
        .collect()
}

/// Builds the unordered co-membership pair set over `universe` induced by the
/// folder/domain nodes of `root`. Envelopes are excluded, mirroring
/// `separate`; pairs form among a container's DIRECT file children only, so an
/// ancestor never welds its descendants' files into one group; pairs are
/// normalized `(smaller, larger)` tuples.
#[must_use]
pub fn co_membership_pairs(
    root: &ContainerNode,
    universe: &BTreeSet<String>,
) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    let mut visit = |node: &ContainerNode, _path: &str| {
        let contained: Vec<String> = direct_members(node)
            .into_iter()
            .filter(|file| universe.contains(file))
            .collect();
        for (index, left) in contained.iter().enumerate() {
            for right in contained.iter().skip(index + 1) {
                pairs.insert(normalize_pair(left, right));
            }
        }
    };
    walk_containers(root, &mut visit);
    pairs
}

/// Normalizes an unordered pair so set comparisons never depend on order.
#[must_use]
fn normalize_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

/// Pair-F1 over reference and predicted co-membership sets; zero guards return
/// 1.0 for an empty denominator on both sides (vacuous agreement) and 0.0 when
/// exactly one side is empty.
#[must_use]
pub fn pair_f1(
    reference: &BTreeSet<(String, String)>,
    predicted: &BTreeSet<(String, String)>,
) -> (f64, f64, f64) {
    let hits = reference.intersection(predicted).count();
    let precision = ratio(hits, predicted.len());
    let recall = ratio(hits, reference.len());
    let f1 = if precision + recall <= f64::EPSILON {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    (precision, recall, f1)
}

/// An overflow-safe count ratio; 0/0 is vacuous agreement (1.0).
#[must_use]
fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 1.0;
    }
    // reason: pair counts are tiny corpus sizes; the f64 mantissa loses nothing
    #[allow(clippy::cast_precision_loss)]
    {
        numerator as f64 / denominator as f64
    }
}

/// Counts how many of `member_files` share at least one token with the
/// container's own tokens; `(aligned, total)`.
///
/// Per CONTRACT.md's tokenization scope: a file contributes only its basename
/// minus extension (`lib/x/format.py` contributes `format`), while a container
/// contributes its last segment only. A member's directory prefix never counts
/// toward alignment. Callers that need a formatted "3/5" read the counts
/// directly instead of re-deriving them from the float fraction.
#[must_use]
pub fn alignment_counts(container_name: &str, member_files: &[String]) -> (usize, usize) {
    let total = member_files.len();
    if total == 0 {
        return (0, 0);
    }
    let container_tokens: BTreeSet<String> =
        tokenize(last_segment(container_name)).into_iter().collect();
    let aligned = member_files
        .iter()
        .filter(|file| {
            let base = file.rsplit('/').next().unwrap_or(file.as_str());
            let stem = base.split('.').next().unwrap_or(base);
            tokenize(stem)
                .iter()
                .any(|token| container_tokens.contains(token))
        })
        .count();
    (aligned, total)
}

/// Fraction of `member_files` sharing at least one token with the container's
/// own tokens (its last segment tokenized); empty sets align vacuously.
#[must_use]
pub fn alignment_sharing(container_name: &str, member_files: &[String]) -> f64 {
    let (aligned, total) = alignment_counts(container_name, member_files);
    if total == 0 {
        return 1.0;
    }
    // reason: member counts are tiny corpus sizes; the mantissa loses nothing
    #[allow(clippy::cast_precision_loss)]
    {
        aligned as f64 / total as f64
    }
}

/// A tree's placement signature: every file mapped to its structural container
/// path, sorted. Candidate-distinctness compares these.
#[must_use]
pub fn placement_signature(root: &ContainerNode) -> Vec<(String, String)> {
    structural_placement(root).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level: Level::File,
            children: None,
            symbols: None,
            production_sloc: None,
        }
    }

    fn container(level: Level, name: &str, children: Vec<ContainerNode>) -> ContainerNode {
        ContainerNode {
            name: name.to_owned(),
            level,
            children: Some(children),
            symbols: None,
            production_sloc: None,
        }
    }

    #[test]
    fn tokenizer_splits_humps_separators_and_drops_digit_tokens() {
        assert_eq!(tokenize("JSONBlob"), vec!["jsonblob".to_owned()]);
        assert_eq!(
            tokenize("string_utils"),
            vec!["string".to_owned(), "utils".to_owned()]
        );
        assert_eq!(
            tokenize("charge-calculator.v2"),
            vec![
                "charge".to_owned(),
                "calculator".to_owned(),
                "v2".to_owned()
            ]
        );
        assert_eq!(tokenize("camelCaseName"), vec!["camel", "case", "name"]);
        assert_eq!(tokenize("v2"), vec!["v2".to_owned()]);
        assert_eq!(tokenize("2024"), Vec::<String>::new());
        assert_eq!(tokenize("__tests__"), vec!["tests".to_owned()]);
        assert_eq!(
            tokenize("invoice2parser"),
            vec!["invoice2parser".to_owned()]
        );
    }

    #[test]
    fn last_segment_reads_the_leaf_of_a_full_prefix_key() {
        assert_eq!(last_segment("ai/adapters/openai"), "openai");
        assert_eq!(last_segment("billing"), "billing");
    }

    #[test]
    fn members_collects_transitive_file_names() {
        let tree = container(
            Level::PackageGroup,
            "root",
            vec![container(
                Level::Package,
                "app",
                vec![container(
                    Level::Domain,
                    "billing",
                    vec![file("billing/invoice.py"), file("billing/pricing.py")],
                )],
            )],
        );
        assert_eq!(
            members(&tree),
            vec![
                "billing/invoice.py".to_owned(),
                "billing/pricing.py".to_owned()
            ]
        );
    }

    #[test]
    fn direct_members_counts_only_the_first_level() {
        let tree = container(
            Level::PackageGroup,
            "root",
            vec![container(
                Level::Domain,
                "hub",
                vec![
                    file("hub/emit_00.py"),
                    container(
                        Level::Folder,
                        "ingest",
                        vec![file("hub/ingest/ingest_00.py")],
                    ),
                ],
            )],
        );
        // The nested ingest half contributes nothing to hub's tally: a
        // folder's membership is its own children, never its descendants'.
        let members = tree
            .children
            .as_deref()
            .and_then(<[_]>::first)
            .map(direct_members);
        assert_eq!(
            members,
            Some(vec!["hub/emit_00.py".to_owned()]),
            "only hub's own file counts; the nested folder is invisible"
        );
    }

    #[test]
    fn structural_placement_maps_files_and_leaves_loose_files_empty() {
        let tree = container(
            Level::PackageGroup,
            "root",
            vec![container(
                Level::Package,
                "app",
                vec![
                    container(Level::Domain, "billing", vec![file("billing/invoice.py")]),
                    file("loose.py"),
                ],
            )],
        );
        let placement = structural_placement(&tree);
        assert_eq!(
            placement.get("billing/invoice.py").map(String::as_str),
            Some("billing")
        );
        assert_eq!(placement.get("loose.py").map(String::as_str), Some(""));
    }

    #[test]
    fn envelope_chain_resets_so_package_paths_do_not_leak_into_placements() {
        let tree = container(
            Level::PackageGroup,
            "root",
            vec![container(
                Level::Package,
                "atlas",
                vec![container(
                    Level::Folder,
                    "store",
                    vec![file("atlas/src/store/kit.ts")],
                )],
            )],
        );
        let placement = structural_placement(&tree);
        assert_eq!(
            placement.get("atlas/src/store/kit.ts").map(String::as_str),
            Some("store")
        );
    }

    #[test]
    fn moved_files_counts_only_placement_changes() {
        let before = container(
            Level::Package,
            "app",
            vec![
                container(Level::Folder, "a", vec![file("x.py")]),
                container(Level::Folder, "b", vec![file("y.py")]),
            ],
        );
        let after = container(
            Level::Package,
            "app",
            vec![container(
                Level::Folder,
                "b",
                vec![file("x.py"), file("y.py")],
            )],
        );
        let moved = moved_files(
            &structural_placement(&before),
            &structural_placement(&after),
        );
        assert_eq!(moved, vec!["x.py".to_owned()]);
    }

    #[test]
    fn co_membership_pairs_exclude_envelopes_and_normalize_order() {
        let universe: BTreeSet<String> = ["a.py", "b.py", "c.py"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let tree = container(
            Level::PackageGroup,
            "root",
            vec![container(
                Level::Package,
                "app",
                vec![container(
                    Level::Domain,
                    "d",
                    vec![file("a.py"), file("b.py"), file("c.py")],
                )],
            )],
        );
        let pairs = co_membership_pairs(&tree, &universe);
        assert_eq!(pairs.len(), 3);
        assert!(pairs.contains(&("a.py".to_owned(), "c.py".to_owned())));
    }

    #[test]
    fn pair_f1_scores_a_perfect_split_against_a_single_reference_group() {
        let universe: BTreeSet<String> = ["a.py", "b.py", "c.py"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let grouped = container(
            Level::Package,
            "app",
            vec![container(
                Level::Domain,
                "d",
                vec![file("a.py"), file("b.py"), file("c.py")],
            )],
        );
        let split = container(
            Level::Package,
            "app",
            vec![
                container(Level::Domain, "one", vec![file("a.py"), file("b.py")]),
                container(Level::Domain, "two", vec![file("c.py")]),
            ],
        );
        let reference = co_membership_pairs(&grouped, &universe);
        let predicted = co_membership_pairs(&split, &universe);
        // Reference keeps all three pairs; the split keeps only (a,b), so
        // precision is hits/predicted = 1/1 and recall is hits/reference = 1/3.
        let (precision, recall, f1) = pair_f1(&reference, &predicted);
        assert!((precision - 1.0).abs() < 1e-12);
        assert!((recall - 1.0 / 3.0).abs() < 1e-12);
        assert!((f1 - 0.5).abs() < 1e-12);
        // And the identical tree scores a perfect 1.0.
        let (_, _, perfect) = pair_f1(&reference, &reference);
        assert!((perfect - 1.0).abs() < 1e-12);
    }

    #[test]
    fn pair_f1_handles_both_sides_empty_as_agreement() {
        let empty = BTreeSet::new();
        let (precision, recall, f1) = pair_f1(&empty, &empty);
        assert!((precision - 1.0).abs() < 1e-12);
        assert!((recall - 1.0).abs() < 1e-12);
        assert!((f1 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn alignment_sharing_counts_token_overlap_from_basenames_only() {
        let members = [
            "lib/string_utils.py".to_owned(),
            "src/string_utils/wrap.rs".to_owned(),
            "app/string_format.py".to_owned(),
            "billing/charge.py".to_owned(),
        ];
        // Only basenames participate: wrap.rs lives under string_utils/ but
        // its basename carries none of the container's tokens.
        assert_eq!(alignment_counts("string_utils", &members), (2, 4));
        let sharing = alignment_sharing("string_utils", &members);
        assert!((sharing - 0.5).abs() < 1e-12);
    }

    #[test]
    fn placement_signature_is_stable_across_equal_trees() {
        let build = || {
            container(
                Level::Package,
                "app",
                vec![container(Level::Folder, "a", vec![file("x.py")])],
            )
        };
        assert_eq!(placement_signature(&build()), placement_signature(&build()));
    }
}
