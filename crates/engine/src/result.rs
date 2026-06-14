//! The `AnalyzeResult` DTO and its delta narration.
//!
//! This is the contract both faces of Strata share: the library returns an owned
//! [`AnalyzeResult`], and the CLI serializes the very same value as JSON for
//! `--format json`. Serialization is camelCase so the JSON matches the reference
//! `AnalyzeResult` interface byte-for-byte; every nested type carries the same
//! field names the reference documents.
//!
//! [`narrate_delta`] turns a pair of container trees into a human-readable move
//! list: it diffs each symbol's container path between the current tree and a
//! candidate, groups the moved symbols by destination, and attaches the dominant
//! reason per group. The narration is what `diff` and the candidate views render.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use strata_ir::{ContainerId, ContainerTree, ScopeLevel};

/// The grouping key for a reparented root container, which has no destination
/// parent of its own; it sorts last so root moves narrate after parented ones.
const ROOT_DESTINATION: u32 = u32::MAX;

/// The top-level analysis result: the snapshot hash, the current tree with its
/// violations, and the per-mode candidate sets.
///
/// `snapshot_hash` keys a deterministic cache — an identical snapshot and config
/// always produce an identical result. `modes` carries up to one [`ModeResult`]
/// per requested mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    /// The hex blake3 hash of the analyzed snapshot.
    pub snapshot_hash: String,
    /// A coarse census of the analyzed graph.
    pub summary: Summary,
    /// The current layout, its score, and its violations.
    pub current: CurrentTree,
    /// The per-mode candidate sets.
    pub modes: Modes,
}

/// A coarse census of the analyzed snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// Total symbol and type nodes.
    pub symbols: u32,
    /// Total dependency edges.
    pub edges: u32,
    /// Total source files (file-level containers).
    pub files: u32,
    /// File counts keyed by language tag (`ts`, `rs`, `py`).
    pub files_by_language: BTreeMap<String, u32>,
}

/// The current layout, mapped onto the level hierarchy, with its score and
/// violations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentTree {
    /// Today's layout as a nested container tree.
    pub tree: ContainerNode,
    /// The objective `J` of the current tree (the anchor term is zero here).
    pub score: f64,
    /// The per-term decomposition of `score`.
    pub score_breakdown: ScoreBreakdown,
    /// The structural violations of the current layout.
    pub violations: Vec<Violation>,
}

/// The per-mode candidate sets; a mode that was not requested is `None`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Modes {
    /// The anchored-mode result, when anchored was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchored: Option<ModeResult>,
    /// The greenfield-mode result, when greenfield was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub greenfield: Option<ModeResult>,
}

/// One mode's candidate set: up to `k` candidates, their pairwise distances, and
/// the convergence flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeResult {
    /// The returned candidates, best score first.
    pub candidates: Vec<Candidate>,
    /// The variation-of-information matrix over the candidates.
    pub pairwise_distance: Vec<Vec<f64>>,
    /// `true` when fewer than `k` candidates survived diversification.
    pub solution_space_converged: bool,
}

/// One proposed restructure: its rank, score, tree, conditional splits, and the
/// narrated moves relative to the current layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// The 1-based candidate index within its mode.
    pub index: u32,
    /// The total objective `J`; lower is better.
    pub score: f64,
    /// The per-term decomposition of `score`.
    pub score_breakdown: ScoreBreakdown,
    /// The proposed laminar structure.
    pub tree: ContainerNode,
    /// Over-cap SCCs with the break preconditions that make a split legal.
    pub conditional_splits: Vec<ConditionalSplit>,
    /// The explained moves versus the current layout.
    pub delta_narration: Vec<Move>,
}

/// The per-term objective decomposition surfaced in the DTO.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreBreakdown {
    /// The height-weighted cut cost.
    pub cut: f64,
    /// The sibling size-imbalance penalty.
    pub imbalance: f64,
    /// The (negated) naming-cohesion bonus.
    pub naming: f64,
    /// The (negated) path-cohesion bonus.
    pub path: f64,
    /// The move-distance anchoring penalty.
    pub anchor: f64,
}

impl From<strata_core::score::ScoreBreakdown> for ScoreBreakdown {
    fn from(value: strata_core::score::ScoreBreakdown) -> Self {
        Self {
            cut: value.cut,
            imbalance: value.imbalance,
            naming: value.naming,
            path: value.path,
            anchor: value.anchor,
        }
    }
}

