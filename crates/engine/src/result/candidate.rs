//! Candidate restructures: scoring breakdown and the rendered container tree.

use serde::{Deserialize, Serialize};
use strata_ir::ScopeLevel;

use super::{CapacityRemainder, ConditionalSplit, Move, SymbolMove};

/// One proposed restructure: its rank, score, tree, conditional splits, and the
/// narrated moves relative to the current layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// The 1-based candidate index within its parameter profile.
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
    /// Scored per-symbol relocations accompanying the whole-file moves (FIX08).
    /// Absent when the candidate proposes none, so file-only candidates keep
    /// their exact prior shape on the wire (the FIX10 additive-field precedent;
    /// no schema bump).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol_moves: Vec<SymbolMove>,
    /// The hard capacity findings left in this candidate's tree; present only
    /// when the parameter profile's current standing is `Infeasible`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_remainder: Option<CapacityRemainder>,
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
    /// The scoped over-capacity binding penalty (FIX03).
    pub capacity: f64,
    /// The charge for moves into files containing dependencies but no consumers.
    pub dependency_only: f64,
    /// The charge for companion types separated from their immutable owners.
    pub companion_separation: f64,
}

impl From<strata_core::score::ScoreBreakdown> for ScoreBreakdown {
    fn from(value: strata_core::score::ScoreBreakdown) -> Self {
        Self {
            cut: value.cut,
            imbalance: value.imbalance,
            naming: value.naming,
            path: value.path,
            anchor: value.anchor,
            capacity: value.capacity,
            dependency_only: value.dependency_only,
            companion_separation: value.companion_separation,
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
