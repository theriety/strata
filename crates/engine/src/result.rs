//! The `AnalyzeResult` DTO.
//!
//! This is the contract both faces of Strata share: the library returns an owned
//! [`AnalyzeResult`], and the CLI serializes the very same value as JSON for
//! `--format json`. Serialization is camelCase so the JSON matches the reference
//! `AnalyzeResult` interface byte-for-byte; every nested type carries the same
//! field names the reference documents.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use strata_ir::ScopeLevel;

/// The schema version stamped into every [`AnalyzeResult`] this crate produces.
///
/// Readers of a saved result reject any other version rather than misreading a
/// future shape.
pub const RESULT_SCHEMA_VERSION: u32 = 2;

/// The top-level analysis result: the snapshot hash, the current tree with its
/// violations, and the per-mode candidate sets.
///
/// `snapshot_hash` keys a deterministic cache — an identical snapshot and config
/// always produce an identical result. `modes` carries up to one [`ModeResult`]
/// per requested mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    /// The result schema version ([`RESULT_SCHEMA_VERSION`]); a saved result
    /// with any other version is rejected on read. Defaults to `0` (unsupported)
    /// when absent, so pre-versioned files fail with a clear message.
    #[serde(default)]
    pub schema_version: u32,
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

/// One mode's candidate set: up to `k` candidates, their pairwise distances,
/// the convergence flag, and where the current layout stands against them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeResult {
    /// The returned candidates, best score first.
    pub candidates: Vec<Candidate>,
    /// The variation-of-information matrix over the candidates.
    pub pairwise_distance: Vec<Vec<f64>>,
    /// `true` when fewer than `k` candidates survived diversification.
    pub solution_space_converged: bool,
    /// The current layout's objective `J` under THIS mode's coefficients.
    pub current_score: f64,
    /// The per-term decomposition of `current_score`.
    pub current_score_breakdown: ScoreBreakdown,
    /// Where the current layout stands relative to the candidates.
    pub current_standing: CurrentStanding,
    /// The hard capacity findings left in the best candidate's tree; present
    /// only when `current_standing` is `Infeasible`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_candidate_capacity: Option<CapacityRemainder>,
}

/// What the best candidate leaves unresolved of the current capacity breaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityRemainder {
    /// Hard capacity findings remaining in the best candidate's tree.
    pub remaining: u32,
    /// How many of `remaining` are file-level breaches, which no move can fix —
    /// only conditional splits can.
    pub file_level: u32,
}

/// Where the current layout stands relative to a mode's candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CurrentStanding {
    /// No candidate beats the current layout; candidate 1 IS the current tree.
    Optimal,
    /// At least one candidate scores better than the current layout.
    Outscored,
    /// The current layout breaches a capacity cap, so it cannot compete;
    /// candidates fix the breach at whatever cut cost they carry.
    Infeasible,
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
    /// `current_score − score`; positive means the candidate beats current.
    pub improvement: f64,
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn should_serialize_current_standing_as_camel_case() {
        let json = serde_json::to_string(&CurrentStanding::Outscored).unwrap_or_default();

        assert_eq!(json, "\"outscored\"");
    }
}