/// A node of the rendered container tree.
///
/// Files carry their `symbols` and `production_sloc`; interior containers carry
/// their `children`. The two are mutually exclusive — a file has no children and
/// an interior node has no symbols — but both fields are optional so the shape
/// matches the reference `ContainerNode` exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerNode {
    /// The container's source-declared name.
    pub name: String,
    /// The level this container occupies.
    pub level: Level,
    /// Child containers; absent on files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<ContainerNode>>,
    /// The symbols placed in this file; present on files only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbols: Option<Vec<SymbolPlacement>>,
    /// The production SLOC of this file; present on files only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_sloc: Option<u32>,
}

/// The camelCase scope level used in the DTO.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    /// A single source file.
    File,
    /// A directory of files.
    Folder,
    /// A cohesive functional domain.
    Domain,
    /// A distributable package.
    Package,
    /// A group of related packages.
    PackageGroup,
}

impl From<ScopeLevel> for Level {
    fn from(value: ScopeLevel) -> Self {
        match value {
            ScopeLevel::File => Self::File,
            ScopeLevel::Folder => Self::Folder,
            ScopeLevel::Domain => Self::Domain,
            ScopeLevel::Package => Self::Package,
            ScopeLevel::PackageGroup => Self::PackageGroup,
        }
    }
}

/// A symbol placed in a file, with its declared visibility level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolPlacement {
    /// The symbol's source-declared name.
    pub name: String,
    /// The declared external visibility of the symbol.
    pub visibility: Level,
}

/// The kind of a narrated change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MoveKind {
    /// Symbols relocate from one container to another.
    Move,
    /// One container's symbols divide across several.
    Split,
    /// Several containers' symbols combine into one.
    Merge,
}

/// One narrated change between the current tree and a candidate.
///
/// `symbols` are the affected names; `from` and `to` are the source and
/// destination container paths; `reason` is the dominant driver of the move, and
/// `follows_subject` is set when a spec file trails its subject under test
/// projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    /// Whether this is a move, split, or merge.
    pub kind: MoveKind,
    /// The affected symbol names.
    pub symbols: Vec<String>,
    /// The source container path(s).
    pub from: Vec<String>,
    /// The destination container path(s).
    pub to: Vec<String>,
    /// The dominant reason the change was proposed.
    pub reason: String,
    /// Set on projected spec-file moves naming the subject they follow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follows_subject: Option<String>,
}

/// A structural violation of the current layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Violation {
    /// The violation class.
    pub kind: ViolationKind,
    /// Whether the finding is a hard violation or a borderline observation.
    pub severity: Severity,
    /// The container path(s) involved.
    pub location: Vec<String>,
    /// A human-readable description of the violation.
    pub detail: String,
    /// Edge-break suggestions; present for cycle violations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_suggestions: Option<Vec<EdgeBreak>>,
}

/// The class of a structural violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ViolationKind {
    /// A dependency cycle.
    Cycle,
    /// A polarity-matrix breach (production depending on test code).
    Polarity,
    /// A capacity cap exceeded.
    Capacity,
    /// A declared visibility wider than the derived scope.
    Visibility,
}

/// The severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// A hard violation.
    Violation,
    /// A borderline observation within the tolerance band; never gates CI.
    Borderline,
}

/// An over-cap SCC and the edge breaks that make a split legal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionalSplit {
    /// The member symbol names of the SCC.
    pub scc: Vec<String>,
    /// The edges to break before the split becomes legal.
    pub preconditions: Vec<EdgeBreak>,
    /// The number of files the split yields once the preconditions are met.
    pub resulting_files: u32,
}

/// One edge a cycle break or split precondition cuts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeBreak {
    /// The source symbol name.
    pub source: String,
    /// The target symbol name.
    pub target: String,
    /// The cut weight of the edge.
    pub weight: f64,
    /// `true` when the break is ILP-proven minimal, `false` for a heuristic.
    pub exact: bool,
}

/// Diffs every container's placement between `current` and `candidate` and
/// narrates the moves, grouped by destination container.
///
/// A container whose parent path differs between the two trees is a move; its
/// source is its old parent path and its destination is its new parent path.
/// Moves are grouped by the destination *container* — every unit relocated into
/// the same destination folder narrates as a single coalesced entry, never one
/// entry per moved unit — and each group is stamped with the dominant reason
/// (cohesion gain by default; a caller refines it per group when cap pressure or
/// a cycle break drove the move). The output is ordered by destination container
/// id, so narration is deterministic.
///
/// A container present in only one of the two trees is not narrated here:
/// creation and deletion are not moves, and the surrounding pipeline reports them
/// through splits and merges instead.
#[must_use]
pub fn narrate_delta(current: &ContainerTree, candidate: &ContainerTree) -> Vec<Move> {
    let current_paths = container_paths(current);
    let candidate_paths = container_paths(candidate);
    let candidate_parent = parent_of(candidate);

    // group every relocated container by its DESTINATION container (its parent in
    // the candidate tree) so co-moved units sharing a destination narrate as one
    // entry; a BTreeMap keys the groups in destination-id order. A reparented root
    // (no destination parent) keys under a sentinel so it still narrates.
    let mut groups: BTreeMap<u32, MoveGroup> = BTreeMap::new();
    for (id, destination) in &candidate_paths {
        let Some(origin) = current_paths.get(id) else {
            continue;
        };
        // the move is detected by a changed PARENT path, not the full path: a
        // container keeps its own leaf name, so its parent prefix is what relocates.
        let origin_parent = parent_path(origin);
        let destination_parent = parent_path(destination);
        if origin_parent == destination_parent {
            continue;
        }
        let destination_key = candidate_parent
            .get(id)
            .copied()
            .flatten()
            .map_or(ROOT_DESTINATION, |parent| parent.0);
        let group = groups.entry(destination_key).or_insert_with(|| MoveGroup {
            symbols: Vec::new(),
            from: origin_parent.to_vec(),
            to: destination_parent.to_vec(),
        });
        if let Some(name) = leaf_name(destination) {
            group.symbols.push(name);
        }
    }

    groups
        .into_values()
        .map(|mut group| {
            group.symbols.sort();
            Move {
                kind: MoveKind::Move,
                symbols: group.symbols,
                from: group.from,
                to: group.to,
                reason: "cohesion gain".to_owned(),
                follows_subject: None,
            }
        })
        .collect()
}

/// A set of containers relocated into one destination, accumulated while grouping.
struct MoveGroup {
    /// The names of the moved leaf containers sharing this destination.
    symbols: Vec<String>,
    /// The source (origin parent) container path.
    from: Vec<String>,
    /// The destination (parent) container path.
    to: Vec<String>,
}

/// Returns each container's immediate parent id, keyed by container id; a root
/// container maps to `None`.
fn parent_of(tree: &ContainerTree) -> BTreeMap<ContainerId, Option<ContainerId>> {
    tree.containers()
        .iter()
        .map(|container| (container.id, container.parent))
        .collect()
}

/// Returns the parent prefix of a root-to-node path (every name but the leaf).
fn parent_path(path: &[String]) -> &[String] {
    path.split_last().map_or(path, |(_, prefix)| prefix)
}

/// Returns each container's root-to-node name path, keyed by container id.
///
/// The path is the chain of container names from the root down to the node, so a
/// move shows where a container sat and where it now sits.
fn container_paths(tree: &ContainerTree) -> BTreeMap<ContainerId, Vec<String>> {
    let containers = tree.containers();
    let by_id: BTreeMap<ContainerId, &strata_ir::Container> = containers
        .iter()
        .map(|container| (container.id, container))
        .collect();

    let mut paths = BTreeMap::new();
    for container in containers {
        let mut path = Vec::new();
        let mut cursor = Some(container.id);
        // walk parents to the root, then reverse into a top-down path. the strict
        // level ascent the tree validates guarantees this chain terminates.
        while let Some(id) = cursor {
            let Some(node) = by_id.get(&id) else {
                break;
            };
            path.push(node.name.to_string());
            cursor = node.parent;
        }
        path.reverse();
        paths.insert(container.id, path);
    }
    paths
}

/// Returns the leaf (last) name of a container path, if any.
fn leaf_name(path: &[String]) -> Option<String> {
    path.last().cloned()
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use strata_ir::Container;

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

    #[test]
    fn should_serialize_a_breakdown_as_camel_case() {
        let breakdown = ScoreBreakdown {
            cut: 1.0,
            imbalance: 0.0,
            naming: -0.5,
            path: 0.0,
            anchor: 0.25,
        };

        let json = serde_json::to_string(&breakdown).unwrap_or_default();

        assert!(json.contains("\"cut\":1.0"));
        assert!(json.contains("\"anchor\":0.25"));
    }

    #[test]
    fn should_serialize_package_group_level_as_camel_case() {
        let json = serde_json::to_string(&Level::PackageGroup).unwrap_or_default();

        assert_eq!(json, "\"packageGroup\"");
    }

    #[test]
    fn should_map_every_scope_level_to_its_dto_level() {
        assert_eq!(Level::from(ScopeLevel::File), Level::File);
        assert_eq!(Level::from(ScopeLevel::PackageGroup), Level::PackageGroup);
    }

    #[test]
    fn should_omit_modes_that_were_not_requested() {
        let modes = Modes {
            anchored: None,
            greenfield: None,
        };

        let json = serde_json::to_string(&modes).unwrap_or_default();

        assert_eq!(json, "{}");
    }

    #[test]
    fn should_narrate_no_moves_for_identical_trees() {
        let tree = ContainerTree::new(vec![
            container(0, "pkg", ScopeLevel::Package, None),
            container(1, "file", ScopeLevel::File, Some(0)),
        ]);

        let moves = narrate_delta(&tree, &tree);

        assert!(moves.is_empty());
    }

    #[test]
    fn should_narrate_a_move_when_a_container_reparents() {
        let current = ContainerTree::new(vec![
            container(0, "old", ScopeLevel::Folder, None),
            container(1, "other", ScopeLevel::Folder, None),
            container(2, "leaf", ScopeLevel::File, Some(0)),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "old", ScopeLevel::Folder, None),
            container(1, "other", ScopeLevel::Folder, None),
            container(2, "leaf", ScopeLevel::File, Some(1)),
        ]);

        let moves = narrate_delta(&current, &candidate);

        assert_eq!(moves.len(), 1);
        let entry = moves.first();
        // from/to are the parent (destination) paths, not the moved unit's own path.
        assert_eq!(entry.map(|m| m.from.clone()), Some(vec!["old".to_owned()]));
        assert_eq!(entry.map(|m| m.to.clone()), Some(vec!["other".to_owned()]));
        assert_eq!(
            entry.map(|m| m.symbols.clone()),
            Some(vec!["leaf".to_owned()])
        );
        assert_eq!(entry.map(|m| m.kind), Some(MoveKind::Move));
    }

    #[test]
    fn should_coalesce_moves_sharing_one_destination_into_a_single_entry() {
        // two files relocate from `old` into the SAME destination folder `dest`;
        // the spec requires one grouped Move keyed by the destination, not two.
        let current = ContainerTree::new(vec![
            container(0, "old", ScopeLevel::Folder, None),
            container(1, "dest", ScopeLevel::Folder, None),
            container(2, "alpha", ScopeLevel::File, Some(0)),
            container(3, "beta", ScopeLevel::File, Some(0)),
        ]);
        let candidate = ContainerTree::new(vec![
            container(0, "old", ScopeLevel::Folder, None),
            container(1, "dest", ScopeLevel::Folder, None),
            container(2, "alpha", ScopeLevel::File, Some(1)),
            container(3, "beta", ScopeLevel::File, Some(1)),
        ]);

        let moves = narrate_delta(&current, &candidate);

        assert_eq!(moves.len(), 1);
        let entry = moves.first().cloned().unwrap_or(Move {
            kind: MoveKind::Move,
            symbols: Vec::new(),
            from: Vec::new(),
            to: Vec::new(),
            reason: String::new(),
            follows_subject: None,
        });
        assert_eq!(entry.to, vec!["dest".to_owned()]);
        assert_eq!(entry.from, vec!["old".to_owned()]);
        assert_eq!(
            entry.symbols,
            vec!["alpha".to_owned(), "beta".to_owned()],
            "both relocated files share the one destination entry"
        );
    }

    #[test]
    fn should_skip_containers_absent_from_the_current_tree() {
        let current = ContainerTree::new(vec![container(0, "root", ScopeLevel::Folder, None)]);
        let candidate = ContainerTree::new(vec![
            container(0, "root", ScopeLevel::Folder, None),
            container(1, "new", ScopeLevel::File, Some(0)),
        ]);

        let moves = narrate_delta(&current, &candidate);

        // the new container has no prior path, so it is not a move.
        assert!(moves.is_empty());
    }
}
